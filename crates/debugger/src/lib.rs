//! Live program data for the Visualizer's Data mode, read through GDB.
//!
//! [`Session`] runs `gdb --interpreter=mi3` as a child process, drives it
//! (breakpoints, run, step) and, at every stop, asks the helper script in
//! `snapshot.py` for a [`Snapshot`]: the stopped thread's frames, their
//! variables, and every object reachable from them through pointers,
//! references and by-value members.
//!
//! Portable and safe: plain `std::process` pipes, no Win32 types and no
//! `unsafe`. GDB reads DWARF, so the program must be built with gcc or
//! clang and `-g` (MinGW on Windows); MSVC PDBs are not readable by GDB.
#![forbid(unsafe_code)]

mod locate;
pub mod mi;
mod session;
mod snapshot;

pub use locate::find_gdb;
pub use session::{Command, DebugEvent, Session, StartConfig, StopReason};
pub use snapshot::{Cell, Field, Frame, Object, ObjectKind, Snapshot, Var};
