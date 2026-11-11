//! Building the open folder with MinGW gcc/g++ on a background thread.
//! Each C/C++ source compiles to `debug\<its path>.o`, a cache: sources older
//! than their object are not recompiled. The objects then link into
//! `target\<folder name>.exe`. The flags come from the folder's
//! `.lattice\build.conf` (see [`Options`]), which defaults to debuggable
//! `-g -O0`.

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

/// Where a folder keeps its compiler options.
pub fn options_path(root: &Path) -> PathBuf {
    root.join(".lattice").join("build.conf")
}

const DEFAULT_OPTIONS: &str = "\
# Compiler options for this folder, read at every build. Passed to gcc and g++
# as written; use \"double quotes\" around anything with spaces. Changing them
# recompiles everything.
#
# The debugger needs -g, and works best with -O0.

# C files (gcc).
cflags = -g -O0
# C++ files (g++), e.g. -std=c++20 -Wall -I\"C:\\my libs\\include\" -DDEBUG
cxxflags = -g -O0
# Linking (g++), after the objects, e.g. -static -lws2_32
ldflags = -g
";

/// Flags for gcc, g++ and the link.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    cflags: Vec<String>,
    cxxflags: Vec<String>,
    ldflags: Vec<String>,
}

impl Options {
    /// Reads `key = flags` lines. A mistake is an error, not a build with
    /// flags the user did not mean.
    fn parse(text: &str) -> Result<Self, String> {
        let mut o = Self {
            cflags: Vec::new(),
            cxxflags: Vec::new(),
            ldflags: Vec::new(),
        };
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = |what: String| format!("line {}: {what}", i + 1);
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| at("expected `name = flags`".into()))?;
            let flags = split_flags(value).map_err(at)?;
            match key.trim() {
                "cflags" => o.cflags = flags,
                "cxxflags" => o.cxxflags = flags,
                "ldflags" => o.ldflags = flags,
                k => return Err(at(format!("unknown option `{k}`"))),
            }
        }
        Ok(o)
    }

    /// The folder's options, or the defaults if it has none.
    fn load(root: &Path) -> Result<Self, String> {
        let path = options_path(root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_OPTIONS.to_string(),
            Err(e) => return Err(format!("could not read {}: {e}", path.display())),
        };
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Splits on whitespace; double quotes keep spaces, and backslashes stay
/// literal so Windows paths need no escaping.
fn split_flags(s: &str) -> Result<Vec<String>, String> {
    let mut flags = Vec::new();
    let mut current: Option<String> = None;
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.get_or_insert_with(String::new);
            }
            c if c.is_whitespace() && !quoted => flags.extend(current.take()),
            c => current.get_or_insert_with(String::new).push(c),
        }
    }
    if quoted {
        return Err("unclosed quote".into());
    }
    flags.extend(current);
    Ok(flags)
}

/// Makes sure the folder has an options file to edit, never replacing one.
pub fn ensure_options_file(root: &Path) -> std::io::Result<PathBuf> {
    let path = options_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(DEFAULT_OPTIONS.as_bytes())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Ok(path)
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
    let options = Options::load(root)?;
    let conf = options_path(root);
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
    let has_debug_info = |flags: &[String]| flags.iter().any(|f| f.starts_with("-g"));
    if !has_debug_info(&options.cflags) && gcc.is_some()
        || !has_debug_info(&options.cxxflags) && sources.iter().any(|s| !is_c(s))
    {
        log(
            false,
            "Note: compiling without -g, so the debugger cannot show lines or variables".into(),
        );
    }

    let mut objects = Vec::with_capacity(sources.len());
    let mut failed = 0;
    for source in &sources {
        let object = object_for(root, source);
        // shortcut: deleting the options file does not recompile; touch a
        // source or delete `debug` if the defaults must take over.
        if !up_to_date(source, &object) || (conf.is_file() && !up_to_date(&conf, &object)) {
            let rel = source.strip_prefix(root).unwrap_or(source);
            log(false, format!("Compiling {}", rel.display()));
            if let Some(dir) = object.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            }
            let (cc, flags) = match &gcc {
                Some(gcc) if is_c(source) => (gcc, &options.cflags),
                _ => (&gxx, &options.cxxflags),
            };
            let mut cmd = Command::new(cc);
            cmd.args(flags).arg("-c").arg(source).arg("-o").arg(&object);
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
    // After the objects, where `-l` libraries must come.
    cmd.args(&objects)
        .arg("-o")
        .arg(&exe)
        .args(&options.ldflags);
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
pub(crate) fn no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn no_console_window(_: &mut Command) {}

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

    #[test]
    fn flags_split_like_a_command_line() {
        assert_eq!(split_flags("  -g   -O0 ").unwrap(), ["-g", "-O0"]);
        assert_eq!(
            split_flags(r#"-I"C:\my libs\inc" "-DA=b c" -DX="""#).unwrap(),
            [r"-IC:\my libs\inc", "-DA=b c", "-DX="]
        );
        assert!(split_flags("").unwrap().is_empty());
        assert_eq!(split_flags(r#""""#).unwrap(), [""]);
        assert!(split_flags(r#"-I"oops"#).is_err());
    }

    #[test]
    fn options_parse_and_reject_mistakes() {
        let o = Options::parse(DEFAULT_OPTIONS).unwrap();
        assert_eq!(o.cflags, ["-g", "-O0"]);
        assert_eq!(o.cxxflags, ["-g", "-O0"]);
        assert_eq!(o.ldflags, ["-g"]);

        // A value may hold `=`; a name not given stays empty.
        let o = Options::parse("# c\n\n cxxflags = -std=c++20 -DA=1\n").unwrap();
        assert_eq!(o.cxxflags, ["-std=c++20", "-DA=1"]);
        assert!(o.cflags.is_empty());

        assert_eq!(
            Options::parse("\nnope").unwrap_err(),
            "line 2: expected `name = flags`"
        );
        assert_eq!(
            Options::parse("cxflags = -g").unwrap_err(),
            "line 1: unknown option `cxflags`"
        );
        assert_eq!(
            Options::parse("cflags = \"x").unwrap_err(),
            "line 1: unclosed quote"
        );
    }

    #[test]
    fn the_options_file_is_written_once_and_never_replaced() {
        let root = std::env::temp_dir().join(format!("lattice-opts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(Options::load(&root).unwrap().ldflags, ["-g"], "defaults");

        let path = ensure_options_file(&root).unwrap();
        assert_eq!(path, options_path(&root));
        std::fs::write(&path, "ldflags = -static\n").unwrap();
        ensure_options_file(&root).unwrap();
        assert_eq!(Options::load(&root).unwrap().ldflags, ["-static"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Flags reach the compiler, and changing them recompiles.
    #[test]
    fn options_change_the_build() {
        if debugger::find_tool("g++").is_none() {
            eprintln!("skipped: no MinGW g++");
            return;
        }
        let root = std::env::temp_dir().join(format!("lattice-flags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".lattice")).unwrap();
        std::fs::write(root.join("main.cpp"), "int main() { return ANSWER; }\n").unwrap();
        let write = |n: u32| {
            std::fs::write(options_path(&root), format!("cxxflags = -g -DANSWER={n}\n")).unwrap();
        };
        let lines = std::cell::RefCell::new(Vec::new());
        let log = |_: bool, t: String| lines.borrow_mut().push(t);
        let run = |exe: &Path| Command::new(exe).status().unwrap().code();

        write(3);
        let exe = build(&root, &log).unwrap_or_else(|e| panic!("{e}: {:?}", lines.borrow()));
        assert_eq!(run(&exe), Some(3));
        write(4);
        let exe = build(&root, &log).unwrap_or_else(|e| panic!("{e}: {:?}", lines.borrow()));
        assert_eq!(
            run(&exe),
            Some(4),
            "new flags must recompile: {:?}",
            lines.borrow()
        );

        std::fs::write(options_path(&root), "bad line\n").unwrap();
        assert!(build(&root, &log).unwrap_err().contains("line 1"));
        let _ = std::fs::remove_dir_all(&root);
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
