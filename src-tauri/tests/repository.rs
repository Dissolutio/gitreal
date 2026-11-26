use gitreal_lib::repo::reader::Backend;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::TempDir;

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

#[test]
fn refs_paging_details_and_read_only_snapshot() {
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
    let refs = backend.get_refs(&id).unwrap();
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
    fixture.commit("After snapshot", "later");
    assert_eq!(backend.get_graph_page(&id, 0, 500).unwrap().total, 3);
    let fresh = fixture.open(&backend);
    assert_eq!(backend.get_graph_page(&fresh, 0, 500).unwrap().total, 4);
    backend.close_repo(&id).unwrap();
    assert_eq!(
        backend.get_refs(&id).unwrap_err().code,
        "unknown_repository"
    );
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
