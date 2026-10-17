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
pub fn is_library_path(file: &str) -> bool {
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
    /// Scope-qualified name without parameters (`Shape::area`), when the
    /// format gives one. CodeView ties methods to classes through it.
    pub(crate) qualified: Option<String>,
}

/// A class, struct or union defined in project code.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClassInfo {
    pub(crate) source: Option<Location>,
    pub(crate) size: Option<u64>,
    /// Non-static data members, in declaration order.
    pub(crate) fields: Vec<VarInfo>,
    /// Linkage names of member functions.
    pub(crate) methods: Vec<String>,
}

/// Functions and globals are keyed by linkage name (the symbol name in the
/// object); classes by their scope-qualified name.
#[derive(Debug, Default)]
pub(crate) struct DebugInfo {
    pub(crate) functions: HashMap<String, FuncInfo>,
    pub(crate) globals: HashMap<String, VarInfo>,
    pub(crate) classes: HashMap<String, ClassInfo>,
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
        for (name, class) in &self.classes {
            let id = b.add_node(NodeKind::Type, &format!("type:{name}"), Some(name));
            if let Some(src) = &class.source {
                b.set_source(id, &src.file, src.line);
                b.mark_library(id, src.library);
            }
            if let Some(size) = class.size {
                b.set_size(id, size);
            }
            for field in &class.fields {
                // Keyed under the type, like locals under their function.
                let key = format!("type:{name}::{}", field.name);
                let field_id = b.add_node(NodeKind::Variable, &key, Some(&field.name));
                if let Some(src) = &field.source {
                    b.set_source(field_id, &src.file, src.line);
                }
                b.add_edge(id, field_id, EdgeKind::Contains);
                add_type(b, field_id, field.type_name.as_deref());
            }
            for linkage in &class.methods {
                let f = b.id_for(NodeKind::Function, linkage);
                if b.contains(f) {
                    b.add_edge(id, f, EdgeKind::Method);
                }
            }
        }
        // Formats without member lists (CodeView) qualify each procedure's
        // name with its class instead.
        for (linkage, f) in &self.functions {
            let Some(class) = f.qualified.as_deref().and_then(owner_scope) else {
                continue;
            };
            let (class_id, f_id) = (
                b.id_for(NodeKind::Type, &format!("type:{class}")),
                b.id_for(NodeKind::Function, linkage),
            );
            if self.classes.contains_key(class) && b.contains(f_id) {
                b.add_edge(class_id, f_id, EdgeKind::Method);
            }
        }
    }
}

/// `ns::Shape` for `ns::Shape::area`: everything before the last `::` that
/// is not inside template brackets.
fn owner_scope(qualified: &str) -> Option<&str> {
    let mut depth = 0i32;
    let bytes = qualified.as_bytes();
    let mut split = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b':' if depth == 0 && bytes.get(i + 1) == Some(&b':') => {
                split = Some(i);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    split.map(|i| &qualified[..i]).filter(|s| !s.is_empty())
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
    fn owner_scope_skips_template_arguments() {
        assert_eq!(owner_scope("Shape::area"), Some("Shape"));
        assert_eq!(owner_scope("ns::Box<a::b>::get"), Some("ns::Box<a::b>"));
        assert_eq!(owner_scope("Box<a::b>"), None);
        assert_eq!(owner_scope("main"), None);
    }

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
