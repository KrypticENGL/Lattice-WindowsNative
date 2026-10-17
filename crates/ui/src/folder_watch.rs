//! Watches the open folder so the Explorer follows files being added,
//! removed or renamed on disk.
//!
//! Threads: the `notify` watcher thread forwards changed paths to
//! `folder-watch`, which waits for a quiet period and then sends the set of
//! affected folders, followed by `Waker::wake`. The UI thread drains them in
//! `Shell::on_wake`. Neither thread touches window APIs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::sys::Waker;

/// Quiet period after the last change before refreshing, so a build that
/// writes many files causes one refresh.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// Longest a refresh waits while changes keep coming.
const MAX_DELAY: Duration = Duration::from_secs(1);
const IDLE_WAIT: Duration = Duration::from_secs(3600);

/// Folders whose listing may have changed.
pub enum Changes {
    Dirs(HashSet<PathBuf>),
    /// Events were lost; every folder needs a fresh look.
    All,
}

enum Notice {
    Paths(Vec<PathBuf>),
    Rescan,
}

pub struct FolderWatcher {
    watcher: Option<RecommendedWatcher>,
    root: Option<PathBuf>,
    results: Receiver<Changes>,
}

impl FolderWatcher {
    /// Starts the watcher. One that cannot start only disables automatic
    /// refresh; `start_error` explains why.
    pub fn new(waker: Waker) -> (Self, Option<String>) {
        let (notice_tx, notice_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let mut error = None;
        if let Err(e) = thread::Builder::new()
            .name("folder-watch".into())
            .spawn(move || worker(&notice_rx, &result_tx, &waker))
        {
            error = Some(format!("folder watch thread failed to start: {e}"));
        }
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let notice = match res {
                Ok(event) if event.need_rescan() => Notice::Rescan,
                // Contents changing does not change a listing.
                Ok(event)
                    if matches!(
                        event.kind,
                        EventKind::Access(_)
                            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
                    ) =>
                {
                    return
                }
                Ok(event) => Notice::Paths(event.paths),
                Err(_) => Notice::Rescan,
            };
            let _ = notice_tx.send(notice);
        });
        let watcher = match watcher {
            Ok(w) => Some(w),
            Err(e) => {
                error.get_or_insert(format!(
                    "file watcher unavailable, Explorer will not refresh: {e}"
                ));
                None
            }
        };
        (
            Self {
                watcher,
                root: None,
                results,
            },
            error,
        )
    }

    /// Watches `root` (recursively) instead of the previous folder. Returns
    /// a message if watching failed.
    pub fn set_root(&mut self, root: &Path) -> Option<String> {
        let watcher = self.watcher.as_mut()?;
        if let Some(old) = self.root.take() {
            let _ = watcher.unwatch(&old);
        }
        match watcher.watch(root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.root = Some(root.to_path_buf());
                None
            }
            Err(e) => Some(format!("cannot watch {}: {e}", root.display())),
        }
    }

    pub fn try_recv(&self) -> Option<Changes> {
        self.results.try_recv().ok()
    }
}

fn worker(notices: &Receiver<Notice>, results: &Sender<Changes>, waker: &Waker) {
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    let mut rescan = false;
    // (first change, last change) of the pending batch.
    let mut pending: Option<(Instant, Instant)> = None;
    loop {
        let wait = pending.map_or(IDLE_WAIT, |(first, last)| {
            (last + DEBOUNCE)
                .min(first + MAX_DELAY)
                .saturating_duration_since(Instant::now())
        });
        match notices.recv_timeout(wait) {
            Ok(notice) => {
                match notice {
                    Notice::Paths(paths) => {
                        for path in paths {
                            // The folder holding the path lists it; the path
                            // itself may be a folder whose listing changed.
                            if let Some(parent) = path.parent() {
                                dirs.insert(parent.to_path_buf());
                            }
                            dirs.insert(path);
                        }
                    }
                    Notice::Rescan => rescan = true,
                }
                let now = Instant::now();
                pending = Some(pending.map_or((now, now), |(first, _)| (first, now)));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some((first, last)) = pending else {
            continue;
        };
        let now = Instant::now();
        if now < last + DEBOUNCE && now < first + MAX_DELAY {
            continue;
        }
        pending = None;
        let changes = if std::mem::take(&mut rescan) {
            dirs.clear();
            Changes::All
        } else {
            Changes::Dirs(std::mem::take(&mut dirs))
        };
        if results.send(changes).is_ok() {
            waker.wake();
        }
    }
}
