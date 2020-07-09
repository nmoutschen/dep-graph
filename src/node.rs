use std::cmp::PartialEq;
use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;

/// Single node in a dependency graph, which might have dependencies or be
/// be used as a dependency by other nodes.
///
/// A node is represented by a unique identifier and may contain a list of
/// dependencies.
pub trait Node: Clone + fmt::Debug + Send + Sync + Sized {
    type Inner: Clone + fmt::Debug + Eq + Hash + PartialEq + Send + Sync;

    /// Node identifer
    ///
    /// This value should be used to identify this node by the nodes that
    /// depend on it.
    fn id(&self) -> &Self::Inner;
    fn deps(&self) -> &HashSet<Self::Inner>;
    fn add_dep(&mut self, dep: &Self::Inner);
}

#[derive(Clone, Debug)]
pub struct StrNode {
    name: String,
    dep_nodes: HashSet<String>,
}

impl StrNode {
    pub fn new(name: &str) -> StrNode {
        StrNode {
            name: name.to_string(),
            dep_nodes: Default::default(),
        }
    }
}

impl Node for StrNode {
    type Inner = String;

    fn id(&self) -> &Self::Inner {
        &self.name
    }

    fn deps(&self) -> &HashSet<Self::Inner> {
        &self.dep_nodes
    }
    fn add_dep(&mut self, dep: &Self::Inner) {
        self.dep_nodes.insert(dep.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_node() {
        let node = StrNode::new("node");

        assert_eq!(node.id(), "node");
        assert_eq!(node.deps().len(), 0);
    }

    #[test]
    fn one_dep() {
        let mut root = StrNode::new("root");
        let dep1 = StrNode::new("dep1");

        root.add_dep(dep1.id());

        assert_eq!(root.deps().len(), 1);
    }

    #[test]
    fn two_deps() {
        let mut root = StrNode::new("root");
        let dep1 = StrNode::new("dep1");
        let dep2 = StrNode::new("dep2");

        root.add_dep(dep1.id());
        root.add_dep(dep2.id());

        assert_eq!(root.deps().len(), 2);
        assert_eq!(dep1.deps().len(), 0);
        assert_eq!(dep2.deps().len(), 0);
    }

    #[test]
    fn diamonds() {
        let mut root = StrNode::new("root");
        let mut dep1 = StrNode::new("dep1");
        let mut dep2 = StrNode::new("dep2");
        let leaf = StrNode::new("leaf");

        root.add_dep(dep1.id());
        root.add_dep(dep2.id());
        dep1.add_dep(leaf.id());
        dep2.add_dep(leaf.id());

        assert_eq!(root.deps().len(), 2);
        assert_eq!(dep1.deps().len(), 1);
        assert_eq!(dep2.deps().len(), 1);
        assert_eq!(leaf.deps().len(), 0);
    }
}
