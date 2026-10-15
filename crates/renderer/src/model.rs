//! The drawable graph: loaded objects merged into one node/edge list with
//! compact indices, filtered to what the Visualizer shows.
//!
//! Merging links objects together the way a linker would, by linkage name:
//! an `External` in one object becomes the definition from another loaded
//! object, externals nobody defines collapse into one node, and types with
//! the same name are shared. Definitions themselves are never merged, so two
//! `static` functions with the same name in different objects stay apart.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use engine::{CodeGraph, EdgeKind, Node, NodeId, NodeKind, SourceLoc};

/// Which optional node classes to show. The default is user code only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filter {
    /// Standard-library and toolchain-header code.
    pub library: bool,
    /// Compiler-generated symbols (RTTI, vtables, literals, ...).
    pub generated: bool,
    /// Parameters and locals. (Types are never nodes: they are listed on
    /// the node that uses them, see [`ModelNode::types`].)
    pub locals: bool,
}

/// Visual class of a node; decides its colour and whether a filter hides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeClass {
    Function,
    Variable,
    External,
    Library,
    Generated,
    Local,
    Type,
}

impl NodeClass {
    pub const COUNT: usize = 7;

    pub fn index(self) -> usize {
        self as usize
    }

    fn visible(self, f: Filter) -> bool {
        match self {
            Self::Function | Self::Variable | Self::External => true,
            Self::Library => f.library,
            Self::Generated => f.generated,
            Self::Local => f.locals,
            Self::Type => false,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Variable => "global variable",
            Self::External => "external",
            Self::Library => "library",
            Self::Generated => "compiler-generated",
            Self::Local => "local / parameter",
            Self::Type => "type",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeClass {
    Calls,
    References,
    /// Function to local, variable to type.
    Structure,
}

impl EdgeClass {
    pub const COUNT: usize = 3;

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone)]
pub struct ModelNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub class: NodeClass,
    pub label: Arc<str>,
    pub source: Option<SourceLoc>,
    /// The object file the node came from (for resolving relative paths).
    pub object: Arc<Path>,
    /// Types this node uses, shown only while it is selected: a variable's
    /// type (empty role), or a function's parameters and locals with their
    /// types (role = the local's name).
    pub types: Arc<[TypeTag]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTag {
    pub role: Arc<str>,
    pub ty: Arc<str>,
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
    /// Nodes left out by the filter.
    pub hidden: usize,
}

impl GraphModel {
    /// Merges `graphs` (any order; the result is deterministic) and applies
    /// `filter`.
    pub fn build(graphs: &[Arc<CodeGraph>], filter: Filter) -> Self {
        let mut graphs: Vec<&CodeGraph> = graphs.iter().map(|g| &**g).collect();
        graphs.sort_by(|a, b| a.path.cmp(&b.path));
        let locals: Vec<HashSet<NodeId>> = graphs
            .iter()
            .map(|g| {
                g.edges()
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Contains)
                    .map(|e| e.to)
                    .collect()
            })
            .collect();

        // Global definitions by linkage name; the first object wins.
        let mut defined: HashMap<&str, NodeId> = HashMap::new();
        for (g, locals) in graphs.iter().zip(&locals) {
            for n in g.nodes() {
                if matches!(n.kind, NodeKind::Function | NodeKind::Variable)
                    && !locals.contains(&n.id)
                {
                    defined.entry(&n.linkage_name).or_insert(n.id);
                }
            }
        }

        // Every node id maps to its canonical id; canonical nodes carry
        // their class and origin.
        let mut canon: HashMap<NodeId, NodeId> = HashMap::new();
        let mut shared: HashMap<(NodeKind, &str), NodeId> = HashMap::new();
        let mut model = Self::default();
        for (g, locals) in graphs.iter().zip(&locals) {
            let object: Arc<Path> = Arc::from(g.path.as_path());
            let types = type_tags(g);
            for n in g.nodes() {
                let target = match n.kind {
                    NodeKind::Section => continue,
                    NodeKind::External => match defined.get(&*n.linkage_name) {
                        Some(&def) => def,
                        None => *shared
                            .entry((NodeKind::External, &n.linkage_name))
                            .or_insert(n.id),
                    },
                    NodeKind::Type => *shared
                        .entry((NodeKind::Type, &n.linkage_name))
                        .or_insert(n.id),
                    _ => n.id,
                };
                canon.insert(n.id, target);
                if target == n.id {
                    let class = classify(n, locals.contains(&n.id));
                    let tags = types.get(&n.id).cloned().unwrap_or_default();
                    if class.visible(filter) {
                        model.push_node(n, class, &object, tags);
                    } else if class != NodeClass::Type {
                        model.hidden += 1;
                    }
                }
            }
        }

        let mut edges = Vec::new();
        for g in &graphs {
            for e in g.edges() {
                let class = match e.kind {
                    EdgeKind::Calls => EdgeClass::Calls,
                    EdgeKind::References => EdgeClass::References,
                    EdgeKind::Contains | EdgeKind::HasType => EdgeClass::Structure,
                    EdgeKind::DefinedIn => continue,
                };
                let ends = canon
                    .get(&e.from)
                    .zip(canon.get(&e.to))
                    .and_then(|(a, b)| model.index.get(a).zip(model.index.get(b)));
                if let Some((&from, &to)) = ends {
                    if from != to {
                        edges.push(ModelEdge { from, to, class });
                    }
                }
            }
        }
        model.set_edges(edges);
        model
    }

    /// A deterministic pseudo-random graph for benchmarks: clusters of about
    /// 40 nodes ("modules"), mostly calling within their cluster.
    pub fn synthetic(nodes: usize, edges_per_node: f32) -> Self {
        let mut rng = Lcg(0x5EED_1234_ABCD_0001);
        let object: Arc<Path> = Arc::from(Path::new("synthetic"));
        let mut model = Self::default();
        let classes = [
            NodeClass::Function,
            NodeClass::Function,
            NodeClass::Function,
            NodeClass::Variable,
            NodeClass::External,
        ];
        for i in 0..nodes {
            let class = classes[rng.below(classes.len())];
            let label: Arc<str> = Arc::from(format!("m{}::item_{i}", i / 40));
            model.index.insert(NodeId(i as u64), i as u32);
            model.nodes.push(ModelNode {
                id: NodeId(i as u64),
                kind: NodeKind::Function,
                class,
                label,
                source: None,
                object: object.clone(),
                types: Arc::from([]),
            });
        }
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
            if a != b {
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
        }
        model.set_edges(edges);
        model
    }

    fn push_node(&mut self, n: &Node, class: NodeClass, object: &Arc<Path>, types: Arc<[TypeTag]>) {
        self.index.insert(n.id, self.nodes.len() as u32);
        self.nodes.push(ModelNode {
            id: n.id,
            kind: n.kind,
            class,
            label: n.display_name.clone(),
            source: n.source.clone(),
            object: object.clone(),
            types,
        });
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

/// Per node of one object: the types it uses (see [`ModelNode::types`]).
fn type_tags(g: &CodeGraph) -> HashMap<NodeId, Arc<[TypeTag]>> {
    let by_id: HashMap<NodeId, &Node> = g.nodes().iter().map(|n| (n.id, n)).collect();
    let mut own: HashMap<NodeId, Vec<Arc<str>>> = HashMap::new();
    let mut contains: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for e in g.edges() {
        match e.kind {
            EdgeKind::HasType => {
                if let Some(t) = by_id.get(&e.to) {
                    own.entry(e.from).or_default().push(t.display_name.clone());
                }
            }
            EdgeKind::Contains => contains.entry(e.from).or_default().push(e.to),
            _ => {}
        }
    }
    let empty: Arc<str> = Arc::from("");
    let mut out = HashMap::new();
    for n in g.nodes() {
        let mut tags: Vec<TypeTag> = own
            .get(&n.id)
            .into_iter()
            .flatten()
            .map(|ty| TypeTag {
                role: empty.clone(),
                ty: ty.clone(),
            })
            .collect();
        for local in contains.get(&n.id).into_iter().flatten() {
            let Some(l) = by_id.get(local) else { continue };
            for ty in own.get(local).into_iter().flatten() {
                tags.push(TypeTag {
                    role: l.display_name.clone(),
                    ty: ty.clone(),
                });
            }
        }
        if !tags.is_empty() {
            out.insert(n.id, Arc::from(tags));
        }
    }
    out
}

fn classify(n: &Node, is_local: bool) -> NodeClass {
    if is_local {
        return NodeClass::Local;
    }
    match n.kind {
        NodeKind::Type => NodeClass::Type,
        _ if n.artificial => NodeClass::Generated,
        _ if n.library => NodeClass::Library,
        NodeKind::Variable => NodeClass::Variable,
        NodeKind::External => NodeClass::External,
        _ => NodeClass::Function,
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
    use std::path::PathBuf;

    fn fixture(name: &str) -> Arc<CodeGraph> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/fixtures")
            .join(name);
        Arc::new(engine::load(&p).expect("fixture loads"))
    }

    fn labels(m: &GraphModel) -> Vec<&str> {
        let mut v: Vec<&str> = m.nodes().iter().map(|n| &*n.label).collect();
        v.sort_unstable();
        v
    }

    fn edge(m: &GraphModel, from: &str, to: &str, class: EdgeClass) -> bool {
        m.edges().iter().any(|e| {
            e.class == class
                && &*m.nodes()[e.from as usize].label == from
                && &*m.nodes()[e.to as usize].label == to
        })
    }

    #[test]
    fn single_object_shows_user_code_with_calls() {
        let m = GraphModel::build(&[fixture("msvc_z7.obj")], Filter::default());
        let names = labels(&m);
        for want in ["caller", "callee", "counter"] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
        assert!(edge(&m, "caller", "callee", EdgeClass::Calls));
        assert!(m
            .nodes()
            .iter()
            .all(|n| !matches!(n.class, NodeClass::Local | NodeClass::Type)));
    }

    #[test]
    fn locals_filter_adds_structure_edges() {
        let g = fixture("msvc_z7.obj");
        let without = GraphModel::build(std::slice::from_ref(&g), Filter::default());
        let with = GraphModel::build(
            &[g],
            Filter {
                locals: true,
                ..Filter::default()
            },
        );
        assert!(with.nodes().len() > without.nodes().len());
        assert!(with.edges().iter().any(|e| e.class == EdgeClass::Structure));
        assert!(without
            .edges()
            .iter()
            .all(|e| e.class != EdgeClass::Structure));
        assert_eq!(
            with.hidden + with.nodes().len(),
            without.hidden + without.nodes().len()
        );
        assert!(with.nodes().iter().all(|n| n.class != NodeClass::Type));
    }

    #[test]
    fn types_ride_on_the_nodes_that_use_them() {
        let m = GraphModel::build(&[fixture("msvc_z7.obj")], Filter::default());
        let tagged: Vec<(&str, &[TypeTag])> = m
            .nodes()
            .iter()
            .filter(|n| !n.types.is_empty())
            .map(|n| (&*n.label, &*n.types))
            .collect();
        assert!(!tagged.is_empty(), "no node carries types");
        // A function lists its locals with their types.
        assert!(
            tagged
                .iter()
                .any(|(_, t)| t.iter().any(|t| !t.role.is_empty())),
            "{tagged:?}"
        );
    }

    #[test]
    fn merged_objects_share_externals_and_are_deterministic() {
        let a = fixture("msvc_z7.obj");
        let b = fixture("mingw_dwarf.o");
        let ab = GraphModel::build(&[a.clone(), b.clone()], Filter::default());
        let ba = GraphModel::build(&[b, a], Filter::default());
        let ids = |m: &GraphModel| m.nodes().iter().map(|n| n.id).collect::<Vec<_>>();
        assert_eq!(ids(&ab), ids(&ba));
        assert_eq!(ab.edges(), ba.edges());
        // Both objects call the same undefined function; it appears once.
        let externals: Vec<&str> = ab
            .nodes()
            .iter()
            .filter(|n| n.class == NodeClass::External)
            .map(|n| &*n.label)
            .collect();
        let mut unique = externals.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(externals.len(), unique.len(), "{externals:?}");
    }

    #[test]
    fn external_links_to_definition_in_another_object() {
        let alone = GraphModel::build(&[fixture("msvc_z7.obj")], Filter::default());
        let ext = alone
            .nodes()
            .iter()
            .find(|n| &*n.label == "external_fn")
            .expect("external_fn");
        assert_eq!(ext.class, NodeClass::External);

        let linked = GraphModel::build(
            &[fixture("msvc_z7.obj"), fixture("mingw_external.o")],
            Filter::default(),
        );
        let defs: Vec<&ModelNode> = linked
            .nodes()
            .iter()
            .filter(|n| &*n.label == "external_fn")
            .collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].class, NodeClass::Function);
        assert!(defs[0].source.is_some());
        assert!(edge(&linked, "caller", "external_fn", EdgeClass::Calls));
    }

    #[test]
    fn adjacency_is_symmetric() {
        let m = GraphModel::synthetic(500, 2.5);
        for e in m.edges() {
            assert!(m.neighbors(e.from).contains(&e.to));
            assert!(m.neighbors(e.to).contains(&e.from));
        }
        assert_eq!(m.neighbors(10_000), &[] as &[u32]);
    }
}
