//! Structured load errors. Malformed input always ends up here, never in a panic.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The file could not be read.
    Io,
    /// The bytes are not a recognised object-file format.
    NotAnObject,
    /// A recognised format this engine does not handle (archive, executable, ...).
    Unsupported,
    /// A recognised object file whose structure is broken or truncated.
    Malformed,
}

#[derive(Debug, Clone)]
pub struct EngineError {
    pub kind: ErrorKind,
    pub path: PathBuf,
    pub detail: String,
}

impl EngineError {
    pub(crate) fn new(kind: ErrorKind, path: &Path, detail: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }
}

impl EngineError {
    /// The error without the path, e.g. "not an object file: unrecognised
    /// file format".
    pub fn reason(&self) -> String {
        let what = match self.kind {
            ErrorKind::Io => "cannot read",
            ErrorKind::NotAnObject => "not an object file",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Malformed => "malformed object file",
        };
        format!("{what}: {}", self.detail)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason())
    }
}

impl std::error::Error for EngineError {}
