---
name: win32-unsafe-boundaries
description: Rules for calling Win32/COM through the `windows` crate in this project. Use whenever writing or reviewing code that touches HWND, HDC, GDI/Direct2D/DWM/ConPTY APIs, window procedures, raw pointers, handles, or any `unsafe` block, and when deciding which crate or module a Win32 call belongs in.
---

# Win32 unsafe boundaries

Goal: all `unsafe` and raw Win32 lives in a few thin wrapper modules. Everything else is safe Rust that never sees an `HWND`, a raw pointer, or an `HRESULT`.

## 1. Where unsafe is allowed

- Allowed only in modules named `sys` (e.g. `crates/app/src/sys/`, `crates/ui/src/sys/`). Nowhere else.
- `crates/engine`: `#![forbid(unsafe_code)]`. No exceptions; it must stay UI-free and portable.
- Every other module: `#![deny(unsafe_code)]`. A `sys` module opts out with `#![allow(unsafe_code)]` at its own top only.
- Every `unsafe` block gets a `// SAFETY:` comment stating the invariant that makes it sound. Enable `clippy::undocumented_unsafe_blocks` and `unsafe_op_in_unsafe_fn` as warnings, and fix them.
- Keep unsafe blocks as small as one call. Do not wrap a whole function body in `unsafe`.

## 2. Error handling

- Use `windows::core::Result<T>` and `?`. Do not call `GetLastError` by hand when the binding already returns `Result`.
- For `BOOL`-returning APIs that are not auto-converted, use `.ok()?` on the `BOOL`. For APIs that signal failure by a null/zero return, convert with `Error::from_win32()` immediately after the call, before any other API can overwrite the last-error value.
- Never `unwrap()` or `expect()` on a Win32 call in non-test code. Failures that cannot be recovered should be logged to the Output panel and shut down cleanly.
- Convert to the project's own error type at the `sys` module boundary. Public APIs outside `sys` must not expose `windows::core::Error`.

## 3. Ownership and RAII

COM interfaces in the `windows` crate are reference counted and release on `Drop`. No wrapper needed; just don't `mem::forget` or leak them. Raw handles are `Copy` and do not release themselves, so wrap them:

| Resource | Guard must call |
|---|---|
| `HDC` from `GetDC` | `ReleaseDC` |
| `BeginPaint` | `EndPaint` |
| `HANDLE` (pipes, processes, threads, ConPTY) | `CloseHandle` |
| `HPEN`/`HBRUSH`/`HFONT`/`HBITMAP` | `DeleteObject` |
| `HPCON` (ConPTY) | `ClosePseudoConsole` |
| `CoInitializeEx` | `CoUninitialize`, once per thread |

- One guard type per resource, in `sys`, not `Clone`. Ownership moves, never copies.
- Wide strings: use `w!("...")` for literals. For owned text build a `Vec<u16>` or `HSTRING` and keep it alive for the whole call. Never pass a `PCWSTR` pointing at a temporary.

## 4. Window procedures

- The `wndproc` is an `extern "system" fn`. Keep it tiny: recover state, call a safe method, return.
- State: allocate a `Box<WindowState>` in the creation code, pass its pointer as `lpCreateParams`, store it with `SetWindowLongPtrW(GWLP_USERDATA)` in `WM_NCCREATE`, read it back at the top of every message, and free it in `WM_NCDESTROY` (then clear the slot). Handle a null pointer, since messages arrive before `WM_NCCREATE`.
- Panics must not cross the FFI boundary. Wrap the handler body in `std::panic::catch_unwind`, log the panic, and fall through to `DefWindowProcW`.
- Re-entrancy: calls like `SetWindowPos`, `DestroyWindow`, `SendMessageW`, and `MessageBoxW` can re-enter the wndproc. Never hold a `RefCell` borrow or a mutex guard across one of these. Use `try_borrow_mut` and handle the busy case, or copy what you need out first.
- Always forward unhandled messages to `DefWindowProcW`.

## 5. Threading

- An `HWND` is only touched on the thread that created it. Background threads (layout, file watching, ConPTY reader) never call window APIs directly.
- Cross-thread results go to the UI thread with `PostMessageW` using a `WM_APP + n` message. Transfer payload ownership with `Box::into_raw` on send and `Box::from_raw` on receive, exactly once. Document the message ID and payload type next to its definition.
- Do not send raw `HWND` or COM pointers across threads unless the interface is documented as free-threaded.

## 6. Layering

- `crates/ui` and `crates/renderer` expose safe types (`Rect`, `Color`, `PanelId`, `Event`). They do not expose `windows::*` types in public signatures.
- `crates/engine` knows nothing about Windows. If a Win32 type appears in an engine signature, that is a bug.
- New Win32 functionality goes into an existing `sys` module if one fits, otherwise a new small `sys` module with one responsibility.

## 7. Before finishing a task

1. `cargo check --workspace` and `cargo clippy --workspace -- -D warnings` pass.
2. `grep -rn "unsafe" crates --include=*.rs` shows hits only under `sys/` (and `forbid(unsafe_code)` lines).
3. Every `unsafe` has a `SAFETY:` comment; every new handle has a guard; no `unwrap()` on Win32 results.
4. No `windows::` types leaked into public APIs outside `sys`.
