//! One GDB process debugging one program.
//!
//! Threads: `gdb-reader` parses GDB's stdout (MI records) into
//! [`DebugEvent`]s and requests a snapshot at every stop; `gdb-stderr`
//! forwards GDB's stderr. Both send over one channel and call the wake
//! callback, so the UI thread only drains [`Session::poll`] on wake.
//! Commands are written to GDB's stdin from the UI thread.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::mi::{self, AsyncKind, Record, StreamKind, Value};
use crate::snapshot::Snapshot;

const SCRIPT: &str = include_str!("snapshot.py");
/// The object cap passed to `-lattice-snapshot`.
pub const MAX_OBJECTS: usize = 2000;

pub type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone)]
pub struct StartConfig {
    pub gdb: PathBuf,
    pub program: PathBuf,
    /// Working directory for the program; its folder if `None`.
    pub cwd: Option<PathBuf>,
    /// Source breakpoints. With none, the program stops at `main`.
    pub breakpoints: Vec<(PathBuf, u32)>,
}

/// Execution commands, valid while the program is stopped (except
/// [`Command::Interrupt`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Continue,
    /// Step over.
    Next,
    /// Step into.
    Step,
    /// Step out.
    Finish,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint,
    Step,
    /// A signal or exception, e.g. `SIGSEGV`.
    Signal(String),
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DebugEvent {
    Running,
    Stopped {
        reason: StopReason,
        func: String,
        file: Option<PathBuf>,
        line: Option<u32>,
    },
    /// Follows every `Stopped`.
    Snapshot(Result<Snapshot, String>),
    /// The program ended (exit code if it exited normally or with a code).
    Exited(Option<i64>),
    /// GDB console text, program output that reached GDB, or stderr.
    Output(String),
    /// A command failed.
    Error(String),
    /// GDB itself is gone.
    Ended,
}

pub struct Session {
    child: Option<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    events: Receiver<DebugEvent>,
    breakpoints: Arc<Mutex<HashMap<String, u32>>>,
}

impl Session {
    /// Starts GDB, loads `program`, sets the breakpoints and runs it.
    pub fn start(config: StartConfig, wake: Wake) -> Result<Self, String> {
        if !config.program.is_file() {
            return Err(format!("{} does not exist", config.program.display()));
        }
        let script = std::env::temp_dir().join(format!("lattice-gdb-{}.py", std::process::id()));
        std::fs::write(&script, SCRIPT)
            .map_err(|e| format!("cannot write {}: {e}", script.display()))?;

        let mut cmd = Process::new(&config.gdb);
        cmd.args(["-q", "--interpreter=mi3"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        no_console_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", config.gdb.display()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err("GDB pipes unavailable".into());
        };
        let stdin = Arc::new(Mutex::new(stdin));
        let breakpoints = Arc::new(Mutex::new(HashMap::new()));
        let (tx, events) = channel();

        let reader = Reader {
            tx: tx.clone(),
            wake: wake.clone(),
            stdin: stdin.clone(),
            breakpoints: breakpoints.clone(),
            trace: std::env::var_os("LATTICE_GDB_TRACE").is_some(),
        };
        let spawned = std::thread::Builder::new()
            .name("gdb-reader".into())
            .spawn(move || reader.run(stdout))
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("gdb-stderr".into())
                    .spawn(move || forward_stderr(stderr, tx, wake))
            });
        if let Err(e) = spawned {
            let _ = child.kill();
            return Err(format!("cannot start GDB threads: {e}"));
        }

        let session = Self {
            child: Some(child),
            stdin,
            events,
            breakpoints,
        };
        let cwd = config
            .cwd
            .clone()
            .or_else(|| config.program.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let mut setup = vec![
            "-gdb-set mi-async on".to_string(),
            "-gdb-set confirm off".to_string(),
            "-gdb-set print object on".to_string(),
            "-enable-pretty-printing".to_string(),
            format!(
                "-interpreter-exec console {}",
                mi::quote(&format!("source {}", slashes(&script)))
            ),
            format!(
                "-file-exec-and-symbols {}",
                mi::quote(&slashes(&config.program))
            ),
            format!("-environment-cd {}", mi::quote(&slashes(&cwd))),
        ];
        if cfg!(windows) {
            // The program gets its own console window, so its output never
            // mixes with MI records on GDB's stdout.
            setup.push("-gdb-set new-console on".to_string());
        }
        for line in setup {
            session.write(&line)?;
        }
        for (file, line) in &config.breakpoints {
            session.set_breakpoint(file, *line, true)?;
        }
        session.write(if config.breakpoints.is_empty() {
            "-exec-run --start"
        } else {
            "-exec-run"
        })?;
        Ok(session)
    }

    /// Events since the last call.
    pub fn poll(&self) -> Vec<DebugEvent> {
        self.events.try_iter().collect()
    }

    pub fn send(&self, c: Command) -> Result<(), String> {
        self.write(match c {
            Command::Continue => "-exec-continue",
            Command::Next => "-exec-next",
            Command::Step => "-exec-step",
            Command::Finish => "-exec-finish",
            Command::Interrupt => "-exec-interrupt",
        })
    }

    /// Adds or removes a source breakpoint. Works while running too.
    pub fn set_breakpoint(&self, file: &Path, line: u32, on: bool) -> Result<(), String> {
        let location = format!("{}:{line}", slashes(file));
        if on {
            self.write(&format!("-break-insert -f {}", mi::quote(&location)))
        } else {
            let number = self
                .breakpoints
                .lock()
                .map_err(|_| "breakpoint table poisoned".to_string())?
                .remove(&location);
            match number {
                Some(n) => self.write(&format!("-break-delete {n}")),
                None => Ok(()),
            }
        }
    }

    /// Kills the program and GDB. Returns at once; a helper thread makes
    /// sure GDB is gone.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.write("-gdb-exit");
        if let Some(mut child) = self.child.take() {
            let _ = std::thread::Builder::new()
                .name("gdb-exit".into())
                .spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < deadline {
                        if matches!(child.try_wait(), Ok(Some(_))) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                });
        }
    }

    fn write(&self, line: &str) -> Result<(), String> {
        write_line(&self.stdin, line)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn write_line(stdin: &Mutex<ChildStdin>, line: &str) -> Result<(), String> {
    let mut w = stdin.lock().map_err(|_| "GDB pipe poisoned".to_string())?;
    w.write_all(line.as_bytes())
        .and_then(|_| w.write_all(b"\n"))
        .and_then(|_| w.flush())
        .map_err(|e| format!("GDB is not running ({e})"))
}

/// GDB takes forward slashes everywhere; backslashes would need escaping
/// inside linespecs.
fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(windows)]
fn no_console_window(cmd: &mut Process) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_: &mut Process) {}

struct Reader {
    tx: Sender<DebugEvent>,
    wake: Wake,
    stdin: Arc<Mutex<ChildStdin>>,
    breakpoints: Arc<Mutex<HashMap<String, u32>>>,
    /// `LATTICE_GDB_TRACE` set: echo every MI line to stderr.
    trace: bool,
}

impl Reader {
    fn run(self, stdout: impl Read) {
        let mut r = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            if self.trace {
                eprintln!("gdb> {}", line.trim_end());
            }
            if let Some(e) = self.handle(mi::parse_line(&line)) {
                if self.tx.send(e).is_err() {
                    return;
                }
                (self.wake)();
            }
        }
        let _ = self.tx.send(DebugEvent::Ended);
        (self.wake)();
    }

    fn handle(&self, record: Record) -> Option<DebugEvent> {
        match record {
            Record::Async {
                kind: AsyncKind::Exec,
                class,
                results,
                ..
            } => match class.as_str() {
                "running" => Some(DebugEvent::Running),
                "stopped" => Some(self.stopped(&results)),
                _ => None,
            },
            Record::Result { class, results, .. } => match class.as_str() {
                "done" => {
                    if let Some(json) = mi::field_str(&results, "json") {
                        return Some(DebugEvent::Snapshot(Snapshot::parse(json)));
                    }
                    if let Some(bkpt) = mi::field(&results, "bkpt") {
                        self.remember_breakpoint(bkpt);
                    }
                    None
                }
                "error" => Some(DebugEvent::Error(
                    mi::field_str(&results, "msg")
                        .unwrap_or("unknown GDB error")
                        .to_string(),
                )),
                _ => None,
            },
            Record::Stream { kind, text } => {
                let text = text.trim_end();
                let noise = text.is_empty()
                    || text.starts_with("[New Thread")
                    || (text.starts_with("[Thread") && text.ends_with("exited with code 0]"))
                    || kind == StreamKind::Log && text.starts_with("source ");
                (!noise).then(|| DebugEvent::Output(text.to_string()))
            }
            Record::Other(text) => {
                let text = text.trim_end();
                (!text.is_empty()).then(|| DebugEvent::Output(text.to_string()))
            }
            Record::Async { .. } | Record::Prompt => None,
        }
    }

    fn stopped(&self, results: &[(String, Value)]) -> DebugEvent {
        let reason = mi::field_str(results, "reason").unwrap_or("");
        if reason.starts_with("exited") {
            let code =
                mi::field_str(results, "exit-code").and_then(|c| i64::from_str_radix(c, 8).ok());
            let code = code.or((reason == "exited-normally").then_some(0));
            return DebugEvent::Exited(code);
        }
        let frame = mi::field(results, "frame");
        let get = |k: &str| frame.and_then(|f| f.get(k)).and_then(Value::as_str);
        let reason = match reason {
            "breakpoint-hit" => StopReason::Breakpoint,
            "end-stepping-range" | "function-finished" | "location-reached" => StopReason::Step,
            "signal-received" => StopReason::Signal(
                mi::field_str(results, "signal-name")
                    .unwrap_or("signal")
                    .to_string(),
            ),
            other => StopReason::Other(other.to_string()),
        };
        // Ask for the data now; the reply arrives as a `json` result.
        let _ = write_line(&self.stdin, &format!("-lattice-snapshot {MAX_OBJECTS}"));
        DebugEvent::Stopped {
            reason,
            func: get("func").unwrap_or("??").to_string(),
            file: get("fullname").map(PathBuf::from),
            line: get("line").and_then(|l| l.parse().ok()),
        }
    }

    fn remember_breakpoint(&self, bkpt: &Value) {
        let (Some(number), Some(location)) = (
            bkpt.get("number")
                .and_then(Value::as_str)
                .and_then(|n| n.parse().ok()),
            bkpt.get("original-location").and_then(Value::as_str),
        ) else {
            return;
        };
        if let Ok(mut map) = self.breakpoints.lock() {
            map.insert(location.to_string(), number);
        }
    }
}

fn forward_stderr(stderr: impl Read, tx: Sender<DebugEvent>, wake: Wake) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if tx.send(DebugEvent::Output(line)).is_err() {
            return;
        }
        wake();
    }
}
