//! Shared helpers for the engine integration tests.
#![allow(dead_code)]

use std::path::PathBuf;

use engine::{CodeGraph, EdgeKind, NodeKind};

pub const ALL_FIXTURES: &[&str] = &[
    "msvc_z7.obj",
    "msvc_nodebug.obj",
    "msvc_comdat.obj",
    "msvc_empty.obj",
    "msvc_cpp.obj",
    "mingw_dwarf.o",
    "mingw_nodebug.o",
    "mingw_cpp.o",
    "elf_dwarf.o",
];

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

pub fn load(name: &str) -> CodeGraph {
    match engine::load(&fixture(name)) {
        Ok(g) => g,
        Err(e) => panic!("{name} failed to load: {e}"),
    }
}

/// Names of nodes of `kind`, sorted.
pub fn names(g: &CodeGraph, kind: NodeKind) -> Vec<String> {
    let mut v: Vec<String> = g
        .nodes_of(kind)
        .map(|n| n.display_name.to_string())
        .collect();
    v.sort();
    v
}

/// Whether an edge `kind` exists from the node named `from` to the node
/// named `to` (display or linkage name, any node kind).
pub fn edge(g: &CodeGraph, from: &str, to: &str, kind: EdgeKind) -> bool {
    let matches =
        |n: &engine::Node, name: &str| &*n.display_name == name || &*n.linkage_name == name;
    g.edges().iter().any(|e| {
        e.kind == kind
            && g.node(e.from).is_some_and(|n| matches(n, from))
            && g.node(e.to).is_some_and(|n| matches(n, to))
    })
}

/// Human-readable dump for debugging fixtures.
pub fn describe(g: &CodeGraph) -> String {
    let mut out = format!(
        "{} ({:?}, {}, debug {:?})\n",
        g.path.display(),
        g.format,
        g.architecture,
        g.debug
    );
    let mut nodes: Vec<_> = g.nodes().iter().collect();
    nodes.sort_by_key(|n| (format!("{:?}", n.kind), n.display_name.clone()));
    for n in nodes {
        let src = n
            .source
            .as_ref()
            .map(|s| format!(" @ {}:{}", s.file, s.line))
            .unwrap_or_default();
        out += &format!(
            "  {:?} {} [{}] size={:?}{src}\n",
            n.kind, n.display_name, n.linkage_name, n.size
        );
    }
    let mut edges: Vec<String> = g
        .edges()
        .iter()
        .map(|e| {
            let name = |id| {
                g.node(id)
                    .map_or("?".to_string(), |n| n.display_name.to_string())
            };
            format!("  {} -{:?}-> {}", name(e.from), e.kind, name(e.to))
        })
        .collect();
    edges.sort();
    out += &edges.join("\n");
    for w in &g.warnings {
        out += &format!("\n  warning: {w}");
    }
    out
}
