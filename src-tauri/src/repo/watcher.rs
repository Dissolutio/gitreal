use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher,
    recommended_watcher,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct MetadataWatcher {
    _watcher: Arc<Mutex<RecommendedWatcher>>,
    stop: Sender<WatchMessage>,
    worker: Option<JoinHandle<()>>,
}

enum WatchMessage {
    Event(notify::Result<Event>),
    Stop,
}

pub enum WatchSignal {
    Changed(WatchChanges),
    Error(String),
}

#[derive(Clone, Copy, Default)]
pub struct WatchChanges {
    pub refs: bool,
    pub status: bool,
    pub force: bool,
}

impl WatchChanges {
    pub(super) const ALL: Self = Self {
        refs: true,
        status: true,
        force: false,
    };

    fn merge(&mut self, other: Self) {
        self.refs |= other.refs;
        self.status |= other.status;
        self.force |= other.force;
    }
}

impl MetadataWatcher {
    pub fn start(
        repo: &gix::Repository,
        callback: impl Fn(WatchSignal) + Send + 'static,
    ) -> notify::Result<Self> {
        let scopes = WatchScopes::new(repo);
        let (events_tx, events_rx) = mpsc::channel();
        let stop = events_tx.clone();
        let mut watcher = recommended_watcher(move |event| {
            let _ = events_tx.send(WatchMessage::Event(event));
        })?;
        scopes.register(&mut watcher)?;

        let watcher = Arc::new(Mutex::new(watcher));
        let worker_watcher = Arc::clone(&watcher);
        let worker = thread::Builder::new()
            .name("gitreal-repo-watcher".into())
            .spawn(move || {
                while let Ok(WatchMessage::Event(event)) = events_rx.recv() {
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            if let Ok(mut watcher) = worker_watcher.lock() {
                                let _ = scopes.register_existing(&mut watcher);
                            }
                            callback(WatchSignal::Error(error.to_string()));
                            continue;
                        }
                    };
                    let Some(ref_change) = scopes.relevant_event(&event) else {
                        continue;
                    };
                    let mut changes = ref_change;
                    let mut last_relevant_event = Instant::now();
                    let mut watcher_error = None;
                    loop {
                        let remaining = DEBOUNCE.saturating_sub(last_relevant_event.elapsed());
                        match events_rx.recv_timeout(remaining) {
                            Ok(WatchMessage::Event(Ok(next))) => {
                                if let Some(ref_change) = scopes.relevant_event(&next) {
                                    changes.merge(ref_change);
                                    last_relevant_event = Instant::now();
                                }
                            }
                            Ok(WatchMessage::Event(Err(error))) => {
                                watcher_error = Some(error.to_string());
                                last_relevant_event = Instant::now();
                            }
                            Ok(WatchMessage::Stop) => return,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }

                    // Re-register metadata scopes after atomic replacement or directory creation.
                    if let Ok(mut watcher) = worker_watcher.lock() {
                        let _ = scopes.register_existing(&mut watcher);
                    }
                    callback(match watcher_error {
                        Some(error) => WatchSignal::Error(error),
                        None => WatchSignal::Changed(changes),
                    });
                }
            })
            .map_err(notify::Error::io)?;

        Ok(Self {
            _watcher: watcher,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for MetadataWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(WatchMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct WatchScopes {
    git_dir: PathBuf,
    common_dir: PathBuf,
    files: HashSet<PathBuf>,
    ref_dirs: Vec<PathBuf>,
    log_dirs: Vec<PathBuf>,
    rebase_dirs: Vec<PathBuf>,
}

impl WatchScopes {
    fn new(repo: &gix::Repository) -> Self {
        let git_dir = repo.git_dir().to_path_buf();
        let common_dir = repo.common_dir().to_path_buf();
        let mut files = HashSet::new();
        for dir in [&git_dir, &common_dir] {
            for name in [
                "HEAD",
                "config",
                "config.worktree",
                "packed-refs",
                "index",
                "FETCH_HEAD",
                "ORIG_HEAD",
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "BISECT_LOG",
            ] {
                files.insert(dir.join(name));
            }
        }
        files.insert(repo.index_path());

        let mut ref_dirs = vec![git_dir.join("refs"), common_dir.join("refs")];
        ref_dirs.sort();
        ref_dirs.dedup();
        let mut log_dirs = vec![git_dir.join("logs"), common_dir.join("logs")];
        log_dirs.sort();
        log_dirs.dedup();
        let rebase_dirs = ["rebase-merge", "rebase-apply"]
            .map(|name| git_dir.join(name))
            .into_iter()
            .collect();
        Self {
            git_dir,
            common_dir,
            files,
            ref_dirs,
            log_dirs,
            rebase_dirs,
        }
    }

    fn register(&self, watcher: &mut RecommendedWatcher) -> notify::Result<()> {
        let mut directories = HashMap::new();
        directories.insert(self.git_dir.clone(), RecursiveMode::NonRecursive);
        directories.insert(self.common_dir.clone(), RecursiveMode::NonRecursive);
        for directory in &self.ref_dirs {
            if directory.is_dir() {
                directories.insert(directory.clone(), RecursiveMode::Recursive);
            }
        }
        for directory in &self.log_dirs {
            if directory.is_dir() {
                directories.insert(directory.clone(), RecursiveMode::Recursive);
            }
        }
        for directory in &self.rebase_dirs {
            if directory.is_dir() {
                directories.insert(directory.clone(), RecursiveMode::Recursive);
            }
        }
        for (directory, mode) in directories {
            watcher.watch(&directory, mode)?;
        }
        self.register_files(watcher)
    }

    fn register_files(&self, watcher: &mut RecommendedWatcher) -> notify::Result<()> {
        for file in &self.files {
            if file.is_file() {
                watcher.watch(file, RecursiveMode::NonRecursive)?;
            }
        }
        Ok(())
    }

    fn register_existing(&self, watcher: &mut RecommendedWatcher) -> notify::Result<()> {
        for directory in [&self.git_dir, &self.common_dir] {
            let _ = watcher.watch(directory, RecursiveMode::NonRecursive);
        }
        for directory in self
            .ref_dirs
            .iter()
            .chain(&self.log_dirs)
            .chain(&self.rebase_dirs)
        {
            if directory.is_dir() {
                let _ = watcher.watch(directory, RecursiveMode::Recursive);
            }
        }
        self.register_files(watcher)
    }

    fn relevant_event(&self, event: &Event) -> Option<WatchChanges> {
        if matches!(event.kind, EventKind::Access(_)) {
            return None;
        }
        event
            .paths
            .iter()
            .filter_map(|path| self.relevant_path(path))
            .reduce(|mut changes, next| {
                changes.merge(next);
                changes
            })
    }

    fn relevant_path(&self, path: &Path) -> Option<WatchChanges> {
        if ignored(path) {
            return None;
        }
        if path == self.git_dir || path == self.common_dir {
            return Some(WatchChanges::ALL);
        }
        if self.files.contains(path) {
            return Some(match path.file_name().and_then(|name| name.to_str()) {
                Some("packed-refs") => WatchChanges {
                    refs: true,
                    status: false,
                    force: true,
                },
                Some("config" | "config.worktree") => WatchChanges {
                    refs: true,
                    status: false,
                    force: false,
                },
                Some("HEAD") => WatchChanges {
                    refs: true,
                    status: true,
                    force: false,
                },
                _ => WatchChanges {
                    refs: false,
                    status: true,
                    force: false,
                },
            });
        }
        if self
            .ref_dirs
            .iter()
            .any(|directory| path.starts_with(directory))
        {
            return Some(WatchChanges {
                refs: true,
                status: false,
                force: false,
            });
        }
        if self
            .log_dirs
            .iter()
            .any(|directory| path == directory.join("refs/stash"))
        {
            return Some(true);
        }
        if self
            .rebase_dirs
            .iter()
            .any(|directory| path.starts_with(directory))
        {
            return Some(WatchChanges {
                refs: false,
                status: true,
                force: false,
            });
        }
        None
    }
}

fn ignored(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".lock")
        || name.ends_with(".tmp")
        || name.ends_with(".new")
        || name.starts_with("tmp_")
        || name.starts_with('.')
        || name.ends_with('~')
}
