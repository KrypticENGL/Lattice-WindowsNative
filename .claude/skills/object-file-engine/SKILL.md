---
name: object-file-engine
description: Design and rules for crates/engine, which loads compiled object files (COFF .obj, ELF .o) at runtime and builds the CodeGraph that the IDE and renderer consume. Use when writing or changing parsing, symbol/relocation analysis, debug-info reading, graph types, or engine tests.
---

# Object-file engine

`crates/engine` turns compiled object files into a `CodeGraph`: nodes for functions, variables, types and sections, edges for calls, references and containment. It has no UI or Win32 dependency.

## 1. Hard rules

- `#![forbid(unsafe_code)]`. Read files with `std::fs::read`, not `memmap2` (which needs unsafe).
- No `windows::` imports. `cargo tree -p engine` must show no `windows` crate.
- Malformed, truncated, or unsupported input returns a structured `EngineError`. It never panics. No `unwrap()` or unchecked slicing on file data.
- Public API: `load(path) -> Result<CodeGraph, EngineError>` and a reload path driven by a file watcher (the `notify` crate, started by the app, not the engine). Everything is `Send` so loading can run off the UI thread.

## 2. Parsing layers (build in this order)

**Layer A: symbols, sections, relocations (works for COFF and ELF).**
Use the `object` crate: `object::File::parse`, then `symbols()`, `sections()`, `section.relocations()`. A relocation whose target is `RelocationTarget::Symbol(idx)` gives a reference from the section containing it to that symbol.

- Functions compiled with function sections (`/Gy` on MSVC, the default in rustc) get one section per function, so a section's relocations are that function's outgoing references. Map symbol to section first.
- For a shared `.text`, derive function ranges by sorting symbols by address. COFF symbols have no size, so use the gap to the next symbol, and prefer debug-info lengths when available.
- Relocations do not say whether a reference is a call or an address-taken use. v1 labels such edges `References`. To refine into `Calls`, decode the instruction at the relocation offset (the `iced-x86` crate is the optional tool); keep that as a separate, testable step.
- Handle COMDAT/duplicate sections, undefined (external) symbols as `External` nodes, and weak symbols.

**Layer B: debug info (names, source locations, types, locals).**
- DWARF (ELF `.o`, or windows-gnu targets): `gimli`. In relocatable objects the DWARF sections contain unresolved relocations, so addresses must be relocated before use; use gimli's relocation-aware reader pattern from its examples, and handle compressed sections.
- MSVC targets: debug info is CodeView. With `/Zi` it lives in a PDB (use the `pdb` crate on the PDB). With `/Z7` it lives in the object's `.debug$S`/`.debug$T`. The `pdb` crate reads PDB files, not raw object sections, so **check the crate docs before promising `.debug$S` support**. If unsupported, document the limitation, and either require a PDB, or recommend the windows-gnu/ELF DWARF path for that feature.
- Debug info is optional. A graph built from Layer A alone must be complete and valid; Layer B only enriches it (source file/line, local variables, types).

## 3. Graph model

Own plain structs, serializable with `serde`. Build `petgraph` views from them as needed rather than making petgraph the storage format.

- `NodeKind`: `Function`, `Variable`, `Type`, `Section`, `External`.
- `EdgeKind`: `Calls`, `References`, `Contains` (function to locals/params), `HasType`, `DefinedIn` (symbol to section).
- Each node: stable `NodeId` derived from (kind, linkage name, object path), a `display_name`, the raw `linkage_name`, optional `SourceLoc { file, line }`, and size when known.
- Stable IDs matter: after a reload, the renderer keeps node positions for IDs that still exist.
- Demangle for display only: `rustc-demangle` (Rust), `msvc-demangler` (MSVC), `cpp_demangle` (Itanium). Keep the linkage name as the identity.

## 4. Tests

- Commit small fixtures (a few KB each) under `crates/engine/tests/fixtures/`, with their tiny C/Rust sources and a script to regenerate them, so tests need no compiler.
- Cover: a function calling another; a global read and written; an unresolved external; duplicate COMDAT sections; an empty object; a truncated file; a non-object file; an object with and without debug info.
- Assert graph shape (nodes and edges by name), not byte offsets.
- Add a malformed-input test for every parse bug found, so it stays fixed.

## 5. Performance and scale

- Parse sections lazily and avoid copying section data; hold slices into the single `Vec<u8>` read from disk.
- Large objects (tens of MB) must load without a UI stall: loading runs on a worker, and progress and errors go to the Output panel through the app layer.
- Intern repeated strings (symbol and file names).

## 6. Before finishing a task

1. `cargo test -p engine`, `cargo clippy -p engine -- -D warnings` pass.
2. `grep -rn unsafe crates/engine` finds only the `forbid` line.
3. Fuzz-style sanity: run `load` on a truncated copy of each fixture and confirm an error, not a panic.
4. Any layer-B limitation found (for example unsupported CodeView) is written in the crate README.
