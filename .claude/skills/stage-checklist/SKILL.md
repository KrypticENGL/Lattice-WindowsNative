---
name: stage-checklist
description: Defines the five build stages of this project, what each stage includes and excludes, and the verification gates and handoff format. Use at the start and end of every task, whenever deciding whether a piece of work belongs in the current stage, and before declaring a stage done.
---

# Stage checklist

The project is built in strict stages. Do only the current stage. If a request touches a later stage, say so and stop at the boundary.

## Process for every stage

1. Read `CLAUDE.md` and this file. State which stage you are working on.
2. Plan first (plan mode), list the files you will create or change, and wait for approval on anything beyond the stage's scope.
3. Implement only what the stage lists.
4. Run the gates below. Fix failures; do not hand over a red build.
5. Update the README or crate docs for what changed.
6. Commit as `stageN: <summary>` and stop. Do not begin the next stage.

## Gates (every stage)

```
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Builds target Windows (MSVC toolchain). If working from a non-Windows host, say which gates could not run and why. A GUI cannot be seen from the terminal, so ask the user to confirm the visible behavior listed under each stage.

## Stage 1: Baseline native window (initiation only)

**Includes:** Cargo workspace with `app`, `ui`, `engine`, `renderer` (the last three empty skeletons); one Win32 window with DWM dark title bar, backdrop with graceful fallback, per-monitor v2 DPI, resize, clean shutdown; README with build steps and a short architecture note.
**Excludes:** any panel, text rendering, parsing, graph, or renderer logic.
**Done when:** gates pass, and the user confirms an empty styled window opens, resizes, and closes cleanly.

## Stage 2: IDE shell and separate Visualizer tab

**Includes:** top-level tabs "Code" and "Visualizer" (placeholder); in the Code tab, Solution Explorer (folder tree, open on double-click), editor (open, edit, save, selection, scroll, undo/redo), Output panel, ConPTY terminal, resizable splitters.
**Excludes:** object-file parsing, graph logic, any graph drawing.
**Done when:** gates pass; user confirms they can open a folder, edit and save a file, see output lines, run commands in the terminal (including resize), and switch tabs with the Visualizer fully separate from the Code layout.

## Stage 3: Object-file engine

**Includes:** everything in the `object-file-engine` skill's layers A and B (with documented limits), `CodeGraph`, `load`/reload API, fixtures and tests, a simple listing of loaded symbols in the Solution Explorer.
**Excludes:** drawing the graph.
**Done when:** gates pass; `cargo tree -p engine` shows no Windows dependency; malformed inputs return errors; a real object file loads and its symbols appear in the IDE.

## Stage 4: Graph renderer

**Includes:** `crates/renderer` drawing `CodeGraph` in the Visualizer tab per the `direct2d-rendering-patterns` skill: off-thread force-directed layout, node and edge styles, pan, zoom, drag, hover, select with neighbor highlight, double-click to jump to source when location data exists, culling and LOD, and a short design note justifying Direct2D vs Direct3D.
**Excludes:** editor features, build integration, settings, installer.
**Done when:** gates pass; the user confirms interaction is smooth on a graph of about 10k nodes (record the measured frame rate in the design note) and that selecting a node highlights its neighbors.

## Stage 5: Finalization (not started until the user schedules it)

Candidate items: syntax highlighting and a language-server hook; build/run integration that auto-loads produced object files; settings, themes, persisted layout; performance and memory pass; error handling and crash reporting; installer, code signing, CI. Treat each item as its own task and ask the user to prioritize before starting.

## Handoff format (end of every task)

- **Done:** what was built, in one or two sentences.
- **Verified:** which gates ran and passed, and what the user still needs to eyeball.
- **Deferred:** anything intentionally left for a later stage, or limitations found.
- **Risks / questions:** anything that needs the user's decision.
