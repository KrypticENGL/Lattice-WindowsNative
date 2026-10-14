# Lattice

A native Windows IDE with a code-graph visualizer, written in Rust directly
against Win32 via the [`windows`](https://crates.io/crates/windows) crate (no
GUI framework).

**Status:** Stage 3, the object-file engine. The IDE shell from stage 2 is
unchanged: top-level **Code** and **Visualizer** tabs, and in the Code tab
the Solution Explorer, a tabbed editor, an Output panel and a ConPTY
terminal. Object files (`.obj`, `.o`) now expand in the Explorer into their
functions, variables and externals, read by `crates/engine`. The engine
reloads them automatically when they change on disk. The Visualizer tab is
still a placeholder until stage 4.

## Build

Requirements: Windows 10 1903 or later (ConPTY; Windows 11 22H2+ for Mica),
and stable Rust with the MSVC toolchain
(`rustup default stable-x86_64-pc-windows-msvc`) plus the Visual Studio C++
build tools.

```powershell
cargo run -p app -- E:\some\folder   # debug build; the folder argument is optional
cargo build -p app --release         # GUI-subsystem binary: target\release\lattice.exe
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
| Inspect an object file | Click a `.obj` / `.o` in the Explorer. Its symbols load in the background, grouped as Functions / Variables / Externals, with `file:line` when debug info has it. Rebuilding the file reloads it |
| Switch top tabs | Click **Code** / **Visualizer**, or `Ctrl+1` / `Ctrl+2` |
| Editor | Type to edit. `Ctrl+S` save, `Ctrl+Z` undo, `Ctrl+Y` / `Ctrl+Shift+Z` redo, `Ctrl+A/C/X/V`, `Ctrl+W` close tab, `Ctrl+Tab` next tab. Shift+arrows or a mouse drag selects; double-click selects a word |
| Terminal | Click **TERMINAL** or press `` Ctrl+` ``. `Ctrl+V` pastes; the mouse wheel scrolls the history. Press `Enter` to restart the shell after it exits |
| Resize panes | Drag the divider between Explorer and editor, or between editor and the bottom panel |
| Debug only | `Ctrl+Shift+F12` simulates GPU device loss to exercise recovery |

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
| `crates/renderer` | Visualizer viewport drawing (skeleton until stage 4) |

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

**Panels** (`ui/src/`):
- `shell.rs` handles focus and input routing. The top strip is shared;
  everything below it belongs to the active tab, so the Visualizer never
  shares layout or input with the Code tab.
- `split.rs` holds the splitters. The editor-to-bottom splitter keeps the
  bottom pane's size when the window resizes.
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
- `object-loader` runs `engine::load`;
- a `notify` watcher reports object files that change and are due a reload,
  debounced by 300 ms.

Each sends data over an mpsc channel and posts one coalesced `WAKE_MESSAGE`
(`WM_APP+1`, no payload). The UI thread drains the channels in
`Shell::on_wake`. No window or COM objects cross threads.

**Known limits (stage 2).** The editor has no syntax highlighting, word
wrap, IME composition window or find. The terminal has no text selection or
copy, and wide (CJK) glyphs are not given two cells. These are candidates for
stage 5.
