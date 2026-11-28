use crate::model::{BackendError, FileChange, Result, WorktreeStatus};
use std::{
    hash::{Hash, Hasher},
    path::Path,
    process::Command,
};

const MAX_ENTRIES: usize = 5_000;

pub fn read_worktree_status(path: &Path, is_bare: bool) -> Result<WorktreeStatus> {
    #[cfg(test)]
    STATUS_READ_COUNT.with(|count| count.set(count.get() + 1));
    if is_bare {
        return parse_status(&[]);
    }
    let output = Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotepath=false",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
        ])
        .current_dir(path)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| BackendError::new("worktree_status", error))?;
    if !output.status.success() {
        return Err(BackendError::new(
            "worktree_status",
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    parse_status(&output.stdout)
}

#[cfg(test)]
thread_local! {
    static STATUS_READ_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_status_read_count() {
    STATUS_READ_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn status_read_count() -> usize {
    STATUS_READ_COUNT.with(std::cell::Cell::get)
}

fn parse_status(bytes: &[u8]) -> Result<WorktreeStatus> {
    let mut status = WorktreeStatus::default();
    let mut entries = bytes.split(|byte| *byte == 0);
    let mut count = 0;
    while let Some(record) = entries.next() {
        if record.is_empty() {
            continue;
        }
        let (kind, path, old_path, staged, unstaged, untracked, conflicted) = match record.first() {
            Some(b'?') => {
                let path = record
                    .strip_prefix(b"? ")
                    .ok_or_else(|| BackendError::new("worktree_status", "Invalid status"))?;
                ("untracked", path, None, false, false, true, false)
            }
            Some(b'1') => {
                let fields = split_fields(record, 9)?;
                let xy = fields[1];
                let path = fields[8];
                (
                    "modified",
                    path,
                    None,
                    xy[0] != b'.',
                    xy[1] != b'.',
                    false,
                    false,
                )
            }
            Some(b'2') => {
                let fields = split_fields(record, 10)?;
                let xy = fields[1];
                let old_path = entries.next().unwrap_or_default();
                (
                    "renamed",
                    fields[9],
                    Some(old_path),
                    xy[0] != b'.',
                    xy[1] != b'.',
                    false,
                    false,
                )
            }
            Some(b'u') => {
                let fields = split_fields(record, 11)?;
                ("conflicted", fields[10], None, false, false, false, true)
            }
            _ => continue,
        };
        let path = String::from_utf8_lossy(path).into_owned();
        let old_path = old_path.map(|path| String::from_utf8_lossy(path).into_owned());
        let staged_kind = if kind == "renamed" {
            kind
        } else {
            status_kind(record.get(2).copied(), kind)
        };
        let unstaged_kind = if kind == "renamed" {
            kind
        } else {
            status_kind(record.get(3).copied(), kind)
        };
        let mut add = |target: &mut Vec<FileChange>, change_kind: &str| {
            if count >= MAX_ENTRIES {
                status.truncated = true;
                return;
            }
            target.push(FileChange {
                path: path.clone(),
                old_path: old_path.clone(),
                kind: change_kind.to_owned(),
            });
            count += 1;
        };
        if conflicted {
            add(&mut status.conflicted, "conflicted");
        } else if untracked {
            add(&mut status.untracked, "untracked");
        } else {
            if staged {
                add(&mut status.staged, staged_kind);
            }
            if unstaged {
                add(&mut status.unstaged, unstaged_kind);
            }
        }
        if count >= MAX_ENTRIES && entries.clone().any(|entry| !entry.is_empty()) {
            status.truncated = true;
            break;
        }
    }
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    status.staged.hash(&mut fingerprint);
    status.unstaged.hash(&mut fingerprint);
    status.untracked.hash(&mut fingerprint);
    status.conflicted.hash(&mut fingerprint);
    status.truncated.hash(&mut fingerprint);
    status.fingerprint = fingerprint.finish();
    Ok(status)
}

fn split_fields(record: &[u8], expected: usize) -> Result<Vec<&[u8]>> {
    let fields = record
        .splitn(expected, |byte| *byte == b' ')
        .collect::<Vec<_>>();
    if fields.len() != expected {
        return Err(BackendError::new(
            "worktree_status",
            "Git returned an invalid status record",
        ));
    }
    Ok(fields)
}

fn status_kind(code: Option<u8>, fallback: &str) -> &'static str {
    match code {
        Some(b'A') => "added",
        Some(b'M') => "modified",
        Some(b'D') => "deleted",
        Some(b'R') => "renamed",
        Some(b'T') => "typechange",
        _ => match fallback {
            "untracked" => "untracked",
            "conflicted" => "conflicted",
            "renamed" => "renamed",
            _ => "modified",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, process::Command};
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repo");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "-b", "main"]);
        (directory, path)
    }

    #[test]
    fn parses_staged_unstaged_conflicted_and_untracked_paths() {
        let mut bytes = b"1 A. N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 staged file\0\
1 .D N... 100644 100644 000000 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 deleted\0\
1 MT N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 type changed\0\
2 R. N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 R100 new name\0old name\0\
u UU N... 100644 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 conflict\0\
"
        .to_vec();
        bytes.extend_from_slice("? untracked unicode ✓\0".as_bytes());

        let status = parse_status(&bytes).unwrap();
        assert_eq!(status.staged[0].path, "staged file");
        assert_eq!(status.staged[0].kind, "added");
        assert_eq!(status.unstaged[0].kind, "deleted");
        assert_eq!(status.staged[1].kind, "modified");
        assert_eq!(status.unstaged[1].kind, "typechange");
        assert_eq!(status.staged[2].old_path.as_deref(), Some("old name"));
        assert_eq!(status.staged[2].kind, "renamed");
        assert_eq!(status.conflicted[0].kind, "conflicted");
        assert_eq!(status.untracked[0].path, "untracked unicode ✓");
        assert!(!status.truncated);
    }

    #[test]
    fn truncates_after_five_thousand_entries() {
        let bytes = (0..=MAX_ENTRIES)
            .map(|index| format!("? file-{index}\0"))
            .collect::<String>();
        let status = parse_status(bytes.as_bytes()).unwrap();

        assert_eq!(status.untracked.len(), MAX_ENTRIES);
        assert!(status.truncated);
    }

    #[test]
    fn reads_ignored_paths_without_running_configured_fsmonitor() {
        let (_directory, path) = repository();
        fs::write(path.join(".gitignore"), "node_modules/\n").unwrap();
        fs::write(path.join("tracked file.txt"), "before").unwrap();
        git(&path, &["add", "."]);
        git(
            &path,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(path.join("tracked file.txt"), "after").unwrap();
        fs::write(path.join("untracked unicode ✓.txt"), "new").unwrap();
        fs::create_dir_all(path.join("node_modules/pkg")).unwrap();
        for index in 0..100 {
            fs::write(
                path.join(format!("node_modules/pkg/ignored-{index}.js")),
                "ignored",
            )
            .unwrap();
        }
        let marker = path.join("fsmonitor-ran");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = path.join("fsmonitor.sh");
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\ntouch '{}'\nprintf 'token\\n'\n",
                    marker.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            git(
                &path,
                &["config", "core.fsmonitor", script.to_str().unwrap()],
            );
        }

        let status = read_worktree_status(&path, false).unwrap();
        assert!(
            status
                .unstaged
                .iter()
                .any(|entry| entry.path == "tracked file.txt")
        );
        assert!(
            status
                .untracked
                .iter()
                .any(|entry| entry.path == "untracked unicode ✓.txt")
        );
        assert!(
            status
                .untracked
                .iter()
                .all(|entry| !entry.path.starts_with("node_modules/")),
            "ignored files must not appear"
        );
        #[cfg(unix)]
        assert!(
            !marker.exists(),
            "configured fsmonitor command must not execute"
        );
    }

    #[test]
    fn reads_untracked_files_before_the_first_commit() {
        let (_directory, path) = repository();
        fs::write(path.join("first.txt"), "first").unwrap();
        let status = read_worktree_status(&path, false).unwrap();
        assert_eq!(status.untracked[0].path, "first.txt");
    }
}
