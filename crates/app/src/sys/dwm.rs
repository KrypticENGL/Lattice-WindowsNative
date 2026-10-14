//! DWM styling: dark title bar and system backdrop, each with a fallback.

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{HWND, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_MAINWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
};
use windows::Win32::UI::Controls::MARGINS;

/// What is painted behind the client area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// Windows 11 22H2+ Mica material shows through the client area.
    Mica,
    /// Opaque dark fill; used wherever Mica is unavailable.
    Solid,
}

/// Pre-20H1 Windows 10 builds used this undocumented id for the same
/// attribute as `DWMWA_USE_IMMERSIVE_DARK_MODE` (20).
const DWMWA_USE_IMMERSIVE_DARK_MODE_LEGACY: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(19);

/// Requests a dark title bar. Returns `false` if the OS does not support it.
pub(super) fn apply_dark_title_bar(hwnd: HWND) -> bool {
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &TRUE).is_ok()
        || set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_LEGACY, &TRUE).is_ok()
}

/// Enables Mica behind the whole window, or reports [`Backdrop::Solid`] if
/// any step is unsupported.
pub(super) fn apply_backdrop(hwnd: HWND) -> Backdrop {
    if set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_MAINWINDOW).is_err() {
        return Backdrop::Solid;
    }
    // Negative margins extend the frame over the entire client area, so
    // pixels painted black (alpha 0) let the backdrop through.
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: `hwnd` is a live window owned by this thread and `margins`
    // outlives the call.
    match unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) } {
        Ok(()) => Backdrop::Mica,
        Err(_) => Backdrop::Solid,
    }
}

fn set_attribute<T: Copy>(
    hwnd: HWND,
    attribute: DWMWINDOWATTRIBUTE,
    value: &T,
) -> windows::core::Result<()> {
    let size = u32::try_from(size_of::<T>()).unwrap_or(u32::MAX);
    // SAFETY: `hwnd` is a live window owned by this thread; `value` points to
    // `size` readable bytes for the duration of the call, and every caller
    // passes the plain-data type DWM documents for `attribute`.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            std::ptr::from_ref(value).cast::<c_void>(),
            size,
        )
    }
}
