//! The code graph: plain, serialisable structs. Views such as petgraph are
//! built from these by consumers; they are not the storage format.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::demangle::demangle;

/// Stable node identity: a hash of (kind, linkage name, object path), so a
/// reloaded graph reuses the ids of nodes that still exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    Function,
    Variable,
    Type,
    Section,
    /// Referenced but not defined in this object.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Control transfer (call or tail jump) to the target.
    Calls,
    /// Any other use: address taken, data read or written.
    References,
    /// Function to its parameters and local variables; record type (class,
    /// struct, union) to its data members.
    Contains,
    /// Variable to its type.
    HasType,
    /// Symbol to the section that holds it.
    DefinedIn,
    /// Record type to one of its member functions.
    Method,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLoc {
    pub file: Arc<str>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Demangled, for display only.
    pub display_name: Arc<str>,
    /// The identity: the raw symbol name (or a synthetic key for locals,
    /// types and sections).
    pub linkage_name: Arc<str>,
    pub source: Option<SourceLoc>,
    /// Size in bytes when known.
    pub size: Option<u64>,
    /// Compiler-generated (RTTI, vtables, string literals, runtime-check
    /// data, static initialisers, EH helpers) rather than user code.
    pub artificial: bool,
    /// Library rather than project code: in a standard-library namespace,
    /// or (with debug info) declared in a toolchain or package header, e.g.
    /// a template instantiated into this object.
    pub library: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectFormat {
    Coff,
    Elf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugFormat {
    None,
    Dwarf,
    CodeView,
}

/// A loaded object file. Nodes and edges are sorted, so lookups by id are
/// binary searches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraph {
    pub path: PathBuf,
    pub format: ObjectFormat,
    pub architecture: String,
    pub debug: DebugFormat,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Non-fatal problems (for example unreadable debug info).
    pub warnings: Vec<String>,
}

impl CodeGraph {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes
            .binary_search_by_key(&id, |n| n.id)
            .ok()
            .and_then(|i| self.nodes.get(i))
    }

    pub fn nodes_of(&self, kind: NodeKind) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(move |n| n.kind == kind)
    }

    /// First node of `kind` whose display or linkage name is `name`.
    pub fn find(&self, kind: NodeKind, name: &str) -> Option<&Node> {
        self.nodes_of(kind)
            .find(|n| &*n.display_name == name || &*n.linkage_name == name)
    }

    pub fn edges_from(&self, id: NodeId) -> impl Iterator<Item = &Edge> {
        let start = self.edges.partition_point(|e| e.from < id);
        self.edges[start..].iter().take_while(move |e| e.from == id)
    }

    pub fn has_edge(&self, from: NodeId, to: NodeId, kind: EdgeKind) -> bool {
        self.edges.binary_search(&Edge { from, to, kind }).is_ok()
    }
}

/// Deduplicates strings so repeated names share one allocation.
#[derive(Default)]
pub(crate) struct Interner {
    strings: HashSet<Arc<str>>,
}

impl Interner {
    pub(crate) fn intern(&mut self, s: &str) -> Arc<str> {
        if let Some(existing) = self.strings.get(s) {
            return existing.clone();
        }
        let arc: Arc<str> = Arc::from(s);
        self.strings.insert(arc.clone());
        arc
    }
}

/// Accumulates nodes and edges while an object is analysed.
pub(crate) struct Builder {
    object_key: String,
    interner: Interner,
    nodes: HashMap<NodeId, Node>,
    edges: HashSet<Edge>,
    pub(crate) warnings: Vec<String>,
}

impl Builder {
    pub(crate) fn new(object_key: String) -> Self {
        Self {
            object_key,
            interner: Interner::default(),
            nodes: HashMap::new(),
            edges: HashSet::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn id_for(&self, kind: NodeKind, linkage: &str) -> NodeId {
        crate::id::node_id(kind, linkage, &self.object_key)
    }

    /// Adds a node (or returns the existing one with the same identity).
    /// `display` defaults to the demangled linkage name.
    pub(crate) fn add_node(
        &mut self,
        kind: NodeKind,
        linkage: &str,
        display: Option<&str>,
    ) -> NodeId {
        let id = self.id_for(kind, linkage);
        if !self.nodes.contains_key(&id) {
            let library = matches!(
                kind,
                NodeKind::Function | NodeKind::Variable | NodeKind::External
            );
            let display = match display {
                Some(d) => self.interner.intern(d),
                None => {
                    let d = demangle(linkage);
                    self.interner.intern(&d)
                }
            };
            let linkage_name = self.interner.intern(linkage);
            let library = library && crate::artificial::is_library_symbol(linkage, &display);
            self.nodes.insert(
                id,
                Node {
                    id,
                    kind,
                    display_name: display,
                    linkage_name,
                    source: None,
                    size: None,
                    artificial: crate::artificial::is_artificial(linkage),
                    library,
                },
            );
        }
        id
    }

    pub(crate) fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub(crate) fn set_size(&mut self, id: NodeId, size: u64) {
        if let Some(n) = self.nodes.get_mut(&id) {
            if n.size.is_none() && size > 0 {
                n.size = Some(size);
            }
        }
    }

    pub(crate) fn set_source(&mut self, id: NodeId, file: &str, line: u32) {
        let file = self.interner.intern(file);
        if let Some(n) = self.nodes.get_mut(&id) {
            if n.source.is_none() {
                n.source = Some(SourceLoc { file, line });
            }
        }
    }

    /// Marks a node as library code (never clears the flag).
    pub(crate) fn mark_library(&mut self, id: NodeId, library: bool) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.library |= library;
        }
    }

    pub(crate) fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        self.edges.insert(Edge { from, to, kind });
    }

    pub(crate) fn finish(
        self,
        path: PathBuf,
        format: ObjectFormat,
        architecture: String,
        debug: DebugFormat,
    ) -> CodeGraph {
        let mut nodes: Vec<Node> = self.nodes.into_values().collect();
        nodes.sort_by_key(|n| n.id);
        let mut edges: Vec<Edge> = self.edges.into_iter().collect();
        edges.sort();
        CodeGraph {
            path,
            format,
            architecture,
            debug,
            nodes,
            edges,
            warnings: self.warnings,
        }
    }
}
