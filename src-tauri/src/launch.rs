use crate::model::{BackendError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
};

const NO_TERMINAL: &str = "No terminal found — set launch_terminal in config";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LaunchSuggestion {
    pub command: String,
    pub reason: String,
}

#[derive(Default)]
struct LaunchFiles {
    package_json: Option<String>,
    pnpm_lock: bool,
    yarn_lock: bool,
    bun_lock: bool,
    package_lock: bool,
    cargo_toml: bool,
    makefile: Option<String>,
    compose: bool,
}

impl LaunchFiles {
    fn read(root: &Path) -> Result<Self> {
        fn optional(root: &Path, name: &str) -> Result<Option<String>> {
            match fs::read_to_string(root.join(name)) {
                Ok(contents) => Ok(Some(contents)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(BackendError::new("launch_error", error)),
            }
        }

        Ok(Self {
            package_json: optional(root, "package.json")?,
            pnpm_lock: root.join("pnpm-lock.yaml").is_file(),
            yarn_lock: root.join("yarn.lock").is_file(),
            bun_lock: root.join("bun.lockb").is_file() || root.join("bun.lock").is_file(),
            package_lock: root.join("package-lock.json").is_file(),
            cargo_toml: root.join("Cargo.toml").is_file(),
            makefile: optional(root, "Makefile")?,
            compose: root.join("docker-compose.yml").is_file()
                || root.join("compose.yaml").is_file(),
        })
    }
}

fn detect_suggestion(files: &LaunchFiles) -> Option<LaunchSuggestion> {
    let package = files
        .package_json
        .as_deref()
        .and_then(|contents| serde_json::from_str::<Value>(contents).ok());

    if let Some(package) = &package {
        let package_manager = package
            .get("packageManager")
            .and_then(Value::as_str)
            .and_then(package_manager_name);
        let (manager, manager_reason) = package_manager
            .map(|manager| {
                let raw = package["packageManager"].as_str().unwrap_or_default();
                (manager, format!("package.json packageManager '{raw}'"))
            })
            .unwrap_or_else(|| {
                let (manager, reason) = if files.pnpm_lock {
                    ("pnpm", "pnpm-lock.yaml")
                } else if files.yarn_lock {
                    ("yarn", "yarn.lock")
                } else if files.bun_lock {
                    ("bun", "bun.lock/bun.lockb")
                } else if files.package_lock {
                    ("npm", "package-lock.json")
                } else {
                    ("npm", "default npm")
                };
                (manager, reason.to_owned())
            });

        let scripts = package.get("scripts");
        let tauri_script = scripts
            .and_then(|scripts| scripts.get("tauri"))
            .and_then(Value::as_str)
            .is_some();
        let tauri_dependency = ["dependencies", "devDependencies"].iter().any(|section| {
            package
                .get(section)
                .and_then(|dependencies| dependencies.get("@tauri-apps/cli"))
                .is_some()
        });
        if tauri_script || tauri_dependency {
            let detail = if tauri_script {
                "package.json script 'tauri'"
            } else {
                "package.json dependency '@tauri-apps/cli'"
            };
            return Some(LaunchSuggestion {
                command: format!("{manager} tauri dev"),
                reason: format!("{manager_reason} + {detail}"),
            });
        }

        for script in ["dev", "start", "serve"] {
            if scripts
                .and_then(|scripts| scripts.get(script))
                .and_then(Value::as_str)
                .is_some()
            {
                return Some(LaunchSuggestion {
                    command: format!("{manager} run {script}"),
                    reason: format!("{manager_reason} + package.json script '{script}'"),
                });
            }
        }
    }

    if files.cargo_toml {
        return Some(LaunchSuggestion {
            command: "cargo run".into(),
            reason: "Cargo.toml".into(),
        });
    }

    if let Some(makefile) = &files.makefile {
        for target in ["dev", "run"] {
            if makefile.lines().any(|line| {
                line.split_once(':')
                    .is_some_and(|(name, _)| name.trim() == target)
            }) {
                return Some(LaunchSuggestion {
                    command: format!("make {target}"),
                    reason: format!("Makefile target '{target}'"),
                });
            }
        }
    }

    files.compose.then(|| LaunchSuggestion {
        command: "docker compose up".into(),
        reason: "docker-compose.yml/compose.yaml".into(),
    })
}

fn package_manager_name(value: &str) -> Option<&'static str> {
    match value.split('@').next()? {
        "pnpm" => Some("pnpm"),
        "yarn" => Some("yarn"),
        "bun" => Some("bun"),
        "npm" => Some("npm"),
        _ => None,
    }
}

pub fn detect_at(root: &Path) -> Result<Option<LaunchSuggestion>> {
    Ok(detect_suggestion(&LaunchFiles::read(root)?))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn terminal_argv(terminal: &str, shell: &str, command: &str, cwd: &Path) -> Vec<OsString> {
    let executable = OsString::from(shell);
    let script = format!("{command}; exec {}", shell_quote(shell));
    let directory = cwd.as_os_str().to_os_string();
    let terminal_name = Path::new(terminal)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(terminal);
    let mut args = Vec::new();
    match terminal_name {
        "cosmic-term" => args.push("--".into()),
        "gnome-terminal" => {
            args.push(format!("--working-directory={}", cwd.display()).into());
            args.push("--".into());
        }
        "konsole" => {
            args.push("--workdir".into());
            args.push(directory);
            args.push("-e".into());
        }
        "kitty" => {
            args.push("--directory".into());
            args.push(directory);
        }
        "alacritty" => {
            args.push("--working-directory".into());
            args.push(directory);
            args.push("-e".into());
        }
        "x-terminal-emulator" | "xterm" => args.push("-e".into()),
        _ => args.push("-e".into()),
    }
    args.push(executable);
    args.push("-lc".into());
    args.push(script.into());
    args
}

#[derive(Clone, Debug)]
struct Terminal {
    name: String,
    executable: PathBuf,
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
        && fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

fn resolve_executable(name: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return is_executable(path).then(|| path.to_path_buf());
    }
    env::split_paths(search_path?)
        .map(|directory| directory.join(name))
        .find(|path| is_executable(path))
}

fn select_terminal(configured: Option<&str>, search_path: Option<&OsStr>) -> Result<Terminal> {
    let candidates: Vec<&str> = configured.map_or_else(
        || {
            vec![
                "cosmic-term",
                "x-terminal-emulator",
                "gnome-terminal",
                "konsole",
                "kitty",
                "alacritty",
                "xterm",
            ]
        },
        |configured| vec![configured],
    );
    for candidate in candidates {
        if let Some(executable) = resolve_executable(candidate, search_path) {
            let name = Path::new(candidate)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or(candidate)
                .to_owned();
            return Ok(Terminal { name, executable });
        }
    }
    Err(BackendError::new("no_terminal", NO_TERMINAL))
}

fn start_terminal(
    cwd: &Path,
    command: &str,
    configured_terminal: Option<&str>,
    search_path: Option<&OsStr>,
    shell: &str,
) -> Result<(String, u32)> {
    if command.trim().is_empty() || command.contains('\0') {
        return Err(BackendError::new(
            "launch_error",
            "Enter a non-empty launch command",
        ));
    }
    let terminal = select_terminal(configured_terminal, search_path)?;
    let args = terminal_argv(&terminal.name, shell, command, cwd);
    let child = Command::new(terminal.executable)
        .args(args)
        .current_dir(cwd)
        .spawn()
        .map_err(|error| BackendError::new("launch_error", error))?;
    let pid = child.id();
    reap_child(child);
    Ok((terminal.name, pid))
}

fn reap_child(child: Child) {
    let child = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
    let reaper = std::sync::Arc::clone(&child);
    if thread::Builder::new()
        .name("gitreal-terminal-reaper".into())
        .spawn(move || {
            if let Some(mut child) = reaper.lock().ok().and_then(|mut child| child.take()) {
                let _ = child.wait();
            }
        })
        .is_err()
        && let Some(mut child) = child.lock().ok().and_then(|mut child| child.take())
    {
        let _ = child.wait();
    }
}

pub fn launch_terminal(
    cwd: &Path,
    command: &str,
    configured_terminal: Option<&str>,
) -> Result<String> {
    let path = env::var_os("PATH");
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    start_terminal(cwd, command, configured_terminal, path.as_deref(), &shell).map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    fn suggestion(contents: &[(&str, &str)]) -> Option<LaunchSuggestion> {
        let directory = tempdir().unwrap();
        for (name, value) in contents {
            fs::write(directory.path().join(name), value).unwrap();
        }
        detect_at(directory.path()).unwrap()
    }

    #[test]
    fn detects_package_manager_commands_and_prioritizes_tauri() {
        assert_eq!(
            suggestion(&[
                (
                    "package.json",
                    r#"{"scripts":{"tauri":"tauri"},"dependencies":{}}"#
                ),
                ("pnpm-lock.yaml", ""),
            ]),
            Some(LaunchSuggestion {
                command: "pnpm tauri dev".into(),
                reason: "pnpm-lock.yaml + package.json script 'tauri'".into(),
            })
        );
        assert_eq!(
            suggestion(&[
                ("package.json", r#"{"scripts":{"dev":"vite"}}"#),
                ("package-lock.json", "")
            ])
            .unwrap()
            .command,
            "npm run dev"
        );
        assert_eq!(
            suggestion(&[
                ("package.json", r#"{"scripts":{"start":"vite"}}"#),
                ("yarn.lock", "")
            ])
            .unwrap()
            .command,
            "yarn run start"
        );
        assert_eq!(
            suggestion(&[
                ("package.json", r#"{"scripts":{"serve":"vite"}}"#),
                ("bun.lock", "")
            ])
            .unwrap()
            .command,
            "bun run serve"
        );
    }

    #[test]
    fn package_manager_field_overrides_lockfile_and_tauri_dependency_is_detected() {
        assert_eq!(
            suggestion(&[
                (
                    "package.json",
                    r#"{"packageManager":"yarn@4.1.0","scripts":{"dev":"vite"}}"#
                ),
                ("pnpm-lock.yaml", ""),
            ])
            .unwrap()
            .command,
            "yarn run dev"
        );
        assert_eq!(
            suggestion(&[(
                "package.json",
                r#"{"devDependencies":{"@tauri-apps/cli":"2.0.0"}}"#,
            )])
            .unwrap()
            .command,
            "npm tauri dev"
        );
    }

    #[test]
    fn detects_cargo_make_compose_and_no_suggestion() {
        assert_eq!(
            suggestion(&[("Cargo.toml", "[package]")]).unwrap().command,
            "cargo run"
        );
        assert_eq!(
            suggestion(&[("Makefile", ".PHONY: dev\ndev:\n\tvite\nrun:\n\tnode .\n")])
                .unwrap()
                .command,
            "make dev"
        );
        assert_eq!(
            suggestion(&[("compose.yaml", "services:")])
                .unwrap()
                .command,
            "docker compose up"
        );
        assert_eq!(suggestion(&[("README.md", "nothing to launch")]), None);
    }

    #[test]
    fn builds_argv_for_each_supported_terminal_and_quotes_shell_script() {
        let cwd = Path::new("/repo with spaces");
        let expected = "echo \"hello world\"; exec '/bin/my'\\''shell'";
        for terminal in [
            "cosmic-term",
            "x-terminal-emulator",
            "gnome-terminal",
            "konsole",
            "kitty",
            "alacritty",
            "xterm",
        ] {
            let argv = terminal_argv(terminal, "/bin/my'shell", "echo \"hello world\"", cwd);
            assert!(
                argv.iter().any(|argument| argument == expected),
                "{terminal}"
            );
            assert!(argv.iter().any(|argument| argument == "/bin/my'shell"));
        }

        let gnome = terminal_argv("gnome-terminal", "/bin/bash", "pnpm tauri dev", cwd);
        assert_eq!(gnome[0], "--working-directory=/repo with spaces");
        assert_eq!(gnome[1], "--");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launch_uses_terminal_argv_cwd_and_reaps_the_child() {
        let directory = tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let terminal = bin.join("xterm");
        let capture = directory.path().join("captured.txt");
        fs::write(
            &terminal,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" > {}\nfor arg do printf '%s\\n' \"$arg\" >> {}; done\nsleep 0.1\n",
                shell_quote(&capture.to_string_lossy()),
                shell_quote(&capture.to_string_lossy()),
            ),
        )
        .unwrap();
        fs::set_permissions(&terminal, fs::Permissions::from_mode(0o755)).unwrap();
        let repo = directory.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let (name, pid) = start_terminal(
            &repo,
            "echo 'hello world'",
            Some("xterm"),
            Some(bin.as_os_str()),
            "/bin/bash",
        )
        .unwrap();
        assert_eq!(name, "xterm");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !capture.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let output = fs::read_to_string(&capture).unwrap();
        let arguments: Vec<_> = output.lines().collect();
        assert_eq!(arguments[0], repo.to_string_lossy());
        assert_eq!(arguments[1], "-e");
        assert_eq!(arguments[2], "/bin/bash");
        assert_eq!(arguments[3], "-lc");
        assert_eq!(arguments[4], "echo 'hello world'; exec '/bin/bash'");

        let process = PathBuf::from(format!("/proc/{pid}"));
        while process.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!process.exists(), "terminal child was not reaped");
    }
}
