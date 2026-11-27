use crate::cli::{self, CliAction};
use crate::config::{AppConfig, ConfigManager};
use crate::model::{
    BackendError, CommitDetail, GraphPage, JobFinished, JobId, JobStarted, OpenRepo, RefsResponse,
    RepoChanged, RepoError, Result,
};
use crate::repo::reader::{Backend, EventSink};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tauri::{
    AppHandle, Emitter, Manager, State, WindowEvent,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
};

#[derive(Default)]
struct PendingOpenPaths(Mutex<PendingOpenState>);

#[derive(Default)]
struct PendingOpenState {
    frontend_ready: bool,
    paths: Vec<String>,
}

#[derive(Default)]
struct TrayState(Mutex<Option<(TrayIcon<tauri::Wry>, Menu<tauri::Wry>)>>);

#[derive(Clone, serde::Serialize)]
struct OpenPathEvent {
    path: String,
}

struct TauriEventSink(AppHandle);

impl EventSink for TauriEventSink {
    fn repo_changed(&self, event: RepoChanged) {
        let _ = self.0.emit("repo-changed", event);
    }

    fn repo_error(&self, event: RepoError) {
        let _ = self.0.emit("repo-error", event);
    }

    fn job_started(&self, event: JobStarted) {
        let _ = self.0.emit("job-started", event);
    }

    fn job_finished(&self, event: JobFinished) {
        let _ = self.0.emit("job-finished", event);
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
async fn fetch(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    remote: Option<String>,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.fetch(&repo_id, remote.as_deref())).await
}

#[tauri::command]
async fn pull(backend: State<'_, Arc<Backend>>, repo_id: String) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.pull(&repo_id)).await
}

#[tauri::command]
async fn push(backend: State<'_, Arc<Backend>>, repo_id: String, branch: String) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.push(&repo_id, &branch)).await
}

#[tauri::command]
async fn force_push(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    branch: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.force_push(&repo_id, &branch)).await
}

#[tauri::command]
async fn get_push_remote(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    branch: String,
) -> Result<String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.push_remote(&repo_id, &branch)).await
}

#[tauri::command]
async fn checkout_local(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    branch: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.checkout_local(&repo_id, &branch)).await
}

#[tauri::command]
async fn checkout_remote(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    remote_branch: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.checkout_remote(&repo_id, &remote_branch)).await
}

#[tauri::command]
async fn reset_local_to_remote(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    branch: String,
    remote_branch: String,
    expected_local_oid: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || {
        backend.reset_local_to_remote(&repo_id, &branch, &remote_branch, &expected_local_oid)
    })
    .await
}

#[tauri::command]
async fn checkout_commit(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    oid: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.checkout_commit(&repo_id, &oid)).await
}

#[tauri::command]
async fn create_branch(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    name: String,
    at_oid: String,
    checkout: bool,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.create_branch(&repo_id, &name, &at_oid, checkout)).await
}

#[tauri::command]
async fn rename_branch(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    old: String,
    new: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.rename_branch(&repo_id, &old, &new)).await
}

#[tauri::command]
async fn delete_local(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    branch: String,
    force: bool,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.delete_local(&repo_id, &branch, force)).await
}

#[tauri::command]
async fn delete_remote(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    remote: String,
    branch: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.delete_remote(&repo_id, &remote, &branch)).await
}

#[tauri::command]
async fn delete_local_and_remote(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    branch: String,
    remote: String,
    remote_branch: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.delete_local_and_remote(&repo_id, &branch, &remote, &remote_branch))
        .await
}

#[tauri::command]
async fn stash_push(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    include_untracked: bool,
    message: Option<String>,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.stash_push(&repo_id, include_untracked, message.as_deref())).await
}

#[tauri::command]
async fn stash_pop(backend: State<'_, Arc<Backend>>, repo_id: String) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.stash_pop(&repo_id)).await
}

#[tauri::command]
async fn stash_apply(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    index: usize,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.stash_apply(&repo_id, index)).await
}

#[tauri::command]
async fn stash_pop_at(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    index: usize,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.stash_pop_at(&repo_id, index)).await
}

#[tauri::command]
async fn stash_drop(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    index: usize,
    expected_oid: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.stash_drop(&repo_id, index, &expected_oid)).await
}

#[tauri::command]
async fn merge_into_current(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    source: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.merge_into_current(&repo_id, &source)).await
}

#[tauri::command]
async fn rebase_current_onto(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    target: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.rebase_current_onto(&repo_id, &target)).await
}

#[tauri::command]
async fn cherry_pick(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    oid: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.cherry_pick(&repo_id, &oid)).await
}

#[tauri::command]
async fn revert(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    oid: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.revert(&repo_id, &oid)).await
}

#[tauri::command]
async fn reset_current(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    oid: String,
    mode: String,
    expected_head: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.reset_current(&repo_id, &oid, &mode, &expected_head)).await
}

#[tauri::command]
async fn continue_operation(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    kind: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.continue_operation(&repo_id, &kind)).await
}

#[tauri::command]
async fn abort_operation(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    kind: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.abort_operation(&repo_id, &kind)).await
}

#[tauri::command]
async fn skip_operation(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    kind: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.skip_operation(&repo_id, &kind)).await
}

#[tauri::command]
async fn open_in_vscode(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
) -> Result<()> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.open_in_vscode(&repo_id)).await
}

#[tauri::command]
async fn stash_and_checkout(
    backend: State<'_, Arc<Backend>>,
    repo_id: String,
    target_kind: String,
    target: String,
) -> Result<JobId> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.stash_and_checkout(&repo_id, &target_kind, &target)).await
}

#[tauri::command]
async fn get_config(config: State<'_, Arc<ConfigManager>>) -> Result<AppConfig> {
    let config = Arc::clone(&config);
    blocking(move || config.get_config()).await
}

#[tauri::command]
fn frontend_ready(app: AppHandle, pending: State<'_, PendingOpenPaths>) {
    let paths = match pending.0.lock() {
        Ok(mut state) => {
            state.frontend_ready = true;
            std::mem::take(&mut state.paths)
        }
        Err(error) => {
            eprintln!("Gitreal could not deliver pending open requests: {error}");
            return;
        }
    };
    for path in paths {
        emit_open_path(&app, path);
    }
}

#[tauri::command]
async fn set_open_tabs(
    app: AppHandle,
    config: State<'_, Arc<ConfigManager>>,
    paths: Vec<String>,
    active: Option<String>,
) -> Result<()> {
    let manager = Arc::clone(&config);
    let menu_paths = paths.clone();
    blocking(move || manager.set_open_tabs(paths, active)).await?;
    let autostart = config.get_config()?.start_at_login;
    refresh_tray_menu(&app, &menu_paths, autostart);
    Ok(())
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

#[tauri::command]
async fn set_start_at_login(
    app: AppHandle,
    config: State<'_, Arc<ConfigManager>>,
    enabled: bool,
) -> Result<()> {
    let manager = Arc::clone(&config);
    blocking(move || manager.set_start_at_login(enabled)).await?;
    let updated = config.get_config()?;
    refresh_tray_menu(
        &app,
        &updated
            .tabs
            .iter()
            .map(|tab| tab.path.clone())
            .collect::<Vec<_>>(),
        updated.start_at_login,
    );
    Ok(())
}

#[tauri::command]
fn get_window_visibility(app: AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

fn emit_open_path(app: &AppHandle, path: String) {
    if let Err(error) = app.emit("open-path", OpenPathEvent { path }) {
        eprintln!("Gitreal could not deliver an open request: {error}");
    }
}

fn queue_open_path(app: &AppHandle, path: String) {
    if let Some(pending) = app.try_state::<PendingOpenPaths>() {
        match pending.0.lock() {
            Ok(mut state) if !state.frontend_ready => {
                state.paths.push(path);
                return;
            }
            Ok(_) => {}
            Err(error) => eprintln!("Gitreal could not queue an open request: {error}"),
        }
    }
    emit_open_path(app, path);
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let _ = app.emit("window-visibility", true);
    }
}

fn hide_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
        let _ = app.emit("window-visibility", false);
    }
}

fn toggle_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
            hide_window(app);
        } else {
            show_window(app);
        }
    }
}

fn open_requested_path(app: &AppHandle, path: Option<&Path>, cwd: &Path) {
    match cli::discover_worktree(path, cwd) {
        Ok(path) => {
            show_window(app);
            queue_open_path(app, path.to_string_lossy().into_owned());
        }
        Err(error) => {
            eprintln!("gitreal open: {}", error.message);
            show_window(app);
        }
    }
}

fn handle_cli_action(app: &AppHandle, action: CliAction, cwd: &Path) {
    match action {
        CliAction::Show => show_window(app),
        CliAction::Open(path) => open_requested_path(app, path.as_deref(), cwd),
        CliAction::Toggle => toggle_window(app),
        CliAction::Hidden => {}
    }
}

fn populate_tray_menu(
    app: &AppHandle,
    menu: &Menu<tauri::Wry>,
    paths: &[String],
    start_at_login: bool,
) -> tauri::Result<()> {
    while menu.remove_at(0)?.is_some() {}
    let heading = MenuItem::with_id(app, "open-tabs", "Open tabs", false, None::<&str>)?;
    menu.append(&heading)?;
    for path in paths {
        let name = Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let item = MenuItem::with_id(app, format!("tab:{path}"), name, true, None::<&str>)?;
        menu.append(&item)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let toggle = MenuItem::with_id(app, "toggle", "Toggle window", true, None::<&str>)?;
    menu.append(&toggle)?;
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    menu.append(&show)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start at login",
        true,
        start_at_login,
        None::<&str>,
    )?;
    menu.append(&autostart)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    menu.append(&quit)?;
    Ok(())
}

fn refresh_tray_menu(app: &AppHandle, paths: &[String], start_at_login: bool) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    let Ok(guard) = state.0.lock() else {
        eprintln!("Gitreal tray menu is unavailable");
        return;
    };
    if let Some((_, menu)) = guard.as_ref()
        && let Err(error) = populate_tray_menu(app, menu, paths, start_at_login)
    {
        eprintln!("Gitreal could not update the tray menu: {error}");
    }
}

fn initialize_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let config = match app.state::<Arc<ConfigManager>>().get_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Gitreal could not load tray settings: {error}");
            AppConfig::default()
        }
    };
    let menu = Menu::new(app)?;
    populate_tray_menu(
        app,
        &menu,
        &config
            .tabs
            .iter()
            .map(|tab| tab.path.clone())
            .collect::<Vec<_>>(),
        config.start_at_login,
    )?;
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            match id {
                "toggle" => toggle_window(app),
                "show" => show_window(app),
                "quit" => app.exit(0),
                "autostart" => {
                    if let Some(config) = app.try_state::<Arc<ConfigManager>>() {
                        match config.get_config() {
                            Ok(current) => {
                                if let Err(error) =
                                    config.set_start_at_login(!current.start_at_login)
                                {
                                    eprintln!("Gitreal could not update autostart: {error}");
                                } else if let Ok(updated) = config.get_config() {
                                    refresh_tray_menu(
                                        app,
                                        &updated
                                            .tabs
                                            .iter()
                                            .map(|tab| tab.path.clone())
                                            .collect::<Vec<_>>(),
                                        updated.start_at_login,
                                    );
                                }
                            }
                            Err(error) => {
                                eprintln!("Gitreal could not read autostart setting: {error}")
                            }
                        }
                    }
                }
                _ => {
                    if let Some(path) = id.strip_prefix("tab:") {
                        open_requested_path(app, Some(Path::new(path)), Path::new(path));
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                toggle_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder.build(app)?;
    let state = app.state::<TrayState>();
    if let Ok(mut state) = state.0.lock() {
        *state = Some((icon, menu));
    } else {
        eprintln!("Gitreal could not store its tray icon");
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let argv = std::env::args().collect::<Vec<_>>();
    let startup_action = match cli::parse_argv(&argv) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("gitreal: {}", error.message);
            CliAction::Show
        }
    };
    let startup_cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, args, cwd| match cli::parse_argv(&args) {
                Ok(action) => handle_cli_action(app, action, Path::new(&cwd)),
                Err(error) => {
                    eprintln!("gitreal: {}", error.message);
                    show_window(app);
                }
            },
        ))
        .setup(move |app| {
            app.manage(Arc::new(Backend::with_event_sink(Arc::new(
                TauriEventSink(app.handle().clone()),
            ))));
            app.manage(Arc::new(ConfigManager::default()));
            app.manage(PendingOpenPaths::default());
            app.manage(TrayState::default());
            if let Err(error) = initialize_tray(app.handle()) {
                eprintln!("Gitreal could not create a tray icon: {error}");
            }
            handle_cli_action(app.handle(), startup_action, &startup_cwd);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                hide_window(window.app_handle());
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            open_repo,
            close_repo,
            get_refs,
            get_graph_page,
            get_commit_detail,
            fetch,
            pull,
            push,
            force_push,
            get_push_remote,
            checkout_local,
            checkout_remote,
            reset_local_to_remote,
            checkout_commit,
            create_branch,
            rename_branch,
            delete_local,
            delete_remote,
            delete_local_and_remote,
            stash_push,
            stash_pop,
            stash_apply,
            stash_pop_at,
            stash_drop,
            merge_into_current,
            rebase_current_onto,
            cherry_pick,
            revert,
            reset_current,
            continue_operation,
            abort_operation,
            skip_operation,
            open_in_vscode,
            stash_and_checkout,
            get_config,
            frontend_ready,
            set_open_tabs,
            add_known_repo,
            remove_known_repo,
            set_start_at_login,
            get_window_visibility,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running Gitreal");
}
