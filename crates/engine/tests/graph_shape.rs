//! Graph shape per fixture, asserted by names rather than offsets.

mod common;

use common::{edge, fixture, load, names};
use engine::{DebugFormat, EdgeKind, NodeKind, ObjectFormat};

/// Objects compiled from `src/graph.c`.
const GRAPH_C: &[&str] = &[
    "msvc_z7.obj",
    "msvc_nodebug.obj",
    "mingw_dwarf.o",
    "mingw_nodebug.o",
];

/// 1-based line of the first line in a fixture source containing `needle`.
fn source_line(source: &str, needle: &str) -> u32 {
    let text = std::fs::read_to_string(fixture("src").join(source)).expect("fixture source");
    let index = text
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in {source}"));
    u32::try_from(index + 1).expect("line fits")
}

#[test]
fn c_objects_have_layer_a_shape() {
    for name in GRAPH_C {
        let g = load(name);
        assert_eq!(g.format, ObjectFormat::Coff, "{name}");
        assert_eq!(
            names(&g, NodeKind::Function),
            ["callee", "caller", "helper", "read_counter"],
            "{name}"
        );
        assert_eq!(names(&g, NodeKind::External), ["external_fn"], "{name}");

        assert!(edge(&g, "caller", "callee", EdgeKind::Calls), "{name}");
        assert!(edge(&g, "callee", "helper", EdgeKind::Calls), "{name}");
        assert!(edge(&g, "caller", "external_fn", EdgeKind::Calls), "{name}");

        // Global written by callee, read by helper and read_counter.
        for user in ["callee", "helper", "read_counter"] {
            assert!(
                edge(&g, user, "counter", EdgeKind::References),
                "{name}: {user}"
            );
        }
        // Taking an address is a reference, not a call.
        assert!(
            edge(&g, "callback", "read_counter", EdgeKind::References),
            "{name}"
        );
        assert!(
            !edge(&g, "callback", "read_counter", EdgeKind::Calls),
            "{name}"
        );

        let callee = g.find(NodeKind::Function, "callee").expect("callee");
        assert!(callee.size.is_some_and(|s| s > 0), "{name}");
        assert!(
            g.edges_from(callee.id)
                .any(|e| e.kind == EdgeKind::DefinedIn),
            "{name}: callee has a section"
        );
        // Unwind and debug sections never become nodes.
        assert!(
            g.nodes_of(NodeKind::Section)
                .all(|s| s.display_name.starts_with(".text")
                    || s.display_name.starts_with(".data")
                    || s.display_name.starts_with(".bss")),
            "{name}: {:?}",
            names(&g, NodeKind::Section)
        );
    }
}

#[test]
fn debug_info_adds_sources_locals_and_types() {
    let callee_line = source_line("graph.c", "NOINLINE int callee(");
    for (name, format) in [
        ("msvc_z7.obj", DebugFormat::CodeView),
        ("mingw_dwarf.o", DebugFormat::Dwarf),
    ] {
        let g = load(name);
        assert_eq!(g.debug, format, "{name}");
        assert!(g.warnings.is_empty(), "{name}: {:?}", g.warnings);

        let callee = g.find(NodeKind::Function, "callee").expect("callee");
        let src = callee.source.as_ref().expect("callee source");
        assert!(src.file.ends_with("graph.c"), "{name}: {}", src.file);
        assert_eq!(src.line, callee_line, "{name}");

        for local in ["a", "b", "sum"] {
            assert!(
                edge(&g, "callee", local, EdgeKind::Contains),
                "{name}: {local}"
            );
            assert!(edge(&g, local, "int", EdgeKind::HasType), "{name}: {local}");
        }
        assert!(edge(&g, "caller", "total", EdgeKind::Contains), "{name}");
        assert!(edge(&g, "counter", "int", EdgeKind::HasType), "{name}");
        assert!(edge(&g, "callback", "fn*", EdgeKind::HasType), "{name}");
    }
}

#[test]
fn objects_without_debug_info_have_no_layer_b() {
    for name in ["msvc_nodebug.obj", "mingw_nodebug.o"] {
        let g = load(name);
        assert_eq!(g.debug, DebugFormat::None, "{name}");
        assert!(g.nodes().iter().all(|n| n.source.is_none()), "{name}");
        assert_eq!(g.nodes_of(NodeKind::Type).count(), 0, "{name}");
    }
}

#[test]
fn elf_rust_object() {
    let g = load("elf_dwarf.o");
    assert_eq!(g.format, ObjectFormat::Elf);
    assert_eq!(g.debug, DebugFormat::Dwarf);
    assert_eq!(
        names(&g, NodeKind::Function),
        [
            "callee",
            "caller",
            "helper",
            "lib::mangled_rust_fn",
            "read_counter"
        ]
    );
    assert_eq!(
        names(&g, NodeKind::External),
        ["core::panicking::panic_bounds_check"]
    );
    // Called through a hoisted GOT load (`mov r13, [rip+callee]; call r13`).
    assert!(edge(&g, "caller", "callee", EdgeKind::Calls));
    assert!(edge(
        &g,
        "caller",
        "core::panicking::panic_bounds_check",
        EdgeKind::Calls
    ));
    assert!(edge(&g, "lib::mangled_rust_fn", "helper", EdgeKind::Calls));
    assert!(edge(&g, "callee", "lib::COUNTER", EdgeKind::References));
    assert!(edge(&g, "lib::COUNTER", "AtomicI32", EdgeKind::HasType));
    assert!(edge(&g, "caller", "values", EdgeKind::Contains));

    let callee = g.find(NodeKind::Function, "callee").expect("callee");
    let src = callee.source.as_ref().expect("source");
    assert!(src.file.ends_with("lib.rs"), "{}", src.file);
    assert_eq!(
        src.line,
        source_line("lib.rs", "pub extern \"C\" fn callee")
    );
}

#[test]
fn comdat_functions_appear_once_with_demangled_names() {
    let g = load("msvc_comdat.obj");
    assert_eq!(
        names(&g, NodeKind::Function),
        [
            "shared_inline",
            "twice<int64_t>",
            "twice<int>",
            "use_comdat"
        ]
    );
    let shared = g.find(NodeKind::Function, "shared_inline").expect("node");
    assert_eq!(&*shared.linkage_name, "?shared_inline@@YAHH@Z");
    assert!(edge(&g, "use_comdat", "shared_inline", EdgeKind::Calls));
    assert!(edge(&g, "use_comdat", "twice<int64_t>", EdgeKind::Calls));
    assert!(edge(&g, "shared_inline", "twice<int>", EdgeKind::Calls));
}

#[test]
fn empty_object_is_a_valid_empty_graph() {
    let g = load("msvc_empty.obj");
    assert_eq!(g.nodes().len(), 0, "{}", common::describe(&g));
    assert!(g.edges().is_empty());
}

#[test]
fn ids_are_stable_across_reloads_and_scoped_by_path() {
    let a = load("msvc_z7.obj");
    let b = load("msvc_z7.obj");
    let ids = |g: &engine::CodeGraph| g.nodes().iter().map(|n| n.id).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b));
    assert_eq!(a.edges(), b.edges());

    let bytes = std::fs::read(fixture("msvc_z7.obj")).expect("fixture");
    let elsewhere = engine::load_bytes(&bytes, std::path::Path::new("other/copy.obj"))
        .expect("loads from memory");
    let callee = |g: &engine::CodeGraph| g.find(NodeKind::Function, "callee").map(|n| n.id);
    assert_ne!(callee(&a), callee(&elsewhere));
}

#[test]
fn mingw_cpp_resolves_refptr_cells_and_flags_generated_symbols() {
    let g = load("mingw_cpp.o");
    let locals: std::collections::HashSet<_> = g
        .edges()
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .map(|e| e.to)
        .collect();
    let user = |kind| {
        let mut v: Vec<String> = g
            .nodes_of(kind)
            .filter(|n| !n.artificial && !locals.contains(&n.id))
            .map(|n| n.display_name.to_string())
            .collect();
        v.sort();
        v
    };

    // `.refptr.imported_value` is resolved to the variable it points at.
    assert!(g
        .nodes()
        .iter()
        .all(|n| !n.linkage_name.starts_with(".refptr.")));
    assert!(edge(&g, "main", "imported_value", EdgeKind::References));

    // Only the user's globals remain once generated data is set aside.
    assert_eq!(user(NodeKind::Variable), ["total", "unit"]);
    let functions = user(NodeKind::Function);
    for f in ["main", "measure(Shape const&)", "Square::area() const"] {
        assert!(functions.iter().any(|n| n == f), "{f} in {functions:?}");
    }
    for linkage in [
        "_ZTV6Square",
        "_ZTI5Shape",
        "_ZTS5Shape",
        "_GLOBAL__sub_I_total",
        "_Z41__static_initialization_and_destruction_0v",
        "__main",
    ] {
        let node = g
            .nodes()
            .iter()
            .find(|n| &*n.linkage_name == linkage)
            .unwrap_or_else(|| panic!("{linkage} missing"));
        assert!(node.artificial, "{linkage}");
    }

    // Locals come from DWARF; compiler-supplied `this` is not one of them.
    for local in ["count", "sq", "sum"] {
        assert!(edge(&g, "main", local, EdgeKind::Contains), "{local}");
    }
    assert!(g.nodes().iter().all(|n| &*n.display_name != "this"));

    // Constructors get their location through the abstract-origin chain.
    let ctor = g
        .find(NodeKind::Function, "Square::Square(double)")
        .expect("ctor");
    assert_eq!(
        ctor.source.as_ref().map(|s| s.line),
        Some(source_line("shapes.cpp", "explicit Square(double v)"))
    );
}

#[test]
fn classes_carry_fields_methods_and_declarations() {
    for name in ["mingw_cpp.o", "msvc_cpp.obj"] {
        let g = load(name);
        let square = g.find(NodeKind::Type, "Square").expect("Square");
        assert_eq!(square.size, Some(16), "{name}");
        assert_eq!(
            square.source.as_ref().map(|s| s.line),
            Some(source_line("shapes.cpp", "struct Square")),
            "{name}"
        );
        let targets = |kind| {
            let mut v: Vec<String> = g
                .edges_from(square.id)
                .filter(|e| e.kind == kind)
                .filter_map(|e| g.node(e.to))
                .map(|n| n.display_name.to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(targets(EdgeKind::Contains), ["s"], "{name}");
        assert!(edge(&g, "s", "double", EdgeKind::HasType), "{name}");
        let methods = targets(EdgeKind::Method);
        assert!(
            methods.iter().any(|m| m.starts_with("Square::area")),
            "{name}: {methods:?}"
        );
        assert!(
            methods.iter().any(|m| m.starts_with("Square::Square")),
            "{name}: {methods:?}"
        );
        // Free functions are nobody's methods; library structs are skipped.
        let measure = g
            .nodes_of(NodeKind::Function)
            .find(|n| n.display_name.starts_with("measure"))
            .expect("measure");
        assert!(g
            .edges()
            .iter()
            .all(|e| e.to != measure.id || e.kind != EdgeKind::Method));
        assert!(g.find(NodeKind::Type, "_iobuf").is_none(), "{name}");
    }
}
