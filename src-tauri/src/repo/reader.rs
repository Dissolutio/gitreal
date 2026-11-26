use crate::{
    model::{
        BackendError, ChangedPath, CommitDetail, GraphPage, GraphRow, OpenRepo, RefInfo,
        RefsResponse, RepoChanged, RepoError, Result,
    },
    repo::graph::assign_lanes,
    repo::watcher::{MetadataWatcher, WatchSignal},
};
use gix::bstr::ByteSlice;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

const MAX_PAGE_SIZE: usize = 500;
const MAX_OPEN_REPOS: usize = 16;

struct Repository {
    path: PathBuf,
    state: RwLock<RepositoryState>,
    refresh_lock: Mutex<()>,
    watcher: Mutex<Option<MetadataWatcher>>,
    closed: AtomicBool,
}

struct RepositoryState {
    refs: Vec<RefInfo>,
    snapshot: Option<Arc<Snapshot>>,
    generation: u64,
}

struct Snapshot {
    rows: Vec<GraphRow>,
    ids: HashSet<String>,
}

pub struct Backend {
    repositories: Mutex<HashMap<String, Arc<Repository>>>,
    next_id: AtomicU64,
    event_sink: Arc<dyn EventSink>,
}

pub trait EventSink: Send + Sync + 'static {
    fn repo_changed(&self, event: RepoChanged);
    fn repo_error(&self, event: RepoError);
}

struct NullEventSink;

impl EventSink for NullEventSink {
    fn repo_changed(&self, _: RepoChanged) {}
    fn repo_error(&self, _: RepoError) {}
}

impl Default for Backend {
    fn default() -> Self {
        Self::with_event_sink(Arc::new(NullEventSink))
    }
}

impl Backend {
    pub fn with_event_sink(event_sink: Arc<dyn EventSink>) -> Self {
        Self {
            repositories: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            event_sink,
        }
    }

    pub fn open_repo(&self, path: String) -> Result<OpenRepo> {
        let path = Path::new(&path)
            .canonicalize()
            .map_err(|error| BackendError::new("invalid_path", error))?;
        // Open exactly the selected repository, not a parent found by discovery.
        let repo = gix::open(&path).map_err(|error| BackendError::new("not_repository", error))?;
        let refs = read_refs(&repo)?;
        let repository = Arc::new(Repository {
            path: path.clone(),
            state: RwLock::new(RepositoryState {
                refs,
                snapshot: None,
                generation: 0,
            }),
            refresh_lock: Mutex::new(()),
            watcher: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let repo_id = format!("repo-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let event_repo_id = repo_id.clone();
        let weak_repository = Arc::downgrade(&repository);
        let event_sink = Arc::clone(&self.event_sink);
        let watcher = MetadataWatcher::start(&repo, move |signal| {
            if let Some(repository) = weak_repository.upgrade() {
                let (refs_changed, watcher_error) = match signal {
                    WatchSignal::Changed(refs_changed) => (refs_changed, None),
                    WatchSignal::Error(message) => (false, Some(message)),
                };
                let mut watcher_error = watcher_error;
                match repository.refresh(refs_changed) {
                    Ok(Some(generation)) => event_sink.repo_changed(RepoChanged {
                        repo_id: event_repo_id.clone(),
                        generation,
                    }),
                    Ok(None) => {}
                    Err(error) => watcher_error = Some(error.message),
                }
                if let Some(message) = watcher_error {
                    event_sink.repo_error(RepoError {
                        repo_id: event_repo_id.clone(),
                        message,
                    });
                }
            }
        })
        .map_err(|error| BackendError::new("watcher_error", error))?;
        *repository
            .watcher
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository watcher unavailable"))? =
            Some(watcher);
        let mut repositories = self
            .repositories
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
        if repositories.len() >= MAX_OPEN_REPOS {
            return Err(BackendError::new(
                "repository_limit",
                "Close a repository before opening another",
            ));
        }
        repositories.insert(repo_id.clone(), repository);
        Ok(OpenRepo {
            repo_id,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            path: path.to_string_lossy().into(),
        })
    }

    pub fn close_repo(&self, repo_id: &str) -> Result<()> {
        let repository = self
            .repositories
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?
            .remove(repo_id);
        if let Some(repository) = repository {
            repository.closed.store(true, Ordering::Release);
            repository
                .watcher
                .lock()
                .map_err(|_| BackendError::new("internal_error", "Repository watcher unavailable"))?
                .take();
        }
        Ok(())
    }

    fn repository(&self, id: &str) -> Result<Arc<Repository>> {
        self.repositories
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?
            .get(id)
            .cloned()
            .ok_or_else(|| {
                BackendError::new(
                    "unknown_repository",
                    "Repository handle is closed or unknown",
                )
            })
    }

    pub fn get_refs(&self, repo_id: &str) -> Result<RefsResponse> {
        let repository = self.repository(repo_id)?;
        let state = repository
            .state
            .read()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
        Ok(RefsResponse {
            refs: state.refs.clone(),
            generation: state.generation,
        })
    }

    pub fn get_graph_page(&self, repo_id: &str, offset: usize, limit: usize) -> Result<GraphPage> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(BackendError::new(
                "invalid_page",
                "Page size must be between 1 and 500",
            ));
        }
        let repository = self.repository(repo_id)?;
        let (snapshot, generation) = repository.snapshot()?;
        let start = offset.min(snapshot.rows.len());
        let end = start.saturating_add(limit).min(snapshot.rows.len());
        Ok(GraphPage {
            rows: snapshot.rows[start..end].to_vec(),
            total: snapshot.rows.len(),
            generation,
        })
    }

    pub fn get_commit_detail(&self, repo_id: &str, commit_id: &str) -> Result<CommitDetail> {
        if !matches!(commit_id.len(), 40 | 64) || !commit_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(BackendError::new(
                "invalid_commit_id",
                "Expected a full hexadecimal object ID",
            ));
        }
        let repository = self.repository(repo_id)?;
        let id = commit_id.to_ascii_lowercase();
        if !repository.snapshot()?.0.ids.contains(&id) {
            return Err(BackendError::new(
                "unknown_commit",
                "Commit is not in this repository snapshot",
            ));
        }
        let repo = gix::open(&repository.path).map_err(BackendError::git)?;
        let oid = gix::ObjectId::from_hex(id.as_bytes()).map_err(BackendError::git)?;
        let commit = repo.find_commit(oid).map_err(BackendError::git)?;
        let author = commit.author().map_err(BackendError::git)?;
        let date = author.time().map_err(BackendError::git)?.seconds;
        let message = commit.message_raw().map_err(BackendError::git)?.to_string();
        let parents: Vec<_> = commit.parent_ids().map(|id| id.to_string()).collect();
        let tree = commit.tree().map_err(BackendError::git)?;
        let is_shallow_boundary = repo
            .shallow_commits()
            .map_err(BackendError::git)?
            .is_some_and(|ids| ids.contains(&oid));
        let previous = match commit.parent_ids().next().filter(|_| !is_shallow_boundary) {
            Some(id) => repo
                .find_commit(id)
                .map_err(BackendError::git)?
                .tree()
                .map_err(BackendError::git)?,
            None => repo.empty_tree(),
        };
        let mut changes = Vec::new();
        previous
            .changes()
            .map_err(BackendError::git)?
            .options(|options| {
                options.track_path().track_rewrites(None);
            })
            .for_each_to_obtain_tree(&tree, |change| {
                use gix::object::tree::diff::Change;
                let (location, mode, status, old_path) = match change {
                    Change::Addition {
                        location,
                        entry_mode,
                        ..
                    } => (location, entry_mode, "added", None),
                    Change::Deletion {
                        location,
                        entry_mode,
                        ..
                    } => (location, entry_mode, "deleted", None),
                    Change::Modification {
                        location,
                        entry_mode,
                        ..
                    } => (location, entry_mode, "modified", None),
                    Change::Rewrite {
                        location,
                        entry_mode,
                        source_location,
                        ..
                    } => (
                        location,
                        entry_mode,
                        "renamed",
                        Some(source_location.to_string()),
                    ),
                };
                if !mode.is_tree() {
                    changes.push(ChangedPath {
                        path: location.to_string(),
                        status,
                        old_path,
                    });
                }
                Ok(std::ops::ControlFlow::Continue(()))
            })
            .map_err(BackendError::git)?;
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(CommitDetail {
            commit_id: id,
            summary: message.lines().next().unwrap_or_default().to_string(),
            message,
            author: author.name.to_string(),
            author_email: author.email.to_string(),
            date,
            parents,
            changed_paths: changes,
        })
    }
}

impl Repository {
    fn snapshot(&self) -> Result<(Arc<Snapshot>, u64)> {
        loop {
            let (refs, generation, snapshot) = {
                let state = self.state.read().map_err(|_| {
                    BackendError::new("internal_error", "Repository state unavailable")
                })?;
                (state.refs.clone(), state.generation, state.snapshot.clone())
            };
            if let Some(snapshot) = snapshot {
                return Ok((snapshot, generation));
            }
            let repo = gix::open(&self.path).map_err(BackendError::git)?;
            let snapshot = Arc::new(build_snapshot(&repo, &refs)?);
            let mut state = self
                .state
                .write()
                .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
            if state.generation == generation && state.refs == refs {
                let snapshot = state.snapshot.get_or_insert(snapshot).clone();
                return Ok((snapshot, state.generation));
            }
        }
    }

    fn refresh(&self, refs_changed: bool) -> Result<Option<u64>> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        let _refresh = self
            .refresh_lock
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository refresh unavailable"))?;
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        let repo = gix::open(&self.path).map_err(BackendError::git)?;
        let refs = read_refs(&repo)?;
        let unchanged = {
            let state = self
                .state
                .read()
                .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
            state.refs == refs
        };
        if unchanged && !refs_changed {
            // Index-only changes are ignored until status information is implemented.
            return Ok(None);
        }
        let snapshot = Arc::new(build_snapshot(&repo, &refs)?);
        let mut state = self
            .state
            .write()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        state.refs = refs;
        state.snapshot = Some(snapshot);
        state.generation = state.generation.saturating_add(1);
        Ok(Some(state.generation))
    }
}

fn build_snapshot(repo: &gix::Repository, refs: &[RefInfo]) -> Result<Snapshot> {
    let roots: HashSet<_> = refs
        .iter()
        .filter_map(|reference| reference.target.as_ref())
        .collect();
    let mut roots: Vec<_> = roots
        .iter()
        .map(|id| gix::ObjectId::from_hex(id.as_bytes()).map_err(BackendError::git))
        .collect::<Result<_>>()?;
    roots.sort();
    let mut labels: HashMap<&str, Vec<String>> = HashMap::new();
    for reference in refs {
        if let Some(target) = &reference.target {
            labels
                .entry(target)
                .or_default()
                .push(reference.name.clone());
        }
    }
    let mut rows = Vec::new();
    if !roots.is_empty() {
        for entry in repo
            .rev_walk(roots)
            .sorting(gix::revision::walk::Sorting::TopoOrder)
            .all()
            .map_err(BackendError::git)?
        {
            let entry = entry.map_err(BackendError::git)?;
            let commit = repo.find_commit(entry.id).map_err(BackendError::git)?;
            let author = commit.author().map_err(BackendError::git)?;
            let commit_id = entry.id.to_string();
            rows.push(GraphRow {
                ref_labels: labels.remove(commit_id.as_str()).unwrap_or_default(),
                summary: commit
                    .message_raw()
                    .map_err(BackendError::git)?
                    .lines()
                    .next()
                    .map(|line| String::from_utf8_lossy(line).into_owned())
                    .unwrap_or_default(),
                author: author.name.to_string(),
                date: author.time().map_err(BackendError::git)?.seconds,
                parents: entry.parent_ids().map(|id| id.to_string()).collect(),
                commit_id,
                lane: 0,
                parent_edges: Vec::new(),
            });
        }
    }
    // Shallow boundaries have no visible parent; don't leave dangling graph edges.
    let ids: HashSet<_> = rows.iter().map(|row| row.commit_id.clone()).collect();
    for row in &mut rows {
        row.parents.retain(|id| ids.contains(id));
    }
    assign_lanes(&mut rows);
    Ok(Snapshot { rows, ids })
}

fn read_refs(repo: &gix::Repository) -> Result<Vec<RefInfo>> {
    let head = repo.head().map_err(BackendError::git)?;
    let head_name = head.referent_name().map(|name| name.as_bstr().to_string());
    let mut refs = Vec::new();
    for reference in repo
        .references()
        .map_err(BackendError::git)?
        .all()
        .map_err(BackendError::git)?
    {
        let mut reference = reference.map_err(BackendError::git)?;
        let full_name = reference.name().as_bstr().to_string();
        let (prefix, kind) = if full_name.starts_with("refs/heads/") {
            ("refs/heads/", "local")
        } else if full_name.starts_with("refs/remotes/") {
            ("refs/remotes/", "remote")
        } else if full_name.starts_with("refs/tags/") {
            ("refs/tags/", "tag")
        } else {
            continue;
        };
        // Tags may intentionally point to blobs/trees, which have no history.
        let object = reference
            .peel_to_id()
            .map_err(BackendError::git)?
            .object()
            .map_err(BackendError::git)?;
        let target = (object.kind == gix::object::Kind::Commit).then(|| object.id.to_string());
        refs.push(RefInfo {
            name: full_name.trim_start_matches(prefix).into(),
            is_head: head_name.as_ref() == Some(&full_name),
            full_name,
            kind,
            target,
        });
    }
    let target = if head.is_unborn() {
        None
    } else {
        Some(
            repo.head_commit()
                .map_err(BackendError::git)?
                .id
                .to_string(),
        )
    };
    refs.push(RefInfo {
        name: "HEAD".into(),
        full_name: "HEAD".into(),
        kind: "head",
        target,
        is_head: true,
    });
    refs.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    Ok(refs)
}
