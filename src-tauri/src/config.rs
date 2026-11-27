use crate::model::{BackendError, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
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
    pub repos: Vec<KnownRepo>,
    pub tabs: Vec<OpenTab>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            active_tab: None,
            start_at_login: false,
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

    pub fn set_start_at_login(&self, enabled: bool) -> Result<()> {
        let _guard = self.lock()?;
        let path = config_path()?;
        let autostart_path = autostart_path()?;
        let mut config = load_config(&path)?;
        set_autostart_file(&autostart_path, enabled)?;
        config.start_at_login = enabled;
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

fn set_autostart_file(path: &Path, enabled: bool) -> Result<()> {
    if enabled {
        let parent = path.parent().ok_or_else(|| {
            BackendError::new("config_error", "Autostart path has no parent directory")
        })?;
        fs::create_dir_all(parent).map_err(|error| BackendError::new("config_error", error))?;
        fs::write(
            path,
            "[Desktop Entry]\nType=Application\nName=Gitreal\nExec=gitreal --hidden\nX-GNOME-Autostart-enabled=true\n",
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
        set_autostart_file(&path, true).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[Desktop Entry]\nType=Application\nName=Gitreal\nExec=gitreal --hidden\nX-GNOME-Autostart-enabled=true\n"
        );
        set_autostart_file(&path, false).unwrap();
        assert!(!path.exists());
        set_autostart_file(&path, false).unwrap();
    }

    #[test]
    fn autostart_path_respects_xdg_config_home() {
        let directory = tempfile::tempdir().unwrap();
        let xdg = directory.path().join("xdg");
        let home = directory.path().join("home");
        let autostart = autostart_path_from(Some(&xdg), Some(&home)).unwrap();
        assert_eq!(autostart, xdg.join("autostart/gitreal.desktop"));
        set_autostart_file(&autostart, true).unwrap();
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
