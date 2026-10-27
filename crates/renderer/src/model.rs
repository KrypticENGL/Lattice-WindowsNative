//! The drawable graph: one level of the program (see [`crate::program`]) as
//! a node/edge list with compact indices and an adjacency index.

use std::collections::HashMap;
use std::sync::Arc;

use engine::NodeId;

use crate::program::Entity;

/// Visual class of a node; decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeClass {
    File,
    Class,
    Function,
    Variable,
    /// A class's data member.
    Field,
    /// A function's parameter or local variable.
    Local,
    /// Defined in another file, or not defined in any loaded object.
    External,
    Library,
    Generated,
}

impl NodeClass {
    pub const COUNT: usize = 9;
    /// In `index` order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::File,
        Self::Class,
        Self::Function,
        Self::Variable,
        Self::Field,
        Self::Local,
        Self::External,
        Self::Library,
        Self::Generated,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::File => "source file",
            Self::Class => "class",
            Self::Function => "function",
            Self::Variable => "global variable",
            Self::Field => "data member",
            Self::Local => "parameter / local",
            Self::External => "external",
            Self::Library => "library",
            Self::Generated => "compiler-generated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeClass {
    /// A file `#include`s another.
    Includes,
    Calls,
    References,
    /// Container to member: file to its contents, class to its members,
    /// function to its locals.
    Structure,
}

impl EdgeClass {
    pub const COUNT: usize = 4;
    /// In `index` order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Includes,
        Self::Calls,
        Self::References,
        Self::Structure,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Includes => "includes",
            Self::Calls => "calls",
            Self::References => "references",
            Self::Structure => "contains",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelNode {
    pub id: NodeId,
    pub class: NodeClass,
    pub label: Arc<str>,
    /// What the node stands for in the program index (`None` for the
    /// synthetic benchmark graph).
    pub entity: Option<Entity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelEdge {
    pub from: u32,
    pub to: u32,
    pub class: EdgeClass,
}

#[derive(Debug, Default)]
pub struct GraphModel {
    nodes: Vec<ModelNode>,
    edges: Vec<ModelEdge>,
    /// Undirected adjacency in CSR form: neighbours of `i` are
    /// `adj[adj_start[i]..adj_start[i + 1]]`.
    adj_start: Vec<u32>,
    adj: Vec<u32>,
    index: HashMap<NodeId, u32>,
    /// The node whose contents this level shows (drawn with a ring).
    pub hub: Option<u32>,
    /// Nodes left out by the filter.
    pub hidden: usize,
}

impl GraphModel {
    /// A model from its parts. Edges that point outside `nodes`, loops and
    /// duplicates are dropped.
    pub fn new(nodes: Vec<ModelNode>, edges: Vec<ModelEdge>, hub: Option<u32>) -> Self {
        let n = nodes.len() as u32;
        let mut model = Self {
            index: nodes
                .iter()
                .enumerate()
                .map(|(i, m)| (m.id, i as u32))
                .collect(),
            nodes,
            hub: hub.filter(|&h| h < n),
            ..Self::default()
        };
        let edges = edges
            .into_iter()
            .filter(|e| e.from < n && e.to < n && e.from != e.to)
            .collect();
        model.set_edges(edges);
        model
    }

    /// A deterministic pseudo-random graph for benchmarks: clusters of about
    /// 40 nodes ("modules"), mostly calling within their cluster.
    pub fn synthetic(nodes: usize, edges_per_node: f32) -> Self {
        let mut rng = Lcg(0x5EED_1234_ABCD_0001);
        let classes = [
            NodeClass::Function,
            NodeClass::Function,
            NodeClass::Function,
            NodeClass::Variable,
            NodeClass::External,
        ];
        let list = (0..nodes)
            .map(|i| ModelNode {
                id: NodeId(i as u64),
                class: classes[rng.below(classes.len())],
                label: Arc::from(format!("m{}::item_{i}", i / 40)),
                entity: None,
            })
            .collect();
        let mut edges = Vec::new();
        let n = nodes.max(1);
        for _ in 0..(nodes as f32 * edges_per_node) as usize {
            let a = rng.below(n);
            let b = if rng.below(10) < 8 {
                // Same cluster.
                let base = a / 40 * 40;
                (base + rng.below(40)).min(n - 1)
            } else {
                rng.below(n)
            };
            let class = if rng.below(4) == 0 {
                EdgeClass::References
            } else {
                EdgeClass::Calls
            };
            edges.push(ModelEdge {
                from: a as u32,
                to: b as u32,
                class,
            });
        }
        Self::new(list, edges, None)
    }

    fn set_edges(&mut self, mut edges: Vec<ModelEdge>) {
        edges.sort_unstable();
        edges.dedup();
        let n = self.nodes.len();
        let mut degree = vec![0u32; n + 1];
        for e in &edges {
            degree[e.from as usize] += 1;
            degree[e.to as usize] += 1;
        }
        let mut start = vec![0u32; n + 1];
        for i in 0..n {
            start[i + 1] = start[i] + degree[i];
        }
        let mut fill = start.clone();
        let mut adj = vec![0u32; start[n] as usize];
        for e in &edges {
            for (a, b) in [(e.from, e.to), (e.to, e.from)] {
                adj[fill[a as usize] as usize] = b;
                fill[a as usize] += 1;
            }
        }
        self.edges = edges;
        self.adj_start = start;
        self.adj = adj;
    }

    pub fn nodes(&self) -> &[ModelNode] {
        &self.nodes
    }

    pub fn edges(&self) -> &[ModelEdge] {
        &self.edges
    }

    pub fn node(&self, i: u32) -> Option<&ModelNode> {
        self.nodes.get(i as usize)
    }

    pub fn index_of(&self, id: NodeId) -> Option<u32> {
        self.index.get(&id).copied()
    }

    /// Neighbours of `i` in either direction (may repeat when two nodes are
    /// joined by edges of several classes).
    pub fn neighbors(&self, i: u32) -> &[u32] {
        let i = i as usize;
        match (self.adj_start.get(i), self.adj_start.get(i + 1)) {
            (Some(&a), Some(&b)) => &self.adj[a as usize..b as usize],
            _ => &[],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Small deterministic generator (no external RNG crate needed).
pub(crate) struct Lcg(pub(crate) u64);

impl Lcg {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacency_is_symmetric() {
        let m = GraphModel::synthetic(500, 2.5);
        for e in m.edges() {
            assert!(m.neighbors(e.from).contains(&e.to));
            assert!(m.neighbors(e.to).contains(&e.from));
        }
        assert_eq!(m.neighbors(10_000), &[] as &[u32]);
    }

    #[test]
    fn new_drops_bad_edges_and_hub() {
        let node = |i| ModelNode {
            id: NodeId(i),
            class: NodeClass::Function,
            label: Arc::from("n"),
            entity: None,
        };
        let e = |from, to| ModelEdge {
            from,
            to,
            class: EdgeClass::Calls,
        };
        assert!(NodeClass::ALL
            .iter()
            .enumerate()
            .all(|(i, c)| c.index() == i));
        assert!(EdgeClass::ALL
            .iter()
            .enumerate()
            .all(|(i, c)| c.index() == i));
        let m = GraphModel::new(
            vec![node(1), node(2)],
            vec![e(0, 1), e(0, 1), e(1, 1), e(0, 7)],
            Some(9),
        );
        assert_eq!(m.edges().len(), 1);
        assert_eq!(m.hub, None);
        assert_eq!(m.index_of(NodeId(2)), Some(1));
    }
}
