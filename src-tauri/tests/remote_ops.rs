use gitreal_lib::{
    model::{JobFinished, JobStarted, RepoChanged, RepoError},
    repo::reader::{Backend, EventSink},
};
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;

enum Event {
    Started(JobStarted),
    Finished(JobFinished),
}

static NEXT_EXTERNAL: AtomicU64 = AtomicU64::new(1);

struct Recorder(mpsc::Sender<Event>);

impl EventSink for Recorder {
    fn repo_changed(&self, _: RepoChanged) {}
    fn repo_error(&self, _: RepoError) {}
    fn job_started(&self, event: JobStarted) {
        let _ = self.0.send(Event::Started(event));
    }
    fn job_finished(&self, event: JobFinished) {
        let _ = self.0.send(Event::Finished(event));
    }
}

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    remote: PathBuf,
    local: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = directory.path().to_path_buf();
        let remote = root.join("remote.git");
        let seed = root.join("seed");
        fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "-b", "main"]);
        configure_user(&seed);
        commit(&seed, "first", "initial.txt");
        git(
            &root,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);
        let local = root.join("local");
        git(
            &root,
            &["clone", remote.to_str().unwrap(), local.to_str().unwrap()],
        );
        configure_user(&local);
        Self {
            _directory: directory,
            root,
            remote,
            local,
        }
    }

    fn external(&self) -> PathBuf {
        let external = self.root.join(format!(
            "external-{}",
            NEXT_EXTERNAL.fetch_add(1, Ordering::Relaxed)
        ));
        git(
            &self.root,
            &[
                "clone",
                self.remote.to_str().unwrap(),
                external.to_str().unwrap(),
            ],
        );
        configure_user(&external);
        external
    }

    fn backend(&self) -> (Backend, String, mpsc::Receiver<Event>) {
        let (sender, receiver) = mpsc::channel();
        let backend = Backend::with_event_sink(Arc::new(Recorder(sender)));
        let repo_id = backend
            .open_repo(self.local.to_string_lossy().into())
            .unwrap()
            .repo_id;
        (backend, repo_id, receiver)
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
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn configure_user(path: &Path) {
    git(path, &["config", "user.name", "Integration Test"]);
    git(path, &["config", "user.email", "integration@example.test"]);
}

fn commit(path: &Path, message: &str, file: &str) -> String {
    fs::write(path.join(file), message).unwrap();
    git(path, &["add", file]);
    git(path, &["commit", "-m", message]);
    git(path, &["rev-parse", "HEAD"])
}

fn bare_main(fixture: &Fixture) -> String {
    git(
        &fixture.root,
        &[
            "--git-dir",
            fixture.remote.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ],
    )
}

fn finished(receiver: &mpsc::Receiver<Event>, job_id: &str) -> JobFinished {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("timed out waiting for job completion")
        {
            Event::Started(event) => assert_eq!(event.job_id, job_id),
            Event::Finished(event) if event.job_id == job_id => return event,
            Event::Finished(_) => {}
        }
    }
}

#[test]
fn fetch_updates_remote_tracking_branches() {
    let fixture = Fixture::new();
    let (backend, repo_id, events) = fixture.backend();
    let external = fixture.external();
    git(&external, &["checkout", "-b", "feature"]);
    commit(&external, "feature commit", "feature.txt");
    git(&external, &["push", "-u", "origin", "feature"]);

    let id = backend.fetch(&repo_id, None).unwrap();
    assert!(finished(&events, &id).ok);
    assert!(
        backend
            .get_refs(&repo_id)
            .unwrap()
            .refs
            .iter()
            .any(|reference| reference.full_name == "refs/remotes/origin/feature")
    );
}

#[test]
fn pull_fast_forwards_the_checked_out_branch() {
    let fixture = Fixture::new();
    let external = fixture.external();
    commit(&external, "remote advance", "remote.txt");
    let expected = git(&external, &["rev-parse", "HEAD"]);
    git(&external, &["push", "origin", "main"]);
    let (backend, repo_id, events) = fixture.backend();

    let id = backend.pull(&repo_id).unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(git(&fixture.local, &["rev-parse", "HEAD"]), expected);
}

#[test]
fn pull_divergence_fails_without_changing_the_local_head() {
    let fixture = Fixture::new();
    let (backend, repo_id, events) = fixture.backend();
    let local_head = commit(&fixture.local, "local-only", "local.txt");
    let external = fixture.external();
    commit(&external, "remote-only", "remote.txt");
    git(&external, &["push", "origin", "main"]);

    let id = backend.pull(&repo_id).unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    assert_eq!(
        result.message,
        "Branch has diverged from upstream; merge or rebase instead"
    );
    assert_eq!(git(&fixture.local, &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn pushing_a_new_branch_sets_its_upstream() {
    let fixture = Fixture::new();
    git(&fixture.local, &["checkout", "-b", "topic"]);
    commit(&fixture.local, "topic commit", "topic.txt");
    let (backend, repo_id, events) = fixture.backend();

    let id = backend.push(&repo_id, "topic").unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(
        git(
            &fixture.local,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]
        ),
        "origin/topic"
    );
}

#[test]
fn push_rejection_has_a_friendly_non_fast_forward_message() {
    let fixture = Fixture::new();
    let (backend, repo_id, events) = fixture.backend();
    commit(&fixture.local, "local advance", "local.txt");
    let external = fixture.external();
    commit(&external, "remote advance", "remote.txt");
    git(&external, &["push", "origin", "main"]);

    let id = backend.push(&repo_id, "main").unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    assert_eq!(result.message, "Push rejected: remote has new commits");
}

#[test]
fn force_push_uses_a_matching_lease_and_rejects_a_stale_lease() {
    let fixture = Fixture::new();
    let expected = bare_main(&fixture);
    let local_head = commit(&fixture.local, "local force advance", "force.txt");
    let (backend, repo_id, events) = fixture.backend();

    let id = backend.force_push(&repo_id, "main").unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(bare_main(&fixture), local_head);
    assert_ne!(expected, local_head);

    let stale_fixture = Fixture::new();
    let (backend, repo_id, events) = stale_fixture.backend();
    let local_head = commit(&stale_fixture.local, "local force advance", "force.txt");
    let external = stale_fixture.external();
    let remote_head = commit(&external, "remote advance", "remote.txt");
    git(&external, &["push", "origin", "main"]);

    let id = backend.force_push(&repo_id, "main").unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    assert_eq!(
        result.message,
        "Force push rejected: remote changed since Gitreal last read it"
    );
    assert_eq!(bare_main(&stale_fixture), remote_head);
    assert_ne!(local_head, remote_head);
}

#[test]
fn network_push_fails_without_prompting_or_hanging() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    git(
        &fixture.local,
        &[
            "config",
            "remote.origin.url",
            &format!("http://127.0.0.1:{port}/unused"),
        ],
    );
    let (backend, repo_id, events) = fixture.backend();
    let started = Instant::now();

    let id = backend.push(&repo_id, "main").unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn configured_push_remote_is_not_silently_replaced_by_another_remote() {
    let fixture = Fixture::new();
    git(
        &fixture.local,
        &["config", "branch.main.pushRemote", "missing-remote"],
    );
    let (backend, repo_id, _) = fixture.backend();

    let error = backend.push(&repo_id, "main").unwrap_err();
    assert_eq!(error.code, "no_remote");
    assert_eq!(
        backend.push_remote(&repo_id, "main").unwrap_err().code,
        "no_remote"
    );
}
