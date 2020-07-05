Dependency Graph
================

This is a rust library to perform iterative operations over dependency graphs.

## Usage

```toml
[dependencies]
depgraph = "0.1"
```

This library supports both sequential and parallel (multi-threaded) operations out of the box. By default, multi-threaded operations will run a number of threads equal to the number of cores.

### Parallel operations

Here is a simple example on how to use this library:

```rust
use dep_graph::{Node, Resolver,StrNode};

fn my_graph() {
    // Create a list of nodes
    let mut root = StrNode::new("root");
    let mut dep1 = StrNode::new("dep1");
    let mut dep2 = StrNode::new("dep2");
    let leaf = StrNode::new("leaf");

    // Map their connections
    root.add_dep(dep1.id());
    root.add_dep(dep2.id());
    dep1.add_dep(leaf.id());
    dep2.add_dep(leaf.id());

    // Create a resolver
    let nodes = vec![root, dep1, dep2, leaf];
    let resolver = Resolver::new(&nodes);

    // Run an operation over all nodes in the graph.
    // The function receives the identity value from the node, not the
    // entire node (e.g. "root", "dep1", etc. in this case).
    resolver
        // If you want to run this sequentially rather than in parallel, you
        // can replace `par_for_each` with `for_each`.
        .par_for_each(&|node| {
            println!("{}", node)
        })
        .unwrap();
}
```