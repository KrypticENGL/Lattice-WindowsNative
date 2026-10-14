//! RAII guard for kernel `HANDLE`s.

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// Owns a kernel handle and closes it on drop. Not `Clone`.
#[derive(Debug)]
pub(crate) struct OwnedHandle(HANDLE);

impl OwnedHandle {
    pub(crate) fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: this guard is the sole owner of the handle and closes
            // it exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: kernel handles (pipes, processes) are process-wide and may be used
// and closed from any thread; ownership moves with the guard.
unsafe impl Send for OwnedHandle {}
