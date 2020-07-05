Dependency Graph
================

This is a rust library to perform iterative operations over dependency graphs.

## Usage

```toml
[dependencies]
depgraph = "0.1"
```

This library supports both sequential and parallel (multi-threaded) operations out of the box. By default, multi-threaded operations will run a number of threads equal to the number of cores.

### Sequential operations

```rust
use depgraph::{Resolver,StrNode};

fn main() {
    let mut root = StrNode::new("root");
    let mut dep1 = StrNode::new("dep1");
    let mut dep2 = StrNode::new("dep2");
    let leaf = StrNode::new("leaf");

    root.add_dep(dep1.id());
    root.add_dep(dep2.id());
    dep1.add_dep(leaf.id());
    dep2.add_dep(leaf.id());
}
```