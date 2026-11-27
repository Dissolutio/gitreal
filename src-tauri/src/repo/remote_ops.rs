use crate::{
    model::{BackendError, RefInfo, Result},
    repo::jobs::JobSpec,
};
use gix::config::AsBStr;
use std::path::PathBuf;

pub fn fetch(path: PathBuf, remote: Option<&str>) -> Result<JobSpec> {
    if let Some(remote) = remote {
        validate_name(remote, "remote")?;
        Ok(JobSpec::new(
            "fetch",
            format!("Fetch {remote}"),
            path,
            vec!["fetch".into(), "--prune".into(), remote.into()],
            format!("Fetched from {remote}"),
        ))
    } else {
        Ok(JobSpec::new(
            "fetch",
            "Fetch all remotes",
            path,
            vec!["fetch".into(), "--all".into(), "--prune".into()],
            "Fetched all remotes",
        ))
    }
}

pub fn pull(path: PathBuf) -> Result<JobSpec> {
    let repo = gix::open(&path).map_err(BackendError::git)?;
    let head = repo.head().map_err(BackendError::git)?;
    let branch = head
        .referent_name()
        .map(|name| name.as_bstr().to_string())
        .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_owned))
        .ok_or_else(|| {
            BackendError::new("no_current_branch", "Pull requires a checked-out branch")
        })?;
    validate_name(&branch, "branch")?;
    let upstream = upstream(&repo, &branch)?;
    if upstream.is_none() {
        return Err(BackendError::new(
            "no_upstream",
            "No upstream configured for this branch",
        ));
    }
    let _target = upstream.unwrap();
    Ok(JobSpec::new(
        "pull",
        format!("Pull {branch}"),
        path,
        vec!["pull".into(), "--ff-only".into()],
        format!("Pulled {branch}"),
    ))
}

pub fn push(path: PathBuf, branch: &str, refs: &[RefInfo]) -> Result<JobSpec> {
    validate_name(branch, "branch")?;
    validate_local_branch(branch, refs)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    let remote = configured_push_remote(&repo, branch)?;
    let config = repo.config_snapshot();
    let upstream = config.string(&format!("branch.{branch}.merge"));
    let has_upstream = upstream.is_some();
    let args = if has_upstream {
        vec!["push".into(), remote.clone(), branch.into()]
    } else {
        vec!["push".into(), "-u".into(), remote.clone(), branch.into()]
    };
    Ok(JobSpec::new(
        "push",
        format!("Push {branch}"),
        path,
        args,
        format!("Pushed {branch} to {remote}"),
    ))
}

pub fn force_push(path: PathBuf, branch: &str, refs: &[RefInfo]) -> Result<JobSpec> {
    validate_name(branch, "branch")?;
    validate_local_branch(branch, refs)?;
    let repo = gix::open(&path).map_err(BackendError::git)?;
    let remote = configured_push_remote(&repo, branch)?;
    let remote_ref = format!("refs/remotes/{remote}/{branch}");
    let expected = refs
        .iter()
        .find(|reference| reference.full_name == remote_ref)
        .and_then(|reference| reference.target.as_deref())
        .ok_or_else(|| {
            BackendError::new(
                "missing_lease",
                "Cannot force push without a previously observed remote-tracking ref",
            )
        })?;
    Ok(JobSpec::new(
        "force-push",
        format!("Force push {branch}"),
        path,
        vec![
            "push".into(),
            format!("--force-with-lease={branch}:{expected}"),
            remote.clone(),
            branch.into(),
        ],
        format!("Force pushed {branch} to {remote} with lease"),
    ))
}

pub fn push_remote(path: PathBuf, branch: &str, refs: &[RefInfo]) -> Result<String> {
    validate_name(branch, "branch")?;
    validate_local_branch(branch, refs)?;
    let repo = gix::open(path).map_err(BackendError::git)?;
    configured_push_remote(&repo, branch)
}

fn configured_push_remote(repo: &gix::Repository, branch: &str) -> Result<String> {
    let config = repo.config_snapshot();
    let push_remote = config.string(&format!("branch.{branch}.pushRemote"));
    let tracking_remote = config.string(&format!("branch.{branch}.remote"));
    select_remote(
        repo,
        push_remote
            .as_ref()
            .or(tracking_remote.as_ref())
            .map(|value| value.to_string())
            .as_deref(),
    )
}

fn upstream(repo: &gix::Repository, branch: &str) -> Result<Option<String>> {
    let config = repo.config_snapshot();
    let remote = config
        .string(&format!("branch.{branch}.remote"))
        .map(|value| value.to_string());
    let merge = config
        .string(&format!("branch.{branch}.merge"))
        .map(|value| value.to_string());
    Ok(remote.zip(merge).map(|(remote, merge)| {
        format!(
            "refs/remotes/{remote}/{}",
            merge.trim_start_matches("refs/heads/")
        )
    }))
}

fn select_remote(repo: &gix::Repository, preferred: Option<&str>) -> Result<String> {
    let remotes = repo.remote_names();
    let remote = if let Some(preferred) = preferred {
        validate_name(preferred, "remote")?;
        if !remotes
            .iter()
            .any(|candidate| candidate.as_bstr() == preferred)
        {
            return Err(BackendError::new(
                "no_remote",
                "The configured push remote is not available",
            ));
        }
        preferred.to_owned()
    } else {
        remotes
            .iter()
            .any(|candidate| candidate.as_bstr() == "origin")
            .then(|| "origin".to_owned())
            .or_else(|| (remotes.len() == 1).then(|| remotes.iter().next().unwrap().to_string()))
            .ok_or_else(|| BackendError::new("no_remote", "No remote configured"))?
    };
    validate_name(&remote, "remote")?;
    Ok(remote)
}

fn validate_local_branch(branch: &str, refs: &[RefInfo]) -> Result<()> {
    if !refs.iter().any(|reference| {
        reference.kind == "local" && reference.full_name == format!("refs/heads/{branch}")
    }) {
        return Err(BackendError::new(
            "unknown_branch",
            "Local branch not found",
        ));
    }
    Ok(())
}

pub fn validate_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('-')
        || name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(BackendError::new(
            "invalid_name",
            format!("Invalid {kind} name"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_option_like_and_whitespace_names() {
        for name in ["--upload-pack=evil", "main branch", "origin\nremote", ""] {
            assert_eq!(
                validate_name(name, "branch").unwrap_err().code,
                "invalid_name"
            );
        }
        assert!(validate_name("feature/topic", "branch").is_ok());
    }
}
