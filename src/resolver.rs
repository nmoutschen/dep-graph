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
}

/// Implementation to transform the resolver into a sequential iterator
impl<N: Node> IntoIterator for Resolver<N> {
    type Item = N::Inner;
    type IntoIter = ResolverIter<N>;

    fn into_iter(self) -> Self::IntoIter {
        ResolverIter::new(&self)
    }
}

/// Implementation to transform the resolver into a parallel iterator
impl<N: Node> Resolver<N> {
    pub fn into_par_iter(self) -> ResolverParIter<N> {
        ResolverParIter::new(&self)
    }
}

/// Sequential iterator for a dependency graph
pub struct ResolverIter<N: Node> {
    /// List of nodes available for processing.
    ready_nodes: Vec<N::Inner>,
    /// Map of nodes to their dependencies.
    deps: DependencySet<N::Inner>,
    /// Map of nodes to their reverse dependencies.
    rdeps: DependencySet<N::Inner>,
}

impl<N: Node> ResolverIter<N> {
    pub fn new(resolver: &Resolver<N>) -> ResolverIter<N> {
        ResolverIter {
            ready_nodes: resolver.ready_nodes.clone(),
            deps: resolver.deps.clone(),
            rdeps: resolver.rdeps.clone(),
        }
    }
}

impl<N: Node> Iterator for ResolverIter<N> {
    type Item = N::Inner;

    /// Advances the iterator and returns the next value.
    /// 
    /// This assumes that the graph does not contain any circular dependencies.
    /// If there is a circular dependency in the graph, this will return None
    /// when all dependencies that _could_ be processed have been processed,
    /// but there might still be some unprocessed dependencies.
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(id) = self.ready_nodes.pop() {
            // Remove dependencies and retrieve next available nodes, if any.
            let next_nodes = remove_node_id::<N>(
                id.clone(),
                &self.deps,
                &self.rdeps,
            ).unwrap();

            // Push ready nodes
            self.ready_nodes.extend_from_slice(&next_nodes);

            // Return the node ID
            Some(id)
        } else {
            // No available node
            None
        }
    }
}

/// Parallel iterator for a dependency graph
pub struct ResolverParIter<N: Node> {
    timeout: Duration,

    node_ready_rx: crossbeam_channel::Receiver<N::Inner>,
    node_done_tx: crossbeam_channel::Sender<N::Inner>,

    _drop_tx: crossbeam_channel::Sender<bool>,

    counter: Arc<AtomicUsize>,
    /// Map of nodes to their dependencies
    deps: DependencySet<N::Inner>,
}

impl<N: Node> ResolverParIter<N> {
    pub fn new(resolver: &Resolver<N>) -> ResolverParIter<N> {
        let timeout = DEFAULT_TIMEOUT;
        let counter = Arc::new(AtomicUsize::new(0));
        let (node_ready_rx, node_done_tx, _drop_tx) = ResolverParIter::<N>::init(
            timeout,
            counter.clone(),
            resolver.deps.clone(),
            resolver.rdeps.clone(),
            resolver.ready_nodes.clone(),
        );

        ResolverParIter {
            timeout,

            node_ready_rx,
            node_done_tx,
            _drop_tx,

            counter,

            deps: resolver.deps.clone(),
        }
    }

    /// Initialize the iterator
    /// 
    /// This will create a thread that will listen for processed nodes.
    fn init(
        timeout: Duration,
        counter: Arc<AtomicUsize>,
        deps: DependencySet<N::Inner>,
        rdeps: DependencySet<N::Inner>,
        ready_nodes: Vec<N::Inner>,
    ) -> (
        // Channel to get new available nodes
        crossbeam_channel::Receiver<N::Inner>,
        // Channel to send when a node has been processed
        crossbeam_channel::Sender<N::Inner>,
        // Channel to close the thread
        crossbeam_channel::Sender<bool>,
    ) {
        // Create communication channel for processed nodes
        let (node_ready_tx, node_ready_rx) = crossbeam_channel::unbounded::<N::Inner>();
        let (node_done_tx, node_done_rx) = crossbeam_channel::unbounded::<N::Inner>();

        // Create communication channel for dropping the Iterator
        let (drop_tx, drop_rx) = crossbeam_channel::unbounded::<bool>();

        // Seed the ready channel with the list of ready nodes
        ready_nodes
            .iter()
            .for_each(|node_id| node_ready_tx.send(node_id.clone()).unwrap());

        // Clone values
        let counter = counter.clone();
        let deps = deps.clone();

        thread::spawn(move || {
            // Listens on processed node and remove their information
            loop {
                crossbeam_channel::select!{
                    // Grab a processed node ID
                    recv(node_done_rx) -> id => {
                        let id = id.unwrap();
                        // Remove the node from all reverse dependencies
                        let next_nodes = remove_node_id::<N>(id, &deps, &rdeps)?;

                        // Send the next available nodes to the channel.
                        next_nodes
                            .iter()
                            .for_each(|node_id| node_ready_tx.send(node_id.clone()).unwrap());

                        // If there are no more nodes, leave the loop
                        if deps.read().unwrap().is_empty() {
                            break;
                        }
                    },
                    // The iterator got dropped
                    recv(drop_rx) -> _ => return Err(Error::IteratorDropped),
                    // Timeout
                    default(timeout) => {
                        let deps = deps.read().unwrap();
                        let counter_val = counter.load(Ordering::SeqCst);
                        if deps.is_empty() {
                            break;
                        } else if counter_val > 0 {
                            continue;
                        } else {
                            return Err(Error::ResolveGraphError("circular dependency detected"));
                        }
                    },
                };
            }

            // Drop channel
            // This will close threads listening to it
            drop(node_ready_tx);
            Ok(())
        });

        (node_ready_rx, node_done_tx, drop_tx)
    }
}

impl<N: Node> Iterator for ResolverParIter<N> {
    type Item = NodeWrapper<N>;

    /// Advances the iterator and returns the next value.
    ///
    /// The `next` method does not leverage multi-threading directly.
    fn next(&mut self) -> Option<Self::Item> {
        // Grab a node ID for work
        loop {
            match get_next_id::<N>(&self.node_ready_rx, self.timeout, &self.counter, &self.deps) {
                Ok(id) => return Some(NodeWrapper::new(id, self.node_done_tx.clone())),
                Err(Error::EmptyListError) => return None,
                Err(Error::NoAvailableNodeError) => continue,
                Err(err) => panic!(err),
            }
        }
    }
}

/// Compatibility with Iterator
impl<N: Node> ResolverParIter<N> {
    pub fn for_each<F>(self, f: &'static F)
    where F: Fn(NodeWrapper<N>) + Send + Sync {
        // Start worker threads
        let handles: Vec<thread::JoinHandle<Result<(), Error>>> = (0..num_cpus::get())
            .map(|i| -> thread::JoinHandle<Result<(), Error>> {
                // Clone data for injection in the threads
                let deps = self.deps.clone();
                let node_ready_rx = self.node_ready_rx.clone();
                let node_done_tx = self.node_done_tx.clone();
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
                        f(NodeWrapper::new(id.clone(), node_done_tx.clone()));

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

        // Wait for threads to close
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
    }
}

/// Wrapping struct for a node ID
pub struct NodeWrapper<N: Node> {
    id: N::Inner,
    callback: crossbeam_channel::Sender<N::Inner>,
}

impl<N: Node> NodeWrapper<N> {
    pub fn new(id: N::Inner, callback: crossbeam_channel::Sender<N::Inner>) -> NodeWrapper<N> {
        NodeWrapper {
            id,
            callback,
        }
    }
}

impl<N: Node> Drop for NodeWrapper<N> {
    /// When the value is dropped, send the ID back to the callback channel.
    fn drop(&mut self) {
        self.callback.send(self.id.clone()).unwrap();
    }
}

impl<N: Node> ops::Deref for NodeWrapper<N> {
    type Target = N::Inner;

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

// impl<N: Node> AsRef<N::Inner> for NodeWrapper<N> {
//     fn as_ref(&self) -> &N::Inner {
//         &self.id
//     }
// }

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