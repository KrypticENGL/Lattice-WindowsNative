//! Lattice entry point: sets up the process, opens the main window and runs
//! the message loop until the window is closed.
//!
//! Everything Win32 lives in [`sys`]; this file is safe Rust only.

// Release builds are GUI-subsystem (no console); debug builds keep a console
// so log lines written to stderr are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sys;

use std::process::ExitCode;

use sys::{Backdrop, MainWindow};

const APP_TITLE: &str = "Lattice";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("lattice: fatal: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<i32, sys::Error> {
    // Must happen before any window exists. Failure is not fatal: the window
    // still works, it is just bitmap-scaled by the system.
    if let Err(err) = sys::dpi::enable_per_monitor_v2() {
        eprintln!("lattice: per-monitor DPI awareness unavailable: {err}");
    }

    let window = MainWindow::create(APP_TITLE)?;
    let appearance = window.appearance();
    if !appearance.dark_title_bar {
        eprintln!("lattice: dark title bar unavailable on this system");
    }
    if appearance.backdrop == Backdrop::Solid {
        eprintln!("lattice: Mica backdrop unavailable, using solid background");
    }

    window.show();
    sys::window::run_message_loop()
}
