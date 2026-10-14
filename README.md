# Lattice

A native Windows IDE with a code-graph visualizer, written in Rust directly
against Win32 via the [`windows`](https://crates.io/crates/windows) crate (no
GUI framework).

**Status:** Stage 1. You get one styled, empty main window.

## Build

Requirements: Windows 10 1703 or later (Windows 11 22H2+ for Mica), and stable
Rust with the MSVC toolchain (`rustup default stable-x86_64-pc-windows-msvc`)
plus the Visual Studio C++ build tools.

```powershell
cargo run -p app            # debug build, keeps a console for log output
cargo build -p app --release   # GUI-subsystem binary: target\release\lattice.exe
```

Quality gates (all must pass before a stage is done):

```powershell
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Architecture

Cargo workspace of small crates:

| Crate | Role |
|---|---|
| `crates/app` | Binary `lattice`: process setup, main window, message loop |
| `crates/ui` | IDE panels and layout (skeleton until stage 2) |
| `crates/engine` | Object-file parsing and graph model; portable, `forbid(unsafe_code)` (skeleton until stage 3) |
| `crates/renderer` | Visualizer viewport drawing (skeleton until stage 4) |

**Unsafe boundary.** `unsafe_code` is denied across the workspace. Only
modules named `sys` (currently `crates/app/src/sys/`) opt back in, and every
`unsafe` block there carries a `// SAFETY:` comment. `sys` exposes safe types
only (`MainWindow`, `Backdrop`, `Error`), and Win32 errors are converted to
the project `Error` at that boundary.

**Main window** (`app/src/sys/window.rs`):
- Per-window state is boxed, handed over in `WM_NCCREATE`, kept in
  `GWLP_USERDATA` behind a `RefCell`, and freed in `WM_NCDESTROY`. Panics are
  caught before they can cross the FFI boundary.
- DPI: the process opts into per-monitor v2 at startup. Sizes are defined in
  DIPs and scaled with the window's DPI. `WM_DPICHANGED` applies the
  system-suggested rect.
- Styling (`sys/dwm.rs`): the dark title bar uses attribute 20, falling back
  to 19 on older Windows 10. Mica (`DWMSBT_MAINWINDOW`) is used with the
  frame extended over the client area; if that is unsupported, the window
  falls back to a solid `#1E1E1E` fill. Any fallback is logged to stderr.
- Shutdown: `WM_CLOSE` → `WM_DESTROY` posts `WM_QUIT(0)` → the loop
  returns, and the process exits with code 0.
