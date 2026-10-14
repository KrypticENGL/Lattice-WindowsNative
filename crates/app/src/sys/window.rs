//! Main window: class registration, window procedure, message loop.
//!
//! Per-window state is a `Box<RefCell<WindowState>>` handed to the window in
//! `WM_NCCREATE`, stored in `GWLP_USERDATA`, and freed in `WM_NCDESTROY`.
//! Handlers borrow it only briefly and never across calls that can re-enter
//! the window procedure.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetStockObject, SetDCBrushColor, DC_BRUSH, HBRUSH, HDC,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA,
    IDC_ARROW, MINMAXINFO, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOWDEFAULT,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

use super::dpi::{self, BASE_DPI};
use super::dwm::{self, Backdrop};
use super::Error;

const CLASS_NAME: PCWSTR = w!("LatticeMainWindow");
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(0);

/// Initial and minimum client sizes, in device-independent pixels.
const INITIAL_CLIENT_DIPS: (i32, i32) = (1280, 800);
const MIN_CLIENT_DIPS: (i32, i32) = (640, 400);

/// Fill colour when no backdrop is available (`0x00BBGGRR`).
const SOLID_BACKGROUND: COLORREF = COLORREF(0x001E_1E1E);
/// Black is alpha 0 to DWM over an extended frame, so Mica shows through.
const TRANSPARENT_BACKGROUND: COLORREF = COLORREF(0x0000_0000);

/// Result of styling the window, reported back to the caller for logging.
#[derive(Debug, Clone, Copy)]
pub struct Appearance {
    pub dark_title_bar: bool,
    pub backdrop: Backdrop,
}

struct WindowState {
    backdrop: Backdrop,
}

/// Passed through `lpCreateParams`. `WM_NCCREATE` takes the state out; if
/// creation fails before that, it is still here and drops normally.
struct CreateParams {
    state: Cell<Option<Box<RefCell<WindowState>>>>,
}

/// The top-level IDE window. Not `Send`: it must stay on the UI thread.
pub struct MainWindow {
    hwnd: HWND,
    appearance: Appearance,
}

impl MainWindow {
    /// Registers the class, creates the (hidden) window, applies DWM styling
    /// and sizes it for the monitor's DPI.
    pub fn create(title: &str) -> Result<Self, Error> {
        let instance = module_instance()?;
        register_class(instance)?;

        let params = CreateParams {
            state: Cell::new(Some(Box::new(RefCell::new(WindowState {
                backdrop: Backdrop::Solid,
            })))),
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
        if let Some(state) = window_state(hwnd) {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.backdrop = backdrop;
            }
        }
        resize_client(hwnd, INITIAL_CLIENT_DIPS)?;

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
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: cursor,
        // No class brush: WM_PAINT fills the whole client area itself.
        hbrBackground: HBRUSH::default(),
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
    // `wndproc`; no state borrow is held here.
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

/// Returns the state stored in `GWLP_USERDATA`, if `WM_NCCREATE` has run and
/// `WM_NCDESTROY` has not.
fn window_state<'a>(hwnd: HWND) -> Option<&'a RefCell<WindowState>> {
    // SAFETY: reading this window's own user-data slot.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<WindowState>;
    // SAFETY: the slot is either null or holds a pointer from
    // `Box::into_raw` set in `WM_NCCREATE`, which stays valid until
    // `WM_NCDESTROY`. Only shared references are created; mutation goes
    // through the `RefCell`, and all access is on the window's thread.
    unsafe { ptr.as_ref() }
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
    let ptr = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut RefCell<WindowState>;
    if !ptr.is_null() {
        // SAFETY: `ptr` came from `Box::into_raw` in `adopt_state`, and the
        // slot is now cleared, so this is the only reclaim.
        drop(unsafe { Box::from_raw(ptr) });
    }
}

fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        // Everything is painted in WM_PAINT; erasing first would flicker.
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_PAINT => {
            paint(hwnd);
            Some(LRESULT(0))
        }
        WM_GETMINMAXINFO => {
            set_min_track_size(hwnd, lparam);
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            apply_suggested_rect(hwnd, lparam);
            let _ = wparam; // New DPI is re-read from the window when needed.
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            // SAFETY: posts WM_QUIT to this thread's queue.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    }
}

fn paint(hwnd: HWND) {
    let backdrop = window_state(hwnd)
        .and_then(|s| s.try_borrow().ok().map(|s| s.backdrop))
        .unwrap_or(Backdrop::Solid);
    let colour = match backdrop {
        Backdrop::Mica => TRANSPARENT_BACKGROUND,
        Backdrop::Solid => SOLID_BACKGROUND,
    };
    let guard = PaintGuard::begin(hwnd);
    let hdc = guard.hdc();
    if hdc.is_invalid() {
        return;
    }
    // SAFETY: `hdc` is the paint DC owned by `guard` for this scope;
    // `DC_BRUSH` is a stock object and must not be deleted.
    unsafe {
        SetDCBrushColor(hdc, colour);
        FillRect(
            hdc,
            &guard.paint.rcPaint,
            HBRUSH(GetStockObject(DC_BRUSH).0),
        );
    }
}

fn set_min_track_size(hwnd: HWND, lparam: LPARAM) {
    // May arrive before WM_NCCREATE; `outer_size_for_client` does not need state.
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
    // SAFETY: `hwnd` is a live window on this thread. Re-enters `wndproc`;
    // no state borrow is held.
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

    fn hdc(&self) -> HDC {
        self.paint.hdc
    }
}

impl Drop for PaintGuard {
    fn drop(&mut self) {
        // SAFETY: matches the `BeginPaint` in `begin` with the same struct.
        // Always succeeds per the documentation.
        let _ = unsafe { EndPaint(self.hwnd, &self.paint) };
    }
}
