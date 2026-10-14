//! Thin Win32 wrappers. This is the only module tree in `app` where `unsafe`
//! is allowed; everything it exposes is safe and free of `windows::` types.
#![allow(unsafe_code)]

pub mod dpi;
pub mod dwm;
mod error;
pub mod window;

pub use dwm::Backdrop;
pub use error::Error;
pub use window::MainWindow;
