//! Lattice entry point: sets up the process, opens the main window and runs
//! the message loop until the window is closed.
//!
//! Everything Win32 lives in [`sys`]; this file is safe Rust only.
//!
//! Usage: `lattice [FOLDER] [--benchmark[=NODES]]`
//!
//! - `FOLDER` opens in the Solution Explorer.
//! - `--benchmark` measures Visualizer frame times once the graph has
//!   settled and writes them to the Output panel: on every object file under
//!   `FOLDER`, or with `=NODES` (or without a folder) on a synthetic graph.

// Release builds are GUI-subsystem (no console); debug builds keep a console
// so log lines written to stderr are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sys;

use std::path::PathBuf;
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
    let dpi_error = sys::dpi::enable_per_monitor_v2().err();

    let mut folder = None;
    let mut benchmark = None;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--benchmark") => benchmark = Some(None),
            Some(a) if a.starts_with("--benchmark=") => {
                benchmark = Some(a["--benchmark=".len()..].parse::<usize>().ok());
            }
            _ => folder = Some(PathBuf::from(arg)).filter(|p| p.is_dir()),
        }
    }
    let window = MainWindow::create(APP_TITLE, folder)?;

    if let Some(err) = dpi_error {
        window.log(
            true,
            &format!("Per-monitor DPI awareness unavailable: {err}"),
        );
    }
    let appearance = window.appearance();
    if !appearance.dark_title_bar {
        window.log(false, "Dark title bar unavailable on this system");
    }
    if appearance.backdrop == Backdrop::Solid {
        window.log(false, "Mica backdrop unavailable; using the default frame");
    }

    window.show();
    if let Some(synthetic) = benchmark {
        window.benchmark(synthetic);
    }
    sys::window::run_message_loop()
}
