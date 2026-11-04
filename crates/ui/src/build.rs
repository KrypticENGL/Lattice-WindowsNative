//! Building the open folder with MinGW gcc/g++ on a background thread.
//! Each C/C++ source compiles (with `-g`, for the debugger) to
//! `debug\<its path>.o`, a cache: sources older than their object are not
//! recompiled. The objects then link into `target\<folder name>.exe`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use crate::objects::find_files;
use crate::sys::Waker;

pub enum BuildEvent {
    Log {
        error: bool,
        text: String,
    },
    /// The build ended: the executable, or `None` if it failed.
    Done(Option<PathBuf>),
}

pub struct Builder {
    waker: Waker,
    /// Set while a build runs.
    rx: Option<Receiver<BuildEvent>>,
}

impl Builder {
    pub fn new(waker: Waker) -> Self {
        Self { waker, rx: None }
    }

    pub fn is_building(&self) -> bool {
        self.rx.is_some()
    }

    /// Builds `root` unless a build is already running.
    pub fn start(&mut self, root: PathBuf) {
        if self.is_building() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let log = |error, text: String| {
                let _ = tx.send(BuildEvent::Log { error, text });
                waker.wake();
            };
            let exe = match build(&root, &log) {
                Ok(exe) => {
                    log(
                        false,
                        format!(
                            "Build succeeded in {:.1} s: {}",
                            started.elapsed().as_secs_f32(),
                            exe.display()
                        ),
                    );
                    Some(exe)
                }
                Err(e) => {
                    log(true, format!("Build failed: {e}"));
                    None
                }
            };
            let _ = tx.send(BuildEvent::Done(exe));
            waker.wake();
        });
    }

    pub fn try_recv(&mut self) -> Option<BuildEvent> {
        let event = self.rx.as_ref()?.try_recv();
        match event {
            Ok(BuildEvent::Done(exe)) => {
                self.rx = None;
                Some(BuildEvent::Done(exe))
            }
            Ok(e) => Some(e),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                Some(BuildEvent::Done(None))
            }
        }
    }
}

fn is_source(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".c", ".cc", ".cpp", ".cxx"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn is_c(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("c"))
}

/// `debug\src\main.cpp.o` for `src\main.cpp`; keeping the extension stops
/// `a.c` and `a.cpp` sharing an object.
fn object_for(root: &Path, source: &Path) -> PathBuf {
    let rel = source.strip_prefix(root).unwrap_or(source);
    let mut o = root.join("debug").join(rel).into_os_string();
    o.push(".o");
    o.into()
}

fn exe_for(root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .map_or_else(|| "program".into(), |n| n.to_string_lossy().into_owned());
    root.join("target")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// shortcut: compares the source's time only, so a changed header alone does
/// not rebuild its users; read gcc's `-MMD` dependency files if that bites.
fn up_to_date(source: &Path, object: &Path) -> bool {
    let time = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    matches!((time(source), time(object)), (Some(s), Some(o)) if o >= s)
}

fn build(root: &Path, log: &dyn Fn(bool, String)) -> Result<PathBuf, String> {
    let sources = find_files(root, is_source);
    if sources.is_empty() {
        return Err(format!("no C or C++ sources under {}", root.display()));
    }
    let tool = |name| {
        debugger::find_tool(name)
            .ok_or_else(|| format!("{name} not found: install MinGW-w64 or put it on PATH"))
    };
    let gxx = tool("g++")?;
    let gcc = if sources.iter().any(|s| is_c(s)) {
        Some(tool("gcc")?)
    } else {
        None
    };
    log(false, format!("Building {}…", root.display()));

    let mut objects = Vec::with_capacity(sources.len());
    let mut failed = 0;
    for source in &sources {
        let object = object_for(root, source);
        if !up_to_date(source, &object) {
            let rel = source.strip_prefix(root).unwrap_or(source);
            log(false, format!("Compiling {}", rel.display()));
            if let Some(dir) = object.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            }
            let cc = match &gcc {
                Some(gcc) if is_c(source) => gcc,
                _ => &gxx,
            };
            let mut cmd = Command::new(cc);
            cmd.args(["-g", "-O0", "-c"])
                .arg(source)
                .arg("-o")
                .arg(&object);
            if !run(cmd, log)? {
                // Drop a half-written object so the next build retries it.
                let _ = std::fs::remove_file(&object);
                failed += 1;
            }
        }
        objects.push(object);
    }
    if failed > 0 {
        return Err(format!(
            "{failed} of {} sources did not compile",
            sources.len()
        ));
    }

    let exe = exe_for(root);
    if let Some(dir) = exe.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    log(false, format!("Linking {}", exe.display()));
    // g++ links C objects too, and brings the C++ runtime when needed.
    let mut cmd = Command::new(&gxx);
    cmd.arg("-g").args(&objects).arg("-o").arg(&exe);
    if !run(cmd, log)? {
        return Err("link failed".into());
    }
    Ok(exe)
}

/// Runs a compiler, logging its messages. `Ok(false)` if it failed.
fn run(mut cmd: Command, log: &dyn Fn(bool, String)) -> Result<bool, String> {
    no_console_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    let ok = out.status.success();
    for text in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(text);
        if !text.trim().is_empty() {
            log(!ok, text.trim_end().to_string());
        }
    }
    Ok(ok)
}

#[cfg(windows)]
fn no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let root = Path::new(r"C:\proj\demo");
        assert_eq!(
            object_for(root, &root.join(r"src\main.cpp")),
            PathBuf::from(r"C:\proj\demo\debug\src\main.cpp.o")
        );
        assert_eq!(
            exe_for(root),
            PathBuf::from(r"C:\proj\demo\target\demo.exe")
        );
        assert!(is_source("A.CPP") && is_source("x.c") && !is_source("x.h"));
        assert!(!is_source("main.cpp.o"));
    }

    /// Builds a mixed C/C++ folder twice; the second build reuses the cache.
    #[test]
    fn builds_and_caches() {
        if debugger::find_tool("g++").is_none() || debugger::find_tool("gcc").is_none() {
            eprintln!("skipped: no MinGW gcc/g++");
            return;
        }
        let root = std::env::temp_dir().join(format!("lattice-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/add.c"),
            "int add(int a, int b) { return a + b; }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.cpp"),
            "extern \"C\" int add(int, int);\nint main() { return add(2, 3) == 5 ? 0 : 1; }\n",
        )
        .unwrap();
        let lines = std::cell::RefCell::new(Vec::new());
        let log = |_: bool, t: String| lines.borrow_mut().push(t);

        let exe = build(&root, &log).unwrap_or_else(|e| panic!("{e}: {:?}", lines.borrow()));
        assert_eq!(exe, exe_for(&root));
        assert!(root.join(r"debug\src\add.c.o").is_file());
        assert!(root.join(r"debug\main.cpp.o").is_file());
        assert!(Command::new(&exe).status().unwrap().success());

        lines.borrow_mut().clear();
        build(&root, &log).unwrap();
        assert!(!lines.borrow().iter().any(|l| l.starts_with("Compiling")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
