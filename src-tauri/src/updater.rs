use crate::model::{BackendError, Result};
use serde::Serialize;
use std::{ffi::OsStr, time::Duration};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

pub const RELEASE_PAGE: &str = "https://github.com/Dissolutio/gitreal/releases/latest";
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const UPDATER_CONFIG: &str = include_str!("../tauri.conf.json");
const PLACEHOLDER_PUBKEY: &str = "REPLACE_WITH_OWNER_PUBKEY";

#[derive(Clone, Debug, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
    pub can_install: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

pub fn updater_configured() -> bool {
    let config: serde_json::Value =
        serde_json::from_str(UPDATER_CONFIG).expect("tauri.conf.json must be valid JSON");
    updater_configured_with_key(config["plugins"]["updater"]["pubkey"].as_str())
}

fn updater_configured_with_key(pubkey: Option<&str>) -> bool {
    pubkey.is_some_and(|key| !key.is_empty() && key != PLACEHOLDER_PUBKEY)
}

pub fn can_install_from_env(appimage: Option<&OsStr>) -> bool {
    appimage.is_some_and(|path| !path.is_empty())
}

pub fn next_check_time(
    start: Duration,
    last_check: Option<Duration>,
    now: Duration,
    enabled: bool,
) -> Option<Duration> {
    if !enabled {
        return None;
    }
    let interval = if last_check.is_some() {
        CHECK_INTERVAL
    } else {
        FIRST_CHECK_DELAY
    };
    Some(
        last_check
            .unwrap_or(start)
            .saturating_add(interval)
            .max(now),
    )
}

pub fn ensure_no_running_jobs(running: bool) -> Result<()> {
    if running {
        Err(BackendError::new(
            "update_jobs_running",
            "Wait for running Git operations to finish before installing an update.",
        ))
    } else {
        Ok(())
    }
}

pub async fn check_update(app: &AppHandle) -> Result<Option<Update>> {
    if cfg!(debug_assertions) || !updater_configured() {
        return Ok(None);
    }
    app.updater()
        .map_err(updater_error)?
        .check()
        .await
        .map_err(updater_error)
}

pub fn update_info(update: &Update) -> UpdateInfo {
    UpdateInfo {
        version: update.version.clone(),
        notes: update.body.clone(),
        date: update.date.map(|date| date.to_string()),
        can_install: can_install_from_env(std::env::var_os("APPIMAGE").as_deref()),
    }
}

pub async fn check_and_emit(app: &AppHandle) -> Result<Option<UpdateInfo>> {
    match check_update(app).await {
        Ok(Some(update)) => {
            let info = update_info(&update);
            let _ = app.emit("update-found", info.clone());
            Ok(Some(info))
        }
        Ok(None) => Ok(None),
        Err(error) => {
            emit_error(app, &error);
            Err(error)
        }
    }
}

pub fn emit_error(app: &AppHandle, error: &BackendError) {
    let _ = app.emit(
        "update-error",
        serde_json::json!({ "message": crate::repo::jobs::redact_credentials(&error.message) }),
    );
}

pub fn updater_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::new(
        "updater_error",
        crate::repo::jobs::redact_credentials(&error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_are_scheduled_thirty_seconds_after_start_then_every_six_hours() {
        let start = Duration::from_secs(12);
        assert_eq!(
            next_check_time(start, None, Duration::from_secs(20), true),
            Some(Duration::from_secs(42))
        );
        assert_eq!(
            next_check_time(
                start,
                Some(Duration::from_secs(100)),
                Duration::from_secs(101),
                true
            ),
            Some(Duration::from_secs(21_700))
        );
        assert_eq!(
            next_check_time(
                start,
                Some(Duration::from_secs(100)),
                Duration::from_secs(22_000),
                true
            ),
            Some(Duration::from_secs(22_000))
        );
        assert_eq!(
            next_check_time(start, None, Duration::from_secs(50), false),
            None
        );
    }

    #[test]
    fn appimage_detection_requires_a_nonempty_environment_value() {
        assert!(can_install_from_env(Some(OsStr::new(
            "/tmp/gitreal.AppImage"
        ))));
        assert!(!can_install_from_env(Some(OsStr::new(""))));
        assert!(!can_install_from_env(None));
    }

    #[test]
    fn updater_config_has_the_expected_endpoint_and_generates_artifacts() {
        let config: serde_json::Value = serde_json::from_str(UPDATER_CONFIG).unwrap();
        assert_eq!(
            config["plugins"]["updater"]["endpoints"][0],
            "https://github.com/Dissolutio/gitreal/releases/latest/download/latest.json"
        );
        assert_eq!(config["plugins"]["updater"]["requireSignedVersion"], true);
        assert_eq!(config["bundle"]["createUpdaterArtifacts"], true);
        assert!(!updater_configured_with_key(Some(PLACEHOLDER_PUBKEY)));
        assert!(!updater_configured_with_key(None));
        assert!(updater_configured_with_key(Some("owner-public-key")));
    }

    #[test]
    fn update_install_is_refused_while_a_git_job_is_running() {
        let error = ensure_no_running_jobs(true).unwrap_err();
        assert_eq!(error.code, "update_jobs_running");
        assert!(error.message.contains("Wait"));
        ensure_no_running_jobs(false).unwrap();
    }
}
