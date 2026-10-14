//! Format-neutral debug information (Layer B), produced by the DWARF and
//! CodeView readers and merged into the graph by symbol name.

use std::collections::HashMap;

use crate::graph::{Builder, EdgeKind, NodeKind};

#[derive(Debug, Clone, Default)]
pub(crate) struct Location {
    pub(crate) file: String,
    pub(crate) line: u32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct VarInfo {
    pub(crate) name: String,
    pub(crate) type_name: Option<String>,
    pub(crate) source: Option<Location>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FuncInfo {
    pub(crate) source: Option<Location>,
    pub(crate) size: Option<u64>,
    /// Parameters and locals, in declaration order.
    pub(crate) locals: Vec<VarInfo>,
}

/// Keyed by linkage name (the symbol name in the object).
#[derive(Debug, Default)]
pub(crate) struct DebugInfo {
    pub(crate) functions: HashMap<String, FuncInfo>,
    pub(crate) globals: HashMap<String, VarInfo>,
}

impl DebugInfo {
    /// Enriches existing symbol nodes. Debug entries without a matching
    /// symbol (inlined or discarded code) are ignored.
    pub(crate) fn apply(&self, b: &mut Builder) {
        for (linkage, f) in &self.functions {
            let id = b.id_for(NodeKind::Function, linkage);
            if !b.contains(id) {
                continue;
            }
            if let Some(src) = &f.source {
                b.set_source(id, &src.file, src.line);
            }
            if let Some(size) = f.size {
                b.set_size(id, size);
            }
            for local in &f.locals {
                // Locals are keyed under their function so equal names in
                // different functions stay distinct.
                let key = format!("{linkage}::{}", local.name);
                let local_id = b.add_node(NodeKind::Variable, &key, Some(&local.name));
                if let Some(src) = &local.source {
                    b.set_source(local_id, &src.file, src.line);
                }
                b.add_edge(id, local_id, EdgeKind::Contains);
                add_type(b, local_id, local.type_name.as_deref());
            }
        }
        for (linkage, g) in &self.globals {
            let id = b.id_for(NodeKind::Variable, linkage);
            if !b.contains(id) {
                continue;
            }
            if let Some(src) = &g.source {
                b.set_source(id, &src.file, src.line);
            }
            add_type(b, id, g.type_name.as_deref());
        }
    }
}

fn add_type(b: &mut Builder, var: crate::graph::NodeId, type_name: Option<&str>) {
    if let Some(t) = type_name.filter(|t| !t.is_empty()) {
        let type_id = b.add_node(NodeKind::Type, &format!("type:{t}"), Some(t));
        b.add_edge(var, type_id, EdgeKind::HasType);
    }
}
