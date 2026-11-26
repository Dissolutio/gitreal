use gitreal_lib::{
    model::{RefsResponse, RepoChanged, RepoError},
    repo::reader::{Backend, EventSink},
};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

enum RecordedEvent {
    Changed(RepoChanged),
    Error(RepoError),
}

struct EventRecorder(mpsc::Sender<RecordedEvent>);

impl EventSink for EventRecorder {
    fn repo_changed(&self, event: RepoChanged) {
        let _ = self.0.send(RecordedEvent::Changed(event));
    }

    fn repo_error(&self, event: RepoError) {
        let _ = self.0.send(RecordedEvent::Error(event));
    }
}

struct Fixture {
    directory: TempDir,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("repository");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "-b", "main"]);
        git(&path, &["config", "user.name", "Test Author"]);
        git(&path, &["config", "user.email", "author@example.test"]);
        Self { directory, path }
    }

    fn commit(&self, message: &str, content: &str) -> String {
        fs::write(self.path.join("tracked.txt"), content).unwrap();
        git(&self.path, &["add", "."]);
        git(&self.path, &["commit", "-m", message]);
        git(&self.path, &["rev-parse", "HEAD"])
    }

    fn open(&self, backend: &Backend) -> String {
        backend
            .open_repo(self.path.to_string_lossy().into())
            .unwrap()
            .repo_id
    }
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}

fn wait_for_generation(backend: &Backend, repo_id: &str, before: u64) -> RefsResponse {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let refs = backend.get_refs(repo_id).unwrap();
        if refs.generation > before {
            return refs;
        }
        assert!(
            Instant::now() < deadline,
            "repository generation did not advance"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_change(
    backend: &Backend,
    repo_id: &str,
    events: &mpsc::Receiver<RecordedEvent>,
    before: u64,
) -> RefsResponse {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match events
            .recv_timeout(remaining)
            .expect("timed out waiting for repo-changed")
        {
            RecordedEvent::Changed(event) => {
                assert_eq!(event.repo_id, repo_id);
                if event.generation > before {
                    let refs = backend.get_refs(repo_id).unwrap();
                    // Another refresh may already have landed after this event.
                    assert!(refs.generation >= event.generation);
                    return refs;
                }
            }
            RecordedEvent::Error(error) => {
                panic!("unexpected repository error: {}", error.message);
            }
        }
    }
}

fn git_mismatch(backend: &Backend, repo_id: &str, path: &Path) -> Option<String> {
    let expected_commits: Vec<_> = git(path, &["log", "--all", "--topo-order", "--format=%H"])
        .lines()
        .map(str::to_owned)
        .collect();
    let page = backend.get_graph_page(repo_id, 0, 500).unwrap();
    let actual_commits: Vec<_> = page.rows.iter().map(|row| row.commit_id.clone()).collect();
    let mut sorted_actual = actual_commits.clone();
    let mut sorted_expected = expected_commits;
    sorted_actual.sort();
    sorted_expected.sort();
    if sorted_actual != sorted_expected {
        return Some(format!(
            "commits differ: gitreal {sorted_actual:?}, git {sorted_expected:?}"
        ));
    }
    let positions: HashMap<_, _> = actual_commits
        .iter()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect();
    for (index, row) in page.rows.iter().enumerate() {
        if !row.parents.iter().all(|parent| positions[parent] > index) {
            return Some(format!("row {index} is not in topological order"));
        }
    }

    let expected_refs: Vec<_> = git(path, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .map(|line| {
            let (name, target) = line.split_once(' ').unwrap();
            (name.to_owned(), target.to_owned())
        })
        .collect();
    let actual_refs: Vec<_> = backend
        .get_refs(repo_id)
        .unwrap()
        .refs
        .into_iter()
        .filter(|reference| reference.kind != "head")
        .map(|reference| {
            (
                reference.full_name,
                reference.target.expect("test refs point to commits"),
            )
        })
        .collect();
    if actual_refs != expected_refs {
        return Some(format!(
            "refs differ: gitreal {actual_refs:?}, git {expected_refs:?}"
        ));
    }
    let refs_generation = backend.get_refs(repo_id).unwrap().generation;
    if page.generation != refs_generation {
        return Some(format!(
            "page generation {} differs from refs generation {refs_generation}",
            page.generation
        ));
    }
    None
}

/// Multi-step git commands (rebase, a burst of commits) can produce several
/// debounced refreshes; wait until the backend settles on git's final state.
fn wait_for_git_state(
    backend: &Backend,
    repo_id: &str,
    events: &mpsc::Receiver<RecordedEvent>,
    path: &Path,
    before: u64,
) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut generation = wait_for_change(backend, repo_id, events, before).generation;
    loop {
        let Some(mismatch) = git_mismatch(backend, repo_id, path) else {
            return generation;
        };
        assert!(
            Instant::now() < deadline,
            "backend never matched git: {mismatch}"
        );
        generation = wait_for_change(backend, repo_id, events, generation).generation;
    }
}

fn observed_backend() -> (Backend, mpsc::Receiver<RecordedEvent>) {
    let (sender, receiver) = mpsc::channel();
    (
        Backend::with_event_sink(Arc::new(EventRecorder(sender))),
        receiver,
    )
}

fn commit_file(fixture: &Fixture, name: &str, content: &str, message: &str) -> String {
    fs::write(fixture.path.join(name), content).unwrap();
    git(&fixture.path, &["add", name]);
    git(&fixture.path, &["commit", "-m", message]);
    git(&fixture.path, &["rev-parse", "HEAD"])
}

#[test]
fn refs_paging_details_and_live_refresh() {
    let fixture = Fixture::new();
    let first = fixture.commit("First commit\n\nFull body", "first");
    git(&fixture.path, &["branch", "feature"]);
    git(
        &fixture.path,
        &["tag", "-a", "v1", "-m", "Annotated tag", &first],
    );
    git(
        &fixture.path,
        &["update-ref", "refs/remotes/origin/main", &first],
    );
    let second = fixture.commit("Second commit", "second");
    git(&fixture.path, &["checkout", "--orphan", "tag-only"]);
    let tag_only = fixture.commit("Only reachable by tag", "tag");
    git(&fixture.path, &["tag", "-a", "archive", "-m", "Archive"]);
    git(&fixture.path, &["checkout", "main"]);
    git(&fixture.path, &["branch", "-D", "tag-only"]);
    let before = git(&fixture.path, &["status", "--porcelain=v1"]);
    let refs_before = git(&fixture.path, &["show-ref"]);
    let backend = Backend::default();
    let id = fixture.open(&backend);
    let refs = backend.get_refs(&id).unwrap().refs;
    assert!(
        refs.iter()
            .any(|r| r.name == "main" && r.is_head && r.kind == "local")
    );
    assert!(
        refs.iter()
            .any(|r| r.kind == "remote" && r.target.as_ref() == Some(&first))
    );
    assert!(
        refs.iter()
            .any(|r| r.name == "v1" && r.target.as_ref() == Some(&first))
    );
    let all = backend.get_graph_page(&id, 0, 500).unwrap();
    assert_eq!(all.total, 3);
    assert!(all.rows.iter().any(|row| row.commit_id == tag_only));
    let positions: HashMap<_, _> = all
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (&r.commit_id, i))
        .collect();
    for (index, row) in all.rows.iter().enumerate() {
        for parent in &row.parents {
            assert!(positions[parent] > index);
        }
        let page = backend.get_graph_page(&id, index, 1).unwrap();
        assert_eq!(
            serde_json::to_value(&page.rows[0]).unwrap(),
            serde_json::to_value(row).unwrap()
        );
    }
    let detail = backend.get_commit_detail(&id, &first).unwrap();
    assert_eq!(detail.message, "First commit\n\nFull body\n");
    assert_eq!(detail.author, "Test Author");
    assert_eq!(detail.author_email, "author@example.test");
    assert_eq!(detail.changed_paths[0].path, "tracked.txt");
    assert_eq!(detail.changed_paths[0].status, "added");
    let detail = backend.get_commit_detail(&id, &second).unwrap();
    assert_eq!(detail.parents, vec![first]);
    assert_eq!(detail.changed_paths[0].status, "modified");
    assert_eq!(git(&fixture.path, &["status", "--porcelain=v1"]), before);
    assert_eq!(git(&fixture.path, &["show-ref"]), refs_before);
    let generation = all.generation;
    fixture.commit("After snapshot", "later");
    wait_for_generation(&backend, &id, generation);
    assert_eq!(backend.get_graph_page(&id, 0, 500).unwrap().total, 4);
    let fresh = fixture.open(&backend);
    assert_eq!(backend.get_graph_page(&fresh, 0, 500).unwrap().total, 4);
    backend.close_repo(&id).unwrap();
    assert_eq!(
        backend.get_refs(&id).unwrap_err().code,
        "unknown_repository"
    );
}

#[test]
fn metadata_watcher_tracks_refs_commits_rebase_and_pack_refs() {
    let fixture = Fixture::new();
    fixture.commit("Root", "root");
    let (backend, events) = observed_backend();
    let id = fixture.open(&backend);
    let mut generation = backend.get_refs(&id).unwrap().generation;

    fixture.commit("External commit", "external");
    generation = wait_for_git_state(&backend, &id, &events, &fixture.path, generation);

    git(&fixture.path, &["branch", "feature"]);
    generation = wait_for_git_state(&backend, &id, &events, &fixture.path, generation);

    git(&fixture.path, &["branch", "-D", "feature"]);
    generation = wait_for_git_state(&backend, &id, &events, &fixture.path, generation);

    git(&fixture.path, &["commit", "--amend", "-m", "Amended"]);
    generation = wait_for_git_state(&backend, &id, &events, &fixture.path, generation);

    git(&fixture.path, &["branch", "topic"]);
    fixture.commit("Main ahead", "main");
    git(&fixture.path, &["checkout", "topic"]);
    commit_file(&fixture, "topic-1.txt", "1", "Topic 1");
    commit_file(&fixture, "topic-2.txt", "2", "Topic 2");
    commit_file(&fixture, "topic-3.txt", "3", "Topic 3");
    git(&fixture.path, &["checkout", "main"]);
    wait_for_git_state(&backend, &id, &events, &fixture.path, generation);

    generation = backend.get_refs(&id).unwrap().generation;
    git(&fixture.path, &["rebase", "main", "topic"]);
    generation = wait_for_git_state(&backend, &id, &events, &fixture.path, generation);

    git(&fixture.path, &["pack-refs", "--all"]);
    wait_for_git_state(&backend, &id, &events, &fixture.path, generation);
}

#[test]
fn metadata_watcher_debounces_bursts_and_ignores_worktree_files() {
    let fixture = Fixture::new();
    fixture.commit("Root", "root");
    let (backend, events) = observed_backend();
    let id = fixture.open(&backend);
    let initial = backend.get_refs(&id).unwrap().generation;

    for index in 0..20 {
        commit_file(
            &fixture,
            &format!("burst-{index}.txt"),
            &index.to_string(),
            &format!("Burst {index}"),
        );
    }
    let settled = wait_for_git_state(&backend, &id, &events, &fixture.path, initial);
    let mut rebuilds = settled - initial;
    while let Ok(event) = events.recv_timeout(Duration::from_millis(250)) {
        match event {
            RecordedEvent::Changed(event) => {
                assert_eq!(event.repo_id, id);
                rebuilds += 1;
            }
            RecordedEvent::Error(error) => panic!("unexpected repository error: {}", error.message),
        }
    }
    assert!(rebuilds < 10, "20 commits caused {rebuilds} rebuilds");
    let settled = backend.get_refs(&id).unwrap().generation;

    fs::write(fixture.path.join("tracked.txt"), "working tree only").unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(350)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(backend.get_refs(&id).unwrap().generation, settled);

    git(&fixture.path, &["add", "tracked.txt"]);
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(350)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(backend.get_refs(&id).unwrap().generation, settled);
}

#[test]
fn close_stops_watcher_and_deleted_repo_keeps_last_snapshot() {
    let fixture = Fixture::new();
    fixture.commit("Root", "root");
    let (backend, events) = observed_backend();
    let id = fixture.open(&backend);
    backend.close_repo(&id).unwrap();
    fixture.commit("After close", "after");
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(350)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let failed_fixture = Fixture::new();
    failed_fixture.commit("Keep me", "contents");
    let failed_id = failed_fixture.open(&backend);
    let last_good = backend.get_graph_page(&failed_id, 0, 10).unwrap();
    fs::remove_dir_all(&failed_fixture.path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(RecordedEvent::Error(error)) if error.repo_id == failed_id => break,
            Ok(RecordedEvent::Changed(_)) => {}
            Ok(RecordedEvent::Error(error)) => panic!("unexpected error for {}", error.repo_id),
            Err(error) => panic!("timed out waiting for repo-error: {error}"),
        }
    }
    let still_cached = backend.get_graph_page(&failed_id, 0, 10).unwrap();
    assert_eq!(still_cached.generation, last_good.generation);
    assert_eq!(still_cached.rows[0].commit_id, last_good.rows[0].commit_id);
}

#[test]
fn linked_worktree_watches_common_refs() {
    let fixture = Fixture::new();
    fixture.commit("Root", "root");
    let linked = fixture.directory.path().join("linked-worktree");
    git(
        &fixture.path,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let (backend, events) = observed_backend();
    let linked_id = backend
        .open_repo(linked.to_string_lossy().into())
        .unwrap()
        .repo_id;
    let generation = backend.get_refs(&linked_id).unwrap().generation;

    git(&fixture.path, &["branch", "external"]);
    wait_for_git_state(&backend, &linked_id, &events, &linked, generation);
}

#[test]
fn unborn_detached_bare_and_validation() {
    let fixture = Fixture::new();
    let backend = Backend::default();
    let unborn = fixture.open(&backend);
    assert_eq!(backend.get_graph_page(&unborn, 0, 10).unwrap().total, 0);
    assert!(
        backend
            .get_refs(&unborn)
            .unwrap()
            .refs
            .iter()
            .any(|r| r.kind == "head" && r.target.is_none())
    );
    let commit = fixture.commit("Detached", "content");
    git(&fixture.path, &["checkout", "--detach"]);
    git(&fixture.path, &["branch", "-D", "main"]);
    let id = fixture.open(&backend);
    assert_eq!(
        backend.get_graph_page(&id, 0, 10).unwrap().rows[0].commit_id,
        commit
    );
    assert!(
        backend
            .get_refs(&id)
            .unwrap()
            .refs
            .iter()
            .all(|r| r.kind == "head")
    );
    for invalid in [
        "HEAD",
        "HEAD~1",
        "../../config",
        &commit[..8],
        "000000000000000000000000000000000000000g",
    ] {
        assert_eq!(
            backend.get_commit_detail(&id, invalid).unwrap_err().code,
            "invalid_commit_id"
        );
    }
    assert_eq!(
        backend
            .get_commit_detail(&id, &"0".repeat(40))
            .unwrap_err()
            .code,
        "unknown_commit"
    );
    assert_eq!(
        backend.get_graph_page(&id, 0, 0).unwrap_err().code,
        "invalid_page"
    );
    assert_eq!(
        backend.get_graph_page(&id, 0, 501).unwrap_err().code,
        "invalid_page"
    );
    assert!(
        backend
            .get_graph_page(&id, usize::MAX, 1)
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        backend.get_refs("arbitrary/path").unwrap_err().code,
        "unknown_repository"
    );
    assert_eq!(
        backend
            .open_repo(fixture.directory.path().to_string_lossy().into())
            .unwrap_err()
            .code,
        "not_repository"
    );
    let bare = fixture.directory.path().join("bare.git");
    git(
        &fixture.path,
        &["clone", "--bare", ".", bare.to_str().unwrap()],
    );
    let bare = backend.open_repo(bare.to_string_lossy().into()).unwrap();
    assert_eq!(
        backend.get_graph_page(&bare.repo_id, 0, 10).unwrap().total,
        1
    );
}

#[test]
fn shallow_boundary_and_deleted_paths() {
    let fixture = Fixture::new();
    let first = fixture.commit("Root", "root");
    let second = fixture.commit("Tip", "tip");
    let shallow = fixture.directory.path().join("shallow");
    git(
        &fixture.path,
        &[
            "clone",
            "--depth=1",
            &format!("file://{}", fixture.path.display()),
            shallow.to_str().unwrap(),
        ],
    );
    let backend = Backend::default();
    let id = backend
        .open_repo(shallow.to_string_lossy().into())
        .unwrap()
        .repo_id;
    let graph = backend.get_graph_page(&id, 0, 10).unwrap();
    assert_eq!(graph.total, 1);
    assert!(graph.rows[0].parent_edges.is_empty());
    let detail = backend.get_commit_detail(&id, &second).unwrap();
    assert_eq!(detail.changed_paths[0].status, "added");
    assert_eq!(detail.parents.len(), 1);
    // A separately fetched ref can make the raw parent visible, but must not
    // reintroduce an edge excluded by the shallow topological traversal.
    git(
        &shallow,
        &[
            "fetch",
            "--depth=1",
            "origin",
            &format!("{first}:refs/heads/older"),
        ],
    );
    let id = backend
        .open_repo(shallow.to_string_lossy().into())
        .unwrap()
        .repo_id;
    let graph = backend.get_graph_page(&id, 0, 10).unwrap();
    assert_eq!(graph.total, 2);
    assert!(
        graph
            .rows
            .iter()
            .find(|row| row.commit_id == second)
            .unwrap()
            .parents
            .is_empty()
    );
    fs::remove_file(fixture.path.join("tracked.txt")).unwrap();
    git(&fixture.path, &["add", "-u"]);
    git(&fixture.path, &["commit", "-m", "Delete"]);
    let tip = git(&fixture.path, &["rev-parse", "HEAD"]);
    let id = fixture.open(&backend);
    assert_eq!(
        backend.get_commit_detail(&id, &tip).unwrap().changed_paths[0].status,
        "deleted"
    );
}

#[test]
fn topological_order_handles_skewed_dates_and_multiple_tips() {
    let fixture = Fixture::new();
    let root = fixture.commit("Root", "root");
    git(&fixture.path, &["branch", "other"]);
    let main = fixture.commit("Main", "main");
    git(&fixture.path, &["checkout", "other"]);
    let tree = git(&fixture.path, &["rev-parse", "HEAD^{tree}"]);
    let output = Command::new("git")
        .current_dir(&fixture.path)
        .args([
            "commit-tree",
            &tree,
            "-p",
            &root,
            "-m",
            "Old timestamp child",
        ])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(output.status.success());
    let other = String::from_utf8_lossy(&output.stdout).trim().to_string();
    git(&fixture.path, &["update-ref", "refs/heads/other", &other]);
    let backend = Backend::default();
    let id = fixture.open(&backend);
    let page = backend.get_graph_page(&id, 0, 10).unwrap();
    let position = |id: &str| {
        page.rows
            .iter()
            .position(|row| row.commit_id == id)
            .unwrap()
    };
    assert!(position(&main) < position(&root));
    assert!(position(&other) < position(&root));
}

#[test]
fn fifty_thousand_commit_snapshot_is_cached_and_pages_are_bounded() {
    let fixture = Fixture::new();
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&fixture.path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = child.stdin.take().unwrap();
        for index in 1..=50_000 {
            writeln!(input, "commit refs/heads/main\nmark :{index}\ncommitter Test <test@example.test> {} +0000\ndata 6\ncommit", 1_000_000_000 + index).unwrap();
            if index > 1 {
                writeln!(input, "from :{}", index - 1).unwrap();
            }
            writeln!(input).unwrap();
        }
        writeln!(input, "done").unwrap();
    }
    assert!(child.wait().unwrap().success());
    let backend = Backend::default();
    let id = fixture.open(&backend);
    let first = backend.get_graph_page(&id, 0, 200).unwrap();
    assert_eq!(first.total, 50_000);
    assert_eq!(first.rows.len(), 200);
    assert!(first.rows.iter().all(|row| row.lane == 0));
    // Make objects unavailable: another graph walk would now fail.
    fs::rename(
        fixture.path.join(".git/objects"),
        fixture.path.join(".git/cached-test-objects"),
    )
    .unwrap();
    let last = backend.get_graph_page(&id, 49_900, 200).unwrap();
    assert_eq!(last.total, 50_000);
    assert_eq!(last.rows.len(), 100);
    assert!(last.rows.last().unwrap().parent_edges.is_empty());
}
