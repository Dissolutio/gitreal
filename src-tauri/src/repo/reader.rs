use crate::{
    model::{
        BackendError, ChangedPath, CommitDetail, GraphPage, GraphRow, OpenRepo, RefInfo, Result,
    },
    repo::graph::assign_lanes,
};
use gix::bstr::ByteSlice;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const MAX_PAGE_SIZE: usize = 500;
const MAX_OPEN_REPOS: usize = 16;

struct Repository {
    path: PathBuf,
    refs: Vec<RefInfo>,
    snapshot: OnceLock<Result<Arc<Snapshot>>>,
}

struct Snapshot {
    rows: Vec<GraphRow>,
    ids: HashSet<String>,
}

#[derive(Default)]
pub struct Backend {
    repositories: Mutex<HashMap<String, Arc<Repository>>>,
    next_id: AtomicU64,
}

impl Backend {
    pub fn open_repo(&self, path: String) -> Result<OpenRepo> {
        let path = Path::new(&path)
            .canonicalize()
            .map_err(|error| BackendError::new("invalid_path", error))?;
        // Open exactly the selected repository, not a parent found by discovery.
        let repo = gix::open(&path).map_err(|error| BackendError::new("not_repository", error))?;
        let refs = read_refs(&repo)?;
        let repository = Arc::new(Repository {
            path: path.clone(),
            refs,
            snapshot: OnceLock::new(),
        });
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
        let repo_id = format!("repo-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
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
        self.repositories
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Repository state unavailable"))?
            .remove(repo_id);
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

    pub fn get_refs(&self, repo_id: &str) -> Result<Vec<RefInfo>> {
        Ok(self.repository(repo_id)?.refs.clone())
    }

    pub fn get_graph_page(&self, repo_id: &str, offset: usize, limit: usize) -> Result<GraphPage> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(BackendError::new(
                "invalid_page",
                "Page size must be between 1 and 500",
            ));
        }
        let repository = self.repository(repo_id)?;
        let snapshot = repository.snapshot()?;
        let start = offset.min(snapshot.rows.len());
        let end = start.saturating_add(limit).min(snapshot.rows.len());
        Ok(GraphPage {
            rows: snapshot.rows[start..end].to_vec(),
            total: snapshot.rows.len(),
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
        if !repository.snapshot()?.ids.contains(&id) {
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
    fn snapshot(&self) -> Result<Arc<Snapshot>> {
        self.snapshot
            .get_or_init(|| self.build_snapshot().map(Arc::new))
            .clone()
    }

    fn build_snapshot(&self) -> Result<Snapshot> {
        let repo = gix::open(&self.path).map_err(BackendError::git)?;
        let roots: HashSet<_> = self
            .refs
            .iter()
            .filter_map(|reference| reference.target.as_ref())
            .collect();
        let mut roots: Vec<_> = roots
            .iter()
            .map(|id| gix::ObjectId::from_hex(id.as_bytes()).map_err(BackendError::git))
            .collect::<Result<_>>()?;
        roots.sort();
        let mut labels: HashMap<&str, Vec<String>> = HashMap::new();
        for reference in &self.refs {
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
