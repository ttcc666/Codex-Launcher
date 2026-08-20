use crate::app_storage::{
    append_bounded_text_log, atomic_write_bytes, atomic_write_json, read_json, AppPaths,
};
use crate::retry_engine::{get_snippet, RunStatus, TaskStatus};
use crate::run_manager::ProcessLock;
use chrono::{Local, Utc};
use tauri::{AppHandle, Emitter};

const DIAGNOSTIC_LOG_MAX_BYTES: usize = 1024 * 1024;

pub fn read_optional_status(paths: &AppPaths) -> Result<Option<TaskStatus>, String> {
    if paths.status_file.exists() {
        read_json(&paths.status_file).map(Some)
    } else {
        Ok(None)
    }
}

pub fn write_status(
    paths: &AppPaths,
    status: &TaskStatus,
    app: Option<&AppHandle>,
) -> Result<(), String> {
    atomic_write_json(&paths.status_file, status)?;
    if let Some(app) = app {
        let _ = app.emit("status-update", status);
    }

    if let Err(error) =
        atomic_write_bytes(&paths.status_html, render_status_html(status).as_bytes())
    {
        let warning = format!(
            "[{}] status HTML mirror 更新失败 [{}]: {}",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            paths.status_html.display(),
            error
        );
        eprintln!("{}", warning);
        let _ = append_bounded_text_log(&paths.crash_log, &warning, DIAGNOSTIC_LOG_MAX_BYTES);
    }

    Ok(())
}

pub fn reconcile_stale_status(
    paths: &AppPaths,
    app: Option<&AppHandle>,
) -> Result<Option<TaskStatus>, String> {
    let Some(observed) = read_optional_status(paths)? else {
        return Ok(None);
    };
    if !observed.status.is_active() {
        return Ok(Some(observed));
    }

    let Some(_probe) = ProcessLock::try_acquire_existing(&paths.run_lock)? else {
        return Ok(Some(observed));
    };

    let Some(mut current) = read_optional_status(paths)? else {
        return Ok(None);
    };
    if current.run_id != observed.run_id || !current.status.is_active() {
        return Ok(Some(current));
    }

    let message = "run owner 已退出，未能持久化 terminal status；已在恢复检查中标记为失败";
    current.status = RunStatus::Failed;
    current.child_pid = None;
    current.child_pids.clear();
    current.active_workers = 0;
    current.message = message.to_string();
    current.last_error_snippet = get_snippet(message, 280);
    current.progress_percent = 100.0;
    current.updated_at = Utc::now().to_rfc3339();
    write_status(paths, &current, app)?;
    Ok(Some(current))
}

pub fn fail_active_run_if_matches(
    paths: &AppPaths,
    run_id: &str,
    message: &str,
    app: Option<&AppHandle>,
) -> Result<Option<TaskStatus>, String> {
    let Some(mut status) = read_optional_status(paths)? else {
        return Ok(None);
    };
    if status.run_id != run_id || !status.status.is_active() {
        return Ok(Some(status));
    }

    status.status = RunStatus::Failed;
    status.child_pid = None;
    status.child_pids.clear();
    status.active_workers = 0;
    status.message = message.to_string();
    status.last_error_snippet = get_snippet(message, 280);
    status.progress_percent = 100.0;
    status.updated_at = Utc::now().to_rfc3339();
    write_status(paths, &status, app)?;
    Ok(Some(status))
}

fn render_status_html(status: &TaskStatus) -> String {
    let (label, color) = match status.status {
        RunStatus::Starting => ("启动中", "#6b7280"),
        RunStatus::Running => ("运行中", "#2563eb"),
        RunStatus::Success => ("成功", "#16a34a"),
        RunStatus::Failed => ("失败", "#dc2626"),
        RunStatus::Stopped => ("已停止", "#ca8a04"),
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="refresh" content="2" />
  <title>Codex 重试进度</title>
  <style>
    body {{ font-family: system-ui, sans-serif; background: #0b1220; color: #e5e7eb; padding: 20px; }}
    .card {{ background: #111827; border: 1px solid #1f2937; border-radius: 12px; padding: 20px; max-width: 800px; margin: 0 auto; }}
    .badge {{ background: {color}; color: #fff; padding: 4px 10px; border-radius: 999px; display: inline-block; font-weight: bold; }}
    .metric {{ background: #0f172a; padding: 10px; border-radius: 8px; margin: 5px 0; }}
  </style>
</head>
<body>
  <div class="card">
    <div class="badge">{label}</div>
    <h1>Codex 重试进度</h1>
    <p>Run ID: {run_id}</p>
    <div class="metric">尝试次数: {attempt}</div>
    <div class="metric">高负载次数: {high_demand_count}</div>
    <div class="metric">并发线程数: {concurrency}（活跃 {active_workers}）</div>
    <p><strong>状态:</strong> {message}</p>
    <p><strong>命令:</strong> <code>{command}</code></p>
    <pre>{preview}</pre>
  </div>
</body>
</html>"#,
        run_id = html_escape(&status.run_id),
        attempt = status.attempt,
        high_demand_count = status.high_demand_count,
        concurrency = status.concurrency,
        active_workers = status.active_workers,
        message = html_escape(&status.message),
        command = html_escape(&status.command),
        preview = html_escape(if status.result_preview.is_empty() {
            &status.last_error_snippet
        } else {
            &status.result_preview
        }),
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
