//! IDE shell: three workspaces (Code, Visualizer, Debugger) whose panels
//! (Solution Explorer, tabbed editor, Output, ConPTY terminal, Visualizer,
//! Data, Debugger, Variables, Call stack, Memory) dock anywhere in any of them. Drawn with Direct2D/DirectWrite.
//!
//! Public APIs expose only safe project types, never `windows::` types. All
//! Win32/COM calls live in the private `sys` module.

mod build;
mod callstack;
mod codepane;
mod dataview;
mod debug;
mod debugview;
mod dock;
mod editor;
mod error;
pub mod event;
mod explorer;
mod fileops;
mod folder_watch;
pub mod geom;
mod hovercard;
mod infomodal;
mod memory;
mod menubar;
mod namefield;
mod objects;
mod output;
mod panel;
mod playbar;
mod replay;
mod shell;
mod syntax;
mod sys;
mod terminal;
pub mod theme;
mod titlebar;
mod variables;
mod visualizer;

pub use error::Error;
pub use shell::{Response, Shell};
pub use sys::{NativeWindow, WAKE_MESSAGE};
pub use titlebar::WindowHit;
