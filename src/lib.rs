//! # Library to perform operations over dependency graphs.
//!
//! This library allow running iterative operations over a dependency graph in
//! the correct evaluation order, or will fail if there are a circular
//! dependencies.
//!
//! To use this library, you create a list of [Nodes](trait.Node.html)
//! containing dependency information (which node depends on which). You then
//! create a [Resolver](struct.Resolver.html) which will allow you to traverse
//! the graph so that you will always get an item for which all dependencies
//! have been processed.
//!
//! ## Processing order
//!
//! Resolvers have two methods: one for sequential operations and one for
//! parallel (multi-threaded) operations. In the first case, it's easy to know
//! in which order nodes can be processed, as only one node will be processed
//! at a time. However, in parallel operations, we need to know if a given node
//! is done processing.
//!
//! This leads to a situation where a given worker thread might not be able to
//! pull a node temporarily, as it needs to wait for another worker to finish
//! processing another node.
//!
//! Let's look at the following case:
//!
//! ```text,no_run
//! (A) <-|
//!       |-- [E] <-|-- [G]
//! (B) <-|         |
//!       |-- [F] <-|-- [H]
//! [C] <-|
//! ```
//!
//! In this case, the nodes __E__ and __F__ are dependent on __A__, __B__ and
//! __C__ and __G__ and __H__ are dependent on both __E__ and __F__. If we
//! process the nodes with two workers, they might pick up nodes A and B first.
//! Since these nodes don't have any dependencies, there is no problem right
//! now.
//!
//! ```text,no_run
//! [ ] <-|
//!       |-- [E] <-|-- [G]
//! [ ] <-|         |
//!       |-- [F] <-|-- [H]
//! (C) <-|
//! ```
//!
//! When one of the worker is done, it can immediately start working on node
//! __C__, as it does not have any dependencies. However, when the second
//! worker is done, there are no available nodes for processing: we need to
//! wait until __C__ is processed before we can start working on __E__ or
//! __F__. One of the worker will then stay idle until the other one is done.
//!
//! ```text,no_run
//! [ ] <-|
//!       |-- (E) <-|-- [G]
//! [ ] <-|         |
//!       |-- (F) <-|-- [H]
//! [ ] <-|
//! ```
//!
//! Once that is done, both workers can work on __E__ and __F__. However, if
//! __E__ takes only a fraction of the time compared to __F__, we will end up
//! in the same situation, as there are no nodes without un-processed
//! dependencies.
//!
//! ## Basic usage
//!
//! ```rust
//! use depgraph::{Node, Resolver,StrNode};
//!
//! fn my_graph() {
//!     // Create a list of nodes
//!     let mut root = StrNode::new("root");
//!     let mut dep1 = StrNode::new("dep1");
//!     let mut dep2 = StrNode::new("dep2");
//!     let leaf = StrNode::new("leaf");
//!
//!     // Map their connections
//!     root.add_dep(dep1.id());
//!     root.add_dep(dep2.id());
//!     dep1.add_dep(leaf.id());
//!     dep2.add_dep(leaf.id());
//!
//!     // Create a resolver
//!     let nodes = vec![root, dep1, dep2, leaf];
//!     let resolver = Resolver::new(&nodes);
//!
//!     // Run an operation over all nodes in the graph.
//!     // The function receives the identity value from the node, not the
//!     // entire node (e.g. "root", "dep1", etc. in this case).
//!     resolver
//!         .par_for_each(&|node| {
//!             println!("{}", node)
//!         })
//!         .unwrap();
//! }
//! ```
//!
//! ## Create your own node type
//!
//! This library provides a node for string types
//! [`StrNode`](struct.StrNode.html).
//!
//! However, you may want to implement your own node type to hold another type
//! of identity information. For this purpose, you can implement the
//! [`Node trait`](trait.Node.html).

mod dep;
pub mod error;
mod resolver;

pub use dep::{Node, StrNode};
pub use resolver::Resolver;
