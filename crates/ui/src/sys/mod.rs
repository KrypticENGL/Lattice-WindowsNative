//! Thin Win32/COM wrappers. This module tree is the only place in `ui` where
//! `unsafe` is allowed. What it exposes is safe and free of `windows::` types.
#![allow(unsafe_code)]

pub mod clipboard;
pub mod conpty;
pub mod dialog;
pub mod gfx;
mod handle;
pub mod text;
pub mod time;
mod window;

pub use window::{NativeWindow, Waker, WAKE_MESSAGE};

use crate::Error;

/// Converts a `windows` error into the crate error at the `sys` boundary.
pub(crate) fn win_err(context: &str, err: windows::core::Error) -> Error {
    Error::new(context, err.code().0, err.message())
}
