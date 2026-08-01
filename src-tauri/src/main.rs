#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app_storage;
mod config_manager;
mod retry_engine;
mod run_manager;
mod snapshot;
mod status_store;
mod task_scheduler;
mod windows_text;

use app_storage::{append_bounded_text_log, read_json, AppPaths};
use config_manager::{load_config, save_config as save_config_file, AppConfig};
use retry_engine::{clear_history_logs, start_run, RunMode, RunOptions, RunStatus, TaskStatus};
use run_manager::{KeepAliveTarget, RunManager, ShutdownWaitResult, StopTarget};
use snapshot::{read_snapshot, SnapshotRequest, SnapshotResponse};
use status_store::{fail_active_run_if_matches, reconcile_stale_status};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

const DIAGNOSTIC_LOG_MAX_BYTES: usize = 1024 * 1024;
const GUI_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

struct AppState {
    paths: AppPaths,
    default_work_dir: PathBuf,
    run_manager: Arc<RunManager>,
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    load_config(&state.paths, &state.default_work_dir).await
}

#[tauri::command]
async fn save_app_config(config: AppConfig, state: State<'_, AppState>) -> Result<(), String> {
    save_config_file(&state.paths, &config).await
}

#[tauri::command]
async fn start_retry(
    config: AppConfig,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    save_config_file(&state.paths, &config).await?;
    let options = run_options_from_config(config);
    start_gui_run(app_handle, &state, options).await
}

#[tauri::command]
async fn start_manual_keep_alive(
    config: AppConfig,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    save_config_file(&state.paths, &config).await?;
    let keep_alive_interval =
        Duration::from_secs(config.keep_alive_interval_minutes.saturating_mul(60));
    let options = run_options_from_config(config)
        .with_keep_alive(true, keep_alive_interval)
        .with_run_mode(RunMode::ManualKeepAlive);
    start_gui_run(app_handle, &state, options).await
}

async fn start_gui_run(
    app_handle: tauri::AppHandle,
    state: &AppState,
    options: RunOptions,
) -> Result<String, String> {
    let started = start_run(
        Some(app_handle),
        state.run_manager.clone(),
        state.paths.clone(),
        options,
    )
    .await?;
    let run_id = started.run_id().to_string();
    started.detach();
    Ok(run_id)
}

#[tauri::command]
fn set_run_keep_alive(
    run_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let target = state
        .run_manager
        .set_keep_alive(&state.paths, &run_id, enabled)?;
    let scope = match target {
        KeepAliveTarget::Local => "本地 run",
        KeepAliveTarget::Remote => "远程 run",
    };
    Ok(if enabled {
        format!("{scope} 的保活已开启")
    } else {
        format!("{scope} 的保活已关闭；若正在保活等待，将立即正常结束")
    })
}

#[tauri::command]
async fn stop_retry(run_id: String, state: State<'_, AppState>) -> Result<String, String> {
    match state.run_manager.request_stop(&state.paths, &run_id)? {
        StopTarget::Local => Ok("已向本地 run 发送停止信号".to_string()),
        StopTarget::Remote => Ok("已写入带 run ID 的远程停止请求".to_string()),
    }
}

#[tauri::command]
async fn get_current_status(state: State<'_, AppState>) -> Result<Option<TaskStatus>, String> {
    reconcile_stale_status(&state.paths, None)
}

#[tauri::command]
async fn get_app_state(
    state: State<'_, AppState>,
    request: SnapshotRequest,
) -> Result<SnapshotResponse, String> {
    read_snapshot(&state.paths, &request).await
}

#[tauri::command]
async fn install_task(config: AppConfig, state: State<'_, AppState>) -> Result<String, String> {
    let exe_path =
        env::current_exe().map_err(|error| format!("无法获取当前 exe 路径: {}", error))?;
    save_config_file(&state.paths, &config).await?;
    task_scheduler::install_daily_task(
        &config.task_name,
        &config.daily_at,
        &exe_path.to_string_lossy(),
    )
    .await
}

#[tauri::command]
async fn uninstall_task(task_name: String) -> Result<String, String> {
    task_scheduler::uninstall_daily_task(&task_name).await
}

#[tauri::command]
async fn check_task_installed(task_name: String) -> Result<bool, String> {
    task_scheduler::check_task_status(&task_name).await
}

#[tauri::command]
async fn get_task_detail_command(task_name: String) -> Result<String, String> {
    task_scheduler::get_task_detail(&task_name).await
}

#[tauri::command]
async fn clear_history_logs_command(state: State<'_, AppState>) -> Result<String, String> {
    let deleted = clear_history_logs(&state.paths)?;
    Ok(format!("已清理 {} 个历史日志文件", deleted))
}

#[tauri::command]
async fn open_dashboard_url(state: State<'_, AppState>) -> Result<(), String> {
    let html = state.paths.status_html.clone();
    if !html.exists() {
        return Err("status.html 尚未生成，请先启动重试任务".to_string());
    }
    tokio::task::spawn_blocking(move || open::that(html))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn open_log_directory(state: State<'_, AppState>) -> Result<(), String> {
    let log_dir = state.paths.logs_dir.clone();
    tokio::task::spawn_blocking(move || open::that(log_dir))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn select_work_directory(app_handle: tauri::AppHandle) -> Option<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app_handle.dialog().file().pick_folder(move |folder| {
        let _ = sender.send(folder.map(|path| path.to_string()));
    });
    receiver.await.ok().flatten()
}

fn run_options_from_config(config: AppConfig) -> RunOptions {
    RunOptions::new(
        config.command,
        PathBuf::from(config.work_dir),
        config.interval,
        config.max_tries,
        config.allowed_base_urls,
    )
    .with_keep_alive(
        config.keep_alive,
        Duration::from_secs(config.keep_alive_interval_minutes * 60),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchMode {
    Gui,
    Headless,
    UninstallCleanup,
}

impl LaunchMode {
    fn detect(arguments: impl IntoIterator<Item = String>) -> Self {
        let mut headless = false;
        for argument in arguments {
            if argument == "--uninstall-cleanup" {
                return Self::UninstallCleanup;
            }
            if argument == "--headless" {
                headless = true;
            }
        }
        if headless {
            Self::Headless
        } else {
            Self::Gui
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadlessOutcome {
    Success,
    ConfigFailure(String),
    StartFailure(String),
    RunFailure(String),
    Stopped(String),
}

impl HeadlessOutcome {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Success => ExitCode::SUCCESS,
            Self::Stopped(_) => ExitCode::from(2),
            Self::ConfigFailure(_) | Self::StartFailure(_) | Self::RunFailure(_) => {
                ExitCode::from(1)
            }
        }
    }

    fn error_message(&self) -> Option<&str> {
        match self {
            Self::Success => None,
            Self::ConfigFailure(message)
            | Self::StartFailure(message)
            | Self::RunFailure(message)
            | Self::Stopped(message) => Some(message),
        }
    }
}

async fn run_headless(
    paths: AppPaths,
    startup_dir: PathBuf,
    run_manager: Arc<RunManager>,
) -> HeadlessOutcome {
    let config = match load_config(&paths, &startup_dir).await {
        Ok(config) => config,
        Err(error) => {
            return record_headless_outcome(&paths, HeadlessOutcome::ConfigFailure(error))
        }
    };
    let started = match start_run(
        None,
        run_manager,
        paths.clone(),
        run_options_from_config(config),
    )
    .await
    {
        Ok(started) => started,
        Err(error) => return record_headless_outcome(&paths, HeadlessOutcome::StartFailure(error)),
    };

    let outcome = match started.wait().await {
        Ok(status) if status.status == RunStatus::Success => HeadlessOutcome::Success,
        Ok(status) if status.status == RunStatus::Stopped => {
            HeadlessOutcome::Stopped(format!("run 已停止: {}", status.message))
        }
        Ok(status) => HeadlessOutcome::RunFailure(format!(
            "run 结束: {:?}: {}",
            status.status, status.message
        )),
        Err(error) => HeadlessOutcome::RunFailure(error),
    };
    record_headless_outcome(&paths, outcome)
}

fn record_headless_outcome(paths: &AppPaths, outcome: HeadlessOutcome) -> HeadlessOutcome {
    if let Some(message) = outcome.error_message() {
        let entry = format!(
            "[{}] {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            message
        );
        if let Err(error) =
            append_bounded_text_log(&paths.headless_log, &entry, DIAGNOSTIC_LOG_MAX_BYTES)
        {
            eprintln!("写入 headless log 失败: {}", error);
        }
        eprintln!("{}", message);
    }
    outcome
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UninstallCleanupOutcome {
    Completed,
    Skipped,
    Failed(String),
}

impl UninstallCleanupOutcome {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Completed | Self::Skipped => ExitCode::SUCCESS,
            Self::Failed(_) => ExitCode::from(1),
        }
    }
}

async fn run_uninstall_cleanup(paths: &AppPaths) -> UninstallCleanupOutcome {
    if !paths.config_file.exists() {
        return UninstallCleanupOutcome::Skipped;
    }

    let config = match read_json::<AppConfig>(&paths.config_file) {
        Ok(config) => config,
        Err(error) => return record_uninstall_failure(paths, error),
    };
    match task_scheduler::cleanup_daily_task(&config.task_name).await {
        Ok(task_scheduler::TaskCleanupOutcome::Removed)
        | Ok(task_scheduler::TaskCleanupOutcome::NotFound) => UninstallCleanupOutcome::Completed,
        Err(error) => record_uninstall_failure(paths, error),
    }
}

fn record_uninstall_failure(paths: &AppPaths, message: String) -> UninstallCleanupOutcome {
    let entry = format!(
        "[{}] {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        message
    );
    if let Err(error) =
        append_bounded_text_log(&paths.uninstall_log, &entry, DIAGNOSTIC_LOG_MAX_BYTES)
    {
        eprintln!("写入 uninstall log 失败: {}", error);
    }
    eprintln!("{}", message);
    UninstallCleanupOutcome::Failed(message)
}

#[tokio::main]
async fn main() -> ExitCode {
    let startup_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let launch_mode = LaunchMode::detect(env::args().skip(1));
    let is_headless = launch_mode == LaunchMode::Headless;
    let paths = match AppPaths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("{}", error);
            return ExitCode::from(1);
        }
    };
    if let Err(error) = paths.ensure_directories() {
        if is_headless {
            let outcome = record_headless_outcome(&paths, HeadlessOutcome::ConfigFailure(error));
            return outcome.exit_code();
        }
        eprintln!("{}", error);
        return ExitCode::from(1);
    }

    let crash_log = paths.crash_log.clone();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(error) = std::fs::write(&crash_log, format!("Crash:\n{:?}\n", info)) {
            eprintln!("写入 crash log 失败 [{}]: {}", crash_log.display(), error);
        }
    }));

    if launch_mode == LaunchMode::UninstallCleanup {
        return run_uninstall_cleanup(&paths).await.exit_code();
    }

    let exe_dir = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from));
    let mut legacy_log_dirs = vec![startup_dir.join("logs")];
    if let Some(exe_dir) = &exe_dir {
        legacy_log_dirs.push(exe_dir.join("logs"));
    }
    if let Err(error) = paths.migrate_legacy_config(legacy_log_dirs) {
        if is_headless {
            let _ = record_headless_outcome(
                &paths,
                HeadlessOutcome::ConfigFailure(format!("legacy 配置迁移失败: {error}")),
            );
        } else {
            eprintln!("{}", error);
        }
    }

    if launch_mode == LaunchMode::Gui && env::var("WEBVIEW2_USER_DATA_FOLDER").is_err() {
        env::set_var("WEBVIEW2_USER_DATA_FOLDER", &paths.webview_data_dir);
    }

    let run_manager = Arc::new(RunManager::new());

    if let Err(error) = reconcile_stale_status(&paths, None) {
        if is_headless {
            let outcome = record_headless_outcome(&paths, HeadlessOutcome::ConfigFailure(error));
            return outcome.exit_code();
        }
        eprintln!("启动时恢复 stale status 失败: {}", error);
    }

    if launch_mode == LaunchMode::Headless {
        return run_headless(paths, startup_dir, run_manager)
            .await
            .exit_code();
    }

    let manager_for_close = run_manager.clone();
    let paths_for_close = paths.clone();
    let closing = Arc::new(AtomicBool::new(false));
    let closing_for_event = closing.clone();
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            paths: paths.clone(),
            default_work_dir: startup_dir,
            run_manager,
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_app_config,
            start_retry,
            start_manual_keep_alive,
            stop_retry,
            set_run_keep_alive,
            get_current_status,
            get_app_state,
            install_task,
            uninstall_task,
            check_task_installed,
            get_task_detail_command,
            clear_history_logs_command,
            open_dashboard_url,
            open_log_directory,
            select_work_directory,
        ])
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let local_run_id = match manager_for_close.local_owned_run_id() {
                    Ok(run_id) => run_id,
                    Err(error) => {
                        eprintln!("关闭窗口时读取本地 run 失败: {}", error);
                        None
                    }
                };
                let Some(run_id) = local_run_id else {
                    return;
                };

                api.prevent_close();
                if closing_for_event.swap(true, Ordering::AcqRel) {
                    return;
                }

                let manager = manager_for_close.clone();
                let paths = paths_for_close.clone();
                let window = window.clone();
                tauri::async_runtime::spawn(async move {
                    match manager
                        .cancel_local_owned_and_wait(GUI_SHUTDOWN_TIMEOUT)
                        .await
                    {
                        Ok(ShutdownWaitResult::Completed(_) | ShutdownWaitResult::NoLocalRun) => {}
                        Ok(ShutdownWaitResult::TimedOut(timed_out_run_id)) => {
                            let message = format!(
                                "GUI shutdown 等待 terminal persistence 超过 {} 秒，已强制标记失败",
                                GUI_SHUTDOWN_TIMEOUT.as_secs()
                            );
                            if let Err(error) = fail_active_run_if_matches(
                                &paths,
                                &timed_out_run_id,
                                &message,
                                None,
                            ) {
                                eprintln!("记录 GUI shutdown timeout 失败: {}", error);
                            }
                        }
                        Err(error) => {
                            let message = format!("GUI shutdown 协调失败: {error}");
                            if let Err(status_error) =
                                fail_active_run_if_matches(&paths, &run_id, &message, None)
                            {
                                eprintln!("记录 GUI shutdown failure 失败: {}", status_error);
                            }
                        }
                    }
                    if let Err(error) = window.destroy() {
                        eprintln!("销毁窗口失败: {}", error);
                    }
                });
            }
        })
        .run(tauri::generate_context!());

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let entry = format!(
                "[{}] Tauri 应用运行发生致命错误: {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                error
            );
            let _ = append_bounded_text_log(&paths.crash_log, &entry, DIAGNOSTIC_LOG_MAX_BYTES);
            eprintln!("{}", entry);
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_outcome_maps_failures_and_stopped_to_nonzero_exit_codes() {
        assert_eq!(HeadlessOutcome::Success.exit_code(), ExitCode::SUCCESS);
        for outcome in [
            HeadlessOutcome::ConfigFailure("config".to_string()),
            HeadlessOutcome::StartFailure("start".to_string()),
            HeadlessOutcome::RunFailure("run".to_string()),
        ] {
            assert_eq!(outcome.exit_code(), ExitCode::from(1));
        }
        assert_eq!(
            HeadlessOutcome::Stopped("stopped".to_string()).exit_code(),
            ExitCode::from(2)
        );
    }

    #[test]
    fn headless_failures_are_persisted_in_the_temp_app_root() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");

        let outcome = record_headless_outcome(
            &paths,
            HeadlessOutcome::ConfigFailure("invalid config".to_string()),
        );

        assert_eq!(outcome.exit_code(), ExitCode::from(1));
        let log = std::fs::read_to_string(&paths.headless_log).expect("read headless log");
        assert!(log.contains("invalid config"));
    }

    #[test]
    fn uninstall_cleanup_mode_takes_precedence_and_has_truthful_exit_codes() {
        assert_eq!(
            LaunchMode::detect(["--headless".to_string(), "--uninstall-cleanup".to_string()]),
            LaunchMode::UninstallCleanup
        );
        assert_eq!(
            UninstallCleanupOutcome::Completed.exit_code(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            UninstallCleanupOutcome::Skipped.exit_code(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            UninstallCleanupOutcome::Failed("failure".to_string()).exit_code(),
            ExitCode::from(1)
        );
    }

    #[tokio::test]
    async fn uninstall_cleanup_with_missing_or_corrupt_config_never_starts_a_run() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        assert_eq!(
            run_uninstall_cleanup(&paths).await,
            UninstallCleanupOutcome::Skipped
        );
        std::fs::write(&paths.config_file, b"{ corrupt").expect("write corrupt config");

        assert!(matches!(
            run_uninstall_cleanup(&paths).await,
            UninstallCleanupOutcome::Failed(_)
        ));
        assert!(paths.uninstall_log.exists());
        assert!(!paths.status_file.exists());
        assert!(!paths.run_lock.exists());
    }
}
