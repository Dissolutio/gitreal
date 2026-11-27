use crate::{
    model::{
        BackendError, ChangedPath, CommitDetail, GraphPage, GraphRow, OpenRepo, OperationInfo,
        RefInfo, RefsResponse, RepoChanged, RepoError, Result,
    },
    repo::graph::{LaneLayout, assign_lanes, page_edges},
    repo::watcher::{MetadataWatcher, WatchSignal},
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

struct Repository {
    path: PathBuf,
    state: RwLock<RepositoryState>,
    refresh_lock: Mutex<()>,
    watcher: Mutex<Option<MetadataWatcher>>,
    closed: AtomicBool,
}

struct RepositoryState {
    refs: Vec<RefInfo>,
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
        let (operation, conflicted_files) = read_operation(&repo, &refs)?;
        let repository = Arc::new(Repository {
            path: path.clone(),
            state: RwLock::new(RepositoryState {
                refs,
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
        let (operation, conflicted_files) = read_operation(&repo, &refs)?;
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
        if refs_unchanged && status_unchanged && !refs_changed {
            // Index-only changes without repository status changes are ignored.
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

fn read_refs(repo: &gix::Repository) -> Result<Vec<RefInfo>> {
    read_refs_with_limit(repo, MAX_UPSTREAM_WALK)
}

fn read_refs_with_limit(repo: &gix::Repository, walk_limit: usize) -> Result<Vec<RefInfo>> {
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
        let upstream = refs
            .iter()
            .find(|reference| reference.full_name == upstream_ref);
        let Some(upstream) = upstream else {
            refs[index].upstream_gone = true;
            continue;
        };
        let Some(local_target) = refs[index].target.as_deref() else {
            continue;
        };
        let Some(upstream_target) = upstream.target.as_deref() else {
            continue;
        };
        let local = gix::ObjectId::from_hex(local_target.as_bytes()).map_err(BackendError::git)?;
        let upstream =
            gix::ObjectId::from_hex(upstream_target.as_bytes()).map_err(BackendError::git)?;
        let (ahead, behind, truncated) = ahead_behind(repo, local, upstream, walk_limit)?;
        refs[index].ahead = Some(ahead);
        refs[index].behind = Some(behind);
        refs[index].truncated = truncated;
    }
    refs.sort_by(|a, b| a.full_name.cmp(&b.full_name));
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
    use std::{hint::black_box, mem::size_of, time::Instant};

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
}
