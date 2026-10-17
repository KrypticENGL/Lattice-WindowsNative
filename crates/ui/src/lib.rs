//! IDE shell: top-level Code/Visualizer tabs, and in the Code tab the
//! Solution Explorer, tabbed editor, Output panel and ConPTY terminal with
//! resizable splitters. Drawn with Direct2D/DirectWrite.
//!
//! Public APIs expose only safe project types, never `windows::` types. All
//! Win32/COM calls live in the private `sys` module.

mod bottom;
mod callstack;
mod codepane;
mod debug;
mod editor;
mod error;
pub mod event;
mod explorer;
mod folder_watch;
pub mod geom;
mod objects;
mod output;
mod panel;
mod shell;
mod split;
mod sys;
mod terminal;
pub mod theme;
mod visualizer;

pub use error::Error;
pub use shell::{Response, Shell};
pub use sys::{NativeWindow, WAKE_MESSAGE};
