//! Opaque window handle and the cross-thread waker.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted (no payload) by [`Waker::wake`] when a background thread has data
/// queued for the UI thread. The app forwards it to `Shell::on_wake`, which
/// drains the channels. wParam and lParam are always zero.
pub const WAKE_MESSAGE: u32 = WM_APP + 1;

/// The top-level window, as an opaque value. Not `Send`: window APIs may only
/// be called from the thread that created the window.
#[derive(Debug, Clone, Copy)]
pub struct NativeWindow {
    raw: isize,
    _not_send: PhantomData<*const ()>,
}

impl NativeWindow {
    /// Wraps a raw `HWND` value.
    ///
    /// # Safety
    /// `raw` must be a valid top-level window created on the calling thread,
    /// and the window must outlive every use of the returned value.
    pub unsafe fn from_raw(raw: isize) -> Self {
        Self {
            raw,
            _not_send: PhantomData,
        }
    }

    pub(crate) fn hwnd(self) -> HWND {
        HWND(self.raw as *mut c_void)
    }
}

/// Wakes the UI thread from any thread. Coalesces: at most one
/// [`WAKE_MESSAGE`] is in flight until the UI thread calls [`Waker::reset`].
#[derive(Debug, Clone)]
pub struct Waker {
    raw: isize,
    pending: Arc<AtomicBool>,
}

impl Waker {
    pub(crate) fn new(window: NativeWindow) -> Self {
        Self {
            raw: window.raw,
            pending: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn wake(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let hwnd = HWND(self.raw as *mut c_void);
        // SAFETY: PostMessageW is documented as callable from any thread. If
        // the window is gone the call fails harmlessly.
        if unsafe { PostMessageW(Some(hwnd), WAKE_MESSAGE, WPARAM(0), LPARAM(0)) }.is_err() {
            self.pending.store(false, Ordering::Release);
        }
    }

    /// Called on the UI thread before draining, so later data posts again.
    pub(crate) fn reset(&self) {
        self.pending.store(false, Ordering::Release);
    }
}
