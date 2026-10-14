//! Unicode text clipboard.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;

use super::{win_err, NativeWindow};
use crate::Error;

/// Holds the clipboard open; closes it on drop.
struct ClipboardGuard;

impl ClipboardGuard {
    fn open(window: NativeWindow) -> Result<Self, Error> {
        // SAFETY: `window` is a live window on this thread.
        unsafe { OpenClipboard(Some(window.hwnd())) }.map_err(|e| win_err("OpenClipboard", e))?;
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: paired with the successful OpenClipboard in `open`.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Keeps an `HGLOBAL` locked; unlocks on drop.
struct LockGuard {
    mem: HGLOBAL,
    ptr: *mut u16,
}

impl LockGuard {
    fn lock(mem: HGLOBAL) -> Option<Self> {
        // SAFETY: `mem` is a movable global handle we own or the clipboard
        // owns while it is open.
        let ptr = unsafe { GlobalLock(mem) }.cast::<u16>();
        (!ptr.is_null()).then_some(Self { mem, ptr })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // SAFETY: paired with the successful GlobalLock in `lock`.
        let _ = unsafe { GlobalUnlock(self.mem) };
    }
}

pub fn set_text(window: NativeWindow, text: &str) -> Result<(), Error> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _clip = ClipboardGuard::open(window)?;
    // SAFETY: the clipboard is open by this thread.
    unsafe { EmptyClipboard() }.map_err(|e| win_err("EmptyClipboard", e))?;
    let bytes = wide.len() * std::mem::size_of::<u16>();
    // SAFETY: plain allocation; freed below unless the clipboard takes it.
    let mem =
        unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }.map_err(|e| win_err("GlobalAlloc", e))?;
    {
        let Some(lock) = LockGuard::lock(mem) else {
            // SAFETY: `mem` was allocated above and is not owned elsewhere.
            let _ = unsafe { GlobalFree(Some(mem)) };
            return Err(Error::new(
                "GlobalLock",
                0,
                "could not lock clipboard memory",
            ));
        };
        // SAFETY: the block holds `wide.len()` u16s and does not overlap `wide`.
        unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), lock.ptr, wide.len()) };
    }
    // SAFETY: on success the system owns `mem`; on failure we still do.
    if let Err(e) = unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(mem.0))) } {
        // SAFETY: ownership did not transfer, so we free it.
        let _ = unsafe { GlobalFree(Some(mem)) };
        return Err(win_err("SetClipboardData", e));
    }
    Ok(())
}

pub fn get_text(window: NativeWindow) -> Option<String> {
    // SAFETY: plain query.
    unsafe { IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)) }.ok()?;
    let _clip = ClipboardGuard::open(window).ok()?;
    // SAFETY: the clipboard is open; the handle stays owned by the system.
    let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT.0)) }.ok()?;
    let mem = HGLOBAL(handle.0);
    // SAFETY: plain size query on a valid global handle.
    let max_units = unsafe { GlobalSize(mem) } / std::mem::size_of::<u16>();
    let lock = LockGuard::lock(mem)?;
    // SAFETY: the locked block holds `max_units` u16s for as long as `lock`
    // lives; the slice is dropped before it.
    let units = unsafe { std::slice::from_raw_parts(lock.ptr, max_units) };
    let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..len]))
}
