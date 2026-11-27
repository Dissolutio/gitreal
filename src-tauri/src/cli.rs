use crate::model::{BackendError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum CliAction {
    Show,
    Open(Option<PathBuf>),
    Toggle,
    Hidden,
}

pub fn parse_args(args: &[String]) -> Result<CliAction> {
    match args {
        [] => Ok(CliAction::Show),
        [command] if command == "--toggle" => Ok(CliAction::Toggle),
        [command] if command == "--hidden" => Ok(CliAction::Hidden),
        [command] if command == "open" => Ok(CliAction::Open(None)),
        [command, path] if command == "open" => Ok(CliAction::Open(Some(PathBuf::from(path)))),
        [argument] if argument.starts_with('-') => Err(BackendError::new(
            "invalid_arguments",
            format!("Unknown Gitreal option: {argument}"),
        )),
        _ => Err(BackendError::new(
            "invalid_arguments",
            "Usage: gitreal [--toggle|--hidden|open [PATH]]",
        )),
    }
}

pub fn parse_argv(argv: &[String]) -> Result<CliAction> {
    let args = argv
        .first()
        .filter(|argument| {
            Path::new(argument)
                .file_name()
                .is_some_and(|name| name == "gitreal" || name == "gitreal.exe")
        })
        .map_or(argv, |_| &argv[1..]);
    parse_args(args)
}

pub fn resolve_path(path: Option<&Path>, cwd: &Path) -> PathBuf {
    path.map_or_else(
        || cwd.to_path_buf(),
        |path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            }
        },
    )
}

pub fn discover_worktree(path: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let requested = resolve_path(path, cwd);
    let repository = gix::discover(&requested).map_err(|_| {
        BackendError::new(
            "not_repository",
            format!("{} is not inside a Git working tree", requested.display()),
        )
    })?;
    let worktree = repository.workdir().ok_or_else(|| {
        BackendError::new(
            "not_repository",
            format!(
                "{} is a bare repository, not a Git working tree",
                requested.display()
            ),
        )
    })?;
    worktree
        .canonicalize()
        .map_err(|error| BackendError::new("invalid_path", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Command, str::FromStr};

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_supported_cli_forms() {
        assert_eq!(parse_args(&[]).unwrap(), CliAction::Show);
        assert_eq!(
            parse_args(&strings(&["open"])).unwrap(),
            CliAction::Open(None)
        );
        assert_eq!(
            parse_args(&strings(&["open", "repo"])).unwrap(),
            CliAction::Open(Some(PathBuf::from("repo")))
        );
        assert_eq!(
            parse_args(&strings(&["--toggle"])).unwrap(),
            CliAction::Toggle
        );
        assert_eq!(
            parse_args(&strings(&["--hidden"])).unwrap(),
            CliAction::Hidden
        );
    }

    #[test]
    fn parses_forwarded_argv_and_resolves_relative_paths_from_caller_cwd() {
        let cwd = PathBuf::from_str("/caller/work").unwrap();
        let action = parse_argv(&strings(&["/usr/bin/gitreal", "open", "nested/repo"])).unwrap();
        let CliAction::Open(Some(path)) = action else {
            panic!("expected an open request");
        };
        assert_eq!(resolve_path(Some(&path), &cwd), cwd.join("nested/repo"));
    }

    #[test]
    fn open_from_nested_directory_resolves_to_worktree_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let nested = root.join("src/nested");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            discover_worktree(Some(Path::new(".")), &nested).unwrap(),
            root
        );
    }

    #[test]
    fn open_outside_a_repository_returns_a_clear_error() {
        let directory = tempfile::tempdir().unwrap();
        let error = discover_worktree(None, directory.path()).unwrap_err();
        assert_eq!(error.code, "not_repository");
        assert!(error.message.contains("not inside a Git working tree"));
    }

    #[test]
    fn invalid_cli_arguments_are_reported() {
        assert!(
            parse_args(&strings(&["--unknown"]))
                .unwrap_err()
                .message
                .contains("Unknown Gitreal option")
        );
        assert!(parse_args(&strings(&["open", "one", "two"])).is_err());
    }
}
