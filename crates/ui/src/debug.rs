//! The debugging session behind the Visualizer's Data mode: which program
//! to run, the source breakpoints, and the GDB session's state.
//!
//! The session itself (`crates/debugger`) runs GDB on its own threads and
//! wakes the UI thread; [`Debug::drain`] turns its events into Output lines
//! and [`Update`]s for the Visualizer and the editor.
//!
//! Play steps the program on its own: one step per [`PLAY_STEP`] after each
//! stop's snapshot arrives, into the user's functions and back out of
//! library code, until the program ends or the user pauses.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use debugger::{Command, DebugEvent, Session, Snapshot, StartConfig, StopReason};

use crate::panel::Cx;
use crate::sys::Waker;

/// How long Play shows each line before the next step.
pub const PLAY_STEP: Duration = Duration::from_millis(600);

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
    /// Play is on.
    playing: bool,
    /// When Play takes its next step: set once the stop's snapshot is in.
    play_at: Option<Instant>,
    /// The toolchain's install prefix (gdb's `bin\..`); sources under it
    /// are library code that Play steps out of.
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
            playing: false,
            play_at: None,
            toolchain: None,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Turns Play on or off. On while stopped, the next step is due now.
    pub fn set_playing(&mut self, on: bool) {
        self.playing = on;
        self.play_at = None;
        if on && self.phase == Phase::Stopped {
            self.schedule_play(Duration::ZERO);
        }
    }

    fn schedule_play(&mut self, delay: Duration) {
        self.play_at = Some(Instant::now() + delay);
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            waker.wake();
        });
    }

    /// Takes Play's next step if it is due. Called on every wake.
    pub fn tick_play(&mut self, cx: &mut Cx) {
        let due = self.play_at.is_some_and(|t| Instant::now() >= t);
        if !(self.playing && due && self.phase == Phase::Stopped) {
            return;
        }
        self.play_at = None;
        let library = self
            .stopped_at
            .as_ref()
            .is_none_or(|(file, _)| is_library(file, self.toolchain.as_deref()));
        self.command(
            if library {
                Command::Finish
            } else {
                Command::Step
            },
            cx,
        );
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
            .filter(|(f, _)| path_key(f) == key && self.phase == Phase::Stopped)
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
        let breakpoints: Vec<(PathBuf, u32)> = self
            .breakpoints
            .values()
            .flat_map(|(p, lines)| lines.iter().map(|&l| (p.clone(), l)))
            .collect();
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
                self.play_at = None;
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
        if !allowed {
            return;
        }
        if let Some(s) = &self.session {
            if let Err(e) = s.send(c) {
                cx.log_error(format!("Debugger: {e}"));
            }
        }
    }

    /// Kills the program and GDB.
    pub fn stop(&mut self, cx: &mut Cx) {
        if let Some(s) = self.session.take() {
            s.stop();
            cx.log("Debugging stopped");
        }
        self.phase = Phase::Idle;
        self.stopped_at = None;
        self.playing = false;
        self.play_at = None;
        cx.repaint();
    }

    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// Handles the session's events.
    pub fn drain(&mut self, cx: &mut Cx) -> Vec<Update> {
        let Some(s) = &self.session else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut ended = false;
        for e in s.poll() {
            match e {
                DebugEvent::Running => {
                    self.phase = Phase::Running;
                    out.push(Update::Running);
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
                    }
                    self.stopped_in = func;
                    self.stopped_at = file.clone().zip(line);
                    out.push(Update::Stopped { file, line });
                }
                DebugEvent::Snapshot(Ok(snap)) => {
                    // A pause reports no location; use the innermost frame.
                    if self.stopped_at.is_none() {
                        if let Some(f) = snap.frames.last() {
                            self.stopped_in = f.func.clone();
                            self.stopped_at = Some((PathBuf::from(&f.file), f.line));
                            out.push(Update::Stopped {
                                file: Some(PathBuf::from(&f.file)),
                                line: Some(f.line),
                            });
                        }
                    }
                    if snap.truncated {
                        cx.log(format!(
                            "Data view: showing the first {} objects; the rest are not followed",
                            snap.objects.len()
                        ));
                    }
                    out.push(Update::Snapshot(Box::new(snap)));
                    if self.playing {
                        self.schedule_play(PLAY_STEP);
                    }
                }
                DebugEvent::Snapshot(Err(e)) => {
                    cx.log_error(format!("Data view: {e}"));
                    if self.playing {
                        self.schedule_play(PLAY_STEP);
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
                    // A refused step would leave Play waiting forever.
                    if self.playing {
                        self.playing = false;
                        self.play_at = None;
                        cx.log("Play stopped");
                    }
                }
                DebugEvent::Ended => {
                    if self.phase != Phase::Idle {
                        cx.log("GDB exited");
                    }
                    ended = true;
                }
            }
        }
        if ended {
            if let Some(s) = self.session.take() {
                s.stop();
            }
            self.phase = Phase::Idle;
            self.stopped_at = None;
            self.playing = false;
            self.play_at = None;
            out.push(Update::Ended);
        }
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

/// Source that Play steps out of rather than into: missing files (no
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
