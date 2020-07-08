use crate::{
    error::Error,
    graph::{remove_node_id, DepGraph, DependencyMap},
};
use crossbeam_channel::{Receiver, Sender};

use rayon::iter::{
    plumbing::{bridge, Consumer, Producer, ProducerCallback, UnindexedConsumer},
    IndexedParallelIterator, IntoParallelIterator, ParallelIterator,
};
use std::cmp;

use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter::{DoubleEndedIterator, ExactSizeIterator};

use std::ops;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

/// Add into_par_iter() to DepGraph
impl<I> IntoParallelIterator for DepGraph<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    type Item = Wrapper<I>;
    type Iter = DepGraphParIter<I>;

    fn into_par_iter(self) -> Self::Iter {
        DepGraphParIter::new(self.ready_nodes, self.deps, self.rdeps)
    }
}

/// Wrapper for an item
///
/// This is used to pass items through iterators
#[cfg(feature = "rayon")]
#[derive(Clone)]
pub struct Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    inner: I,
    counter: Arc<AtomicUsize>,
    item_done_tx: Sender<I>,
}

#[cfg(feature = "rayon")]
impl<I> Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    pub fn new(inner: I, counter: Arc<AtomicUsize>, item_done_tx: Sender<I>) -> Self {
        (*counter).fetch_add(1, Ordering::SeqCst);
        Self {
            inner,
            counter,
            item_done_tx,
        }
    }
}

#[cfg(feature = "rayon")]
impl<I> Drop for Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        (*self.counter).fetch_sub(1, Ordering::SeqCst);
        self.item_done_tx
            .send(self.inner.clone())
            .expect("could not send message")
    }
}

#[cfg(feature = "rayon")]
impl<I> ops::Deref for Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    type Target = I;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(feature = "rayon")]
impl<I> ops::DerefMut for Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[cfg(feature = "rayon")]
impl<I> Eq for Wrapper<I> where I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static
{}

#[cfg(feature = "rayon")]
impl<I> Hash for Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.hash(state)
    }
}

#[cfg(feature = "rayon")]
impl<I> cmp::PartialEq for Wrapper<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

/// Parallel iterator for DepGraph
#[cfg(feature = "rayon")]
pub struct DepGraphParIter<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    timeout: Duration,
    counter: Arc<AtomicUsize>,
    item_ready_rx: Receiver<I>,
    item_done_tx: Sender<I>,
}

#[cfg(feature = "rayon")]
impl<I> DepGraphParIter<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    /// Create a new parallel iterator
    ///
    /// This will create a thread and crossbeam channels to listen/send
    /// available and processed nodes.
    pub fn new(ready_nodes: Vec<I>, deps: DependencyMap<I>, rdeps: DependencyMap<I>) -> Self {
        let counter = Arc::new(AtomicUsize::new(0));

        let (item_ready_rx, item_done_tx) =
            DepGraphParIter::init(counter.clone(), deps, rdeps, ready_nodes);

        DepGraphParIter {
            timeout: DEFAULT_TIMEOUT,
            counter,

            item_ready_rx,
            item_done_tx,
        }
    }

    /// Initialize the DepGraph
    fn init(
        counter: Arc<AtomicUsize>,
        deps: DependencyMap<I>,
        rdeps: DependencyMap<I>,
        ready_nodes: Vec<I>,
    ) -> (Receiver<I>, Sender<I>) {
        // Create communication channel for processed nodes
        let (item_ready_tx, item_ready_rx) = crossbeam_channel::unbounded::<I>();
        let (item_done_tx, item_done_rx) = crossbeam_channel::unbounded::<I>();

        // Inject ready nodes
        ready_nodes
            .iter()
            .for_each(|node| item_ready_tx.send(node.clone()).unwrap());

        // Start dispatcher thread
        thread::spawn(move || {
            loop {
                crossbeam_channel::select! {
                    // Grab a processed node ID
                    recv(item_done_rx) -> id => {
                        let id = id.unwrap();
                        // Remove the node from all reverse dependencies
                        let next_nodes = remove_node_id::<I>(id, &deps, &rdeps)?;

                        // Send the next available nodes to the channel.
                        next_nodes
                            .iter()
                            .for_each(|node_id| item_ready_tx.send(node_id.clone()).unwrap());

                        // If there are no more nodes, leave the loop
                        if deps.read().unwrap().is_empty() {
                            break;
                        }
                    },
                    // Timeout
                    default(DEFAULT_TIMEOUT) => {
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
            drop(item_ready_tx);
            Ok(())
        });

        (item_ready_rx, item_done_tx)
    }
}

#[cfg(feature = "rayon")]
impl<I> ParallelIterator for DepGraphParIter<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    type Item = Wrapper<I>;

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        bridge(self, consumer)
    }
}

#[cfg(feature = "rayon")]
impl<I> IndexedParallelIterator for DepGraphParIter<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    fn len(&self) -> usize {
        num_cpus::get()
    }

    fn drive<C>(self, consumer: C) -> C::Result
    where
        C: Consumer<Self::Item>,
    {
        bridge(self, consumer)
    }

    fn with_producer<CB>(self, callback: CB) -> CB::Output
    where
        CB: ProducerCallback<Self::Item>,
    {
        callback.callback(DepGraphProducer {
            timeout: self.timeout,
            counter: self.counter.clone(),
            item_ready_rx: self.item_ready_rx,
            item_done_tx: self.item_done_tx,
        })
    }
}

#[cfg(feature = "rayon")]
struct DepGraphProducer<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    timeout: Duration,
    counter: Arc<AtomicUsize>,
    item_ready_rx: Receiver<I>,
    item_done_tx: Sender<I>,
}

#[cfg(feature = "rayon")]
impl<I> Iterator for DepGraphProducer<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    type Item = Wrapper<I>;

    fn next(&mut self) -> Option<Self::Item> {
        // TODO: Check until there is an item available
        match self.item_ready_rx.recv() {
            Ok(item) => Some(Wrapper::new(
                item,
                self.counter.clone(),
                self.item_done_tx.clone(),
            )),
            Err(_) => None,
        }
    }
}

#[cfg(feature = "rayon")]
impl<I> DoubleEndedIterator for DepGraphProducer<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        self.next()
    }
}

#[cfg(feature = "rayon")]
impl<I> ExactSizeIterator for DepGraphProducer<I> where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static
{
}

#[cfg(feature = "rayon")]
impl<I> Producer for DepGraphProducer<I>
where
    I: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync + 'static,
{
    type Item = Wrapper<I>;
    type IntoIter = Self;

    fn into_iter(self) -> Self::IntoIter {
        Self {
            timeout: self.timeout,
            counter: self.counter.clone(),
            item_ready_rx: self.item_ready_rx.clone(),
            item_done_tx: self.item_done_tx,
        }
    }

    fn split_at(self, _: usize) -> (Self, Self) {
        (
            Self {
                timeout: self.timeout,
                counter: self.counter.clone(),
                item_ready_rx: self.item_ready_rx.clone(),
                item_done_tx: self.item_done_tx.clone(),
            },
            Self {
                timeout: self.timeout,
                counter: self.counter.clone(),
                item_ready_rx: self.item_ready_rx.clone(),
                item_done_tx: self.item_done_tx,
            },
        )
    }
}
