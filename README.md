# Lattice

A native Windows IDE with a code-graph visualizer, written in Rust directly
against Win32 via the [`windows`](https://crates.io/crates/windows) crate (no
GUI framework).

**Status:** Stage 5 in progress (dockable panels). The app has two
top-level workspaces, and every panel in them can be docked anywhere:
- **Code:** the Solution Explorer, a tabbed editor, an Output panel and a
  ConPTY terminal. Object files (`.obj`, `.o`) expand in the Explorer into
  their functions, variables and externals, read by `crates/engine`, and
  reload automatically when they change on disk.
- **Visualizer**, in two modes:
  - **Call graph:** the call and reference graph of every loaded object,
    merged into one graph and laid out by a force simulation on a worker
    thread. You can pan, zoom, drag, hover and select, and double-click a
    node to open its source.
  - **Data:** the live data of a program stopped under GDB, drawn by
    shape: trees as trees, linked lists as chains, arrays and vectors as
    rows of cells, 2-D arrays as grids, with the stack frames beside them.
    Run, break and step from the toolbar or with the usual debugger keys.

## Build

Requirements: Windows 10 1903 or later (ConPTY; Windows 11 22H2+ for Mica),
and stable Rust with the MSVC toolchain
(`rustup default stable-x86_64-pc-windows-msvc`) plus the Visual Studio C++
build tools.

The Visualizer's Data mode also needs GDB with Python (MinGW-w64 or MSYS2
builds have it; tested with GDB 16.3) on `PATH`, or `LATTICE_GDB` pointing
at `gdb.exe`. The programs it debugs must be built with gcc or g++ and
`-g`; GDB cannot read MSVC's PDB debug info.

```powershell
cargo run -p app -- E:\some\folder   # debug build; the folder argument is optional
cargo build -p app --release         # GUI-subsystem binary: target\release\lattice.exe
target\release\lattice.exe --benchmark=10000          # Visualizer frame-rate benchmark on a synthetic graph
target\release\lattice.exe E:\some\folder --benchmark # ... or on every object file under the folder
```

Quality gates (all must pass before a stage is done):

```powershell
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Using it

| Action | How |
|---|---|
| Open a folder | `Ctrl+O`, the **Open Folder** button, or a path on the command line |
| Open a file | Double-click it in the Explorer (or select it and press `Enter`) |
| Inspect an object file | Click a `.obj` / `.o` in the Explorer. Its symbols load in the background, grouped as Functions / Variables / Externals, with `file:line` when debug info has it. Expand a function to see its parameters and locals, with their types. Standard-library code and compiler-generated symbols are in collapsed **Library** and **Compiler-generated** groups. Rebuilding the file reloads it |
| Switch top tabs | Click **Code** / **Visualizer**, or `Ctrl+1` / `Ctrl+2` |
| See the graph | Every object you load shows up in the **Visualizer**, merged into one graph: a call to an external in one object links to its definition in another. **Add folder objects** loads every `.obj` / `.o` under the open folder. Lime nodes are functions, teal globals, amber unresolved externals. Calls are the brighter edges; arrowheads appear when zoomed in |
| Move around the graph | Drag empty space to pan, wheel to zoom, drag a node to move it. **Fit** (or `F`) frames everything. The arrow keys pan and `+` / `-` zoom. `Space` re-runs the layout |
| Inspect a node | Hover for its name. Click to select: its neighbours and edges light up, everything else dims, and a card shows its kind, source location and caller/callee counts. `Esc` clears the selection. Double-click opens its source at the line, in the Code tab (needs debug info) |
| Filter the graph | **Library** (standard library and toolchain headers), **Generated** (compiler-generated symbols) and **Locals** are off by default; toggling them keeps existing nodes where they are. Types are never nodes: selecting a node shows the types it uses (a variable's type, a function's parameters and locals with theirs) as chips around it |
| Switch Visualizer mode | **Call graph** / **Data** at the left of the Visualizer toolbar. Each mode keeps its own view |
| Debug a program (Data mode) | Build it with `g++ -g` (or `gcc -g`). Set breakpoints with `F9` on the caret line, or click the editor's gutter. `F5` runs it: the program is `foo.exe` next to the open `foo.cpp`, or the one chosen with **Program**. With no breakpoints it stops at `main`. It gets its own console window for input and output |
| Step | `F5` continue, `F10` step over, `F11` step into, `Shift+F11` step out, `Shift+F5` stop, or the toolbar buttons; **Pause** interrupts a running program. In the Code tab the editor follows each stop |
| Play | **Play** (or `F6`) steps through the whole program on its own, starting it if needed: a line every 0.6 s, into your functions and straight back out of library code (sources under the GDB toolchain, or with no source), with the data view, Source and Call stack following each line. It runs until the program ends; **Playing…**, `F6`, **Pause** or any step stops it |
| Read the data | Frames are on the left, outermost first. Pointers are dots with arrows to what they point at; back pointers (`prev`, `parent`, cycles, shared nodes) are grey curves. A value that changed in the last step is tinted lime. Click a box to focus it: its type, size and address and each field's type appear, and unrelated boxes dim. Double-click a frame to open its source line. Drag to pan, wheel to zoom, `F` or **Fit** to frame everything |
| Source and call stack (Data mode) | Two panels, left of the canvas by default. **Source** shows the file of the stopped line, marked with a lime chevron like the editor, and scrolls to it at every stop; the gutter toggles breakpoints, a double-click opens the line in the editor, Shift+wheel scrolls sideways. Below it, the call stack lists the frames innermost first with `file:line` and arguments. Click a frame to focus and centre its box; the Source panel then shows its call site. **Source** and **Call stack** in the toolbar close or reopen each panel |
| Dock panels | Drag a panel by its tab. Dropping on a side of another panel docks it there, on its tab strip or middle joins its tabs, and on an outer edge of the workspace docks it along that whole edge; a lime overlay shows where. Drag onto the **Code** / **Visualizer** title to move a panel to the other workspace. `Esc` cancels a drag |
| Close and reopen panels | The `×` on a tab (or a middle-click) closes it; **Panels** in the top strip lists every panel and reopens a closed one where it was. The editor and the Visualizer canvas can move but not close |
| Benchmark | `Ctrl+Shift+G` loads a synthetic 10,000-node graph; `Ctrl+Shift+B` measures frame times and prints them to Output (see [`crates/renderer/DESIGN.md`](crates/renderer/DESIGN.md)) |
| Editor | Type to edit. `Ctrl+S` save, `Ctrl+Z` undo, `Ctrl+Y` / `Ctrl+Shift+Z` redo, `Ctrl+A/C/X/V`, `Ctrl+W` close tab, `Ctrl+Tab` next tab. Shift+arrows or a mouse drag selects; double-click selects a word |
| Terminal | Click its tab or press `` Ctrl+` `` (reopens it if closed). `Ctrl+V` pastes; the mouse wheel scrolls the history. Press `Enter` to restart the shell after it exits |
| Resize panels | Drag any divider between panels |
| Debug only | `Ctrl+Shift+F12` simulates GPU device loss to exercise recovery |

Locals and types come from debug info. Compile with `-g` (gcc/clang) or
`/Z7`, `/Zi` or `/ZI` (MSVC; VS Debug builds use `/ZI`). An object built
without debug info shows "no debug info" and lists only functions, globals
and externals.

Files that are not valid UTF-8 open read-only. Saving keeps the original
line endings (LF/CRLF) and BOM. Closing a modified tab, or the window, asks
whether to save.

## Architecture

Cargo workspace of small crates:

| Crate | Role |
|---|---|
| `crates/app` | Binary `lattice`: process setup, main window, message loop. It translates window messages into `ui` events |
| `crates/ui` | The IDE shell: Direct2D/DirectWrite drawing, panels, layout, the editor buffer, the terminal emulator |
| `crates/engine` | Object-file parsing (COFF, ELF) into a `CodeGraph`. Portable, `forbid(unsafe_code)`, no Windows dependency. See [`crates/engine/README.md`](crates/engine/README.md) for what it reads and its limits |
| `crates/debugger` | Runs GDB (`--interpreter=mi3`) as a child process: an MI parser, the session (breakpoints, run, step) on reader threads, and `snapshot.py`, a GDB Python command that returns the stopped program's frames and every object reachable from them as JSON. Portable, `forbid(unsafe_code)`, no Windows dependency |
| `crates/renderer` | The Visualizer. Data mode (`data/`): a shape-aware layout of a snapshot and its viewport. Call graph: it merges the loaded objects into one model, runs the force layout on a worker thread, and handles the camera, hit testing, culling, level of detail and input. It draws through a `Canvas` trait. Portable, `forbid(unsafe_code)`, no Windows dependency. [`DESIGN.md`](crates/renderer/DESIGN.md) explains why it uses Direct2D rather than Direct3D and records the measured frame rates |

**Unsafe boundary.** `unsafe_code` is denied across the workspace. Only
modules named `sys` (`crates/app/src/sys/`, `crates/ui/src/sys/`) opt back
in, and every `unsafe` block carries a `// SAFETY:` comment. `sys` exposes
safe types only, and `windows::` types never appear in public APIs outside
it. `app` hands the window to `ui` as an opaque `ui::NativeWindow`.

**Window** (`app/src/sys/window.rs`):
- Per-window state is boxed, handed over in `WM_NCCREATE`, and freed in
  `WM_NCDESTROY`.
- The `ui::Shell` sits behind a `RefCell` that is borrowed for one call at a
  time. Re-entry from a modal dialog finds it busy and falls back to default
  handling. Panics are caught before they cross the FFI boundary.
- DPI: the process is per-monitor v2. Mouse coordinates are converted from
  pixels to DIPs once, here. `WM_DPICHANGED` applies the suggested rect and
  updates the Direct2D DPI.
- The title bar is dark, and DWM Mica is applied to the frame. The client
  area is opaque, so Mica shows only in the title bar.

**Rendering** (`ui/src/sys/gfx.rs`, `ui/src/sys/text.rs`):
- One D3D11 device, one D2D device context, and one DXGI flip-discard swap
  chain. Paint happens on demand only (`WM_PAINT` after an invalidate).
- Device-independent resources (D2D and DirectWrite factories, text formats)
  live for the app's lifetime. Device-dependent ones (swap chain, target,
  brush) are dropped and rebuilt after `D2DERR_RECREATE_TARGET` or device
  removal.
- Panels draw through a safe `DrawCtx` that offers rects, text, layouts and
  clip scopes. Colours come from `Theme`.
- The editor caches one `IDWriteTextLayout` per visible line, keyed by line
  id. A line gets a new id whenever it changes. Off-screen layouts are
  evicted after each paint.
- The graph renderer draws through `DrawCtx`, which implements
  `renderer::Canvas`. Nodes and edges go through one `ID2D1SpriteBatch` per
  frame, one call per style bucket: about 3 ms for 10k nodes and 24k edges.
  The batch and its textures are device resources, rebuilt after device
  loss.

**Panels** (`ui/src/`):
- `shell.rs` owns every panel and routes input. The top strip is shared;
  below it is the active workspace's dock area, so the two workspaces never
  share layout or input.
- `dock/` is the docking: `tree.rs` is the layout (splits holding shares
  of their space, leaves that are tab groups) with insert, remove, restore
  and divider drags, pure and unit-tested; `mod.rs` lays a tree out, draws
  tab strips, dividers and the drop overlay, and turns tab drags into
  drops.
- `explorer.rs` is a lazily loaded tree with directories first; `.git` is
  hidden.
- `editor/` contains the line buffer with byte-column positions, undo
  groups (typing coalesces per word), the document view, and the tab strip.
- `output.rs` keeps timestamped log lines.
- `terminal/` contains:
  - a cell grid driven by the `vte` parser: cursor movement, erase,
    insert/delete, SGR with 16/256/RGB colours, scroll regions, the alternate
    screen and DSR replies;
  - the key-to-VT encoding.

**Threads.** Background threads never touch the window:
- `pty-reader` and `pty-wait` serve the terminal;
- `object-loader` runs `engine::load`, and walks folders for **Add folder
  objects**;
- `graph-layout` runs the force simulation and publishes position
  snapshots, at most 60 a second, until the layout settles. Large graphs
  split the repulsion step across scoped threads;
- a `notify` watcher reports object files that change and are due a reload,
  debounced by 300 ms;
- `gdb-reader` and `gdb-stderr` read the debugger's output, turn it into
  events and ask for a snapshot at every stop; `gdb-exit` makes sure GDB is
  gone after Stop.

Each sends data over an mpsc channel and posts one coalesced `WAKE_MESSAGE`
(`WM_APP+1`, no payload). The UI thread drains the channels in
`Shell::on_wake`. No window or COM objects cross threads.

**Known limits.** See `crates/renderer/DESIGN.md` for the Visualizer's
limits: for the call graph, straight edges, linkage-name merging and
hairballs on very dense graphs; for Data mode, gcc/g++ programs only, the
stopped thread's frames as the only roots, and caps of 2,000 objects and
100 elements per container.

From stage 2: the editor has no syntax highlighting, word
wrap, IME composition window or find. The terminal has no text selection or
copy, and wide (CJK) glyphs are not given two cells. These are candidates for
stage 5.
