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
fn checking_out_a_remote_branch_creates_a_tracking_branch() {
    let fixture = Fixture::new();
    let external = fixture.external();
    git(&external, &["checkout", "-b", "feature"]);
    commit(&external, "feature commit", "feature.txt");
    git(&external, &["push", "-u", "origin", "feature"]);
    let (backend, repo_id, events) = fixture.backend();
    let fetch = backend.fetch(&repo_id, None).unwrap();
    assert!(finished(&events, &fetch).ok);

    let id = backend.checkout_remote(&repo_id, "origin/feature").unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(
        git(
            &fixture.local,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]
        ),
        "origin/feature"
    );
}

#[test]
fn checking_out_an_existing_local_tracking_branch_keeps_it_local() {
    let fixture = Fixture::new();
    let external = fixture.external();
    git(&external, &["checkout", "-b", "feature"]);
    commit(&external, "feature commit", "feature.txt");
    git(&external, &["push", "-u", "origin", "feature"]);
    git(&fixture.local, &["fetch", "origin"]);
    git(
        &fixture.local,
        &["switch", "--track", "-c", "feature", "origin/feature"],
    );
    git(&fixture.local, &["switch", "main"]);
    let (backend, repo_id, events) = fixture.backend();

    let id = backend.checkout_remote(&repo_id, "origin/feature").unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(
        git(&fixture.local, &["branch", "--show-current"]),
        "feature"
    );
}

#[test]
fn reset_updates_a_non_checked_out_local_branch() {
    let fixture = Fixture::new();
    let external = fixture.external();
    git(&external, &["checkout", "-b", "topic"]);
    let remote_oid = commit(&external, "remote topic", "topic.txt");
    git(&external, &["push", "-u", "origin", "topic"]);
    git(&fixture.local, &["branch", "topic", "main"]);
    git(&fixture.local, &["fetch", "origin"]);
    let old_oid = git(&fixture.local, &["rev-parse", "refs/heads/topic"]);
    let (backend, repo_id, events) = fixture.backend();

    let id = backend
        .reset_local_to_remote(&repo_id, "topic", "origin/topic", &old_oid)
        .unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/heads/topic"]),
        remote_oid
    );
}

#[test]
fn reset_updates_a_checked_out_branch_only_when_clean() {
    let fixture = Fixture::new();
    let external = fixture.external();
    git(&external, &["checkout", "-b", "topic"]);
    let remote_oid = commit(&external, "remote topic", "topic.txt");
    git(&external, &["push", "-u", "origin", "topic"]);
    git(&fixture.local, &["branch", "topic", "main"]);
    git(&fixture.local, &["fetch", "origin"]);
    git(&fixture.local, &["switch", "topic"]);
    let old_oid = git(&fixture.local, &["rev-parse", "HEAD"]);
    let (backend, repo_id, events) = fixture.backend();

    let id = backend
        .reset_local_to_remote(&repo_id, "topic", "origin/topic", &old_oid)
        .unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(git(&fixture.local, &["rev-parse", "HEAD"]), remote_oid);
}

#[test]
fn reset_refuses_dirty_worktrees_and_stale_local_oids() {
    let fixture = Fixture::new();
    git(&fixture.local, &["branch", "topic", "main"]);
    git(&fixture.local, &["switch", "topic"]);
    let original = git(&fixture.local, &["rev-parse", "HEAD"]);
    let (backend, repo_id, events) = fixture.backend();
    fs::write(fixture.local.join("initial.txt"), "dirty\n").unwrap();
    let dirty_id = backend
        .reset_local_to_remote(&repo_id, "topic", "origin/main", &original)
        .unwrap();
    let dirty = finished(&events, &dirty_id);
    assert!(!dirty.ok);
    assert!(dirty.message.contains("working tree is not clean"));

    git(&fixture.local, &["checkout", "--", "initial.txt"]);
    commit(&fixture.local, "advance local topic", "local-topic.txt");
    let stale_id = backend
        .reset_local_to_remote(&repo_id, "topic", "origin/main", &original)
        .unwrap();
    let stale = finished(&events, &stale_id);
    assert!(!stale.ok);
    assert!(stale.message.contains("changed since"));
}

#[test]
fn checkout_blocked_by_local_changes_offers_stash_and_retry() {
    let fixture = Fixture::new();
    git(&fixture.local, &["switch", "-c", "topic"]);
    commit(&fixture.local, "topic changes shared file", "initial.txt");
    git(&fixture.local, &["switch", "main"]);
    let (backend, repo_id, events) = fixture.backend();
    fs::write(fixture.local.join("initial.txt"), "uncommitted change\n").unwrap();

    let id = backend.checkout_local(&repo_id, "topic").unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    assert_eq!(
        result.message,
        "Checkout blocked by local changes; stash them to continue"
    );
    let action = result.action.unwrap();
    assert_eq!(action.kind, "stash_and_retry");
    assert_eq!(action.label, "Stash & checkout");
}

#[test]
fn stash_and_checkout_runs_in_one_job() {
    let fixture = Fixture::new();
    git(&fixture.local, &["switch", "-c", "topic"]);
    commit(&fixture.local, "topic changes shared file", "initial.txt");
    git(&fixture.local, &["switch", "main"]);
    let (backend, repo_id, events) = fixture.backend();
    fs::write(fixture.local.join("initial.txt"), "uncommitted change\n").unwrap();

    let id = backend
        .stash_and_checkout(&repo_id, "local", "topic")
        .unwrap();
    assert!(finished(&events, &id).ok);
    assert_eq!(git(&fixture.local, &["branch", "--show-current"]), "topic");
    assert!(git(&fixture.local, &["stash", "list"]).contains("WIP"));
}

#[test]
fn stash_and_checkout_stashes_conflicting_untracked_files() {
    let fixture = Fixture::new();
    git(&fixture.local, &["switch", "-c", "topic"]);
    commit(
        &fixture.local,
        "topic adds untracked target",
        "conflict.txt",
    );
    git(&fixture.local, &["switch", "main"]);
    let (backend, repo_id, events) = fixture.backend();
    fs::write(fixture.local.join("conflict.txt"), "untracked conflict\n").unwrap();

    let blocked = backend.checkout_local(&repo_id, "topic").unwrap();
    let blocked = finished(&events, &blocked);
    assert!(!blocked.ok);
    assert_eq!(blocked.action.unwrap().kind, "stash_and_retry");

    let retry = backend
        .stash_and_checkout(&repo_id, "local", "topic")
        .unwrap();
    assert!(finished(&events, &retry).ok);
    assert_eq!(git(&fixture.local, &["branch", "--show-current"]), "topic");
    assert_eq!(
        fs::read_to_string(fixture.local.join("conflict.txt")).unwrap(),
        "topic adds untracked target"
    );
    assert!(git(&fixture.local, &["stash", "list"]).contains("WIP"));
}

#[test]
fn deleting_an_unmerged_branch_offers_force_delete() {
    let fixture = Fixture::new();
    git(&fixture.local, &["switch", "-c", "unmerged"]);
    commit(&fixture.local, "unmerged commit", "unmerged.txt");
    git(&fixture.local, &["switch", "main"]);
    let (backend, repo_id, events) = fixture.backend();

    let id = backend.delete_local(&repo_id, "unmerged", false).unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    let action = result.action.unwrap();
    assert_eq!(action.kind, "force_delete");
    assert_eq!(action.label, "Force delete");
}

#[test]
fn deleting_the_checked_out_branch_is_refused() {
    let fixture = Fixture::new();
    let (backend, repo_id, _) = fixture.backend();
    let error = backend.delete_local(&repo_id, "main", false).unwrap_err();
    assert_eq!(error.code, "checked_out_branch");
}

#[test]
fn stash_push_pop_and_optional_untracked_files_work() {
    let fixture = Fixture::new();
    let (backend, repo_id, events) = fixture.backend();
    fs::write(fixture.local.join("initial.txt"), "modified\n").unwrap();
    fs::write(fixture.local.join("untracked.txt"), "untracked\n").unwrap();

    let push = backend
        .stash_push(&repo_id, true, Some("from test"))
        .unwrap();
    assert!(finished(&events, &push).ok);
    assert!(!fixture.local.join("untracked.txt").exists());
    let pop = backend.stash_pop(&repo_id).unwrap();
    assert!(finished(&events, &pop).ok);
    assert_eq!(
        fs::read_to_string(fixture.local.join("untracked.txt")).unwrap(),
        "untracked\n"
    );
    assert!(
        fs::read_to_string(fixture.local.join("initial.txt"))
            .unwrap()
            .contains("modified")
    );
}

#[test]
fn create_rename_and_delete_local_remote_branches() {
    let fixture = Fixture::new();
    let oid = git(&fixture.local, &["rev-parse", "HEAD"]);
    let (backend, repo_id, events) = fixture.backend();
    let create = backend
        .create_branch(&repo_id, "topic", &oid, false)
        .unwrap();
    assert!(finished(&events, &create).ok);
    git(
        &fixture.local,
        &["branch", "--set-upstream-to", "origin/main", "topic"],
    );
    let rename = backend.rename_branch(&repo_id, "topic", "renamed").unwrap();
    let renamed = finished(&events, &rename);
    assert!(renamed.ok);
    assert!(renamed.message.contains("remote branch was left unchanged"));

    let external = fixture.external();
    git(&external, &["checkout", "-b", "remote-topic"]);
    commit(&external, "remote topic", "remote-topic.txt");
    git(&external, &["push", "-u", "origin", "remote-topic"]);
    let delete = backend
        .delete_remote(&repo_id, "origin", "remote-topic")
        .unwrap();
    assert!(finished(&events, &delete).ok);
    assert!(
        git(
            &fixture.root,
            &[
                "--git-dir",
                fixture.remote.to_str().unwrap(),
                "branch",
                "--list",
                "remote-topic"
            ]
        )
        .is_empty()
    );
    let delete_local = backend.delete_local(&repo_id, "renamed", false).unwrap();
    assert!(finished(&events, &delete_local).ok);
}

#[test]
fn combined_delete_rolls_back_remote_if_local_delete_fails() {
    let fixture = Fixture::new();
    git(&fixture.local, &["switch", "-c", "unmerged"]);
    commit(&fixture.local, "unmerged remote branch", "unmerged.txt");
    git(&fixture.local, &["push", "-u", "origin", "unmerged"]);
    git(&fixture.local, &["switch", "main"]);
    let (backend, repo_id, events) = fixture.backend();

    let id = backend
        .delete_local_and_remote(&repo_id, "unmerged", "origin", "unmerged")
        .unwrap();
    let result = finished(&events, &id);
    assert!(!result.ok);
    assert_eq!(result.action.unwrap().kind, "force_delete");
    assert!(
        !git(
            &fixture.root,
            &[
                "--git-dir",
                fixture.remote.to_str().unwrap(),
                "rev-parse",
                "--verify",
                "refs/heads/unmerged"
            ]
        )
        .is_empty()
    );
    assert!(
        git(
            &fixture.local,
            &["show-ref", "--verify", "refs/heads/unmerged"]
        )
        .contains("unmerged")
    );
}

#[test]
fn checkout_commit_detaches_head_and_branch_creation_can_check_out() {
    let fixture = Fixture::new();
    let oid = git(&fixture.local, &["rev-parse", "HEAD"]);
    let (backend, repo_id, events) = fixture.backend();
    let detached = backend.checkout_commit(&repo_id, &oid).unwrap();
    assert!(finished(&events, &detached).ok);
    assert_eq!(
        git(&fixture.local, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD"
    );
    let create = backend
        .create_branch(&repo_id, "checked-out", &oid, true)
        .unwrap();
    assert!(finished(&events, &create).ok);
    assert_eq!(
        git(&fixture.local, &["branch", "--show-current"]),
        "checked-out"
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
