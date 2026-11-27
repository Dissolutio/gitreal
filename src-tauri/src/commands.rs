use crate::config::{AppConfig, ConfigManager};
use crate::model::{
    BackendError, CommitDetail, GraphPage, OpenRepo, RefsResponse, RepoChanged, RepoError, Result,
};
use crate::repo::reader::{Backend, EventSink};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

struct TauriEventSink(AppHandle);

impl EventSink for TauriEventSink {
    fn repo_changed(&self, event: RepoChanged) {
        let _ = self.0.emit("repo-changed", event);
    }

    fn repo_error(&self, event: RepoError) {
        let _ = self.0.emit("repo-error", event);
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| BackendError::new("internal_error", error))?
}

#[tauri::command]
async fn open_repo(backend: State<'_, Arc<Backend>>, path: String) -> Result<OpenRepo> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.open_repo(path)).await
}

#[tauri::command]
async fn close_repo(backend: State<'_, Arc<Backend>>, repo_id: String) -> Result<()> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.close_repo(&repo_id)).await
}

#[tauri::command]
async fn get_refs(backend: State<'_, Arc<Backend>>, repo_id: String) -> Result<RefsResponse> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.get_refs(&repo_id)).await
}

#[tauri::command]
async fn get_graph_page(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    offset: usize,
    limit: usize,
) -> Result<GraphPage> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.get_graph_page(&repo_id, offset, limit)).await
}

#[tauri::command]
async fn get_commit_detail(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    commit_id: String,
) -> Result<CommitDetail> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.get_commit_detail(&repo_id, &commit_id)).await
}

#[tauri::command]
async fn get_config(config: State<'_, Arc<ConfigManager>>) -> Result<AppConfig> {
    let config = Arc::clone(&config);
    blocking(move || config.get_config()).await
}

#[tauri::command]
async fn set_open_tabs(
    config: State<'_, Arc<ConfigManager>>,
    paths: Vec<String>,
    active: Option<String>,
) -> Result<()> {
    let config = Arc::clone(&config);
    blocking(move || config.set_open_tabs(paths, active)).await
}

#[tauri::command]
async fn add_known_repo(config: State<'_, Arc<ConfigManager>>, path: String) -> Result<()> {
    let config = Arc::clone(&config);
    blocking(move || config.add_known_repo(path)).await
}

#[tauri::command]
async fn remove_known_repo(config: State<'_, Arc<ConfigManager>>, path: String) -> Result<()> {
    let config = Arc::clone(&config);
    blocking(move || config.remove_known_repo(path)).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(Arc::new(Backend::with_event_sink(Arc::new(
                TauriEventSink(app.handle().clone()),
            ))));
            app.manage(Arc::new(ConfigManager::default()));
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            open_repo,
            close_repo,
            get_refs,
            get_graph_page,
            get_commit_detail,
            get_config,
            set_open_tabs,
            add_known_repo,
            remove_known_repo
        ])
        .run(tauri::generate_context!())
        .expect("error while running Gitreal");
}
