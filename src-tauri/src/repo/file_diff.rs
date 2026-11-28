use crate::model::{
    BackendError, FileDiff, FileDiffHunk, FileDiffLine, FileDiffSource, FileDiffStats, Result,
};
use gix::diff::blob::{Algorithm, Diff, InternedInput, TokenSource};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_LINES: usize = 20_000;
const BINARY_SAMPLE_BYTES: usize = 8 * 1024;

struct LineTokens<'a>(&'a [String]);

struct LineIter<'a>(std::slice::Iter<'a, String>);

impl<'a> Iterator for LineIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(String::as_str)
    }
}

impl<'a> TokenSource for LineTokens<'a> {
    type Token = &'a str;
    type Tokenizer = LineIter<'a>;

    fn tokenize(&self) -> Self::Tokenizer {
        LineIter(self.0.iter())
    }

    fn estimate_tokens(&self) -> u32 {
        self.0.len().min(u32::MAX as usize) as u32
    }
}

pub fn read_file_diff(
    repo_path: &Path,
    source: FileDiffSource,
    path: &str,
    old_path: Option<&str>,
    force: bool,
) -> Result<FileDiff> {
    validate_path(path)?;
    if let Some(old_path) = old_path {
        validate_path(old_path)?;
    }
    let repo = gix::open(repo_path).map_err(BackendError::git)?;
    if repo.is_bare() && !matches!(&source, FileDiffSource::Commit { .. }) {
        return Err(BackendError::new(
            "unsupported_file_diff",
            "Working-tree diffs are unavailable for bare repositories",
        ));
    }

    let (old_bytes, new_bytes) = match source {
        FileDiffSource::WorktreeUnstaged => {
            let index = repo
                .index_or_load_from_head_or_empty()
                .map_err(BackendError::git)?;
            let old = index_blob(&repo, &index, old_path.unwrap_or(path))?;
            let new = read_worktree_file(repo.workdir().unwrap_or(repo_path), path)?;
            (old, new)
        }
        FileDiffSource::WorktreeStaged => {
            let old = head_tree(&repo)?
                .as_ref()
                .map(|tree| tree_blob(tree, old_path.unwrap_or(path)))
                .transpose()?
                .flatten();
            let index = repo
                .index_or_load_from_head_or_empty()
                .map_err(BackendError::git)?;
            let new = index_blob(&repo, &index, path)?;
            (old, new)
        }
        FileDiffSource::Commit { oid } => {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(BackendError::new(
                    "invalid_commit_id",
                    "Expected a full hexadecimal object ID",
                ));
            }
            let id = gix::ObjectId::from_hex(oid.as_bytes()).map_err(BackendError::git)?;
            let commit = repo.find_commit(id).map_err(BackendError::git)?;
            let new_tree = commit.tree().map_err(BackendError::git)?;
            let shallow = repo
                .shallow_commits()
                .map_err(BackendError::git)?
                .is_some_and(|ids| ids.contains(&id));
            let old_tree = match commit.parent_ids().next().filter(|_| !shallow) {
                Some(parent) => Some(
                    repo.find_commit(parent)
                        .map_err(BackendError::git)?
                        .tree()
                        .map_err(BackendError::git)?,
                ),
                None => None,
            };
            let old = old_tree
                .as_ref()
                .map(|tree| tree_blob(tree, old_path.unwrap_or(path)))
                .transpose()?
                .flatten();
            let new = tree_blob(&new_tree, path)?;
            (old, new)
        }
    };

    let old_bytes = old_bytes.unwrap_or_default();
    let new_bytes = new_bytes.unwrap_or_default();
    let binary = is_binary(&old_bytes) || is_binary(&new_bytes);
    let old_lines = decoded_lines(&old_bytes);
    let new_lines = decoded_lines(&new_bytes);
    let too_large = old_bytes.len() > MAX_FILE_BYTES
        || new_bytes.len() > MAX_FILE_BYTES
        || old_lines.len() > MAX_LINES
        || new_lines.len() > MAX_LINES;
    let old_no_newline = !old_bytes.is_empty() && !old_bytes.ends_with(b"\n");
    let new_no_newline = !new_bytes.is_empty() && !new_bytes.ends_with(b"\n");
    let (stats, lines, hunks) = if binary {
        (FileDiffStats::default(), Vec::new(), Vec::new())
    } else {
        let input = InternedInput::new(LineTokens(&old_lines), LineTokens(&new_lines));
        let diff = Diff::compute(Algorithm::Histogram, &input);
        let stats = FileDiffStats {
            added: diff.count_additions() as usize,
            removed: diff.count_removals() as usize,
        };
        let (lines, hunks) = if too_large && !force {
            (Vec::new(), Vec::new())
        } else {
            merge_lines(&old_lines, &new_lines, &input, &diff)
        };
        (stats, lines, hunks)
    };

    Ok(FileDiff {
        path: path.to_owned(),
        old_path: old_path.map(str::to_owned),
        binary,
        too_large: too_large && !force,
        old_no_newline,
        new_no_newline,
        lines,
        hunks,
        stats,
    })
}

fn validate_path(path: &str) -> Result<PathBuf> {
    let path_buf = Path::new(path);
    if path.is_empty()
        || path.contains('\0')
        || !path_buf
            .components()
            .all(|component| matches!(component, Component::Normal(name) if name != ".git"))
    {
        return Err(BackendError::new(
            "invalid_path",
            "Expected a repository-relative file path",
        ));
    }
    Ok(path_buf.to_owned())
}

fn head_tree(repo: &gix::Repository) -> Result<Option<gix::Tree<'_>>> {
    match repo.head().map_err(BackendError::git)?.id() {
        Some(id) => Ok(Some(
            id.object()
                .map_err(BackendError::git)?
                .peel_to_commit()
                .map_err(BackendError::git)?
                .tree()
                .map_err(BackendError::git)?,
        )),
        None => Ok(None),
    }
}

fn tree_blob(tree: &gix::Tree<'_>, path: &str) -> Result<Option<Vec<u8>>> {
    let Some(entry) = tree.lookup_entry_by_path(path).map_err(BackendError::git)? else {
        return Ok(None);
    };
    if entry.mode().is_tree() {
        return Ok(None);
    }
    let object = entry.object().map_err(BackendError::git)?;
    let mut blob = object.try_into_blob().ok();
    Ok(blob.as_mut().map(|blob| blob.take_data()))
}

fn index_blob(
    repo: &gix::Repository,
    index: &gix::index::File,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    use gix::bstr::ByteSlice;
    let oid = index
        .entries_with_paths_by_filter_map(|_, entry| {
            (entry.stage() == gix::index::entry::Stage::Unconflicted).then_some(entry.id)
        })
        .find(|(entry_path, _)| entry_path.as_bytes() == path.as_bytes())
        .map(|(_, id)| id);
    oid.map(|id| {
        let mut blob = repo.find_blob(id).map_err(BackendError::git)?;
        Ok(blob.take_data())
    })
    .transpose()
}

#[cfg(unix)]
fn read_worktree_file(repo_path: &Path, path: &str) -> Result<Option<Vec<u8>>> {
    use std::{
        ffi::CString,
        io::Read,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };

    let relative_path = validate_path(path)?;
    let root = CString::new(repo_path.as_os_str().as_bytes())
        .map_err(|error| BackendError::new("invalid_path", error))?;
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY,
        )
    };
    if root_fd < 0 {
        return Err(BackendError::new(
            "file_read",
            std::io::Error::last_os_error(),
        ));
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    let components: Vec<_> = relative_path.components().collect();
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_os_str().as_bytes())
            .map_err(|error| BackendError::new("invalid_path", error))?;
        let is_last = index + 1 == components.len();
        if is_last {
            let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    &mut metadata,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } < 0
            {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(BackendError::new("file_read", error));
            }
            if metadata.st_mode & libc::S_IFMT == libc::S_IFLNK {
                let mut target = vec![0_u8; 256];
                loop {
                    let length = unsafe {
                        libc::readlinkat(
                            directory.as_raw_fd(),
                            name.as_ptr(),
                            target.as_mut_ptr().cast(),
                            target.len(),
                        )
                    };
                    if length < 0 {
                        return Err(BackendError::new(
                            "file_read",
                            std::io::Error::last_os_error(),
                        ));
                    }
                    let length = length as usize;
                    if length < target.len() {
                        target.truncate(length);
                        return Ok(Some(target));
                    }
                    target.resize(target.len() * 2, 0);
                }
            }
            if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Ok(None);
            }
        }

        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if is_last { 0 } else { libc::O_DIRECTORY };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(BackendError::new("unsafe_path", error));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if is_last {
            let mut file = fs::File::from(opened);
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| BackendError::new("file_read", error))?;
            return Ok(Some(bytes));
        }
        directory = opened;
    }
    Ok(None)
}

#[cfg(not(unix))]
fn read_worktree_file(repo_path: &Path, path: &str) -> Result<Option<Vec<u8>>> {
    let relative_path = validate_path(path)?;
    let full_path = repo_path.join(relative_path);
    let mut current = repo_path.to_owned();
    let mut components = full_path
        .strip_prefix(repo_path)
        .map_err(BackendError::git)?
        .components()
        .peekable();
    while let Some(component) = components.next() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BackendError::new("file_read", error)),
        };
        if metadata.file_type().is_symlink() {
            if components.peek().is_some() {
                return Err(BackendError::new(
                    "unsafe_path",
                    "File paths cannot traverse a symbolic link",
                ));
            }
            let target =
                fs::read_link(&current).map_err(|error| BackendError::new("file_read", error))?;
            return Ok(Some(target.as_os_str().as_encoded_bytes().to_vec()));
        }
        if components.peek().is_none() {
            if !metadata.is_file() {
                return Ok(None);
            }
            return fs::read(&current)
                .map(Some)
                .map_err(|error| BackendError::new("file_read", error));
        }
        if !metadata.is_dir() {
            return Ok(None);
        }
    }
    Ok(None)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(BINARY_SAMPLE_BYTES)
        .any(|byte| *byte == 0)
}

fn decoded_lines(bytes: &[u8]) -> Vec<String> {
    let decoded = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let mut lines: Vec<_> = decoded.split('\n').map(str::to_owned).collect();
    if decoded.ends_with('\n') {
        lines.pop();
    }
    if decoded.is_empty() {
        lines.clear();
    }
    lines
}

fn merge_lines(
    old: &[String],
    new: &[String],
    input: &InternedInput<&str>,
    diff: &Diff,
) -> (Vec<FileDiffLine>, Vec<FileDiffHunk>) {
    let mut lines = Vec::new();
    let mut hunks = Vec::new();
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    for hunk in diff.hunks() {
        let old_start = hunk.before.start as usize;
        let new_start = hunk.after.start as usize;
        while old_cursor < old_start {
            lines.push(FileDiffLine {
                kind: "context",
                old_no: Some(old_cursor + 1),
                new_no: Some(new_cursor + 1),
                text: input.interner[input.before[old_cursor]].to_owned(),
            });
            old_cursor += 1;
            new_cursor += 1;
        }
        let start_index = lines.len();
        for (index, text) in old
            .iter()
            .enumerate()
            .take(hunk.before.end as usize)
            .skip(old_start)
        {
            lines.push(FileDiffLine {
                kind: "removed",
                old_no: Some(index + 1),
                new_no: None,
                text: text.clone(),
            });
        }
        for (index, text) in new
            .iter()
            .enumerate()
            .take(hunk.after.end as usize)
            .skip(new_start)
        {
            lines.push(FileDiffLine {
                kind: "added",
                old_no: None,
                new_no: Some(index + 1),
                text: text.clone(),
            });
        }
        if lines.len() > start_index {
            hunks.push(FileDiffHunk {
                start_index,
                end_index: lines.len(),
            });
        }
        old_cursor = hunk.before.end as usize;
        new_cursor = hunk.after.end as usize;
    }
    while old_cursor < old.len() && new_cursor < new.len() {
        lines.push(FileDiffLine {
            kind: "context",
            old_no: Some(old_cursor + 1),
            new_no: Some(new_cursor + 1),
            text: old[old_cursor].clone(),
        });
        old_cursor += 1;
        new_cursor += 1;
    }
    (lines, hunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Command, time::Instant};
    use tempfile::TempDir;

    fn diff(old: &str, new: &str) -> Vec<(String, &'static str)> {
        let old_lines = decoded_lines(old.as_bytes());
        let new_lines = decoded_lines(new.as_bytes());
        let input = InternedInput::new(LineTokens(&old_lines), LineTokens(&new_lines));
        let edits = Diff::compute(Algorithm::Histogram, &input);
        merge_lines(&old_lines, &new_lines, &input, &edits)
            .0
            .into_iter()
            .map(|line| (line.text, line.kind))
            .collect()
    }

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn init_repo() -> TempDir {
        let temp = TempDir::new().expect("temporary repository");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.name", "File diff test"]);
        git(
            temp.path(),
            &["config", "user.email", "file-diff@example.invalid"],
        );
        temp
    }

    fn commit(path: &Path, message: &str) -> String {
        git(path, &["add", "-A"]);
        git(path, &["commit", "--quiet", "-m", message]);
        git(path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn full_file_merge_handles_insertions_deletions_and_rewrites() {
        assert_eq!(
            diff("a\n", "a\nb\n"),
            [("a".into(), "context"), ("b".into(), "added")]
        );
        assert_eq!(
            diff("a\nb\n", "a\n"),
            [("a".into(), "context"), ("b".into(), "removed")]
        );
        assert_eq!(
            diff("first\nold\nlast\n", "first\nnew\nlast\n"),
            [
                ("first".into(), "context"),
                ("old".into(), "removed"),
                ("new".into(), "added"),
                ("last".into(), "context")
            ]
        );
        assert_eq!(
            diff("old\n", "new\n"),
            [("old".into(), "removed"), ("new".into(), "added")]
        );
        assert_eq!(diff("", "one\n"), [("one".into(), "added")]);
        assert_eq!(diff("one\n", ""), [("one".into(), "removed")]);
        assert_eq!(
            diff("old\nmiddle\nlast\n", "new\nmiddle\nfinal\n"),
            [
                ("old".into(), "removed"),
                ("new".into(), "added"),
                ("middle".into(), "context"),
                ("last".into(), "removed"),
                ("final".into(), "added")
            ]
        );
    }

    #[test]
    fn line_normalization_keeps_crlf_and_eof_metadata_separate() {
        assert_eq!(decoded_lines(b"one\r\ntwo\r\n"), ["one", "two"]);
        assert_eq!(diff("one", "one\n"), [("one".into(), "context")]);
    }

    #[test]
    fn binary_detection_samples_only_the_first_eight_kibibytes() {
        assert!(is_binary(b"first\0line"));
        let mut bytes = vec![b'a'; BINARY_SAMPLE_BYTES];
        bytes.push(0);
        assert!(!is_binary(&bytes));
    }

    #[test]
    fn path_validation_rejects_absolute_and_parent_paths() {
        assert!(validate_path("src/main.rs").is_ok());
        assert!(validate_path("../outside").is_err());
        assert!(validate_path("/etc/passwd").is_err());
        assert!(validate_path(".git/config").is_err());
    }

    #[test]
    fn staged_and_unstaged_sources_use_different_file_contents() {
        let temp = init_repo();
        let path = temp.path().join("file.txt");
        fs::write(&path, "base\n").unwrap();
        commit(temp.path(), "base");
        fs::write(&path, "staged\n").unwrap();
        git(temp.path(), &["add", "file.txt"]);
        fs::write(&path, "unstaged\n").unwrap();

        let staged = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeStaged,
            "file.txt",
            None,
            false,
        )
        .unwrap();
        let unstaged = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "file.txt",
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            staged
                .lines
                .iter()
                .find(|line| line.kind == "added")
                .unwrap()
                .text,
            "staged"
        );
        assert_eq!(
            unstaged
                .lines
                .iter()
                .find(|line| line.kind == "added")
                .unwrap()
                .text,
            "unstaged"
        );
    }

    #[test]
    fn root_commit_and_renamed_modified_file_diffs_work() {
        let temp = init_repo();
        fs::write(temp.path().join("root.txt"), "root\nsecond\n").unwrap();
        let root = commit(temp.path(), "root");
        let root_diff = read_file_diff(
            temp.path(),
            FileDiffSource::Commit { oid: root },
            "root.txt",
            None,
            false,
        )
        .unwrap();
        assert_eq!(root_diff.stats.added, 2);

        fs::rename(
            temp.path().join("root.txt"),
            temp.path().join("renamed.txt"),
        )
        .unwrap();
        fs::write(temp.path().join("renamed.txt"), "changed\nsecond\n").unwrap();
        let renamed = commit(temp.path(), "rename and modify");
        let rename_diff = read_file_diff(
            temp.path(),
            FileDiffSource::Commit { oid: renamed },
            "renamed.txt",
            Some("root.txt"),
            false,
        )
        .unwrap();
        assert_eq!(rename_diff.old_path.as_deref(), Some("root.txt"));
        assert!(
            rename_diff
                .lines
                .iter()
                .any(|line| line.kind == "removed" && line.text == "root")
        );
        assert!(
            rename_diff
                .lines
                .iter()
                .any(|line| line.kind == "added" && line.text == "changed")
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlinks_are_shown_as_their_target_without_following_them() {
        use std::os::unix::fs::symlink;

        let temp = init_repo();
        symlink("../outside-secret", temp.path().join("link")).unwrap();
        let result = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "link",
            None,
            false,
        )
        .unwrap();
        assert_eq!(result.lines[0].text, "../outside-secret");
    }

    #[test]
    fn binary_and_large_files_return_bounded_responses() {
        let temp = init_repo();
        fs::write(temp.path().join("binary"), b"text\0data").unwrap();
        let binary = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "binary",
            None,
            false,
        )
        .unwrap();
        assert!(binary.binary);
        assert!(binary.lines.is_empty());

        let contents = (0..=MAX_LINES)
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        fs::write(temp.path().join("large"), contents).unwrap();
        let large = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "large",
            None,
            false,
        )
        .unwrap();
        assert!(large.too_large);
        assert!(large.lines.is_empty());
        assert_eq!(large.stats.added, MAX_LINES + 1);
        let forced = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "large",
            None,
            true,
        )
        .unwrap();
        assert!(!forced.too_large);
        assert_eq!(forced.lines.len(), MAX_LINES + 1);

        fs::write(temp.path().join("wide"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        let wide = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "wide",
            None,
            false,
        )
        .unwrap();
        assert!(wide.too_large);
        assert!(wide.lines.is_empty());
        assert_eq!(wide.stats.added, 1);
    }

    #[test]
    fn crlf_eof_flags_and_external_diff_configuration_are_safe() {
        let temp = init_repo();
        let path = temp.path().join("file.txt");
        fs::write(&path, "same\r\n").unwrap();
        commit(temp.path(), "initial");
        fs::write(&path, "same").unwrap();
        let no_newline = read_file_diff(
            temp.path(),
            FileDiffSource::WorktreeUnstaged,
            "file.txt",
            None,
            false,
        )
        .unwrap();
        assert!(!no_newline.old_no_newline);
        assert!(no_newline.new_no_newline);
        assert_eq!(no_newline.lines.len(), 1);
        assert_eq!(no_newline.lines[0].kind, "context");

        let marker = temp.path().join("textconv-ran");
        fs::write(temp.path().join(".gitattributes"), "file.txt diff=marker\n").unwrap();
        let marker_command = format!("sh -c 'touch {}'", marker.display());
        git(
            temp.path(),
            &["config", "diff.marker.textconv", &marker_command],
        );
        git(
            temp.path(),
            &["config", "diff.marker.command", &marker_command],
        );
        fs::write(&path, "updated\n").unwrap();
        let commit_id = commit(temp.path(), "with custom diff driver");
        let result = read_file_diff(
            temp.path(),
            FileDiffSource::Commit { oid: commit_id },
            "file.txt",
            None,
            false,
        )
        .unwrap();
        assert_eq!(result.stats.added, 1);
        assert!(!marker.exists());
    }

    #[test]
    fn twenty_thousand_lines_with_scattered_edits_are_measured() {
        let old: Vec<_> = (0..20_000).map(|line| format!("line {line}")).collect();
        let mut new = old.clone();
        for line in (0..20_000).step_by(100) {
            new[line] = format!("changed {line}");
        }
        let started = Instant::now();
        let input = InternedInput::new(LineTokens(&old), LineTokens(&new));
        let diff = Diff::compute(Algorithm::Histogram, &input);
        let _ = merge_lines(&old, &new, &input, &diff);
        println!(
            "20k-line file diff with 200 scattered edits: {} ms",
            started.elapsed().as_millis()
        );
    }
}
