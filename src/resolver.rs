use crate::{error::Error, Node};
use std::collections::{HashMap, HashSet};
use std::ops;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, RwLock,
};
use std::thread;
use std::time::Duration;
use tracing::{debug, instrument, span, trace, Level};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

type DependencySet<I> = Arc<RwLock<HashMap<I, HashSet<I>>>>;

/// Dependency graph resolver
#[derive(Clone, Debug)]
pub struct Resolver<N: Node> {
    timeout: Duration,

    counter: Arc<AtomicUsize>,
    ready_nodes: Vec<N::Inner>,
    /// Map of nodes to their dependencies
    deps: DependencySet<N::Inner>,
    /// Map of nodes to their reverse dependencies.
    ///
    /// Having a map of reverse dependencies speed up the process of looking up
    /// for nodes that are available. When a node is processed, we can look up
    /// its reverse dependencies directly and see which ones were only
    /// depending on that node.
    rdeps: DependencySet<N::Inner>,
    // nodes: Arc<RwLock<HashMap<N::Inner, N>>>,
}

impl<N: Node> Resolver<N> {
    #[instrument(skip(nodes))]
    pub fn new(nodes: &[N]) -> Resolver<N> {
        // Create ready channels
        // These need to be built in advance as processing the nodes will
        // create messages on the channel.

        // Create the resolver
        let mut resolver = Resolver {
            timeout: DEFAULT_TIMEOUT,

            counter: Default::default(),
            ready_nodes: Default::default(),
            // The capacity for deps and rdeps should be equal to the number of
            // nodes.
            deps: Arc::new(RwLock::new(HashMap::with_capacity(nodes.len()))),
            rdeps: Arc::new(RwLock::new(HashMap::with_capacity(nodes.len()))),
        };

        // Inject nodes
        nodes.iter().for_each(|node| resolver.push(node));

        // Return the resolver
        resolver
    }

    #[instrument(skip(self, node))]
    fn push(&mut self, node: &N) {
        // Insert the node in the dependencies map
        {
            self.deps
                .write()
                .unwrap()
                .insert(node.id().clone(), node.deps().clone());
        }

        // The node is immediately available. We can send it on the channel
        // and can skip
        if node.deps().is_empty() {
            self.ready_nodes.push(node.id().clone());
            return;
        }

        // Insert reverse dependencies of the node
        let mut rdeps = self.rdeps.write().unwrap();
        for node_dep in node.deps() {
            if !(*rdeps).contains_key(node_dep) {
                // If the reverse dependency does not exist, create the HashSet
                // in rdeps. Before processing, rdeps should not be bigger than
                // deps.
                let mut dep_rdeps = HashSet::new();
                dep_rdeps.insert(node.id().clone());
                rdeps.insert(node_dep.clone(), dep_rdeps.clone());
            } else {
                let dep_rdeps = rdeps.get_mut(node_dep).unwrap();
                dep_rdeps.insert(node.id().clone());
            }
        }
    }

    /// Sequential iteration over the list of dependencies, in correct
    /// order.
    pub fn for_each<F>(&self, mut func: F) -> Result<(), Error>
    where
        F: ops::FnMut(&N::Inner),
    {
        // Prepare communication channels
        let (node_ready_tx, node_ready_rx) = crossbeam_channel::unbounded::<N::Inner>();

        // Populate channel
        self.ready_nodes
            .iter()
            .for_each(|id| node_ready_tx.send(id.clone()).unwrap());

        loop {
            let span = span!(Level::INFO, "map_loop");
            let _enter = span.enter();
            debug!("Start next iteration");

            // Grab a node ID for work
            let id = match get_next_id::<N>(&node_ready_rx, self.timeout, &self.counter, &self.deps)
            {
                Ok(id) => id,
                Err(Error::EmptyListError) => return Ok(()),
                Err(Error::NoAvailableNodeError) => continue,
                Err(err) => return Err(err),
            };

            // Do work
            func(&id);

            // Remove the node from all reverse dependencies
            let next_nodes = remove_node_id::<N>(id, &self.deps, &self.rdeps)?;

            // Send the next available nodes to the channel.
            next_nodes
                .iter()
                .for_each(|node_id| node_ready_tx.send(node_id.clone()).unwrap());

            // If there are no more nodes, leave the loop
            if self.deps.read().unwrap().is_empty() {
                break;
            }
        }

        Ok(())
    }

    /// Parallel iteration over the list of dependencies, in correct
    /// order.
    pub fn par_for_each<F>(&self, func: &'static F) -> Result<(), Error>
    where
        F: Fn(&N::Inner) + Send + Sync + 'static,
    {
        // Create communication channel for processed nodes
        let (node_ready_tx, node_ready_rx) = crossbeam_channel::unbounded::<N::Inner>();
        let (node_done_tx, node_done_rx) = crossbeam_channel::unbounded::<N::Inner>();

        // Populate channel
        self.ready_nodes
            .iter()
            .for_each(|id| node_ready_tx.send(id.clone()).unwrap());

        // Start worker threads
        let handles: Vec<thread::JoinHandle<Result<(), Error>>> = (0..num_cpus::get())
            .map(|i| -> thread::JoinHandle<Result<(), Error>> {
                // Clone data for injection in the threads
                let deps = self.deps.clone();
                let node_ready_rx = node_ready_rx.clone();
                let node_done_tx = node_done_tx.clone();
                let counter = self.counter.clone();
                let timeout = self.timeout;

                // Spawn worker loop
                thread::spawn(move || {
                    let worker_id = i;
                    loop {
                        let span = span!(Level::DEBUG, "worker_loop", id = worker_id);
                        let _enter = span.enter();
                        debug!("Start next iteration for worker {}", worker_id);

                        // Grab a node ID for work
                        let id = match get_next_id::<N>(&node_ready_rx, timeout, &counter, &deps) {
                            Ok(id) => id,
                            Err(Error::EmptyListError) => return Ok(()),
                            Err(Error::NoAvailableNodeError) => continue,
                            Err(err) => return Err(err),
                        };

                        // Increment the counter
                        counter.fetch_add(1, Ordering::SeqCst);

                        // Do work
                        func(&id);

                        // Send confirmation that the node is processed
                        node_done_tx.send(id).unwrap();

                        // Decrement the counter
                        counter.fetch_sub(1, Ordering::SeqCst);

                        // If there are no more nodes, leave the loop
                        if deps.read().unwrap().is_empty() {
                            break;
                        }
                    }

                    Ok(())
                })
            })
            .collect();

        // Listens on processed node and remove their information
        loop {
            // Grab a processed node ID
            let id = match get_next_id::<N>(&node_done_rx, self.timeout, &self.counter, &self.deps)
            {
                Ok(id) => id,
                Err(Error::EmptyListError) => break,
                Err(Error::NoAvailableNodeError) => continue,
                Err(err) => return Err(err),
            };

            // Remove the node from all reverse dependencies
            let next_nodes = remove_node_id::<N>(id, &self.deps, &self.rdeps)?;

            // Send the next available nodes to the channel.
            next_nodes
                .iter()
                .for_each(|node_id| node_ready_tx.send(node_id.clone()).unwrap());

            // If there are no more nodes, leave the loop
            if self.deps.read().unwrap().is_empty() {
                break;
            }
        }

        // Drop channel
        drop(node_ready_tx);

        // Wait for threads to close
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        Ok(())
    }
}

/// Retrieve a node from a channel
///
/// # Errors
///
/// This can return one of the follow values from [`depgraph::error::Error`]:
///
/// * `EmptyListError`: there are no more dependencies to process, therefore
///   we cannot retrieve a node ID.
/// * `NoAvailableNodeError`: there are currently no available nodes, but there
///   might be in the future (retriable error).
/// * `ResolveGraphError`: there is probably a circular dependency in the list
///   of nodes, therefore it's not possible to retrieve a new node.
fn get_next_id<N: Node>(
    node_rx: &crossbeam_channel::Receiver<N::Inner>,
    timeout: Duration,
    counter: &Arc<AtomicUsize>,
    deps: &DependencySet<N::Inner>,
) -> Result<N::Inner, Error> {
    let span = span!(Level::TRACE, "next_node_id");
    let _enter = span.enter();
    trace!("Fetching next node ID");

    match node_rx.recv_timeout(timeout) {
        // Node received
        Ok(id) => Ok(id),
        // No node received. However, this could be for
        // multiple reasons:
        //
        // * There are no more nodes to process and we
        //   are done.
        // * Some nodes are still being processed and
        //   we need to wait until they are done.
        // * No nodes are being processed and we have a
        //   circular dependency.
        Err(_) => {
            let deps = deps.read().unwrap();
            let counter_val = counter.load(Ordering::SeqCst);
            if deps.is_empty() {
                Err(Error::EmptyListError)
            } else if counter_val > 0 {
                Err(Error::NoAvailableNodeError)
            } else {
                Err(Error::ResolveGraphError("circular dependency detected"))
            }
        }
    }
}

/// Remove all references to the node ID in the dependencies.
///
fn remove_node_id<N: Node>(
    id: N::Inner,
    deps: &DependencySet<N::Inner>,
    rdeps: &DependencySet<N::Inner>,
) -> Result<Vec<N::Inner>, Error> {
    let rdep_ids = {
        let span = span!(Level::TRACE, "remove_node_id");
        let _enter = span.enter();
        trace!("Fetching reverse dependencies for {}", id);

        match rdeps.read().unwrap().get(&id) {
            Some(node) => node.clone(),
            // If no node depends on a node, it will not appear
            // in rdeps.
            None => Default::default(),
        }
    };

    let span = span!(Level::TRACE, "remove_deps");
    let _enter = span.enter();
    trace!("Removing {} from reverse dependencies", id);

    let mut deps = deps.write().unwrap();
    let next_nodes = rdep_ids
        .iter()
        .filter_map(|rdep_id| {
            let rdep = match deps.get_mut(&rdep_id) {
                Some(rdep) => rdep,
                None => {
                    println!("{:?}", deps);
                    return None;
                }
            };

            rdep.remove(&id);

            if rdep.is_empty() {
                Some(rdep_id.clone())
            } else {
                None
            }
        })
        .collect();

    // Remove the current node from the list of dependencies.
    deps.remove(&id);

    Ok(next_nodes)
}