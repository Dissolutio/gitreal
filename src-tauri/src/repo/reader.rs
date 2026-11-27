use crate::{
    model::{
        BackendError, ChangedPath, CommitDetail, GraphPage, GraphRow, JobFinished, JobId,
        JobStarted, OpenRepo, OperationInfo, RefInfo, RefsResponse, RepoChanged, RepoError, Result,
    },
    repo::branch_ops,
    repo::graph::{LaneLayout, assign_lanes, page_edges},
    repo::jobs::{JobQueue, JobSpec},
    repo::remote_ops,
    repo::watcher::{MetadataWatcher, WatchChanges, WatchSignal},
};
use gix::bstr::ByteSlice;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

const MAX_PAGE_SIZE: usize = 500;
const MAX_OPEN_REPOS: usize = 16;
const MAX_UPSTREAM_WALK: usize = 10_000;
type UpstreamCache = HashMap<(gix::ObjectId, gix::ObjectId), (usize, usize, bool)>;
type RefFingerprintEntry = (PathBuf, u64, Option<std::time::SystemTime>, bool, bool);
#[derive(Clone, Debug, Eq, PartialEq)]
struct RefFingerprint(Vec<RefFingerprintEntry>);

struct Repository {
    path: PathBuf,
    state: RwLock<RepositoryState>,
    refresh_lock: Mutex<()>,
    watcher: Mutex<Option<MetadataWatcher>>,
    closed: AtomicBool,
}

struct RepositoryState {
    refs: Vec<RefInfo>,
    upstream_cache: UpstreamCache,
    ref_fingerprint: RefFingerprint,
    operation: Option<OperationInfo>,
    conflicted_files: usize,
    snapshot: Option<Arc<Snapshot>>,
    generation: u64,
}

struct Snapshot {
    rows: Vec<SnapshotRow>,
    parents: Vec<Vec<u32>>,
    ids: HashSet<gix::ObjectId>,
    layout: LaneLayout,
}

struct SnapshotRow {
    id: gix::ObjectId,
    ref_labels: Vec<String>,
    summary: String,
    author: String,
    date: i64,
}

pub struct Backend {
    repositories: Mutex<HashMap<String, Arc<Repository>>>,
    job_queues: Mutex<HashMap<String, Arc<JobQueue>>>,
    next_id: AtomicU64,
    event_sink: Arc<dyn EventSink>,
}

pub trait EventSink: Send + Sync + 'static {
    fn repo_changed(&self, event: RepoChanged);
    fn repo_error(&self, event: RepoError);
    fn job_started(&self, event: JobStarted);
    fn job_finished(&self, event: JobFinished);
}

struct NullEventSink;

impl EventSink for NullEventSink {
    fn repo_changed(&self, _: RepoChanged) {}
    fn repo_error(&self, _: RepoError) {}
    fn job_started(&self, _: JobStarted) {}
    fn job_finished(&self, _: JobFinished) {}
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
            job_queues: Mutex::new(HashMap::new()),
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
        let mut upstream_cache = UpstreamCache::new();
        let refs = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut upstream_cache)?;
        let ref_fingerprint = read_ref_fingerprint(&repo)?;
        let (operation, conflicted_files) = read_operation(&repo, &refs)?;
        let repository = Arc::new(Repository {
            path: path.clone(),
            state: RwLock::new(RepositoryState {
                refs,
                upstream_cache,
                ref_fingerprint,
                operation,
                conflicted_files,
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
                let (changes, watcher_error) = match signal {
                    WatchSignal::Changed(changes) => (changes, None),
                    WatchSignal::Error(message) => (WatchChanges::ALL, Some(message)),
                };
                let mut watcher_error = watcher_error;
                match repository.refresh(changes) {
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
        let mut repositories = self
            .repositories
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
        let mut queues = self
            .job_queues
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Git job queue unavailable"))?;
        if let Some(queue) = queues.remove(repo_id) {
            queue.cancel_queued();
        }
        let repository = repositories.remove(repo_id);
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

    pub fn fetch(&self, repo_id: &str, remote: Option<&str>) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = remote_ops::fetch(repository.path.clone(), remote)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn pull(&self, repo_id: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = remote_ops::pull(repository.path.clone())?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn push(&self, repo_id: &str, branch: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let refs = repository
            .state
            .read()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?
            .refs
            .clone();
        let spec = remote_ops::push(repository.path.clone(), branch, &refs)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn force_push(&self, repo_id: &str, branch: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let refs = repository
            .state
            .read()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?
            .refs
            .clone();
        let spec = remote_ops::force_push(repository.path.clone(), branch, &refs)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn push_remote(&self, repo_id: &str, branch: &str) -> Result<String> {
        let repository = self.repository(repo_id)?;
        let refs = repository
            .state
            .read()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?
            .refs
            .clone();
        remote_ops::push_remote(repository.path.clone(), branch, &refs)
    }

    pub fn checkout_local(&self, repo_id: &str, branch: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::checkout_local(repository.path.clone(), branch)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn checkout_remote(&self, repo_id: &str, remote_branch: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::checkout_remote(repository.path.clone(), remote_branch)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn reset_local_to_remote(
        &self,
        repo_id: &str,
        branch: &str,
        remote_branch: &str,
        expected_local_oid: &str,
    ) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::reset_local_to_remote(
            repository.path.clone(),
            branch,
            remote_branch,
            expected_local_oid,
        )?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn checkout_commit(&self, repo_id: &str, oid: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::checkout_commit(repository.path.clone(), oid)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn create_branch(
        &self,
        repo_id: &str,
        name: &str,
        at_oid: &str,
        checkout: bool,
    ) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::create_branch(repository.path.clone(), name, at_oid, checkout)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn rename_branch(&self, repo_id: &str, old: &str, new: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::rename_branch(repository.path.clone(), old, new)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn delete_local(&self, repo_id: &str, branch: &str, force: bool) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::delete_local(repository.path.clone(), branch, force)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn delete_remote(&self, repo_id: &str, remote: &str, branch: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::delete_remote(repository.path.clone(), remote, branch)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn delete_local_and_remote(
        &self,
        repo_id: &str,
        branch: &str,
        remote: &str,
        remote_branch: &str,
    ) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::delete_local_and_remote(
            repository.path.clone(),
            branch,
            remote,
            remote_branch,
        )?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn stash_push(
        &self,
        repo_id: &str,
        include_untracked: bool,
        message: Option<&str>,
    ) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::stash_push(repository.path.clone(), include_untracked, message)?;
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn stash_pop(&self, repo_id: &str) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let spec = branch_ops::stash_pop(repository.path.clone());
        self.enqueue_job(repo_id, repository, spec)
    }

    pub fn stash_and_checkout(
        &self,
        repo_id: &str,
        target_kind: &str,
        target: &str,
    ) -> Result<JobId> {
        let repository = self.repository(repo_id)?;
        let checkout = match target_kind {
            "local" => branch_ops::checkout_local(repository.path.clone(), target)?,
            "remote" => branch_ops::checkout_remote(repository.path.clone(), target)?,
            "commit" => branch_ops::checkout_commit(repository.path.clone(), target)?,
            _ => {
                return Err(BackendError::new(
                    "invalid_checkout_target",
                    "Unknown checkout target",
                ));
            }
        };
        let spec = branch_ops::stash_and_checkout(repository.path.clone(), checkout);
        self.enqueue_job(repo_id, repository, spec)
    }

    fn enqueue_job(
        &self,
        repo_id: &str,
        repository: Arc<Repository>,
        spec: JobSpec,
    ) -> Result<JobId> {
        let queue =
            {
                let repositories = self.repositories.lock().map_err(|_| {
                    BackendError::new("internal_error", "Repository state unavailable")
                })?;
                if !repositories
                    .get(repo_id)
                    .is_some_and(|open| Arc::ptr_eq(open, &repository))
                {
                    return Err(BackendError::new(
                        "unknown_repository",
                        "Repository handle is closed or unknown",
                    ));
                }
                let mut queues = self.job_queues.lock().map_err(|_| {
                    BackendError::new("internal_error", "Git job queue unavailable")
                })?;
                Arc::clone(queues.entry(repo_id.to_owned()).or_insert_with(|| {
                    Arc::new(JobQueue::new(
                        repo_id.to_owned(),
                        Arc::clone(&self.event_sink),
                    ))
                }))
            };
        let weak_repository = Arc::downgrade(&repository);
        let event_sink = Arc::clone(&self.event_sink);
        let event_repo_id = repo_id.to_owned();
        queue
            .enqueue(spec, move || {
                if let Some(repository) = weak_repository.upgrade() {
                    match repository.refresh(WatchChanges::ALL) {
                        Ok(Some(generation)) => event_sink.repo_changed(RepoChanged {
                            repo_id: event_repo_id.clone(),
                            generation,
                        }),
                        Ok(None) => {}
                        Err(error) => event_sink.repo_error(RepoError {
                            repo_id: event_repo_id.clone(),
                            message: error.message,
                        }),
                    }
                }
            })
            .map_err(|error| BackendError::new("internal_error", error))
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
            operation: state.operation.clone(),
            conflicted_files: state.conflicted_files,
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
        Ok(snapshot.graph_page(start, end, generation))
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
        let oid = gix::ObjectId::from_hex(commit_id.as_bytes()).map_err(BackendError::git)?;
        if !repository.snapshot()?.0.ids.contains(&oid) {
            return Err(BackendError::new(
                "unknown_commit",
                "Commit is not in this repository snapshot",
            ));
        }
        let repo = gix::open(&repository.path).map_err(BackendError::git)?;
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
            commit_id: oid.to_string(),
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

    fn refresh(&self, changes: WatchChanges) -> Result<Option<u64>> {
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
        let (mut refs, mut upstream_cache, mut ref_fingerprint) = {
            let state = self
                .state
                .read()
                .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
            (
                state.refs.clone(),
                state.upstream_cache.clone(),
                state.ref_fingerprint.clone(),
            )
        };
        let should_read_refs =
            changes.refs && (changes.force || read_ref_fingerprint(&repo)? != ref_fingerprint);
        if should_read_refs {
            refs = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut upstream_cache)?;
            ref_fingerprint = read_ref_fingerprint(&repo)?;
        }
        let (operation, conflicted_files) = if changes.refs || changes.status {
            read_operation(&repo, &refs)?
        } else {
            let state = self
                .state
                .read()
                .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
            (state.operation.clone(), state.conflicted_files)
        };
        let (refs_unchanged, graph_unchanged, status_unchanged) = {
            let state = self
                .state
                .read()
                .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
            (
                state.refs == refs,
                state.refs.len() == refs.len()
                    && state.refs.iter().zip(&refs).all(|(old, new)| {
                        old.full_name == new.full_name
                            && old.kind == new.kind
                            && old.target == new.target
                    }),
                state.operation == operation && state.conflicted_files == conflicted_files,
            )
        };
        if refs_unchanged && status_unchanged && !changes.force {
            // Unchanged refs and status do not need a new generation or event.
            if should_read_refs {
                let mut state = self.state.write().map_err(|_| {
                    BackendError::new("internal_error", "Repository state unavailable")
                })?;
                state.upstream_cache = upstream_cache;
                state.ref_fingerprint = ref_fingerprint;
            }
            return Ok(None);
        }
        let snapshot = (!graph_unchanged)
            .then(|| build_snapshot(&repo, &refs))
            .transpose()?;
        let mut state = self
            .state
            .write()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?;
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        state.refs = refs;
        state.upstream_cache = upstream_cache;
        state.ref_fingerprint = ref_fingerprint;
        state.operation = operation;
        state.conflicted_files = conflicted_files;
        if let Some(snapshot) = snapshot {
            state.snapshot = Some(Arc::new(snapshot));
        }
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
    let mut raw_rows = Vec::new();
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
            let id = entry.id;
            raw_rows.push((
                entry.parent_ids().map(|id| id.detach()).collect::<Vec<_>>(),
                SnapshotRow {
                    id,
                    ref_labels: labels.remove(id.to_string().as_str()).unwrap_or_default(),
                    summary: commit
                        .message_raw()
                        .map_err(BackendError::git)?
                        .lines()
                        .next()
                        .map(|line| String::from_utf8_lossy(line).into_owned())
                        .unwrap_or_default(),
                    author: author.name.to_string(),
                    date: author.time().map_err(BackendError::git)?.seconds,
                },
            ));
        }
    }
    let positions: HashMap<_, _> = raw_rows
        .iter()
        .enumerate()
        .map(|(index, (_, row))| (row.id, index as u32))
        .collect();
    let ids: HashSet<_> = positions.keys().copied().collect();
    let parents: Vec<Vec<u32>> = raw_rows
        .iter()
        .map(|(parent_ids, _)| {
            parent_ids
                .iter()
                .filter_map(|id| positions.get(id).copied())
                .collect()
        })
        .collect();
    let rows: Vec<_> = raw_rows.into_iter().map(|(_, row)| row).collect();
    let layout = assign_lanes(&parents);
    Ok(Snapshot {
        rows,
        parents,
        ids,
        layout,
    })
}

impl Snapshot {
    fn graph_page(&self, start: usize, end: usize, generation: u64) -> GraphPage {
        let parent_edges = page_edges(&self.parents, &self.layout, start, end, |index| {
            Arc::from(self.rows[index as usize].id.to_string())
        });
        let rows = self.rows[start..end]
            .iter()
            .enumerate()
            .zip(parent_edges)
            .map(|((offset, row), parent_edges)| {
                let index = start + offset;
                GraphRow {
                    commit_id: row.id.to_string(),
                    lane: self.layout.lanes[index] as usize,
                    parent_edges,
                    ref_labels: row.ref_labels.clone(),
                    summary: row.summary.clone(),
                    author: row.author.clone(),
                    date: row.date,
                    parents: self.parents[index]
                        .iter()
                        .map(|parent| self.rows[*parent as usize].id.to_string())
                        .collect(),
                }
            })
            .collect();
        GraphPage {
            rows,
            total: self.rows.len(),
            max_lane: self.layout.max_lane,
            generation,
        }
    }
}

#[cfg(test)]
impl Snapshot {
    fn estimated_heap_bytes(&self) -> usize {
        use std::mem::size_of;
        self.rows.capacity() * size_of::<SnapshotRow>()
            + self
                .rows
                .iter()
                .map(|row| {
                    row.ref_labels.capacity() * size_of::<String>()
                        + row.ref_labels.iter().map(String::capacity).sum::<usize>()
                        + row.summary.capacity()
                        + row.author.capacity()
                })
                .sum::<usize>()
            + self.parents.capacity() * size_of::<Vec<u32>>()
            + self
                .parents
                .iter()
                .map(|parents| parents.capacity() * size_of::<u32>())
                .sum::<usize>()
            + self.ids.capacity() * (size_of::<gix::ObjectId>() + 1)
            + self.layout.lanes.capacity() * size_of::<u32>()
            + self.layout.checkpoints.capacity() * size_of::<crate::repo::graph::LaneCheckpoint>()
            + self
                .layout
                .checkpoints
                .iter()
                .map(|checkpoint| checkpoint.lanes.capacity() * size_of::<Option<u32>>())
                .sum::<usize>()
    }
}

fn read_ref_fingerprint(repo: &gix::Repository) -> Result<RefFingerprint> {
    let mut paths: Vec<RefFingerprintEntry> = Vec::new();
    let mut ref_directories = vec![repo.git_dir().join("refs"), repo.common_dir().join("refs")];
    ref_directories.sort();
    ref_directories.dedup();
    for directory in ref_directories {
        collect_ref_metadata(&directory, &mut paths)?;
    }
    for path in [
        repo.git_dir().join("HEAD"),
        repo.git_dir().join("config"),
        repo.git_dir().join("config.worktree"),
        repo.git_dir().join("packed-refs"),
        repo.common_dir().join("config"),
        repo.common_dir().join("config.worktree"),
        repo.common_dir().join("packed-refs"),
    ] {
        collect_ref_metadata(&path, &mut paths)?;
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    paths.dedup_by(|a, b| a.0 == b.0);
    Ok(RefFingerprint(paths))
}

fn collect_ref_metadata(path: &Path, entries: &mut Vec<RefFingerprintEntry>) -> Result<()> {
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(BackendError::new("git_error", error)),
        };
        let is_dir = metadata.is_dir();
        let is_symlink = metadata.file_type().is_symlink();
        entries.push((
            path.clone(),
            metadata.len(),
            metadata.modified().ok(),
            is_dir,
            is_symlink,
        ));
        if is_dir {
            for entry in fs::read_dir(path).map_err(BackendError::git)? {
                let entry = entry.map_err(BackendError::git)?;
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn read_refs_with_limit(repo: &gix::Repository, walk_limit: usize) -> Result<Vec<RefInfo>> {
    read_refs_with_cache(repo, walk_limit, &mut UpstreamCache::new())
}

fn read_refs_with_cache(
    repo: &gix::Repository,
    walk_limit: usize,
    upstream_cache: &mut UpstreamCache,
) -> Result<Vec<RefInfo>> {
    #[cfg(test)]
    let ref_iteration_started = std::time::Instant::now();
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
        let peeled = match reference.peel_to_id() {
            Ok(id) => id,
            Err(_) if reference.try_id().is_none() => continue,
            Err(error) => return Err(BackendError::git(error)),
        };
        let object = peeled.object().map_err(BackendError::git)?;
        let target = (object.kind == gix::object::Kind::Commit).then(|| object.id.to_string());
        refs.push(RefInfo {
            name: full_name.trim_start_matches(prefix).into(),
            is_head: head_name.as_ref() == Some(&full_name),
            full_name,
            kind,
            target,
            upstream: None,
            ahead: None,
            behind: None,
            upstream_gone: false,
            truncated: false,
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
        upstream: None,
        ahead: None,
        behind: None,
        upstream_gone: false,
        truncated: false,
    });
    let config = repo.config_snapshot();
    refs.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    #[cfg(test)]
    let ref_iteration_elapsed = ref_iteration_started.elapsed();
    #[cfg(test)]
    let config_started = std::time::Instant::now();
    #[cfg(test)]
    let mut walk_elapsed = std::time::Duration::ZERO;
    let mut used_cache_keys = HashSet::new();
    for index in 0..refs.len() {
        if refs[index].kind != "local" {
            continue;
        }
        let branch = refs[index].name.as_str();
        let (Some(remote), Some(merge)) = (
            config.string_by("branch", branch, "remote"),
            config.string_by("branch", branch, "merge"),
        ) else {
            continue;
        };
        let remote = remote.to_string();
        let merge = merge.to_string();
        let (default_upstream_ref, upstream_name) =
            if let Some(name) = merge.strip_prefix("refs/heads/") {
                if remote == "." {
                    (merge.clone(), name.to_owned())
                } else {
                    (
                        format!("refs/remotes/{remote}/{name}"),
                        format!("{remote}/{name}"),
                    )
                }
            } else {
                (merge.clone(), merge.clone())
            };
        refs[index].upstream = Some(upstream_name);
        let upstream_ref = if remote == "." {
            Some(default_upstream_ref)
        } else {
            let local_ref = repo
                .find_reference(refs[index].full_name.as_str())
                .map_err(BackendError::git)?;
            repo.branch_remote_tracking_ref_name(local_ref.name(), gix::remote::Direction::Fetch)
                .transpose()
                .map_err(BackendError::git)?
                .map(|name| name.as_bstr().to_string())
        };
        let Some(upstream_ref) = upstream_ref else {
            continue;
        };
        let upstream_index = match refs
            .binary_search_by(|reference| reference.full_name.as_str().cmp(upstream_ref.as_str()))
        {
            Ok(index) => index,
            Err(_) => {
                refs[index].upstream_gone = true;
                continue;
            }
        };
        let Some(local_target) = refs[index].target.as_deref() else {
            continue;
        };
        let Some(upstream_target) = refs[upstream_index].target.as_deref() else {
            continue;
        };
        let local = gix::ObjectId::from_hex(local_target.as_bytes()).map_err(BackendError::git)?;
        let upstream =
            gix::ObjectId::from_hex(upstream_target.as_bytes()).map_err(BackendError::git)?;
        let key = (local, upstream);
        used_cache_keys.insert(key);
        let counts = match upstream_cache.get(&key) {
            Some(counts) => *counts,
            None => {
                #[cfg(test)]
                let walk_started = std::time::Instant::now();
                let counts = ahead_behind(repo, local, upstream, walk_limit)?;
                #[cfg(test)]
                {
                    walk_elapsed += walk_started.elapsed();
                }
                upstream_cache.insert(key, counts);
                counts
            }
        };
        let (ahead, behind, truncated) = counts;
        refs[index].ahead = Some(ahead);
        refs[index].behind = Some(behind);
        refs[index].truncated = truncated;
    }
    upstream_cache.retain(|key, _| used_cache_keys.contains(key));
    refs.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    #[cfg(test)]
    tests::record_read_profile(
        ref_iteration_elapsed,
        config_started.elapsed().saturating_sub(walk_elapsed),
        walk_elapsed,
    );
    Ok(refs)
}

fn ahead_behind(
    repo: &gix::Repository,
    local: gix::ObjectId,
    upstream: gix::ObjectId,
    limit: usize,
) -> Result<(usize, usize, bool)> {
    if local == upstream {
        return Ok((0, 0, false));
    }
    let (ahead, ahead_truncated) = count_exclusive(repo, local, upstream, limit)?;
    let (behind, behind_truncated) = count_exclusive(repo, upstream, local, limit)?;
    let truncated = ahead_truncated || behind_truncated;
    Ok((ahead, behind, truncated))
}

fn count_exclusive(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    hidden: gix::ObjectId,
    limit: usize,
) -> Result<(usize, bool)> {
    #[cfg(test)]
    tests::record_upstream_walk();
    let mut count = 0;
    for entry in repo
        .rev_walk([tip])
        .with_hidden([hidden])
        .sorting(gix::revision::walk::Sorting::BreadthFirst)
        .all()
        .map_err(BackendError::git)?
        .take(limit.saturating_add(1))
    {
        entry.map_err(BackendError::git)?;
        count += 1;
    }
    let truncated = count > limit;
    Ok((count.min(limit), truncated))
}

fn read_operation(
    repo: &gix::Repository,
    refs: &[RefInfo],
) -> Result<(Option<OperationInfo>, usize)> {
    let git_dir = repo.git_dir();
    let rebase_merge = git_dir.join("rebase-merge");
    let rebase_apply = git_dir.join("rebase-apply");
    let operation = if rebase_merge.is_dir() {
        Some(rebase_operation(&rebase_merge, refs, false))
    } else if rebase_apply.is_dir() {
        Some(rebase_operation(&rebase_apply, refs, true))
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        Some(simple_operation("cherry-pick"))
    } else if git_dir.join("REVERT_HEAD").is_file() {
        Some(simple_operation("revert"))
    } else if git_dir.join("MERGE_HEAD").is_file() {
        Some(simple_operation("merge"))
    } else if git_dir.join("BISECT_LOG").is_file() {
        Some(simple_operation("bisect"))
    } else {
        None
    };
    let conflicted_files = if operation.is_some() {
        let index = repo.index_or_empty().map_err(BackendError::git)?;
        index
            .entries()
            .iter()
            .filter(|entry| entry.stage_raw() != 0)
            .map(|entry| entry.path(&index).to_vec())
            .collect::<HashSet<_>>()
            .len()
    } else {
        0
    };
    Ok((operation, conflicted_files))
}

fn simple_operation(kind: &'static str) -> OperationInfo {
    OperationInfo {
        kind,
        head_name: None,
        onto: None,
        step: None,
        total_steps: None,
    }
}

fn rebase_operation(path: &Path, refs: &[RefInfo], apply: bool) -> OperationInfo {
    let read = |name: &str| {
        fs::read_to_string(path.join(name))
            .ok()
            .map(|value| value.trim().to_owned())
    };
    let head_name =
        read("head-name").map(|name| name.strip_prefix("refs/heads/").unwrap_or(&name).to_owned());
    let onto = read("onto").map(|id| {
        refs.iter()
            .filter(|reference| reference.kind == "local")
            .chain(refs.iter().filter(|reference| reference.kind == "remote"))
            .find(|reference| reference.target.as_deref() == Some(id.as_str()))
            .map(|reference| reference.name.clone())
            .unwrap_or(id)
    });
    OperationInfo {
        kind: "rebase",
        head_name,
        onto,
        step: read(if apply { "next" } else { "msgnum" }).and_then(|value| value.parse().ok()),
        total_steps: read(if apply { "last" } else { "end" }).and_then(|value| value.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        hint::black_box,
        mem::size_of,
        process::Command,
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct ReadProfile {
        ref_iteration: Duration,
        config_resolution: Duration,
        walks: Duration,
    }

    thread_local! {
        static UPSTREAM_WALK_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static READ_PROFILE: RefCell<ReadProfile> = RefCell::default();
    }

    pub(super) fn record_upstream_walk() {
        UPSTREAM_WALK_COUNT.with(|count| count.set(count.get() + 1));
    }

    fn reset_upstream_walk_count() {
        UPSTREAM_WALK_COUNT.with(|count| count.set(0));
    }

    fn upstream_walk_count() -> usize {
        UPSTREAM_WALK_COUNT.with(std::cell::Cell::get)
    }

    fn reset_read_profile() {
        READ_PROFILE.with(|profile| *profile.borrow_mut() = ReadProfile::default());
    }

    fn read_profile() -> (Duration, Duration, Duration) {
        READ_PROFILE.with(|profile| {
            let profile = profile.borrow();
            (
                profile.ref_iteration,
                profile.config_resolution,
                profile.walks,
            )
        })
    }

    pub(super) fn record_read_profile(
        ref_iteration: Duration,
        config_resolution: Duration,
        walks: Duration,
    ) {
        READ_PROFILE.with(|profile| {
            let mut profile = profile.borrow_mut();
            profile.ref_iteration += ref_iteration;
            profile.config_resolution += config_resolution;
            profile.walks += walks;
        });
    }

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit_tree(path: &Path, tree: &str, parent: &str, message: &str) -> String {
        git(path, &["commit-tree", tree, "-p", parent, "-m", message])
    }

    #[derive(Clone)]
    struct LegacyEdge {
        from_lane: usize,
        to_lane: usize,
        parent_id: String,
    }

    #[derive(Clone)]
    struct LegacyRow {
        commit_id: String,
        lane: usize,
        parent_edges: Vec<LegacyEdge>,
        ref_labels: Vec<String>,
        summary: String,
        author: String,
        date: i64,
        parents: Vec<String>,
    }

    fn synthetic_snapshot() -> Snapshot {
        let branch_count = 200;
        let branch_rows = branch_count * 2;
        let row_count = 20_000;
        let mut rows = Vec::with_capacity(row_count);
        let mut parents = Vec::with_capacity(row_count);
        for index in 0..row_count {
            let id = gix::ObjectId::from_hex(format!("{:040x}", index + 1).as_bytes()).unwrap();
            let row_parents = if index < branch_count {
                vec![(branch_count + index) as u32]
            } else if index < branch_rows {
                vec![(branch_rows + (index - branch_count) * 95) as u32]
            } else if index + 1 < row_count {
                vec![(index + 1) as u32]
            } else {
                Vec::new()
            };
            parents.push(row_parents);
            rows.push(SnapshotRow {
                id,
                ref_labels: if index < branch_count {
                    vec![format!("feature-{index}")]
                } else {
                    Vec::new()
                },
                summary: "synthetic commit".into(),
                author: "Snapshot Test".into(),
                date: index as i64,
            });
        }
        let ids = rows.iter().map(|row| row.id).collect();
        let layout = assign_lanes(&parents);
        Snapshot {
            rows,
            parents,
            ids,
            layout,
        }
    }

    fn legacy_snapshot(snapshot: &Snapshot) -> Vec<LegacyRow> {
        let mut rows: Vec<_> = snapshot
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| LegacyRow {
                commit_id: row.id.to_string(),
                lane: 0,
                parent_edges: Vec::new(),
                ref_labels: row.ref_labels.clone(),
                summary: row.summary.clone(),
                author: row.author.clone(),
                date: row.date,
                parents: snapshot.parents[index]
                    .iter()
                    .map(|parent| snapshot.rows[*parent as usize].id.to_string())
                    .collect(),
            })
            .collect();
        let mut lanes: Vec<Option<String>> = Vec::new();
        for row in &mut rows {
            let lane = lanes
                .iter()
                .position(|id| id.as_ref() == Some(&row.commit_id))
                .unwrap_or_else(|| {
                    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
                        lanes.push(None);
                        lanes.len() - 1
                    })
                });
            row.lane = lane;
            lanes[lane] = None;
            row.parent_edges = lanes
                .iter()
                .enumerate()
                .filter_map(|(index, id)| {
                    id.as_ref().map(|id| LegacyEdge {
                        from_lane: index,
                        to_lane: index,
                        parent_id: id.clone(),
                    })
                })
                .collect();
            for parent in &row.parents {
                let target = lanes
                    .iter()
                    .position(|id| id.as_ref() == Some(parent))
                    .unwrap_or_else(|| {
                        let target = lanes.iter().position(Option::is_none).unwrap_or_else(|| {
                            lanes.push(None);
                            lanes.len() - 1
                        });
                        lanes[target] = Some(parent.clone());
                        target
                    });
                row.parent_edges.push(LegacyEdge {
                    from_lane: lane,
                    to_lane: target,
                    parent_id: parent.clone(),
                });
            }
            while lanes.last() == Some(&None) {
                lanes.pop();
            }
        }
        rows
    }

    fn legacy_heap_bytes(rows: &[LegacyRow], row_capacity: usize, ids: &HashSet<String>) -> usize {
        row_capacity * size_of::<LegacyRow>()
            + rows
                .iter()
                .map(|row| {
                    let _ = row.date;
                    row.commit_id.capacity()
                        + row.ref_labels.capacity() * size_of::<String>()
                        + row.ref_labels.iter().map(String::capacity).sum::<usize>()
                        + row.summary.capacity()
                        + row.author.capacity()
                        + row.parents.capacity() * size_of::<String>()
                        + row.parents.iter().map(String::capacity).sum::<usize>()
                        + row.parent_edges.capacity() * size_of::<LegacyEdge>()
                        + row
                            .parent_edges
                            .iter()
                            .map(|edge| {
                                let _ = (edge.from_lane, edge.to_lane);
                                edge.parent_id.capacity()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
            + ids.capacity() * (size_of::<String>() + 1)
            + ids.iter().map(String::capacity).sum::<usize>()
    }

    #[test]
    fn twenty_thousand_commit_snapshot_stays_compact_with_two_hundred_branches() {
        let snapshot = synthetic_snapshot();
        let old_rows = legacy_snapshot(&snapshot);
        let old_ids: HashSet<_> = old_rows.iter().map(|row| row.commit_id.clone()).collect();
        let old_bytes = legacy_heap_bytes(&old_rows, old_rows.capacity(), &old_ids);
        let new_bytes = snapshot.estimated_heap_bytes();
        const SNAPSHOT_HEAP_LIMIT: usize = 8 * 1024 * 1024;
        assert!(new_bytes < old_bytes);
        assert!(
            new_bytes < SNAPSHOT_HEAP_LIMIT,
            "snapshot used {new_bytes} bytes (8 MiB leaves about 2x headroom over the measured compact snapshot)"
        );
        eprintln!(
            "Synthetic 20k/200 snapshot estimated heap: before {old_bytes} bytes, after {new_bytes} bytes (8 MiB cap; about 2x the compact size and below legacy)"
        );

        let repository = Arc::new(Repository {
            path: PathBuf::new(),
            state: RwLock::new(RepositoryState {
                refs: Vec::new(),
                upstream_cache: UpstreamCache::new(),
                ref_fingerprint: RefFingerprint(Vec::new()),
                operation: None,
                conflicted_files: 0,
                snapshot: Some(Arc::new(snapshot)),
                generation: 0,
            }),
            refresh_lock: Mutex::new(()),
            watcher: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let backend = Backend::default();
        backend
            .repositories
            .lock()
            .unwrap()
            .insert("synthetic".into(), repository);
        let iterations = 50;

        let started = Instant::now();
        for _ in 0..iterations {
            black_box(old_rows[..200].to_vec());
        }
        let before = started.elapsed() / iterations;
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(backend.get_graph_page("synthetic", 0, 200).unwrap());
        }
        let after = started.elapsed() / iterations;
        eprintln!(
            "Synthetic 20k/200 get_graph_page(0, 200) average: before {before:?}, after {after:?}"
        );
    }

    #[test]
    fn upstream_counts_truncate_at_the_test_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(path)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Test Author"]);
        git(&["config", "user.email", "author@example.test"]);
        fs::write(path.join("tracked.txt"), "0").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "root"]);
        let upstream = git(&["rev-parse", "HEAD"]);
        git(&["update-ref", "refs/remotes/origin/main", &upstream]);
        git(&[
            "config",
            "remote.origin.url",
            "https://example.invalid/repository",
        ]);
        git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]);
        git(&["config", "branch.main.remote", "origin"]);
        git(&["config", "branch.main.merge", "refs/heads/main"]);
        for index in 1..=4 {
            fs::write(path.join("tracked.txt"), index.to_string()).unwrap();
            git(&["add", "."]);
            git(&["commit", "-m", &format!("commit {index}")]);
        }

        let repo = gix::open(path).unwrap();
        let main = read_refs_with_limit(&repo, 2).unwrap();
        let main = main
            .iter()
            .find(|reference| reference.name == "main")
            .unwrap();
        assert_eq!(main.ahead, Some(2));
        assert_eq!(main.behind, Some(0));
        assert!(main.truncated);
    }

    #[test]
    fn upstream_cache_tracks_moved_and_deleted_refs_and_skips_unchanged_walks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.name", "Test Author"]);
        git(path, &["config", "user.email", "author@example.test"]);
        fs::write(path.join("tracked.txt"), "root").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "root"]);
        let root = git(path, &["rev-parse", "HEAD"]);
        let tree = git(path, &["rev-parse", "HEAD^{tree}"]);
        git(
            path,
            &[
                "config",
                "remote.origin.url",
                "https://example.invalid/repo",
            ],
        );
        git(
            path,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        );
        git(path, &["config", "branch.main.remote", "origin"]);
        git(path, &["config", "branch.main.merge", "refs/heads/main"]);
        let local_one = commit_tree(path, &tree, &root, "local one");
        let upstream_one = commit_tree(path, &tree, &root, "upstream one");
        git(path, &["update-ref", "refs/heads/main", &local_one]);
        git(
            path,
            &["update-ref", "refs/remotes/origin/main", &upstream_one],
        );

        let mut cache = UpstreamCache::new();
        let repo = gix::open(path).unwrap();
        let initial = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        let main = initial
            .iter()
            .find(|reference| reference.name == "main")
            .unwrap();
        assert_eq!((main.ahead, main.behind), (Some(1), Some(1)));
        assert_eq!(upstream_walk_count(), 2);

        reset_upstream_walk_count();
        let repo = gix::open(path).unwrap();
        read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        assert_eq!(upstream_walk_count(), 0);

        let local_two = commit_tree(path, &tree, &local_one, "local two");
        git(path, &["update-ref", "refs/heads/main", &local_two]);
        reset_upstream_walk_count();
        let repo = gix::open(path).unwrap();
        let moved_local = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        let main = moved_local
            .iter()
            .find(|reference| reference.name == "main")
            .unwrap();
        assert_eq!((main.ahead, main.behind), (Some(2), Some(1)));
        assert_eq!(upstream_walk_count(), 2);

        let upstream_two = commit_tree(path, &tree, &upstream_one, "upstream two");
        git(
            path,
            &["update-ref", "refs/remotes/origin/main", &upstream_two],
        );
        reset_upstream_walk_count();
        let repo = gix::open(path).unwrap();
        let moved_upstream = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        let main = moved_upstream
            .iter()
            .find(|reference| reference.name == "main")
            .unwrap();
        assert_eq!((main.ahead, main.behind), (Some(2), Some(2)));
        assert_eq!(upstream_walk_count(), 2);

        git(path, &["branch", "-m", "renamed"]);
        reset_upstream_walk_count();
        let repo = gix::open(path).unwrap();
        let renamed_refs = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        let renamed_ref = renamed_refs
            .iter()
            .find(|reference| reference.name == "renamed")
            .unwrap();
        assert_eq!((renamed_ref.ahead, renamed_ref.behind), (Some(2), Some(2)));
        assert_eq!(upstream_walk_count(), 0);

        let operation = read_operation(&repo, &renamed_refs).unwrap();
        let ref_fingerprint = read_ref_fingerprint(&repo).unwrap();
        let repository = Repository {
            path: path.to_path_buf(),
            state: RwLock::new(RepositoryState {
                refs: renamed_refs,
                upstream_cache: cache.clone(),
                ref_fingerprint,
                operation: operation.0,
                conflicted_files: operation.1,
                snapshot: None,
                generation: 0,
            }),
            refresh_lock: Mutex::new(()),
            watcher: Mutex::new(None),
            closed: AtomicBool::new(false),
        };
        reset_upstream_walk_count();
        assert_eq!(
            repository
                .refresh(WatchChanges {
                    refs: true,
                    status: true,
                    force: false,
                })
                .unwrap(),
            None
        );
        assert_eq!(upstream_walk_count(), 0);

        git(path, &["update-ref", "-d", "refs/remotes/origin/main"]);
        reset_upstream_walk_count();
        let repo = gix::open(path).unwrap();
        let deleted = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        let renamed_ref = deleted
            .iter()
            .find(|reference| reference.name == "renamed")
            .unwrap();
        assert!(renamed_ref.upstream_gone);
        assert_eq!(renamed_ref.ahead, None);
        assert_eq!(renamed_ref.behind, None);
        assert!(cache.is_empty());
        assert_eq!(upstream_walk_count(), 0);
    }

    #[test]
    fn upstream_cache_synthetic_cold_warm_and_moved_branch_timings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        git(path, &["init", "-b", "seed"]);
        git(path, &["config", "user.name", "Test Author"]);
        git(path, &["config", "user.email", "author@example.test"]);
        fs::write(path.join("tracked.txt"), "root").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "root"]);
        let root = git(path, &["rev-parse", "HEAD"]);
        let tree = git(path, &["rev-parse", "HEAD^{tree}"]);
        git(
            path,
            &[
                "config",
                "remote.upstream.url",
                "https://example.invalid/repo",
            ],
        );
        git(
            path,
            &[
                "config",
                "remote.upstream.fetch",
                "+refs/heads/*:refs/remotes/upstream/*",
            ],
        );
        let mut local_chain = vec![root.clone()];
        let mut upstream_chain = vec![root.clone()];
        for index in 1..=300 {
            local_chain.push(commit_tree(
                path,
                &tree,
                local_chain.last().unwrap(),
                &format!("local {index}"),
            ));
            upstream_chain.push(commit_tree(
                path,
                &tree,
                upstream_chain.last().unwrap(),
                &format!("upstream {index}"),
            ));
        }

        for index in 0..215 {
            let name = format!("remote-{index:03}");
            git(
                path,
                &[
                    "update-ref",
                    &format!("refs/remotes/upstream/{name}"),
                    &root,
                ],
            );
        }
        for index in 0..35 {
            let branch = format!("bench-{index:03}");
            let tracking = format!("tracked-{index:03}");
            let (local_target, upstream_target) = match index {
                0..12 => (&local_chain[265 + index], &root),
                12..23 => (&root, &upstream_chain[265 + index - 12]),
                _ => (&local_chain[265 + index], &upstream_chain[100 + index - 23]),
            };
            git(
                path,
                &["update-ref", &format!("refs/heads/{branch}"), local_target],
            );
            git(
                path,
                &[
                    "update-ref",
                    &format!("refs/remotes/upstream/{tracking}"),
                    upstream_target,
                ],
            );
            git(
                path,
                &["config", &format!("branch.{branch}.remote"), "upstream"],
            );
            git(
                path,
                &[
                    "config",
                    &format!("branch.{branch}.merge"),
                    &format!("refs/heads/{tracking}"),
                ],
            );
        }
        git(path, &["symbolic-ref", "HEAD", "refs/heads/bench-000"]);
        git(path, &["branch", "-D", "seed"]);

        let repo = gix::open(path).unwrap();
        let mut cache = UpstreamCache::new();
        reset_upstream_walk_count();
        reset_read_profile();
        let started = Instant::now();
        let cold_refs = read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut cache).unwrap();
        let cold = started.elapsed();
        let cold_walks = upstream_walk_count();
        let cold_profile = read_profile();
        assert_eq!(
            cold_refs
                .iter()
                .filter(|reference| reference.kind == "local")
                .count(),
            35
        );
        assert_eq!(cache.len(), 35);

        reset_upstream_walk_count();
        let started = Instant::now();
        let uncached_refs =
            read_refs_with_cache(&repo, MAX_UPSTREAM_WALK, &mut UpstreamCache::new()).unwrap();
        let uncached_no_change = started.elapsed();
        let uncached_walks = upstream_walk_count();
        assert_eq!(uncached_refs, cold_refs);
        assert_eq!(uncached_walks, cold_walks);

        let ref_fingerprint = read_ref_fingerprint(&repo).unwrap();
        let operation = read_operation(&repo, &cold_refs).unwrap();
        let repository = Repository {
            path: path.to_path_buf(),
            state: RwLock::new(RepositoryState {
                refs: cold_refs.clone(),
                upstream_cache: cache.clone(),
                ref_fingerprint,
                operation: operation.0,
                conflicted_files: operation.1,
                snapshot: None,
                generation: 0,
            }),
            refresh_lock: Mutex::new(()),
            watcher: Mutex::new(None),
            closed: AtomicBool::new(false),
        };

        reset_upstream_walk_count();
        let started = Instant::now();
        assert_eq!(
            repository.refresh(WatchChanges::ALL).unwrap(),
            None,
            "unchanged repository refresh must not emit"
        );
        let warm = started.elapsed();
        assert_eq!(upstream_walk_count(), 0);

        git(
            path,
            &["update-ref", "refs/heads/bench-000", &local_chain[264]],
        );
        reset_upstream_walk_count();
        reset_read_profile();
        let mut moved_cache = repository.state.read().unwrap().upstream_cache.clone();
        let started = Instant::now();
        let moved_refs = read_refs_with_cache(
            &gix::open(path).unwrap(),
            MAX_UPSTREAM_WALK,
            &mut moved_cache,
        )
        .unwrap();
        let moved_read_refs = started.elapsed();
        let moved_read_walks = upstream_walk_count();
        let moved_profile = read_profile();
        assert_eq!(moved_read_walks, 2);
        let moved_branch = moved_refs
            .iter()
            .find(|reference| reference.name == "bench-000")
            .unwrap();
        assert_eq!(
            (moved_branch.ahead, moved_branch.behind),
            (Some(264), Some(0))
        );

        reset_upstream_walk_count();
        let started = Instant::now();
        assert!(repository.refresh(WatchChanges::ALL).unwrap().is_some());
        let moved_refresh = started.elapsed();
        let moved_walks = upstream_walk_count();
        let moved_refs = repository.state.read().unwrap().refs.clone();
        assert_eq!(moved_walks, 2);
        let moved_branch = moved_refs
            .iter()
            .find(|reference| reference.name == "bench-000")
            .unwrap();
        assert_eq!(
            (moved_branch.ahead, moved_branch.behind),
            (Some(264), Some(0))
        );
        assert_eq!(repository.state.read().unwrap().upstream_cache.len(), 35);
        assert_ne!(moved_refs, cold_refs);

        let cpu = fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|contents| {
                contents
                    .lines()
                    .find_map(|line| line.strip_prefix("model name\t: "))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unknown".into());
        eprintln!(
            "Synthetic 250 remote/35 local upstream refs ({cpu}): cold read_refs {cold:?}; uncached no-change read_refs {uncached_no_change:?} ({uncached_walks} walks); cached warm no-change refresh {warm:?} (0 walks); one branch moved read_refs {moved_read_refs:?} ({moved_read_walks} walks), full moved refresh {moved_refresh:?} ({moved_walks} walks); cold phases ref iteration/config resolution/walks {:?}/{:?}/{:?}, moved read_refs phases {:?}/{:?}/{:?}",
            cold_profile.0,
            cold_profile.1,
            cold_profile.2,
            moved_profile.0,
            moved_profile.1,
            moved_profile.2,
        );
    }
}
