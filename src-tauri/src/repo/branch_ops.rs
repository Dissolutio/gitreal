use crate::{
    model::{BackendError, JobAction, Result},
    repo::{
        jobs::{JobPrecondition, JobSpec},
        remote_ops::validate_name,
    },
};
use std::path::PathBuf;

pub fn checkout_local(path: PathBuf, branch: &str) -> Result<JobSpec> {
    validate_branch(branch)?;
    let mut spec = JobSpec::new(
        "checkout",
        format!("Checkout {branch}"),
        path,
        vec!["switch".into(), branch.into()],
        format!("Checked out {branch}"),
    );
    spec.action_on_failure = Some(stash_retry_action("local", branch));
    Ok(spec)
}

pub fn checkout_remote(path: PathBuf, remote_branch: &str) -> Result<JobSpec> {
    let (remote, branch) = split_remote_branch(remote_branch)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    let local_exists = repo
        .try_find_reference(&format!("refs/heads/{branch}"))
        .map_err(BackendError::git)?
        .is_some();
    let tracks_target = {
        let config = repo.config_snapshot();
        config
            .string(&format!("branch.{branch}.remote"))
            .is_some_and(|value| value.to_string() == remote)
            && config
                .string(&format!("branch.{branch}.merge"))
                .is_some_and(|value| value.to_string() == format!("refs/heads/{branch}"))
    };
    let (args, message) = if !local_exists {
        (
            vec![
                "switch".into(),
                "--track".into(),
                "-c".into(),
                branch.to_owned(),
                remote_branch.to_owned(),
            ],
            format!("Checked out {remote_branch} as a tracking branch"),
        )
    } else {
        (
            vec!["switch".into(), branch.to_owned()],
            if tracks_target {
                format!("Checked out {branch}")
            } else {
                format!("Checked out {branch}; it tracks a different upstream")
            },
        )
    };
    let mut spec = JobSpec::new(
        "checkout-remote",
        format!("Checkout {remote_branch}"),
        path,
        args,
        message,
    );
    spec.action_on_failure = Some(stash_retry_action("remote", remote_branch));
    Ok(spec)
}

pub fn reset_local_to_remote(
    path: PathBuf,
    branch: &str,
    remote_branch: &str,
    expected_local_oid: &str,
) -> Result<JobSpec> {
    validate_branch(branch)?;
    split_remote_branch(remote_branch)?;
    validate_oid(expected_local_oid)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    let expected_head = format!("refs/heads/{branch}");
    let current = repo
        .head()
        .ok()
        .and_then(|head| head.referent_name().map(|name| name.as_bstr().to_string()))
        .as_deref()
        == Some(expected_head.as_str());
    let mut spec = JobSpec::new(
        "reset-local-to-remote",
        format!("Reset {branch} to {remote_branch}"),
        path,
        if current {
            vec!["reset".into(), "--hard".into(), remote_branch.into()]
        } else {
            vec![
                "branch".into(),
                "-f".into(),
                branch.into(),
                remote_branch.into(),
            ]
        },
        format!("Reset {branch} to {remote_branch}"),
    );
    spec.preconditions.push(JobPrecondition::BranchOid {
        branch: branch.into(),
        expected: expected_local_oid.into(),
    });
    if current {
        spec.preconditions.push(JobPrecondition::CurrentBranch {
            branch: branch.into(),
        });
        spec.preconditions.push(JobPrecondition::WorkingTreeClean);
    }
    Ok(spec)
}

pub fn checkout_commit(path: PathBuf, oid: &str) -> Result<JobSpec> {
    validate_oid(oid)?;
    let mut spec = JobSpec::new(
        "checkout-commit",
        format!("Checkout commit {}", &oid[..7.min(oid.len())]),
        path,
        vec!["switch".into(), "--detach".into(), oid.into()],
        "Checked out commit in detached HEAD state",
    );
    spec.action_on_failure = Some(stash_retry_action("commit", oid));
    Ok(spec)
}

pub fn create_branch(path: PathBuf, name: &str, at_oid: &str, checkout: bool) -> Result<JobSpec> {
    validate_branch(name)?;
    validate_oid(at_oid)?;
    let args = if checkout {
        vec!["switch".into(), "-c".into(), name.into(), at_oid.into()]
    } else {
        vec!["branch".into(), name.into(), at_oid.into()]
    };
    Ok(JobSpec::new(
        "create-branch",
        format!("Create branch {name}"),
        path,
        args,
        format!("Created branch {name}"),
    ))
}

pub fn rename_branch(path: PathBuf, old: &str, new: &str) -> Result<JobSpec> {
    validate_branch(old)?;
    validate_branch(new)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    let has_upstream = repo
        .config_snapshot()
        .string(&format!("branch.{old}.remote"))
        .is_some();
    Ok(JobSpec::new(
        "rename-branch",
        format!("Rename {old} to {new}"),
        path,
        vec!["branch".into(), "-m".into(), old.into(), new.into()],
        if has_upstream {
            format!("Renamed {old} to {new}; the remote branch was left unchanged")
        } else {
            format!("Renamed {old} to {new}")
        },
    ))
}

pub fn delete_local(path: PathBuf, branch: &str, force: bool) -> Result<JobSpec> {
    validate_branch(branch)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    if repo
        .head()
        .ok()
        .and_then(|head| head.referent_name().map(|name| name.as_bstr().to_string()))
        .as_deref()
        == Some(format!("refs/heads/{branch}").as_str())
    {
        return Err(BackendError::new(
            "checked_out_branch",
            "Cannot delete the checked-out branch",
        ));
    }
    let mut spec = JobSpec::new(
        "delete-local",
        format!("Delete local branch {branch}"),
        path,
        vec![
            "branch".into(),
            if force { "-D".into() } else { "-d".into() },
            branch.into(),
        ],
        format!("Deleted local branch {branch}"),
    );
    if !force {
        spec.action_on_failure = Some(JobAction {
            kind: "force_delete".into(),
            label: "Force delete".into(),
            payload: serde_json::json!({ "branch": branch }),
        });
    }
    Ok(spec)
}

pub fn delete_remote(path: PathBuf, remote: &str, branch: &str) -> Result<JobSpec> {
    validate_name(remote, "remote")?;
    validate_branch(branch)?;
    Ok(JobSpec::new(
        "delete-remote",
        format!("Delete remote branch {remote}/{branch}"),
        path,
        vec![
            "push".into(),
            remote.into(),
            "--delete".into(),
            branch.into(),
        ],
        format!("Deleted remote branch {remote}/{branch}"),
    ))
}

pub fn delete_local_and_remote(
    path: PathBuf,
    branch: &str,
    remote: &str,
    remote_branch: &str,
) -> Result<JobSpec> {
    validate_branch(branch)?;
    validate_name(remote, "remote")?;
    validate_branch(remote_branch)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    if repo
        .head()
        .ok()
        .and_then(|head| head.referent_name().map(|name| name.as_bstr().to_string()))
        .as_deref()
        == Some(format!("refs/heads/{branch}").as_str())
    {
        return Err(BackendError::new(
            "checked_out_branch",
            "Cannot delete the checked-out branch",
        ));
    }
    let local_oid = repo
        .try_find_reference(&format!("refs/heads/{branch}"))
        .map_err(BackendError::git)?
        .ok_or_else(|| BackendError::new("unknown_branch", "Local branch not found"))?
        .peel_to_id()
        .map_err(BackendError::git)?
        .to_string();
    let mut spec = JobSpec::new(
        "delete-local-and-remote",
        format!("Delete local and remote branch {branch}"),
        path,
        vec![
            "push".into(),
            remote.into(),
            "--delete".into(),
            remote_branch.into(),
        ],
        format!("Deleted local branch {branch} and remote branch {remote}/{remote_branch}"),
    );
    spec.additional_steps = vec![vec!["branch".into(), "-d".into(), branch.into()]];
    spec.rollback_steps = vec![vec![
        "push".into(),
        format!("--force-with-lease=refs/heads/{remote_branch}:"),
        remote.into(),
        format!("{local_oid}:refs/heads/{remote_branch}"),
    ]];
    spec.step_labels = vec!["Delete remote branch".into(), "Delete local branch".into()];
    spec.action_on_failure = Some(JobAction {
        kind: "force_delete".into(),
        label: "Force delete".into(),
        payload: serde_json::json!({
            "branch": branch,
            "remote": remote,
            "remote_branch": remote_branch,
        }),
    });
    Ok(spec)
}

pub fn stash_push(
    path: PathBuf,
    include_untracked: bool,
    message: Option<&str>,
) -> Result<JobSpec> {
    let mut args = vec!["stash".into(), "push".into()];
    if include_untracked {
        args.push("-u".into());
    }
    if let Some(message) = message {
        args.push("-m".into());
        args.push(message.into());
    }
    Ok(JobSpec::new(
        "stash",
        "Stash changes",
        path,
        args,
        "Stashed working tree changes",
    ))
}

pub fn stash_pop(path: PathBuf) -> JobSpec {
    JobSpec::new(
        "stash-pop",
        "Pop stash",
        path,
        vec!["stash".into(), "pop".into()],
        "Popped the latest stash",
    )
}

pub fn stash_and_checkout(path: PathBuf, checkout: JobSpec) -> JobSpec {
    let mut spec = JobSpec::new(
        "stash-and-checkout",
        format!("Stash and {}", checkout.label.to_lowercase()),
        path,
        vec!["stash".into(), "push".into(), "-u".into()],
        "Stashed changes and checked out branch",
    );
    spec.additional_steps.push(checkout.args);
    spec.step_labels = vec!["Stash".into(), "Checkout".into()];
    spec
}

fn stash_retry_action(target_kind: &str, target: &str) -> JobAction {
    JobAction {
        kind: "stash_and_retry".into(),
        label: "Stash & checkout".into(),
        payload: serde_json::json!({ "target_kind": target_kind, "target": target }),
    }
}

pub fn validate_branch(branch: &str) -> Result<()> {
    validate_name(branch, "branch")?;
    let components: Vec<_> = branch.split('/').collect();
    if branch == "@"
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.contains("..")
        || branch.contains("@{")
        || branch
            .chars()
            .any(|character| "~^:?*[\\ ".contains(character))
        || components.iter().any(|component| {
            component.is_empty()
                || component.starts_with('.')
                || component.ends_with(".lock")
                || component.ends_with('.')
        })
    {
        return Err(BackendError::new("invalid_name", "Invalid branch name"));
    }
    Ok(())
}

fn split_remote_branch(remote_branch: &str) -> Result<(&str, &str)> {
    let (remote, branch) = remote_branch.split_once('/').ok_or_else(|| {
        BackendError::new("invalid_name", "Remote branch must include a remote name")
    })?;
    validate_name(remote, "remote")?;
    validate_branch(branch)?;
    Ok((remote, branch))
}

fn validate_oid(oid: &str) -> Result<()> {
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BackendError::new("invalid_commit", "Invalid commit ID"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_git_branch_format_constraints() {
        for invalid in [
            "-x",
            "a..b",
            "foo.lock",
            "@{",
            "topic/",
            "has space",
            "a//b",
        ] {
            assert!(validate_branch(invalid).is_err(), "{invalid}");
        }
        assert!(validate_branch("feature/topic").is_ok());
    }

    #[test]
    fn accepts_sha1_and_sha256_object_ids() {
        assert!(validate_oid(&"a".repeat(40)).is_ok());
        assert!(validate_oid(&"b".repeat(64)).is_ok());
        assert!(validate_oid(&"g".repeat(40)).is_err());
    }
}
