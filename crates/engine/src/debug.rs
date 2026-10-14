//! Format-neutral debug information (Layer B), produced by the DWARF and
//! CodeView readers and merged into the graph by symbol name.

use std::collections::HashMap;

use crate::graph::{Builder, EdgeKind, NodeKind};

#[derive(Debug, Clone, Default)]
pub(crate) struct Location {
    pub(crate) file: String,
    pub(crate) line: u32,
    /// The file lies outside the project (see [`is_library_path`]).
    pub(crate) library: bool,
}

/// Path fragments of toolchain, SDK and package-manager install locations.
/// Matched against the lower-cased path with `/` separators.
const LIBRARY_PATHS: &[&str] = &[
    "/microsoft visual studio/",
    "/windows kits/",
    "/mingw",
    "/msys64/",
    "/cygwin",
    "/include/c++/",
    "/lib/gcc/",
    "/usr/include/",
    "/usr/lib/",
    "/usr/local/include/",
    "/.rustup/",
    "/rustlib/",
    "/.cargo/registry/",
    "/.cargo/git/",
    "/vcpkg/",
];

/// Whether `file` belongs to an installed toolchain or library rather than
/// the user's project: Visual Studio and Windows SDK headers, MinGW/MSYS and
/// libstdc++, system includes, the Rust standard library (`/rustc/<hash>/`
/// or rustup) and cargo/vcpkg packages. Relative paths are project files.
pub(crate) fn is_library_path(file: &str) -> bool {
    let path = file.replace('\\', "/").to_ascii_lowercase();
    path.starts_with("/rustc/") || LIBRARY_PATHS.iter().any(|p| path.contains(p))
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
                b.mark_library(id, src.library);
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
                b.mark_library(id, src.library);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_paths() {
        for lib in [
            r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.51\include\xstring",
            r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\ucrt\stdio.h",
            "C:/ProgramData/mingw64/mingw64/lib/gcc/x86_64-w64-mingw32/15.2.0/include/c++/bits/basic_string.h",
            "/usr/include/c++/13/bits/basic_string.h",
            "/rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/slice/mod.rs",
            r"C:\Users\me\.cargo\registry\src\index\object-0.40.0\src\read\mod.rs",
        ] {
            assert!(is_library_path(lib), "{lib}");
        }
        for user in [
            "src/main.cpp",
            "./src/graph.c",
            r"E:\Lattice\crates\engine\tests\fixtures\src\graph.c",
            "/home/me/proj/include/shapes.h",
        ] {
            assert!(!is_library_path(user), "{user}");
        }
    }
}
