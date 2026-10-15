//! Loads object files with the engine off the UI thread, and reloads them
//! when they change on disk.
//!
//! Threads: `object-loader` runs `engine::load` for each request (and walks
//! folders for "load every object under the folder"); the
//! `notify` watcher thread forwards file changes to it. Results return over
//! an mpsc channel followed by `Waker::wake`, and the UI thread drains them
//! in `Shell::on_wake`. Neither thread touches window APIs.

use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use engine::CodeGraph;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::sys::Waker;

/// Quiet period after the last change before reloading, so a compiler
/// writing the file in several steps causes one reload.
const DEBOUNCE: Duration = Duration::from_millis(300);
const IDLE_WAIT: Duration = Duration::from_secs(3600);

/// Whether a file name looks like a relocatable object file.
pub fn is_object_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".obj") || lower.ends_with(".o")
}

pub struct LoadResult {
    pub path: PathBuf,
    /// `true` when triggered by a change on disk.
    pub reload: bool,
    /// Part of a folder load: summarised by [`ObjectEvent::FolderDone`]
    /// instead of logged one by one.
    pub quiet: bool,
    pub elapsed: Duration,
    pub graph: Result<Arc<CodeGraph>, LoadError>,
}

pub enum ObjectEvent {
    Loaded(LoadResult),
    /// A folder load finished.
    FolderDone {
        root: PathBuf,
        loaded: usize,
        failed: usize,
        elapsed: Duration,
    },
}

pub struct LoadError {
    /// Short form for the Explorer row.
    pub reason: String,
    /// With the path, for the Output panel.
    pub message: String,
}

enum Request {
    Load(PathBuf),
    LoadFolder(PathBuf),
    Changed(PathBuf),
}

pub struct ObjectService {
    requests: Sender<Request>,
    results: Receiver<ObjectEvent>,
    watcher: Option<RecommendedWatcher>,
    watched_dirs: HashSet<PathBuf>,
}

impl ObjectService {
    /// Starts the loader thread and the file watcher. A watcher that cannot
    /// start only disables automatic reload; `start_error` explains why.
    pub fn new(waker: Waker) -> (Self, Option<String>) {
        let (requests, request_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let mut error = None;
        if let Err(e) = thread::Builder::new()
            .name("object-loader".into())
            .spawn(move || worker(&request_rx, &result_tx, &waker))
        {
            error = Some(format!("object loader thread failed to start: {e}"));
        }
        let changes = requests.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else {
                return;
            };
            if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                for path in event.paths {
                    let _ = changes.send(Request::Changed(path));
                }
            }
        });
        let watcher = match watcher {
            Ok(w) => Some(w),
            Err(e) => {
                error.get_or_insert(format!("file watcher unavailable, no auto-reload: {e}"));
                None
            }
        };
        (
            Self {
                requests,
                results,
                watcher,
                watched_dirs: HashSet::new(),
            },
            error,
        )
    }

    /// Queues a load and starts watching the file's folder. Returns a
    /// message if watching failed.
    pub fn load(&mut self, path: &Path) -> Option<String> {
        let _ = self.requests.send(Request::Load(path.to_path_buf()));
        let dir = path.parent()?.to_path_buf();
        let watcher = self.watcher.as_mut()?;
        if self.watched_dirs.contains(&dir) {
            return None;
        }
        match watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                self.watched_dirs.insert(dir);
                None
            }
            Err(e) => Some(format!("cannot watch {}: {e}", dir.display())),
        }
    }

    /// Queues every object file under `root` (recursively, skipping hidden
    /// folders) and watches the tree. Returns a message if watching failed.
    pub fn load_folder(&mut self, root: &Path) -> Option<String> {
        let _ = self.requests.send(Request::LoadFolder(root.to_path_buf()));
        let watcher = self.watcher.as_mut()?;
        if self.watched_dirs.contains(root) {
            return None;
        }
        match watcher.watch(root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.watched_dirs.insert(root.to_path_buf());
                None
            }
            Err(e) => Some(format!("cannot watch {}: {e}", root.display())),
        }
    }

    pub fn try_recv(&self) -> Option<ObjectEvent> {
        self.results.try_recv().ok()
    }
}

/// Object files under `root`, sorted; hidden folders (`.git`, ...) skipped.
fn find_objects(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') {
                    dirs.push(entry.path());
                }
            } else if kind.is_file() && is_object_file(&name) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

fn worker(requests: &Receiver<Request>, results: &Sender<ObjectEvent>, waker: &Waker) {
    let mut loaded: HashSet<PathBuf> = HashSet::new();
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
    loop {
        let wait = pending
            .values()
            .map(|t| (*t + DEBOUNCE).saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(IDLE_WAIT);
        match requests.recv_timeout(wait) {
            Ok(Request::Load(path)) => {
                pending.remove(&path);
                loaded.insert(path.clone());
                run(&path, false, false, results, waker);
            }
            Ok(Request::LoadFolder(root)) => {
                let start = Instant::now();
                let (mut ok, mut failed) = (0, 0);
                for path in find_objects(&root) {
                    pending.remove(&path);
                    loaded.insert(path.clone());
                    if run(&path, false, true, results, waker) {
                        ok += 1;
                    } else {
                        failed += 1;
                    }
                }
                let done = ObjectEvent::FolderDone {
                    root,
                    loaded: ok,
                    failed,
                    elapsed: start.elapsed(),
                };
                if results.send(done).is_ok() {
                    waker.wake();
                }
            }
            Ok(Request::Changed(path)) => {
                if loaded.contains(&path) {
                    pending.insert(path, Instant::now());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        let due: Vec<PathBuf> = pending
            .iter()
            .filter(|(_, t)| now >= **t + DEBOUNCE)
            .map(|(p, _)| p.clone())
            .collect();
        for path in due {
            pending.remove(&path);
            run(&path, true, false, results, waker);
        }
    }
}

/// Loads one object and sends the result. Returns whether it loaded.
fn run(
    path: &Path,
    reload: bool,
    quiet: bool,
    results: &Sender<ObjectEvent>,
    waker: &Waker,
) -> bool {
    let start = Instant::now();
    // The engine never panics on bad input by design; this is a backstop so
    // a bug cannot leave the row stuck at "loading".
    let graph = match panic::catch_unwind(AssertUnwindSafe(|| engine::load(path))) {
        Ok(Ok(graph)) => Ok(Arc::new(graph)),
        Ok(Err(e)) => Err(LoadError {
            reason: e.reason(),
            message: e.to_string(),
        }),
        Err(_) => Err(LoadError {
            reason: "internal error while loading".into(),
            message: format!("{}: internal error while loading", path.display()),
        }),
    };
    let ok = graph.is_ok();
    let result = LoadResult {
        path: path.to_path_buf(),
        reload,
        quiet,
        elapsed: start.elapsed(),
        graph,
    };
    if results.send(ObjectEvent::Loaded(result)).is_ok() {
        waker.wake();
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_extensions() {
        assert!(is_object_file("a.obj"));
        assert!(is_object_file("B.OBJ"));
        assert!(is_object_file("lib.o"));
        assert!(!is_object_file("a.rs"));
        assert!(!is_object_file("objdump"));
        assert!(!is_object_file("x.out"));
    }

    #[test]
    fn finds_objects_recursively_skipping_hidden_dirs() {
        let root = std::env::temp_dir().join(format!("lattice-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["a/b", ".git", "c"] {
            std::fs::create_dir_all(root.join(dir)).expect("mkdir");
        }
        for file in ["a/b/x.obj", "a/y.o", ".git/z.o", "c/readme.md", "top.OBJ"] {
            std::fs::write(root.join(file), b"").expect("write");
        }
        let found: Vec<String> = find_objects(&root)
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found, ["a/b/x.obj", "a/y.o", "top.OBJ"]);
    }
}
