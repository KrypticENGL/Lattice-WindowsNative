//! IDE shell: two workspaces (Code, Visualizer) whose panels (Solution
//! Explorer, tabbed editor, Output, ConPTY terminal, Visualizer, Source,
//! Call stack) dock anywhere in either. Drawn with Direct2D/DirectWrite.
//!
//! Public APIs expose only safe project types, never `windows::` types. All
//! Win32/COM calls live in the private `sys` module.

mod callstack;
mod codepane;
mod debug;
mod dock;
mod editor;
mod error;
pub mod event;
mod explorer;
mod folder_watch;
pub mod geom;
mod infomodal;
mod menubar;
mod objects;
mod output;
mod panel;
mod playbar;
mod replay;
mod shell;
mod sys;
mod terminal;
pub mod theme;
mod titlebar;
mod visualizer;

pub use error::Error;
pub use shell::{Response, Shell};
pub use sys::{NativeWindow, WAKE_MESSAGE};
pub use titlebar::WindowHit;
