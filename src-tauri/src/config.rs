use crate::model::{BackendError, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const CONFIG_VERSION: u32 = 1;
const MAX_KNOWN_REPOS: usize = 50;
const MAX_TABS: usize = 16;
static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

fn default_check_for_updates() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct KnownRepo {
    pub path: String,
    pub name: String,
    pub last_opened: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct OpenTab {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tab: Option<String>,
    #[serde(default)]
    pub start_at_login: bool,
    #[serde(default = "default_check_for_updates")]
    pub check_for_updates: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub launch_commands: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_terminal: Option<String>,
    pub repos: Vec<KnownRepo>,
    pub tabs: Vec<OpenTab>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            active_tab: None,
            start_at_login: false,
            check_for_updates: true,
            launch_commands: HashMap::new(),
            launch_terminal: None,
            repos: Vec::new(),
            tabs: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct ConfigManager {
    gate: Mutex<()>,
}

impl ConfigManager {
    pub fn get_config(&self) -> Result<AppConfig> {
        let _guard = self.lock()?;
        load_config(&config_path()?)
    }

    pub fn set_open_tabs(&self, paths: Vec<String>, active: Option<String>) -> Result<()> {
        let _guard = self.lock()?;
        let path = config_path()?;
        let mut config = load_config(&path)?;
        config.tabs = normalized_tabs(paths)?;
        config.active_tab = active
            .map(|path| canonicalize_or_preserve(&path))
            .transpose()?
            .filter(|active| config.tabs.iter().any(|tab| &tab.path == active));
        save_config(&path, &config)
    }

    pub fn add_known_repo(&self, path: String) -> Result<()> {
        let _guard = self.lock()?;
        let config_path = config_path()?;
        let mut config = load_config(&config_path)?;
        add_known_repo(&mut config, Path::new(&path), unix_time())?;
        save_config(&config_path, &config)
    }

    pub fn remove_known_repo(&self, path: String) -> Result<()> {
        let _guard = self.lock()?;
        let config_path = config_path()?;
        let mut config = load_config(&config_path)?;
        let path = canonicalize_or_preserve(&path)?;
        config.repos.retain(|repo| repo.path != path);
        save_config(&config_path, &config)
    }

    pub fn set_launch_command(&self, repo_path: String, command: String) -> Result<()> {
        if command.trim().is_empty() || command.contains('\0') {
            return Err(BackendError::new(
                "launch_error",
                "Enter a non-empty launch command",
            ));
        }
        let _guard = self.lock()?;
        let path = config_path()?;
        let mut config = load_config(&path)?;
        config
            .launch_commands
            .insert(canonicalize_or_preserve(&repo_path)?, command);
        save_config(&path, &config)
    }

    pub fn reset_launch_command(&self, repo_path: String) -> Result<()> {
        let _guard = self.lock()?;
        let path = config_path()?;
        let mut config = load_config(&path)?;
        remove_launch_command(&mut config, &repo_path)?;
        save_config(&path, &config)
    }

    pub fn set_start_at_login(&self, enabled: bool) -> Result<()> {
        let _guard = self.lock()?;
        let path = config_path()?;
        let autostart_path = autostart_path()?;
        let mut config = load_config(&path)?;
        let executable = if enabled {
            Some(autostart_executable()?)
        } else {
            None
        };
        set_autostart_file(&autostart_path, enabled, executable.as_deref())?;
        config.start_at_login = enabled;
        save_config(&path, &config)
    }

    pub fn set_check_for_updates(&self, enabled: bool) -> Result<()> {
        let _guard = self.lock()?;
        let path = config_path()?;
        let mut config = load_config(&path)?;
        config.check_for_updates = enabled;
        save_config(&path, &config)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.gate
            .lock()
            .map_err(|_| BackendError::new("internal_error", "Configuration state unavailable"))
    }
}

fn config_path() -> Result<PathBuf> {
    config_path_from(
        std::env::var_os("XDG_CONFIG_HOME")
            .as_deref()
            .map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn config_path_from(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = xdg_config_home.filter(|path| path.is_absolute()) {
        return Ok(path.join("gitreal").join("config.toml"));
    }
    let home = home.ok_or_else(|| {
        BackendError::new(
            "config_error",
            "Neither XDG_CONFIG_HOME nor HOME is available",
        )
    })?;
    Ok(home.join(".config").join("gitreal").join("config.toml"))
}

fn autostart_path() -> Result<PathBuf> {
    autostart_path_from(
        std::env::var_os("XDG_CONFIG_HOME")
            .as_deref()
            .map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn autostart_path_from(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    let config_dir = if let Some(path) = xdg_config_home.filter(|path| path.is_absolute()) {
        path.to_path_buf()
    } else {
        let home = home.ok_or_else(|| {
            BackendError::new(
                "config_error",
                "Neither XDG_CONFIG_HOME nor HOME is available",
            )
        })?;
        home.join(".config")
    };
    Ok(config_dir.join("autostart").join("gitreal.desktop"))
}

fn autostart_executable() -> Result<PathBuf> {
    let appimage = std::env::var_os("APPIMAGE");
    let current_exe = if appimage.as_deref().is_none_or(|path| path.is_empty()) {
        Some(std::env::current_exe().map_err(|error| BackendError::new("config_error", error))?)
    } else {
        None
    };
    let current_dir =
        std::env::current_dir().map_err(|error| BackendError::new("config_error", error))?;
    autostart_executable_from(appimage.as_deref(), current_exe.as_deref(), &current_dir)
}

fn autostart_executable_from(
    appimage: Option<&std::ffi::OsStr>,
    current_exe: Option<&Path>,
    current_dir: &Path,
) -> Result<PathBuf> {
    let executable = appimage
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| current_exe.map(Path::to_path_buf))
        .ok_or_else(|| BackendError::new("config_error", "Could not determine executable path"))?;
    Ok(if executable.is_absolute() {
        executable
    } else {
        current_dir.join(executable)
    })
}

fn exec_line(executable: &Path) -> String {
    let path = executable.to_string_lossy();
    let quoted = path.chars().fold(String::new(), |mut output, character| {
        match character {
            '\\' | '"' | '$' | '`' => {
                output.push('\\');
                output.push(character);
            }
            '%' => output.push_str("%%"),
            _ => output.push(character),
        }
        output
    });
    format!("Exec=\"{quoted}\" --hidden\n")
}

fn set_autostart_file(path: &Path, enabled: bool, executable: Option<&Path>) -> Result<()> {
    if enabled {
        let executable = executable.ok_or_else(|| {
            BackendError::new("config_error", "Could not determine executable path")
        })?;
        let parent = path.parent().ok_or_else(|| {
            BackendError::new("config_error", "Autostart path has no parent directory")
        })?;
        fs::create_dir_all(parent).map_err(|error| BackendError::new("config_error", error))?;
        fs::write(
            path,
            format!(
                "[Desktop Entry]\nType=Application\nName=Gitreal\n{}X-GNOME-Autostart-enabled=true\n",
                exec_line(executable)
            ),
        )
        .map_err(|error| BackendError::new("config_error", error))
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(BackendError::new("config_error", error)),
        }
    }
}

fn load_config(path: &Path) -> Result<AppConfig> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppConfig::default());
        }
        Err(error) => return Err(BackendError::new("config_error", error)),
    };
    match toml::from_str::<AppConfig>(&contents) {
        Ok(mut config) if config.version == CONFIG_VERSION => {
            config.repos.truncate(MAX_KNOWN_REPOS);
            config.tabs.truncate(MAX_TABS);
            deduplicate_repos(&mut config);
            normalize_loaded_tabs(&mut config);
            Ok(config)
        }
        _ => {
            backup_corrupt(path);
            Ok(AppConfig::default())
        }
    }
}

fn save_config(path: &Path, config: &AppConfig) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        BackendError::new("config_error", "Configuration path has no parent directory")
    })?;
    fs::create_dir_all(parent).map_err(|error| BackendError::new("config_error", error))?;
    let contents =
        toml::to_string_pretty(config).map_err(|error| BackendError::new("config_error", error))?;
    let (temporary_path, mut file) = loop {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_extension(format!("toml.tmp-{}-{id}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(BackendError::new("config_error", error)),
        }
    };
    let result = (|| {
        file.write_all(contents.as_bytes())
            .map_err(|error| BackendError::new("config_error", error))?;
        file.sync_all()
            .map_err(|error| BackendError::new("config_error", error))?;
        fs::rename(&temporary_path, path).map_err(|error| BackendError::new("config_error", error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary_path);
    }
    result
}

fn backup_corrupt(path: &Path) {
    let backup = path.with_extension("toml.bak");
    let _ = fs::remove_file(&backup);
    if fs::rename(path, &backup).is_err() {
        let _ = fs::copy(path, backup);
    }
}

fn normalized_tabs(paths: Vec<String>) -> Result<Vec<OpenTab>> {
    let mut seen = HashSet::new();
    let mut tabs = Vec::new();
    for path in paths {
        let path = canonicalize_or_preserve(&path)?;
        if seen.insert(path.clone()) {
            tabs.push(OpenTab { path });
        }
        if tabs.len() == MAX_TABS {
            break;
        }
    }
    Ok(tabs)
}

fn canonicalize_or_preserve(path: &str) -> Result<String> {
    match Path::new(path).canonicalize() {
        Ok(path) => Ok(path.to_string_lossy().into_owned()),
        Err(_) if Path::new(path).is_absolute() => Ok(path.to_owned()),
        Err(error) => Err(BackendError::new("invalid_path", error)),
    }
}

fn deduplicate_repos(config: &mut AppConfig) {
    let mut seen = HashSet::new();
    config.repos.retain_mut(|repo| {
        if let Ok(path) = Path::new(&repo.path).canonicalize() {
            repo.path = path.to_string_lossy().into_owned();
        }
        seen.insert(repo.path.clone())
    });
}

fn normalize_loaded_tabs(config: &mut AppConfig) {
    let mut seen = HashSet::new();
    config.tabs.retain_mut(|tab| {
        if let Ok(path) = Path::new(&tab.path).canonicalize() {
            tab.path = path.to_string_lossy().into_owned();
        }
        seen.insert(tab.path.clone())
    });
    if let Some(active) = &mut config.active_tab
        && let Ok(path) = Path::new(active.as_str()).canonicalize()
    {
        *active = path.to_string_lossy().into_owned();
    }
    if !config
        .active_tab
        .as_ref()
        .is_some_and(|active| config.tabs.iter().any(|tab| &tab.path == active))
    {
        config.active_tab = None;
    }
}

fn remove_launch_command(config: &mut AppConfig, repo_path: &str) -> Result<()> {
    config
        .launch_commands
        .remove(&canonicalize_or_preserve(repo_path)?);
    Ok(())
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn add_known_repo(config: &mut AppConfig, path: &Path, last_opened: u64) -> Result<()> {
    let path = path
        .canonicalize()
        .map_err(|error| BackendError::new("invalid_path", error))?;
    let canonical = path.to_string_lossy().into_owned();
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    config.repos.retain(|repo| repo.path != canonical);
    config.repos.insert(
        0,
        KnownRepo {
            path: canonical,
            name,
            last_opened,
        },
    );
    config.repos.truncate(MAX_KNOWN_REPOS);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let config = AppConfig {
            version: CONFIG_VERSION,
            active_tab: Some("/repo/one".into()),
            start_at_login: true,
            check_for_updates: true,
            launch_commands: HashMap::from([("/repo/one".into(), "pnpm tauri dev".into())]),
            launch_terminal: Some("cosmic-term".into()),
            repos: vec![KnownRepo {
                path: "/repo/one".into(),
                name: "one".into(),
                last_opened: 42,
            }],
            tabs: vec![OpenTab {
                path: "/repo/one".into(),
            }],
        };
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        assert_eq!(
            fs::read_dir(directory.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from("config.toml")]
        );
    }

    #[test]
    fn missing_config_returns_defaults() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            load_config(&directory.path().join("missing.toml")).unwrap(),
            AppConfig::default()
        );
    }

    #[test]
    fn older_config_defaults_start_at_login_to_false() {
        let config: AppConfig = toml::from_str("version = 1\nrepos = []\ntabs = []").unwrap();
        assert!(!config.start_at_login);
        assert!(config.check_for_updates);
        assert!(config.launch_commands.is_empty());
        assert_eq!(config.launch_terminal, None);
    }

    #[test]
    fn resetting_launch_command_removes_only_that_repository_command() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let mut config = AppConfig::default();
        config
            .launch_commands
            .insert(first.to_string_lossy().into_owned(), "npm run dev".into());
        config
            .launch_commands
            .insert(second.to_string_lossy().into_owned(), "cargo run".into());
        save_config(&path, &config).unwrap();
        let mut loaded = load_config(&path).unwrap();
        remove_launch_command(&mut loaded, &first.to_string_lossy()).unwrap();
        save_config(&path, &loaded).unwrap();

        assert_eq!(
            load_config(&path).unwrap().launch_commands,
            HashMap::from([(second.to_string_lossy().into_owned(), "cargo run".into())])
        );
    }

    #[test]
    fn corrupt_config_is_backed_up_and_defaults_are_returned() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "not = [valid toml").unwrap();
        assert_eq!(load_config(&path).unwrap(), AppConfig::default());
        assert_eq!(
            fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            "not = [valid toml"
        );
        assert!(!path.exists());
    }

    #[test]
    fn xdg_config_home_is_respected() {
        let directory = tempfile::tempdir().unwrap();
        let xdg = directory.path().join("xdg");
        let home = directory.path().join("home");
        assert_eq!(
            config_path_from(Some(&xdg), Some(&home)).unwrap(),
            xdg.join("gitreal/config.toml")
        );
        assert_eq!(
            config_path_from(None, Some(&home)).unwrap(),
            home.join(".config/gitreal/config.toml")
        );
    }

    #[test]
    fn autostart_file_is_written_and_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("autostart/gitreal.desktop");
        let executable = std::env::current_exe().unwrap();
        set_autostart_file(&path, true, Some(&executable)).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!(
                "[Desktop Entry]\nType=Application\nName=Gitreal\n{}X-GNOME-Autostart-enabled=true\n",
                exec_line(&executable)
            )
        );
        set_autostart_file(&path, false, None).unwrap();
        assert!(!path.exists());
        set_autostart_file(&path, false, None).unwrap();
    }

    #[test]
    fn autostart_exec_quotes_spaces_and_uses_appimage_when_set() {
        let directory = tempfile::tempdir().unwrap();
        let normal = directory.path().join("gitreal");
        assert_eq!(
            autostart_executable_from(None, Some(&normal), directory.path()).unwrap(),
            normal
        );
        assert_eq!(
            exec_line(&normal),
            format!("Exec=\"{}\" --hidden\n", normal.display())
        );

        let spaced = directory.path().join("Gitreal App/gitreal");
        assert_eq!(
            exec_line(&spaced),
            format!("Exec=\"{}\" --hidden\n", spaced.display())
        );

        let appimage = directory.path().join("Gitreal.AppImage");
        assert_eq!(
            autostart_executable_from(Some(appimage.as_os_str()), None, directory.path()).unwrap(),
            appimage
        );
    }

    #[test]
    fn autostart_path_respects_xdg_config_home() {
        let directory = tempfile::tempdir().unwrap();
        let xdg = directory.path().join("xdg");
        let home = directory.path().join("home");
        let autostart = autostart_path_from(Some(&xdg), Some(&home)).unwrap();
        assert_eq!(autostart, xdg.join("autostart/gitreal.desktop"));
        set_autostart_file(&autostart, true, Some(&std::env::current_exe().unwrap())).unwrap();
        assert!(autostart.is_file());
        assert_eq!(
            autostart_path_from(None, Some(&home)).unwrap(),
            home.join(".config/autostart/gitreal.desktop")
        );
    }

    #[test]
    fn adding_known_repositories_orders_and_caps_the_list() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        for index in 0..MAX_KNOWN_REPOS + 1 {
            let path = directory.path().join(format!("repo-{index}"));
            fs::create_dir(&path).unwrap();
            add_known_repo(&mut config, &path, index as u64).unwrap();
        }
        assert_eq!(config.repos.len(), MAX_KNOWN_REPOS);
        assert_eq!(config.repos[0].name, "repo-50");
        assert_eq!(config.repos[MAX_KNOWN_REPOS - 1].name, "repo-1");
    }

    #[test]
    fn known_repositories_are_deduplicated_after_canonicalization() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repo");
        fs::create_dir(&repository).unwrap();
        let alias = directory.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&repository, &alias).unwrap();
        #[cfg(not(unix))]
        let alias = repository.clone();

        let mut config = AppConfig::default();
        add_known_repo(&mut config, &repository, 1).unwrap();
        add_known_repo(&mut config, &alias, 2).unwrap();
        assert_eq!(config.repos.len(), 1);
        assert_eq!(
            config.repos[0].path,
            repository.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(config.repos[0].last_opened, 2);
    }
}
