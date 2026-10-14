//! Project error type that `windows::core::Error` is converted into at the
//! `sys` boundary.

use std::fmt;

#[derive(Debug, Clone)]
pub struct Error {
    context: &'static str,
    code: i32,
    message: String,
}

impl Error {
    /// Wraps a Win32/COM error with the name of the API that produced it.
    pub(super) fn win32(context: &'static str, err: windows::core::Error) -> Self {
        Self {
            context,
            code: err.code().0,
            message: err.message(),
        }
    }

    /// Captures the calling thread's last-error value. Call immediately after
    /// the failing API, before anything else can overwrite it.
    pub(super) fn last_win32(context: &'static str) -> Self {
        Self::win32(context, windows::core::Error::from_thread())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} failed: {} (0x{:08X})",
            self.context,
            self.message.trim_end(),
            self.code
        )
    }
}

impl std::error::Error for Error {}
