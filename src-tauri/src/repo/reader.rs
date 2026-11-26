use crate::{
    model::{
        BackendError, ChangedPath, CommitDetail, GraphPage, GraphRow, OpenRepo, RefInfo, Result,
    },
    repo::graph::{LaneLayout, assign_lanes, page_edges},
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
        Ok(snapshot.graph_page(start, end))
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
        if !repository.snapshot()?.ids.contains(&oid) {
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
}

impl Snapshot {
    fn graph_page(&self, start: usize, end: usize) -> GraphPage {
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
            refs: Vec::new(),
            snapshot: OnceLock::new(),
        });
        assert!(repository.snapshot.set(Ok(Arc::new(snapshot))).is_ok());
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
}
