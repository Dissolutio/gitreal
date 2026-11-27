use crate::{
    model::{BackendError, Result},
    repo::jobs::{JobPrecondition, JobSpec},
};
use std::path::{Path, PathBuf};

pub fn merge_into_current(path: PathBuf, source: &str) -> Result<JobSpec> {
    validate_ref(source)?;
    let mut spec = JobSpec::new(
        "merge",
        format!("Merge {source}"),
        path,
        vec!["merge".into(), "--no-edit".into(), source.into()],
        format!("Merged {source}"),
    );
    spec.preconditions.push(JobPrecondition::NoOperation);
    Ok(spec)
}

pub fn rebase_current_onto(path: PathBuf, target: &str) -> Result<JobSpec> {
    validate_ref(target)?;
    let mut spec = JobSpec::new(
        "rebase",
        format!("Rebase onto {target}"),
        path,
        vec!["rebase".into(), target.into()],
        format!("Rebased onto {target}"),
    );
    spec.preconditions.push(JobPrecondition::NoOperation);
    spec.preconditions.push(JobPrecondition::WorkingTreeClean);
    Ok(spec)
}

pub fn cherry_pick(path: PathBuf, oid: &str) -> Result<JobSpec> {
    validate_commit(path.as_path(), oid, "Cherry-pick")?;
    let mut spec = JobSpec::new(
        "cherry-pick",
        format!("Cherry-pick {}", short_oid(oid)),
        path,
        vec!["cherry-pick".into(), oid.into()],
        format!("Cherry-picked {}", short_oid(oid)),
    );
    spec.preconditions.push(JobPrecondition::NoOperation);
    Ok(spec)
}

pub fn revert(path: PathBuf, oid: &str) -> Result<JobSpec> {
    validate_commit(path.as_path(), oid, "Revert")?;
    let mut spec = JobSpec::new(
        "revert",
        format!("Revert {}", short_oid(oid)),
        path,
        vec!["revert".into(), "--no-edit".into(), oid.into()],
        format!("Reverted {}", short_oid(oid)),
    );
    spec.preconditions.push(JobPrecondition::NoOperation);
    Ok(spec)
}

pub fn reset_current(path: PathBuf, oid: &str, mode: &str, expected_head: &str) -> Result<JobSpec> {
    validate_oid(oid)?;
    validate_oid(expected_head)?;
    if !matches!(mode, "soft" | "mixed" | "hard") {
        return Err(BackendError::new(
            "invalid_reset_mode",
            "Invalid reset mode",
        ));
    }
    let mut spec = JobSpec::new(
        "reset",
        format!("Reset to {} ({mode})", short_oid(oid)),
        path,
        vec!["reset".into(), format!("--{mode}"), oid.into()],
        format!("Reset to {} ({mode})", short_oid(oid)),
    );
    spec.preconditions.push(JobPrecondition::HeadOid {
        expected: expected_head.into(),
    });
    spec.preconditions.push(JobPrecondition::NoOperation);
    Ok(spec)
}

pub fn continue_operation(path: PathBuf, kind: &str) -> Result<JobSpec> {
    let args = match kind {
        "merge" => vec!["merge".into(), "--continue".into()],
        "rebase" => vec!["rebase".into(), "--continue".into()],
        "cherry-pick" => vec!["cherry-pick".into(), "--continue".into()],
        "revert" => vec!["revert".into(), "--continue".into()],
        _ => return Err(unknown_operation()),
    };
    let mut spec = JobSpec::new(
        "continue-operation",
        format!("Continue {kind}"),
        path,
        args,
        format!("Continued {kind}"),
    );
    spec.preconditions
        .push(JobPrecondition::OperationIs { kind: kind.into() });
    spec.envs.push(("GIT_EDITOR".into(), "true".into()));
    Ok(spec)
}

pub fn abort_operation(path: PathBuf, kind: &str) -> Result<JobSpec> {
    let args = operation_args(kind, "--abort")?;
    let mut spec = JobSpec::new(
        "abort-operation",
        format!("Abort {kind}"),
        path,
        args,
        format!("Aborted {kind}"),
    );
    spec.preconditions
        .push(JobPrecondition::OperationIs { kind: kind.into() });
    Ok(spec)
}

pub fn skip_operation(path: PathBuf, kind: &str) -> Result<JobSpec> {
    if !matches!(kind, "rebase" | "cherry-pick") {
        return Err(BackendError::new(
            "unsupported_operation_action",
            "Skip is only available during rebase or cherry-pick",
        ));
    }
    let mut spec = JobSpec::new(
        "skip-operation",
        format!("Skip {kind} commit"),
        path,
        vec![kind.into(), "--skip".into()],
        format!("Skipped {kind} commit"),
    );
    spec.preconditions
        .push(JobPrecondition::OperationIs { kind: kind.into() });
    Ok(spec)
}

pub fn stash_apply(path: PathBuf, index: usize) -> JobSpec {
    let mut spec = stash_spec(
        path,
        "stash-apply",
        "Apply stash",
        "apply",
        index,
        "Applied stash",
    );
    spec.preconditions.push(JobPrecondition::NoOperation);
    spec
}

pub fn stash_pop(path: PathBuf, index: usize) -> JobSpec {
    let mut spec = stash_spec(path, "stash-pop", "Pop stash", "pop", index, "Popped stash");
    spec.preconditions.push(JobPrecondition::NoOperation);
    spec
}

pub fn stash_drop(path: PathBuf, index: usize, expected_oid: &str) -> Result<JobSpec> {
    validate_oid(expected_oid)?;
    let mut spec = stash_spec(
        path,
        "stash-drop",
        "Drop stash",
        "drop",
        index,
        "Dropped stash",
    );
    spec.preconditions.push(JobPrecondition::StashOid {
        index,
        expected: expected_oid.into(),
    });
    spec.preconditions.push(JobPrecondition::NoOperation);
    Ok(spec)
}

pub fn open_in_vscode(path: &Path) -> Result<()> {
    open_in_vscode_with_search_path(path, None)
}

pub fn open_release_page() -> Result<()> {
    let mut child = std::process::Command::new("xdg-open")
        .arg(crate::updater::RELEASE_PAGE)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| {
            BackendError::new(
                "opener_unavailable",
                format!("Could not open the release page: {error}"),
            )
        })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn open_in_vscode_with_search_path(
    path: &Path,
    search_path: Option<&std::ffi::OsStr>,
) -> Result<()> {
    let mut command = std::process::Command::new("code");
    if let Some(search_path) = search_path {
        command.env("PATH", search_path);
    }
    let mut child = command
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| {
            BackendError::new(
                "vscode_unavailable",
                format!("Could not open VSCode: {error}. Ensure `code` is on PATH."),
            )
        })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn detect_operation(path: &Path) -> Result<Option<&'static str>> {
    let repo = gix::open(path).map_err(BackendError::git)?;
    let git_dir = repo.git_dir();
    Ok(
        if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
            Some("rebase")
        } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
            Some("cherry-pick")
        } else if git_dir.join("REVERT_HEAD").is_file() {
            Some("revert")
        } else if git_dir.join("MERGE_HEAD").is_file() {
            Some("merge")
        } else if git_dir.join("BISECT_LOG").is_file() {
            Some("bisect")
        } else {
            None
        },
    )
}

fn validate_commit(path: &Path, oid: &str, action: &str) -> Result<()> {
    validate_oid(oid)?;
    let repo = gix::open(path).map_err(BackendError::git)?;
    let id = gix::ObjectId::from_hex(oid.as_bytes()).map_err(BackendError::git)?;
    let commit = repo.find_commit(id).map_err(BackendError::git)?;
    if commit.parent_ids().count() > 1 {
        return Err(BackendError::new(
            "merge_commit_unsupported",
            format!("{action} of a merge commit is not supported"),
        ));
    }
    Ok(())
}

fn validate_ref(reference: &str) -> Result<()> {
    if reference.is_empty()
        || reference.starts_with('-')
        || reference.starts_with('/')
        || reference.ends_with('/')
        || reference.ends_with('.')
        || reference.contains("..")
        || reference.contains("@{")
        || reference
            .chars()
            .any(|character| "~^:?*[\\ \t\r\n".contains(character) || character.is_control())
        || reference.split('/').any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.ends_with(".lock")
                || part.ends_with('.')
        })
    {
        return Err(BackendError::new("invalid_ref", "Invalid Git reference"));
    }
    Ok(())
}

fn validate_oid(oid: &str) -> Result<()> {
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BackendError::new("invalid_commit", "Invalid commit ID"));
    }
    Ok(())
}

fn operation_args(kind: &str, action: &str) -> Result<Vec<String>> {
    if !matches!(kind, "merge" | "rebase" | "cherry-pick" | "revert") {
        return Err(unknown_operation());
    }
    Ok(vec![kind.into(), action.into()])
}

fn unknown_operation() -> BackendError {
    BackendError::new(
        "unknown_operation",
        "No supported Git operation is in progress",
    )
}

fn stash_spec(
    path: PathBuf,
    kind: &str,
    label: &str,
    action: &str,
    index: usize,
    success: &str,
) -> JobSpec {
    let stash = format!("stash@{{{index}}}");
    JobSpec::new(
        kind,
        format!("{label} {index}"),
        path,
        vec!["stash".into(), action.into(), stash],
        format!("{success} {index}"),
    )
}

fn short_oid(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    #[test]
    fn validates_refs_and_oids_before_building_arguments() {
        for invalid in [
            "--help",
            "feature..bad",
            "refs/heads/x ",
            "topic.lock",
            "a@{1}",
        ] {
            assert!(validate_ref(invalid).is_err(), "{invalid}");
        }
        // Git allows `;` in ref names; argv is passed without a shell, so it's harmless.
        assert!(validate_ref("topic;touch").is_ok());
        assert!(validate_ref("refs/heads/feature/topic").is_ok());
        assert!(validate_oid(&"a".repeat(40)).is_ok());
        assert!(validate_oid("HEAD").is_err());
    }

    #[test]
    fn builds_fixed_argv_for_history_operations() {
        let path = PathBuf::from("/repo");
        let merge = merge_into_current(path.clone(), "feature").unwrap();
        assert_eq!(merge.args, ["merge", "--no-edit", "feature"]);
        let reset = reset_current(path.clone(), &"a".repeat(40), "hard", &"b".repeat(40)).unwrap();
        assert_eq!(reset.args, ["reset", "--hard", &"a".repeat(40)]);
        assert!(reset_current(path, &"a".repeat(40), "other", &"b".repeat(40)).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn vscode_launchers_are_reaped_without_waiting_for_them() {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let marker = directory.path().join("launches");
        let code = bin.join("code");
        std::fs::write(&code, "#!/bin/sh\nprintf x >> \"$1\"\n").unwrap();
        let mut permissions = std::fs::metadata(&code).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&code, permissions).unwrap();

        assert!(
            zombie_child_pids().is_empty(),
            "test started with zombie children"
        );
        for _ in 0..5 {
            open_in_vscode_with_search_path(&marker, Some(bin.as_os_str())).unwrap();
        }

        let launch_deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::fs::metadata(&marker)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            < 5
            && std::time::Instant::now() < launch_deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::metadata(&marker).unwrap().len(), 5);

        let reap_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut zombies = zombie_child_pids();
        while !zombies.is_empty() && std::time::Instant::now() < reap_deadline {
            std::thread::sleep(Duration::from_millis(10));
            zombies = zombie_child_pids();
        }
        assert!(
            zombies.is_empty(),
            "found zombie child processes: {zombies:?}"
        );
    }

    #[cfg(target_os = "linux")]
    fn zombie_child_pids() -> Vec<u32> {
        let mut zombies = Vec::new();
        for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            let children = task.path().join("children");
            let Ok(children) = std::fs::read_to_string(children) else {
                continue;
            };
            for pid in children
                .split_whitespace()
                .filter_map(|pid| pid.parse().ok())
            {
                let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                    continue;
                };
                let Some((_, fields)) = stat.rsplit_once(") ") else {
                    continue;
                };
                let mut fields = fields.split_whitespace();
                let state = fields.next();
                let parent_pid = fields.next().and_then(|pid| pid.parse::<u32>().ok());
                if state == Some("Z") && parent_pid == Some(std::process::id()) {
                    zombies.push(pid);
                }
            }
        }
        zombies.sort_unstable();
        zombies.dedup();
        zombies
    }
}
