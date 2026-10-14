//! Error type for the `ui` crate; Win32 errors are converted in `sys`.

use std::fmt;

#[derive(Debug, Clone)]
pub struct Error {
    context: String,
    code: i32,
    message: String,
}

impl Error {
    pub(crate) fn new(context: impl Into<String>, code: i32, message: impl Into<String>) -> Self {
        Self {
            context: context.into(),
            code,
            message: message.into(),
        }
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
