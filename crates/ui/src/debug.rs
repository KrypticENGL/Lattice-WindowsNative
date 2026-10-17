//! The debugging session behind the Visualizer's Data mode: which program
//! to run, the source breakpoints, and the GDB session's state.
//!
//! The session itself (`crates/debugger`) runs GDB on its own threads and
//! wakes the UI thread; [`Debug::drain`] turns its events into Output lines
//! and [`Update`]s for the Visualizer and the editor.
//!
//! [`Debug::record`] runs the whole program for a replay: from `main` it
//! steps line by line as soon as each stop's snapshot is in, into the
//! user's functions and back out of library code, keeping the snapshot of
//! every line until the program exits. The exit itself is not a step, so
//! the replay ends on the last line that ran.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use debugger::{Command, DebugEvent, Session, Snapshot, StartConfig, StopReason};

use crate::panel::Cx;
use crate::sys::Waker;

/// A recording ends after this many steps.
pub const MAX_STEPS: usize = 5000;
/// ... or once its snapshots hold this many objects in all (memory).
const MAX_RECORDED_OBJECTS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    /// GDB started; the program has not stopped or run yet.
    Starting,
    Running,
    Stopped,
}

/// What the rest of the UI needs to react to.
#[derive(Debug)]
pub enum Update {
    Running,
    Stopped {
        file: Option<PathBuf>,
        line: Option<u32>,
    },
    Snapshot(Box<Snapshot>),
    /// The program or GDB ended.
    Ended,
    /// A recording took another step.
    Recording,
    /// A recording finished: one snapshot per line, in order.
    Recorded(Vec<Snapshot>),
}

/// A run being recorded for replay.
#[derive(Default)]
struct Recorder {
    steps: Vec<Snapshot>,
    objects: usize,
    /// The last command was a step out of library code: its stop lands
    /// mid-line in the caller and is not a step of its own.
    finishing: bool,
    /// The program stopped on a signal; that line is the last step.
    signalled: bool,
}

pub struct Debug {
    waker: Waker,
    session: Option<Session>,
    phase: Phase,
    program: Option<PathBuf>,
    /// Keyed by [`path_key`], so `C:\a\B.cpp` and `c:/a/b.cpp` are one file.
    breakpoints: BTreeMap<String, (PathBuf, BTreeSet<u32>)>,
    stopped_at: Option<(PathBuf, u32)>,
    stopped_in: String,
    recorder: Option<Recorder>,
    /// Updates produced outside [`Debug::drain`], returned by its next call.
    pending: Vec<Update>,
    /// The toolchain's install prefix (gdb's `bin\..`); sources under it
    /// are library code that a recording steps out of.
    toolchain: Option<PathBuf>,
}

impl Debug {
    pub fn new(waker: Waker) -> Self {
        Self {
            waker,
            session: None,
            phase: Phase::Idle,
            program: None,
            breakpoints: BTreeMap::new(),
            stopped_at: None,
            stopped_in: String::new(),
            recorder: None,
            pending: Vec::new(),
            toolchain: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn program(&self) -> Option<&Path> {
        self.program.as_deref()
    }

    pub fn set_program(&mut self, program: PathBuf) {
        self.program = Some(program);
    }

    /// Steps recorded so far, while recording.
    pub fn recording(&self) -> Option<usize> {
        self.recorder.as_ref().map(|r| r.steps.len())
    }

    /// `func` and `file:line` of the current stop.
    pub fn stop_description(&self) -> Option<String> {
        let (file, line) = self.stopped_at.as_ref()?;
        let name = file.file_name().map_or_else(
            || file.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Some(format!("{} at {name}:{line}", self.stopped_in))
    }

    /// Breakpoint lines and the stopped line (if any) for an editor file.
    pub fn marks_for(&self, path: &Path) -> (Vec<u32>, Option<u32>) {
        let key = path_key(path);
        let lines = self
            .breakpoints
            .get(&key)
            .map(|(_, l)| l.iter().copied().collect())
            .unwrap_or_default();
        let exec = self
            .stopped_at
            .as_ref()
            .filter(|(f, _)| {
                path_key(f) == key && self.phase == Phase::Stopped && self.recorder.is_none()
            })
            .map(|(_, l)| *l);
        (lines, exec)
    }

    pub fn toggle_breakpoint(&mut self, path: &Path, line: u32, cx: &mut Cx) {
        let entry = self
            .breakpoints
            .entry(path_key(path))
            .or_insert_with(|| (path.to_path_buf(), BTreeSet::new()));
        let on = entry.1.insert(line);
        if !on {
            entry.1.remove(&line);
        }
        let file = entry.0.clone();
        if let Some(s) = &self.session {
            if let Err(e) = s.set_breakpoint(&file, line, on) {
                cx.log_error(format!("Breakpoint: {e}"));
            }
        }
        cx.repaint();
    }

    /// Starts the chosen program under GDB.
    pub fn start(&mut self, cx: &mut Cx) {
        let breakpoints: Vec<(PathBuf, u32)> = self
            .breakpoints
            .values()
            .flat_map(|(p, lines)| lines.iter().map(|&l| (p.clone(), l)))
            .collect();
        self.launch(breakpoints, cx);
    }

    /// Restarts the chosen program from `main` and records every line it
    /// runs; [`Update::Recorded`] follows when it exits.
    pub fn record(&mut self, cx: &mut Cx) {
        if self.session.is_some() {
            if let Some(s) = self.session.take() {
                s.stop();
            }
            self.phase = Phase::Idle;
            self.stopped_at = None;
        }
        self.recorder = None;
        // No breakpoints: the session stops at `main`, and stepping from
        // there passes every line anyway.
        self.launch(Vec::new(), cx);
        if self.session.is_some() {
            self.recorder = Some(Recorder::default());
            cx.log("Recording the program for replay\u{2026}");
        }
    }

    fn launch(&mut self, breakpoints: Vec<(PathBuf, u32)>, cx: &mut Cx) {
        let Some(program) = self.program.clone() else {
            return;
        };
        let Some(gdb) = debugger::find_gdb() else {
            cx.log_error(
                "GDB not found. Install MinGW-w64 GDB (for example with MSYS2), add it to \
                 PATH, or set LATTICE_GDB to gdb.exe",
            );
            return;
        };
        let stop_at = if breakpoints.is_empty() {
            "stopping at main".to_string()
        } else {
            format!("{} breakpoints", breakpoints.len())
        };
        let waker = self.waker.clone();
        match Session::start(
            StartConfig {
                gdb: gdb.clone(),
                program: program.clone(),
                cwd: None,
                breakpoints,
            },
            Arc::new(move || waker.wake()),
        ) {
            Ok(s) => {
                cx.log(format!(
                    "Debugging {} with {} ({stop_at})",
                    program.display(),
                    gdb.display()
                ));
                self.session = Some(s);
                self.phase = Phase::Starting;
                self.stopped_at = None;
                self.toolchain = gdb.parent().and_then(Path::parent).map(Path::to_path_buf);
            }
            Err(e) => cx.log_error(format!("Could not start the debugger: {e}")),
        }
        cx.repaint();
    }

    pub fn command(&mut self, c: Command, cx: &mut Cx) {
        let allowed = match c {
            Command::Interrupt => self.phase == Phase::Running,
            _ => self.phase == Phase::Stopped,
        };
        if !allowed || self.recorder.is_some() {
            return;
        }
        self.send(c, cx);
    }

    fn send(&self, c: Command, cx: &mut Cx) {
        if let Some(s) = &self.session {
            if let Err(e) = s.send(c) {
                cx.log_error(format!("Debugger: {e}"));
            }
        }
    }

    /// Kills the program and GDB. A recording keeps the steps it has.
    pub fn stop(&mut self, cx: &mut Cx) {
        if self.recorder.is_some() {
            self.finish_recording("Recording stopped", cx);
            self.waker.wake();
            return;
        }
        if let Some(s) = self.session.take() {
            s.stop();
            cx.log("Debugging stopped");
        }
        self.phase = Phase::Idle;
        self.stopped_at = None;
        cx.repaint();
    }

    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// Ends the session and queues [`Update::Recorded`].
    fn finish_recording(&mut self, why: &str, cx: &mut Cx) {
        let Some(rec) = self.recorder.take() else {
            return;
        };
        if let Some(s) = self.session.take() {
            s.stop();
        }
        self.phase = Phase::Idle;
        self.stopped_at = None;
        cx.log(format!("{why}: {} steps recorded", rec.steps.len()));
        self.pending.push(Update::Recorded(rec.steps));
        cx.repaint();
    }

    /// A recording's stop: keeps the line's snapshot and takes the next
    /// step. Returns why the recording must end, if it must.
    fn record_stop(&mut self, snap: Option<Snapshot>, cx: &mut Cx) -> Option<&'static str> {
        let toolchain = self.toolchain.clone();
        let rec = self.recorder.as_mut()?;
        let library = snap.as_ref().is_some_and(|s| {
            s.frames
                .last()
                .is_none_or(|f| is_library(Path::new(&f.file), toolchain.as_deref()))
        });
        if let Some(snap) = snap.filter(|_| !library && !rec.finishing) {
            rec.objects += snap.objects.len();
            rec.steps.push(snap);
        }
        rec.finishing = library;
        if rec.signalled {
            return Some("Recording ended at the signal");
        }
        if rec.steps.len() >= MAX_STEPS {
            return Some("Recording reached its step limit");
        }
        if rec.objects >= MAX_RECORDED_OBJECTS {
            return Some("Recording reached its memory limit");
        }
        let next = if library {
            Command::Finish
        } else {
            Command::Step
        };
        self.send(next, cx);
        None
    }

    /// Handles the session's events.
    pub fn drain(&mut self, cx: &mut Cx) -> Vec<Update> {
        let mut out = std::mem::take(&mut self.pending);
        let Some(s) = &self.session else {
            return out;
        };
        let events = s.poll();
        let mut ended = false;
        for e in events {
            let recording = self.recorder.is_some();
            match e {
                DebugEvent::Running => {
                    self.phase = Phase::Running;
                    if !recording {
                        out.push(Update::Running);
                    }
                }
                DebugEvent::Stopped {
                    reason,
                    func,
                    file,
                    line,
                } => {
                    self.phase = Phase::Stopped;
                    if let StopReason::Signal(sig) = &reason {
                        cx.log_error(format!("Program received {sig} in {func}"));
                        if let Some(rec) = &mut self.recorder {
                            rec.signalled = true;
                        }
                    }
                    self.stopped_in = func;
                    self.stopped_at = file.clone().zip(line);
                    if !recording {
                        out.push(Update::Stopped { file, line });
                    }
                }
                DebugEvent::Snapshot(Ok(snap)) => {
                    // A pause reports no location; use the innermost frame.
                    if self.stopped_at.is_none() {
                        if let Some(f) = snap.frames.last() {
                            self.stopped_in = f.func.clone();
                            self.stopped_at = Some((PathBuf::from(&f.file), f.line));
                            if !recording {
                                out.push(Update::Stopped {
                                    file: Some(PathBuf::from(&f.file)),
                                    line: Some(f.line),
                                });
                            }
                        }
                    }
                    if snap.truncated && !recording {
                        cx.log(format!(
                            "Data view: showing the first {} objects; the rest are not followed",
                            snap.objects.len()
                        ));
                    }
                    if recording {
                        if let Some(why) = self.record_stop(Some(snap), cx) {
                            self.finish_recording(why, cx);
                            break;
                        }
                        out.push(Update::Recording);
                    } else {
                        out.push(Update::Snapshot(Box::new(snap)));
                    }
                }
                DebugEvent::Snapshot(Err(e)) => {
                    cx.log_error(format!("Data view: {e}"));
                    if let Some(why) = self.record_stop(None, cx) {
                        self.finish_recording(why, cx);
                        break;
                    }
                }
                DebugEvent::Exited(code) => {
                    match code {
                        Some(c) => cx.log(format!("Program exited with code {c}")),
                        None => cx.log("Program exited"),
                    }
                    ended = true;
                }
                DebugEvent::Output(text) => cx.log(text),
                DebugEvent::Error(msg) => {
                    cx.log_error(format!("GDB: {msg}"));
                    // A refused step would leave the recording waiting.
                    if recording {
                        self.finish_recording("Recording ended early", cx);
                        break;
                    }
                }
                DebugEvent::Ended => {
                    if self.phase != Phase::Idle && !recording {
                        cx.log("GDB exited");
                    }
                    ended = true;
                }
            }
        }
        if ended && self.recorder.is_some() {
            self.finish_recording("Program finished", cx);
        } else if ended {
            if let Some(s) = self.session.take() {
                s.stop();
            }
            self.phase = Phase::Idle;
            self.stopped_at = None;
            out.push(Update::Ended);
        }
        out.append(&mut self.pending);
        if !out.is_empty() {
            cx.repaint();
        }
        out
    }
}

/// Case- and separator-insensitive key for a source path (Windows paths
/// from the editor and from GDB differ in both).
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Source that a recording steps out of rather than into: missing files (no
/// source to show) and anything under the toolchain, such as libstdc++'s
/// headers.
fn is_library(file: &Path, toolchain: Option<&Path>) -> bool {
    if !file.is_file() {
        return true;
    }
    toolchain.is_some_and(|t| path_key(file).starts_with(&(path_key(t) + "\\")))
}

/// The program for a source file: `foo.exe` next to `foo.c` / `foo.cpp`.
pub fn program_for_source(source: &Path) -> Option<PathBuf> {
    let ext = source.extension()?.to_string_lossy().to_lowercase();
    if !matches!(ext.as_str(), "c" | "cc" | "cpp" | "cxx" | "c++") {
        return None;
    }
    let exe = source.with_extension(if cfg!(windows) { "exe" } else { "" });
    exe.is_file().then_some(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_keys_ignore_case_and_separators() {
        if cfg!(windows) {
            assert_eq!(
                path_key(Path::new(r"C:\Src\Shapes.cpp")),
                path_key(Path::new("c:/src/shapes.cpp"))
            );
        }
        assert_ne!(path_key(Path::new("a.cpp")), path_key(Path::new("b.cpp")));
    }

    #[test]
    fn toolchain_sources_are_library_code() {
        let dir = std::env::temp_dir().join(format!("lattice-lib-{}", std::process::id()));
        let tc = dir.join("ucrt64");
        let header = tc.join("include").join("vector");
        let user = dir.join("ucrt64-app").join("main.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(user.parent().unwrap()).unwrap();
        std::fs::write(&header, "").unwrap();
        std::fs::write(&user, "").unwrap();
        assert!(is_library(&header, Some(&tc)));
        assert!(!is_library(&user, Some(&tc)));
        assert!(!is_library(&user, None));
        assert!(is_library(&dir.join("gone.cpp"), Some(&tc)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn program_next_to_source() {
        let dir = std::env::temp_dir().join(format!("lattice-prog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("tree.cpp");
        std::fs::write(&src, "int main(){}").unwrap();
        assert_eq!(program_for_source(&src), None);
        let exe = src.with_extension(if cfg!(windows) { "exe" } else { "" });
        std::fs::write(&exe, "").unwrap();
        assert_eq!(program_for_source(&src), Some(exe));
        assert_eq!(program_for_source(&dir.join("notes.txt")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
