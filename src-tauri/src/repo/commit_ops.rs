use crate::{
    model::{BackendError, Result},
    repo::jobs::{JobPrecondition, JobSpec},
};
use std::path::{Component, Path, PathBuf};

pub fn stage_paths(path: PathBuf, paths: &[String]) -> Result<JobSpec> {
    let paths = validate_paths(paths)?;
    let mut args = vec!["add".into(), "--".into()];
    args.extend(paths);
    Ok(JobSpec::new(
        "stage",
        format!(
            "Stage {} file{}",
            args.len() - 2,
            if args.len() == 3 { "" } else { "s" }
        ),
        path,
        args,
        "Staged changes",
    ))
}

pub fn stage_all(path: PathBuf) -> JobSpec {
    JobSpec::new(
        "stage",
        "Stage all changes",
        path,
        vec!["add".into(), "-A".into()],
        "Staged all changes",
    )
}

pub fn unstage_paths(path: PathBuf, paths: &[String], has_head: bool) -> Result<JobSpec> {
    let paths = validate_paths(paths)?;
    let mut args = if has_head {
        vec!["restore".into(), "--staged".into(), "--".into()]
    } else {
        vec!["rm".into(), "--cached".into(), "--".into()]
    };
    args.extend(paths);
    Ok(JobSpec::new(
        "unstage",
        format!(
            "Unstage {} file{}",
            args.len() - if has_head { 3 } else { 3 },
            if args.len() == 4 { "" } else { "s" }
        ),
        path,
        args,
        "Unstaged changes",
    ))
}

pub fn unstage_all(path: PathBuf, has_head: bool) -> JobSpec {
    let args = if has_head {
        vec!["restore".into(), "--staged".into(), "--".into(), ".".into()]
    } else {
        vec!["rm".into(), "--cached".into(), "--".into(), ".".into()]
    };
    JobSpec::new(
        "unstage",
        "Unstage all changes",
        path,
        args,
        "Unstaged all changes",
    )
}

pub fn commit(
    path: PathBuf,
    summary: &str,
    description: &str,
    amend: bool,
    amend_pushed: Option<String>,
) -> Result<JobSpec> {
    if summary.trim().is_empty() {
        return Err(BackendError::new(
            "empty_summary",
            "Enter a commit summary before committing",
        ));
    }
    let mut args = vec!["commit".into(), "-m".into(), summary.into()];
    if !description.trim().is_empty() {
        args.extend(["-m".into(), description.into()]);
    }
    if amend {
        args.push("--amend".into());
    }
    let mut spec = JobSpec::new(
        "commit",
        if amend {
            "Amend commit"
        } else {
            "Commit changes"
        },
        path,
        args,
        "changes",
    );
    spec.envs.push(("GIT_EDITOR".into(), "true".into()));
    if amend && let Some(upstream) = amend_pushed {
        spec.preconditions
            .push(JobPrecondition::HeadNotPushed { upstream });
    }
    Ok(spec)
}

fn validate_paths(paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Err(BackendError::new("empty_paths", "Select at least one file"));
    }
    paths
        .iter()
        .map(|path| {
            let path_obj = Path::new(path);
            if path.is_empty()
                || path_obj.is_absolute()
                || path_obj.components().any(|component| {
                    matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
            {
                return Err(BackendError::new(
                    "invalid_path",
                    "File paths must stay inside the repository",
                ));
            }
            Ok(path.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{JobFinished, JobStarted, RepoChanged, RepoError},
        repo::{jobs::JobQueue, reader::EventSink},
    };
    use std::{
        fs,
        process::Command,
        sync::{Arc, mpsc},
        time::Duration,
    };
    use tempfile::TempDir;

    struct FinishedSink(mpsc::Sender<JobFinished>);

    impl EventSink for FinishedSink {
        fn repo_changed(&self, _: RepoChanged) {}
        fn repo_error(&self, _: RepoError) {}
        fn job_started(&self, _: JobStarted) {}
        fn job_finished(&self, event: JobFinished) {
            let _ = self.0.send(event);
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
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
    }

    fn run(_path: &Path, spec: JobSpec) -> JobFinished {
        let (sender, receiver) = mpsc::channel();
        let queue = JobQueue::new("commit-ops-test".into(), Arc::new(FinishedSink(sender)));
        queue.enqueue(spec, || {}).unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn repository() -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repo");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "-b", "main"]);
        git(&path, &["config", "user.name", "Test Author"]);
        git(&path, &["config", "user.email", "author@example.test"]);
        (directory, path)
    }

    #[test]
    fn stage_and_unstage_paths_use_separator_and_reject_escape() {
        let paths = vec!["a file.txt".into(), "unicodé.txt".into()];
        let staged = stage_paths(PathBuf::from("."), &paths).unwrap();
        assert_eq!(staged.args, ["add", "--", "a file.txt", "unicodé.txt"]);
        let unstaged = unstage_paths(PathBuf::from("."), &paths, true).unwrap();
        assert_eq!(
            unstaged.args,
            ["restore", "--staged", "--", "a file.txt", "unicodé.txt"]
        );
        assert!(stage_paths(PathBuf::from("."), &["../outside".into()]).is_err());
        assert!(unstage_paths(PathBuf::from("."), &["/outside".into()], true).is_err());
    }

    #[test]
    fn unborn_unstage_uses_cached_remove_and_commit_rejects_empty_summary() {
        let spec = unstage_paths(PathBuf::from("."), &["new.txt".into()], false).unwrap();
        assert_eq!(spec.args, ["rm", "--cached", "--", "new.txt"]);
        assert_eq!(
            unstage_all(PathBuf::from("."), false).args,
            ["rm", "--cached", "--", "."]
        );
        assert_eq!(
            commit(PathBuf::from("."), "  ", "", false, None)
                .err()
                .unwrap()
                .code,
            "empty_summary"
        );
    }

    #[test]
    fn commit_arguments_preserve_description_and_editor_suppression() {
        let spec = commit(
            PathBuf::from("."),
            "summary",
            "description\nmore",
            true,
            Some("refs/remotes/origin/main".into()),
        )
        .unwrap();
        assert_eq!(
            spec.args,
            [
                "commit",
                "-m",
                "summary",
                "-m",
                "description\nmore",
                "--amend"
            ]
        );
        assert_eq!(spec.envs[0], ("GIT_EDITOR".into(), "true".into()));
        assert_eq!(spec.preconditions.len(), 1);
    }

    #[test]
    fn stage_unstage_and_commit_work_in_an_unborn_repository() {
        let (_directory, path) = repository();
        fs::write(path.join("new file.txt"), "hello").unwrap();

        assert!(
            run(
                &path,
                stage_paths(path.clone(), &["new file.txt".into()]).unwrap()
            )
            .ok
        );
        assert_eq!(
            path.join(".git/index").is_file(),
            true,
            "staging should create the index"
        );
        assert!(
            run(
                &path,
                unstage_paths(path.clone(), &["new file.txt".into()], false).unwrap()
            )
            .ok
        );
        assert!(
            !Command::new("git")
                .args(["ls-files", "--error-unmatch", "new file.txt"])
                .current_dir(&path)
                .output()
                .unwrap()
                .status
                .success()
        );

        run(
            &path,
            stage_paths(path.clone(), &["new file.txt".into()]).unwrap(),
        );
        let finished = run(
            &path,
            commit(path.clone(), "Summary", "Description\nbody", false, None).unwrap(),
        );
        assert!(finished.ok, "{}", finished.message);
        assert!(finished.message.starts_with("Committed "));
        let log = Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "Summary\n\nDescription\nbody"
        );

        fs::write(path.join("another.txt"), "another").unwrap();
        assert!(run(&path, stage_all(path.clone())).ok);
        assert!(run(&path, unstage_all(path.clone(), true)).ok);
        let nothing_staged = run(
            &path,
            commit(path.clone(), "nothing", "", false, None).unwrap(),
        );
        assert!(!nothing_staged.ok);
        assert_eq!(nothing_staged.message, "Nothing staged to commit");

        fs::write(path.join("new file.txt"), "updated").unwrap();
        assert!(
            run(
                &path,
                stage_paths(path.clone(), &["new file.txt".into()]).unwrap()
            )
            .ok
        );
        let amended = run(
            &path,
            commit(path.clone(), "Amended summary", "", true, None).unwrap(),
        );
        assert!(amended.ok, "{}", amended.message);
        let log = Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "Amended summary"
        );
    }

    #[test]
    fn pushed_head_blocks_amend_and_failing_hook_reports_details() {
        let (_directory, path) = repository();
        fs::write(path.join("tracked.txt"), "before").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-m", "original"]);
        let remote = path.with_extension("git");
        git(
            remote.parent().unwrap(),
            &["init", "--bare", remote.to_str().unwrap()],
        );
        git(
            &path,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&path, &["push", "-u", "origin", "main"]);
        git(&path, &["fetch", "origin"]);

        let finished = run(
            &path,
            commit(
                path.clone(),
                "amended",
                "",
                true,
                Some("refs/remotes/origin/main".into()),
            )
            .unwrap(),
        );
        assert!(!finished.ok);
        assert!(finished.message.contains("already on origin/main"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let hook = path.join(".git/hooks/pre-commit");
            fs::write(&hook, "#!/bin/sh\necho 'test hook rejection' >&2\nexit 1\n").unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(path.join("tracked.txt"), "after").unwrap();
            run(
                &path,
                stage_paths(path.clone(), &["tracked.txt".into()]).unwrap(),
            );
            let failed = run(
                &path,
                commit(path.clone(), "blocked", "", false, None).unwrap(),
            );
            assert!(!failed.ok);
            assert!(
                failed.message.contains("hook rejected"),
                "{} ({})",
                failed.message,
                failed.stderr_tail
            );
            assert!(failed.stderr_tail.contains("test hook rejection"));
        }
    }
}
