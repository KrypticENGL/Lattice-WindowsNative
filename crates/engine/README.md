# engine

Loads compiled object files and builds a `CodeGraph`. The crate is portable
and has no UI or Windows dependency. It is `#![forbid(unsafe_code)]` and
spawns no threads. Malformed input returns an `EngineError`; it never
panics.

```rust
let graph = engine::load(Path::new("foo.obj"))?;   // or engine::load_bytes(&bytes, path)
for f in graph.nodes_of(engine::NodeKind::Function) { /* ... */ }
```

Reload means calling `load` again. Node ids hash `(kind, linkage name,
object path)`, so they stay the same across reloads, and a renderer can keep
positions for nodes that still exist. Watching files for changes is up to the
caller (the IDE does it in `crates/ui`).

## What is read

| Layer | Source | Produces |
|---|---|---|
| A | Symbols, sections, relocations (`object`) | `Function`, `Variable`, `External`, `Section` nodes. `References` / `Calls` / `DefinedIn` edges |
| A | x86-64 code at each relocation (`iced-x86`) | Splits `Calls` from `References`. Covers `call`/`jmp`, `call [rip+x]`, and a RIP-relative load whose register is later called |
| B | DWARF (`gimli`): ELF objects, and MinGW/windows-gnu COFF objects | Source file and line, function size, parameters and locals (`Contains`), types (`HasType`) |
| B | CodeView `.debug$S` / `.debug$T` (MSVC `/Z7`) | The same, parsed in this crate |

Unwind tables (`.pdata`, `.xdata`, `.eh_frame`), debug sections and linker
directives are not graphed. MSVC associative COMDATs are ignored with them.
A linkage name defined more than once becomes a single node.

Exception-handling funclets (`?dtor$N@?0?f@4HA` and the like) do not
become nodes. They are cleanup code belonging to `f`, so their code range,
and any calls made from it, are attributed to the function before them.

On this repository's own `target/` folder (2,923 rustc objects, 161 MB),
the bulk test loads every object with no errors in about 2.5 s (release
build). 99.9% of functions get a source location.

## Limitations

- **MSVC `/Zi`:** the object holds only a reference to an external PDB, so
  there is no Layer B for it. Build with `/Z7`, or use the DWARF path
  (windows-gnu or ELF), to get debug info. Layer A works either way.
- **`pdb` crate not used:** `pdb` 0.8 reads PDB files only, and its
  symbol-record parser is not public, so the CodeView reader here handles
  just the records it needs: procedures, data, `S_LOCAL`, `S_REGREL32`,
  `S_BPREL32`, line starts, and type names.
- **CodeView types are names only.** Supported: primitives, pointers,
  `const`, arrays, struct/class/union/enum and procedures.
- **`Calls` refinement is x86-64 only.** On other architectures every
  symbol relocation is `References`. The register tracking behind the
  hoisted-load case is a simple linear scan, not data flow.
- **No archives (`.lib`/`.a`/`.rlib`) or linked executables.** Load the
  member `.obj`/`.o` instead. Mach-O is not supported.
- **DWARF is matched to symbols by linkage name.** Debug entries for
  functions without a symbol (fully inlined or discarded) are ignored.

## Tests

`tests/fixtures/` holds small committed objects built from `tests/fixtures/src/`.
Regenerate them with `tests/fixtures/regen.ps1`, which needs MSVC, MinGW gcc
and `rustup target add x86_64-unknown-linux-gnu`. The tests themselves need
no compiler.

- `graph_shape.rs` asserts the nodes and edges of each fixture by name.
- `robustness.rs` checks that inputs which are not objects, are truncated,
  or have random bytes flipped all return `Ok` or an error, never a panic.
- To print every fixture's graph:
  `cargo test -p engine --test dump -- --ignored --nocapture`.
- `bulk.rs` (ignored by default) loads every object under a folder:
  `LATTICE_BULK_DIR=target cargo test -p engine --release --test bulk -- --ignored --nocapture`.
