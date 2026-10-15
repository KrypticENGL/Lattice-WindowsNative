//! Main window: class registration, window procedure, message loop.
//!
//! Per-window state is a `Box<WindowState>` handed to the window in
//! `WM_NCCREATE`, stored in `GWLP_USERDATA`, and freed in `WM_NCDESTROY`.
//! The UI lives in `WindowState::shell` behind a `RefCell`; it is borrowed
//! only for the duration of one call, and a busy borrow (re-entry from a
//! modal dialog inside the shell) falls back to default handling.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

use ui::event::{CursorShape, Event, Key, Modifiers, MouseButton};
use ui::geom::PointF;
use ui::{NativeWindow, Response, Shell, WAKE_MESSAGE};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F24,
    VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_OEM_3, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetCursor,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_DBLCLKS,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM,
    IDC_SIZEALL, IDC_SIZENS, IDC_SIZEWE, MINMAXINFO, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SW_SHOWDEFAULT, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS,
    WM_SIZE, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

use super::dpi::{self, BASE_DPI};
use super::dwm::{self, Backdrop};
use super::Error;

const CLASS_NAME: PCWSTR = w!("LatticeMainWindow");
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(0);

/// Initial and minimum client sizes, in device-independent pixels.
const INITIAL_CLIENT_DIPS: (i32, i32) = (1280, 800);
const MIN_CLIENT_DIPS: (i32, i32) = (800, 500);

/// `MK_LBUTTON` bit in the wParam of mouse messages.
const MK_LBUTTON: usize = 0x0001;
const WHEEL_DELTA: f32 = 120.0;

/// Result of styling the window, reported back to the caller for logging.
#[derive(Debug, Clone, Copy)]
pub struct Appearance {
    pub dark_title_bar: bool,
    pub backdrop: Backdrop,
}

#[derive(Default)]
struct WindowState {
    shell: RefCell<Option<Shell>>,
    /// First half of a UTF-16 surrogate pair from WM_CHAR.
    high_surrogate: Cell<Option<u16>>,
    /// A wake arrived while the shell was busy; re-post it afterwards.
    pending_wake: Cell<bool>,
    tracking_leave: Cell<bool>,
}

/// Passed through `lpCreateParams`. `WM_NCCREATE` takes the state out; if
/// creation fails before that, it is still here and drops normally.
struct CreateParams {
    state: Cell<Option<Box<WindowState>>>,
}

/// The top-level IDE window. Not `Send`: it must stay on the UI thread.
pub struct MainWindow {
    hwnd: HWND,
    appearance: Appearance,
}

impl MainWindow {
    /// Creates the (hidden) window, styles it, sizes it for the monitor's
    /// DPI and builds the UI, optionally opening `folder`.
    pub fn create(title: &str, folder: Option<PathBuf>) -> Result<Self, Error> {
        let instance = module_instance()?;
        register_class(instance)?;

        let params = CreateParams {
            state: Cell::new(Some(Box::default())),
        };
        let title = HSTRING::from(title);
        // SAFETY: the class is registered with `wndproc`; `title` and
        // `params` are alive for the whole call, and `wndproc` only reads
        // `params` during `WM_NCCREATE`, which happens inside this call.
        let hwnd = unsafe {
            CreateWindowExW(
                EX_STYLE,
                CLASS_NAME,
                &title,
                STYLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance),
                Some(std::ptr::from_ref(&params).cast::<c_void>()),
            )
        }
        .map_err(|e| Error::win32("CreateWindowExW", e))?;

        // Style before the first show so the title bar never flashes white.
        let dark_title_bar = dwm::apply_dark_title_bar(hwnd);
        let backdrop = dwm::apply_backdrop(hwnd);
        resize_client(hwnd, INITIAL_CLIENT_DIPS)?;

        // SAFETY: `hwnd` is a live top-level window created on this thread,
        // and it outlives the shell (dropped in WM_DESTROY).
        let native = unsafe { NativeWindow::from_raw(hwnd.0 as isize) };
        let shell = match Shell::new(native, window_dpi(hwnd), folder) {
            Ok(shell) => shell,
            Err(e) => {
                // SAFETY: destroying the window we just created on this thread.
                let _ = unsafe { DestroyWindow(hwnd) };
                return Err(Error::other("Shell::new", e.to_string()));
            }
        };
        if let Some(state) = window_state(hwnd) {
            *state.shell.borrow_mut() = Some(shell);
        }
        let (width, height) = client_size(hwnd);
        with_shell(hwnd, |s| s.resize(width, height));

        Ok(Self {
            hwnd,
            appearance: Appearance {
                dark_title_bar,
                backdrop,
            },
        })
    }

    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// Writes a line to the Output panel.
    pub fn log(&self, error: bool, text: &str) {
        with_shell(self.hwnd, |s| s.log(error, text));
    }

    /// Starts a Visualizer benchmark (see `Shell::benchmark`).
    pub fn benchmark(&self, synthetic: Option<usize>) {
        let response = with_shell(self.hwnd, |s| s.benchmark(synthetic));
        apply(self.hwnd, response);
    }

    pub fn show(&self) {
        // SAFETY: `hwnd` is a live window created on this thread. The return
        // value is the previous visibility, not an error.
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWDEFAULT) };
    }
}

/// Pumps messages until `WM_QUIT`, returning its exit code.
pub fn run_message_loop() -> Result<i32, Error> {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer for the duration of the call.
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match ret.0 {
            0 => return Ok(msg.wParam.0 as i32),
            -1 => return Err(Error::last_win32("GetMessageW")),
            _ => {
                // SAFETY: `msg` was just filled in by `GetMessageW`. The
                // return values carry no error information.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

fn module_instance() -> Result<HINSTANCE, Error> {
    // SAFETY: a null module name returns the handle of the current
    // executable, which is valid for the life of the process.
    let module =
        unsafe { GetModuleHandleW(None) }.map_err(|e| Error::win32("GetModuleHandleW", e))?;
    Ok(module.into())
}

fn register_class(instance: HINSTANCE) -> Result<(), Error> {
    // SAFETY: `IDC_ARROW` is a predefined system cursor; no instance needed.
    let cursor =
        unsafe { LoadCursorW(None, IDC_ARROW) }.map_err(|e| Error::win32("LoadCursorW", e))?;
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: cursor,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and `CLASS_NAME` is a static
    // string, so every pointer it holds outlives the call.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(Error::last_win32("RegisterClassExW"));
    }
    Ok(())
}

/// Sets the client area to `dips` at the window's current DPI.
fn resize_client(hwnd: HWND, dips: (i32, i32)) -> Result<(), Error> {
    let (width, height) = outer_size_for_client(hwnd, dips)?;
    // SAFETY: `hwnd` is a live window owned by this thread. This re-enters
    // `wndproc`; no shell borrow is held here.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| Error::win32("SetWindowPos", e))
}

/// Outer window size whose client area is `dips` at the window's DPI.
fn outer_size_for_client(hwnd: HWND, (width, height): (i32, i32)) -> Result<(i32, i32), Error> {
    let dpi = window_dpi(hwnd);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: dpi::scale(width, dpi),
        bottom: dpi::scale(height, dpi),
    };
    // SAFETY: `rect` is a valid in/out pointer for the duration of the call.
    unsafe { AdjustWindowRectExForDpi(&mut rect, STYLE, false, EX_STYLE, dpi) }
        .map_err(|e| Error::win32("AdjustWindowRectExForDpi", e))?;
    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

fn window_dpi(hwnd: HWND) -> u32 {
    // SAFETY: plain query on a window handle; returns 0 for an invalid one.
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => BASE_DPI,
        dpi => dpi,
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out-pointer; `hwnd` is live.
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return (0, 0);
    }
    (
        u32::try_from(rect.right - rect.left).unwrap_or(0),
        u32::try_from(rect.bottom - rect.top).unwrap_or(0),
    )
}

/// Returns the state stored in `GWLP_USERDATA`, if `WM_NCCREATE` has run and
/// `WM_NCDESTROY` has not.
fn window_state<'a>(hwnd: HWND) -> Option<&'a WindowState> {
    // SAFETY: reading this window's own user-data slot.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
    // SAFETY: the slot is either null or holds a pointer from
    // `Box::into_raw` set in `WM_NCCREATE`, which stays valid until
    // `WM_NCDESTROY`. Only shared references are created; mutation goes
    // through `Cell`/`RefCell`, and all access is on the window's thread.
    unsafe { ptr.as_ref() }
}

/// Runs `f` on the shell if it exists and is not already borrowed. Applies
/// repaint requests and re-posts a wake that arrived while it was busy.
fn with_shell<R>(hwnd: HWND, f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    let state = window_state(hwnd)?;
    let result = {
        let mut guard = state.shell.try_borrow_mut().ok()?;
        let shell = guard.as_mut()?;
        f(shell)
    };
    if state.pending_wake.take() {
        // SAFETY: posting to our own live window from its thread.
        let _ = unsafe { PostMessageW(Some(hwnd), WAKE_MESSAGE, WPARAM(0), LPARAM(0)) };
    }
    Some(result)
}

fn apply(hwnd: HWND, response: Option<Response>) {
    if response.is_some_and(|r| r.repaint) {
        // SAFETY: invalidates our own live window; no rect, no erase.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }
}

fn dispatch(hwnd: HWND, event: Event) {
    let response = with_shell(hwnd, |s| s.on_event(event));
    apply(hwnd, response);
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = panic::catch_unwind(AssertUnwindSafe(|| match msg {
        WM_NCCREATE => {
            adopt_state(hwnd, lparam);
            None
        }
        WM_NCDESTROY => {
            release_state(hwnd);
            None
        }
        _ => handle_message(hwnd, msg, wparam, lparam),
    }));
    match handled {
        Ok(Some(result)) => result,
        Ok(None) => default_proc(hwnd, msg, wparam, lparam),
        Err(_) => {
            eprintln!("lattice: panic in window procedure (message 0x{msg:04X})");
            default_proc(hwnd, msg, wparam, lparam)
        }
    }
}

fn default_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwards the exact arguments the system passed to `wndproc`.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn adopt_state(hwnd: HWND, lparam: LPARAM) {
    // SAFETY: for WM_NCCREATE, `lparam` points to the CREATESTRUCTW built by
    // `CreateWindowExW`, valid for this message.
    let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
    // SAFETY: `MainWindow::create` passes a `&CreateParams` that outlives the
    // `CreateWindowExW` call this message is delivered within.
    let Some(params) = (unsafe { create.lpCreateParams.cast::<CreateParams>().as_ref() }) else {
        return;
    };
    if let Some(state) = params.state.take() {
        // SAFETY: stores an owned pointer in this window's own slot; it is
        // reclaimed exactly once in `release_state`.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize) };
    }
}

fn release_state(hwnd: HWND) {
    // SAFETY: clears this window's own slot and returns the previous value.
    let ptr = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut WindowState;
    if !ptr.is_null() {
        // SAFETY: `ptr` came from `Box::into_raw` in `adopt_state`, and the
        // slot is now cleared, so this is the only reclaim.
        drop(unsafe { Box::from_raw(ptr) });
    }
}

fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let state = window_state(hwnd)?;
    match msg {
        // Direct2D paints everything; erasing first would flicker.
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_PAINT => {
            // Validates the update region even when the shell is busy.
            let _guard = PaintGuard::begin(hwnd);
            let response = with_shell(hwnd, Shell::paint);
            apply(hwnd, response);
            Some(LRESULT(0))
        }
        WM_SIZE => {
            let width = u32::from(loword(lparam.0 as usize));
            let height = u32::from(hiword(lparam.0 as usize));
            let response = with_shell(hwnd, |s| s.resize(width, height));
            apply(hwnd, response);
            Some(LRESULT(0))
        }
        WM_GETMINMAXINFO => {
            set_min_track_size(hwnd, lparam);
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = u32::from(loword(wparam.0));
            with_shell(hwnd, |s| s.set_dpi(dpi));
            apply_suggested_rect(hwnd, lparam);
            apply(hwnd, Some(Response { repaint: true }));
            Some(LRESULT(0))
        }
        WM_SETCURSOR if u32::from(loword(lparam.0 as usize)) == HTCLIENT => {
            let shape = state
                .shell
                .try_borrow()
                .ok()
                .and_then(|s| s.as_ref().map(Shell::cursor))
                .unwrap_or_default();
            set_cursor(shape);
            Some(LRESULT(1))
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (button, clicks) = match msg {
                WM_LBUTTONDBLCLK => (MouseButton::Left, 2),
                WM_RBUTTONDOWN => (MouseButton::Right, 1),
                WM_MBUTTONDOWN => (MouseButton::Middle, 1),
                _ => (MouseButton::Left, 1),
            };
            if button == MouseButton::Left {
                // SAFETY: captures the mouse for our own window on its thread.
                unsafe { SetCapture(hwnd) };
            }
            dispatch(
                hwnd,
                Event::MouseDown {
                    pos: client_point(hwnd, lparam),
                    button,
                    mods: modifiers(),
                    clicks,
                },
            );
            Some(LRESULT(0))
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let button = match msg {
                WM_RBUTTONUP => MouseButton::Right,
                WM_MBUTTONUP => MouseButton::Middle,
                _ => MouseButton::Left,
            };
            if button == MouseButton::Left {
                // SAFETY: releases a capture this thread set; harmless if none.
                let _ = unsafe { ReleaseCapture() };
            }
            dispatch(
                hwnd,
                Event::MouseUp {
                    pos: client_point(hwnd, lparam),
                    button,
                    mods: modifiers(),
                },
            );
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE => {
            if !state.tracking_leave.get() {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: `track` is valid for the call; `hwnd` is ours.
                if unsafe { TrackMouseEvent(&mut track) }.is_ok() {
                    state.tracking_leave.set(true);
                }
            }
            dispatch(
                hwnd,
                Event::MouseMove {
                    pos: client_point(hwnd, lparam),
                    mods: modifiers(),
                    left_down: wparam.0 & MK_LBUTTON != 0,
                },
            );
            Some(LRESULT(0))
        }
        WM_MOUSELEAVE => {
            state.tracking_leave.set(false);
            dispatch(hwnd, Event::MouseLeave);
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = f32::from(hiword(wparam.0) as i16) / WHEEL_DELTA;
            let horizontal = msg == WM_MOUSEHWHEEL;
            dispatch(
                hwnd,
                Event::Wheel {
                    pos: screen_to_client_point(hwnd, lparam),
                    // Tilting right means "toward the end", like wheel down.
                    lines: if horizontal { -delta } else { delta },
                    horizontal,
                    mods: modifiers(),
                },
            );
            Some(LRESULT(0))
        }
        WM_KEYDOWN => {
            let key = translate_key(VIRTUAL_KEY(loword(wparam.0)))?;
            dispatch(
                hwnd,
                Event::KeyDown {
                    key,
                    mods: modifiers(),
                },
            );
            Some(LRESULT(0))
        }
        WM_CHAR => {
            if let Some(c) = decode_char(state, loword(wparam.0)) {
                if !c.is_control() {
                    dispatch(hwnd, Event::Char(c));
                }
            }
            Some(LRESULT(0))
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            dispatch(hwnd, Event::Focus(msg == WM_SETFOCUS));
            Some(LRESULT(0))
        }
        WAKE_MESSAGE => {
            match with_shell(hwnd, Shell::on_wake) {
                Some(response) => apply(hwnd, Some(response)),
                None => state.pending_wake.set(true),
            }
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            // `None` (busy) means a modal is already up; ignore the close.
            match with_shell(hwnd, Shell::query_close) {
                Some(true) => None,
                _ => Some(LRESULT(0)),
            }
        }
        WM_DESTROY => {
            // Drop the UI (closing the terminal and GPU resources) while the
            // window still exists, then end the message loop.
            if let Ok(mut shell) = state.shell.try_borrow_mut() {
                shell.take();
            }
            // SAFETY: posts WM_QUIT to this thread's queue.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    }
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

fn scale_to_dips(hwnd: HWND, x: i32, y: i32) -> PointF {
    let scale = window_dpi(hwnd) as f32 / BASE_DPI as f32;
    PointF::new(x as f32 / scale, y as f32 / scale)
}

/// Client-area point from a mouse message's lParam, in DIPs.
fn client_point(hwnd: HWND, lparam: LPARAM) -> PointF {
    let raw = lparam.0 as usize;
    let x = i32::from(loword(raw) as i16);
    let y = i32::from(hiword(raw) as i16);
    scale_to_dips(hwnd, x, y)
}

/// Wheel messages carry screen coordinates.
fn screen_to_client_point(hwnd: HWND, lparam: LPARAM) -> PointF {
    let raw = lparam.0 as usize;
    let mut p = POINT {
        x: i32::from(loword(raw) as i16),
        y: i32::from(hiword(raw) as i16),
    };
    // SAFETY: `p` is a valid in/out pointer; `hwnd` is live.
    let _ = unsafe { ScreenToClient(hwnd, &mut p) };
    scale_to_dips(hwnd, p.x, p.y)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain query of this thread's keyboard state.
    let state = unsafe { GetKeyState(i32::from(vk.0)) };
    state < 0
}

fn modifiers() -> Modifiers {
    Modifiers {
        ctrl: key_down(VK_CONTROL),
        shift: key_down(VK_SHIFT),
        alt: key_down(VK_MENU),
    }
}

fn translate_key(vk: VIRTUAL_KEY) -> Option<Key> {
    let key = match vk {
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_DELETE => Key::Delete,
        VK_INSERT => Key::Insert,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_SPACE => Key::Space,
        VK_OEM_3 => Key::Backquote,
        v if (VK_F1.0..=VK_F24.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
        v if (0x41..=0x5A).contains(&v.0) => Key::Letter(char::from(v.0 as u8)),
        v if (0x30..=0x39).contains(&v.0) => Key::Digit((v.0 - 0x30) as u8),
        _ => return None,
    };
    Some(key)
}

/// Joins UTF-16 surrogate pairs that arrive as two WM_CHAR messages.
fn decode_char(state: &WindowState, unit: u16) -> Option<char> {
    match unit {
        0xD800..=0xDBFF => {
            state.high_surrogate.set(Some(unit));
            None
        }
        0xDC00..=0xDFFF => {
            let high = state.high_surrogate.take()?;
            char::decode_utf16([high, unit]).next()?.ok()
        }
        _ => {
            state.high_surrogate.set(None);
            char::from_u32(u32::from(unit))
        }
    }
}

fn set_cursor(shape: CursorShape) {
    let id = match shape {
        CursorShape::Arrow => IDC_ARROW,
        CursorShape::IBeam => IDC_IBEAM,
        CursorShape::Hand => IDC_HAND,
        CursorShape::SizeWE => IDC_SIZEWE,
        CursorShape::SizeNS => IDC_SIZENS,
        CursorShape::Move => IDC_SIZEALL,
    };
    // SAFETY: loads a shared system cursor (never freed) and sets it for
    // this thread.
    if let Ok(cursor) = unsafe { LoadCursorW(None, id) } {
        // SAFETY: as above.
        unsafe { SetCursor(Some(cursor)) };
    }
}

fn set_min_track_size(hwnd: HWND, lparam: LPARAM) {
    // May arrive before WM_NCCREATE; `outer_size_for_client` needs no state.
    let Ok((width, height)) = outer_size_for_client(hwnd, MIN_CLIENT_DIPS) else {
        return;
    };
    // SAFETY: for WM_GETMINMAXINFO, `lparam` points to a MINMAXINFO the
    // system owns for the duration of this message.
    if let Some(info) = unsafe { (lparam.0 as *mut MINMAXINFO).as_mut() } {
        info.ptMinTrackSize.x = width;
        info.ptMinTrackSize.y = height;
    }
}

fn apply_suggested_rect(hwnd: HWND, lparam: LPARAM) {
    // SAFETY: for WM_DPICHANGED, `lparam` points to a RECT with the
    // suggested window bounds, valid for this message. Copied out at once.
    let Some(rect) = (unsafe { (lparam.0 as *const RECT).as_ref() }).copied() else {
        return;
    };
    // SAFETY: `hwnd` is a live window on this thread. Re-enters `wndproc`
    // (WM_SIZE); no shell borrow is held.
    let moved = unsafe {
        SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if let Err(err) = moved {
        eprintln!("lattice: {}", Error::win32("SetWindowPos", err));
    }
}

/// Pairs `BeginPaint` with `EndPaint`.
struct PaintGuard {
    hwnd: HWND,
    paint: PAINTSTRUCT,
}

impl PaintGuard {
    fn begin(hwnd: HWND) -> Self {
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: called while handling WM_PAINT for `hwnd`; `paint` is a
        // valid out-pointer. `Drop` calls the matching `EndPaint`.
        unsafe { BeginPaint(hwnd, &mut paint) };
        Self { hwnd, paint }
    }
}

impl Drop for PaintGuard {
    fn drop(&mut self) {
        // SAFETY: matches the `BeginPaint` in `begin` with the same struct.
        // Always succeeds per the documentation.
        let _ = unsafe { EndPaint(self.hwnd, &self.paint) };
    }
}
