use crate::app_storage::{
    append_bounded_text_log, atomic_write_bytes, atomic_write_json, read_json, AppPaths,
};
use crate::retry_engine::{get_snippet, LaunchSource, RunStatus, TaskStatus};
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
    let (label, color, pulse_color) = match status.status {
        RunStatus::Starting => ("启动中", "#6b7280", "156, 163, 175"),
        RunStatus::Running => ("运行中", "#10b981", "16, 185, 129"),
        RunStatus::Success => ("成功", "#10b981", "16, 185, 129"),
        RunStatus::Failed => ("失败", "#ef4444", "239, 68, 68"),
        RunStatus::Stopped => ("已停止", "#f59e0b", "245, 158, 11"),
    };

    let run_mode_label = match status.run_mode {
        crate::retry_engine::RunMode::Retry => "retry",
        crate::retry_engine::RunMode::ManualKeepAlive => "keep-alive",
    };

    let keep_alive_badge = if status.keep_alive_enabled {
        "<span class=\"pill pill-keep-alive\">保活</span>"
    } else {
        ""
    };

    let progress_bar = if status.progress_percent > 0.0 {
        let width = status.progress_percent.min(100.0);
        format!(
            r#"<div class="progress-bar">
      <div class="progress-fill" style="width: {:.1}%"></div>
      <span class="progress-text">{:.1}%</span>
    </div>"#,
            width, status.progress_percent
        )
    } else {
        String::new()
    };

    let exit_code_display = status
        .last_exit_code
        .map(|code| format!("<div class=\"metric-item\"><div class=\"metric-label\">退出码</div><div class=\"metric-value\">{}</div></div>", code))
        .unwrap_or_else(|| "<div class=\"metric-item\"><div class=\"metric-label\">退出码</div><div class=\"metric-value metric-na\">--</div></div>".to_string());

    let max_tries_display = if status.max_tries == 0 {
        "∞".to_string()
    } else {
        status.max_tries.to_string()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <meta http-equiv="refresh" content="2" />
  <title>Codex 重试进度 · {label}</title>
  <style>
    * {{ margin: 0; padding: 0; box-sizing: border-box; }}
    body {{
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
      background: #0a0f1a;
      color: #e5e7eb;
      min-height: 100vh;
      padding: 1.5rem;
      overflow-x: hidden;
    }}
    .aurora {{
      position: fixed;
      inset: 0;
      z-index: 0;
      pointer-events: none;
      overflow: hidden;
    }}
    .aurora-blob {{
      position: absolute;
      border-radius: 50%;
      filter: blur(80px);
      opacity: 0.15;
      animation: float 20s ease-in-out infinite;
    }}
    .aurora-blob:nth-child(1) {{
      top: -10%;
      left: -5%;
      width: 400px;
      height: 400px;
      background: #10b981;
      animation-delay: 0s;
    }}
    .aurora-blob:nth-child(2) {{
      top: 30%;
      right: -5%;
      width: 500px;
      height: 500px;
      background: #8b5cf6;
      animation-delay: 7s;
    }}
    .aurora-blob:nth-child(3) {{
      bottom: -10%;
      left: 20%;
      width: 350px;
      height: 350px;
      background: #3b82f6;
      animation-delay: 14s;
    }}
    @keyframes float {{
      0%, 100% {{ transform: translate(0, 0) scale(1); }}
      33% {{ transform: translate(30px, -30px) scale(1.1); }}
      66% {{ transform: translate(-20px, 20px) scale(0.9); }}
    }}
    .container {{
      position: relative;
      z-index: 1;
      max-width: 900px;
      margin: 0 auto;
    }}
    .card {{
      background: rgba(17, 24, 39, 0.8);
      backdrop-filter: blur(20px);
      border: 1px solid rgba(55, 65, 81, 0.5);
      border-radius: 16px;
      padding: 2rem;
      box-shadow: 0 20px 60px rgba(0, 0, 0, 0.3);
    }}
    .header {{
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 0.75rem;
      margin-bottom: 1.5rem;
    }}
    .status-indicator {{
      display: flex;
      align-items: center;
      gap: 0.5rem;
      background: {color};
      color: #fff;
      padding: 0.5rem 1rem;
      border-radius: 999px;
      font-weight: 600;
      font-size: 0.875rem;
      box-shadow: 0 0 20px rgba({pulse_color}, 0.4);
    }}
    .status-dot {{
      width: 8px;
      height: 8px;
      background: #fff;
      border-radius: 50%;
      animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
    }}
    @keyframes pulse {{
      0%, 100% {{ opacity: 1; }}
      50% {{ opacity: 0.5; }}
    }}
    .pill {{
      padding: 0.375rem 0.75rem;
      border-radius: 999px;
      font-size: 0.75rem;
      font-weight: 500;
      background: rgba(55, 65, 81, 0.5);
      border: 1px solid rgba(75, 85, 99, 0.5);
    }}
    .pill-keep-alive {{
      background: rgba(59, 130, 246, 0.2);
      border-color: rgba(59, 130, 246, 0.4);
      color: #60a5fa;
    }}
    h1 {{
      font-size: 1.5rem;
      font-weight: 700;
      margin-bottom: 0.25rem;
      background: linear-gradient(135deg, #10b981 0%, #3b82f6 100%);
      -webkit-background-clip: text;
      -webkit-text-fill-color: transparent;
      background-clip: text;
    }}
    .run-id {{
      font-size: 0.875rem;
      color: #9ca3af;
      font-family: 'Courier New', monospace;
      margin-bottom: 1.5rem;
    }}
    .metrics-grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
      gap: 0.75rem;
      margin-bottom: 1.5rem;
    }}
    .metric-item {{
      background: linear-gradient(135deg, rgba(15, 23, 42, 0.8) 0%, rgba(17, 24, 39, 0.6) 100%);
      border: 1px solid rgba(55, 65, 81, 0.3);
      border-radius: 12px;
      padding: 1rem;
      transition: all 0.3s ease;
    }}
    .metric-item:hover {{
      border-color: rgba(16, 185, 129, 0.4);
      transform: translateY(-2px);
    }}
    .metric-label {{
      font-size: 0.75rem;
      color: #9ca3af;
      margin-bottom: 0.5rem;
      text-transform: uppercase;
      letter-spacing: 0.05em;
    }}
    .metric-value {{
      font-size: 1.5rem;
      font-weight: 700;
      color: #10b981;
      font-variant-numeric: tabular-nums;
    }}
    .metric-na {{
      color: #6b7280;
    }}
    .progress-bar {{
      position: relative;
      background: rgba(31, 41, 55, 0.5);
      border-radius: 9999px;
      height: 1.5rem;
      margin-bottom: 1.5rem;
      overflow: hidden;
      border: 1px solid rgba(55, 65, 81, 0.3);
    }}
    .progress-fill {{
      position: absolute;
      inset: 0;
      background: linear-gradient(90deg, #10b981 0%, #059669 100%);
      transition: width 0.3s ease;
      border-radius: 9999px;
      box-shadow: 0 0 20px rgba(16, 185, 129, 0.5);
    }}
    .progress-text {{
      position: relative;
      z-index: 1;
      display: block;
      text-align: center;
      line-height: 1.5rem;
      font-size: 0.875rem;
      font-weight: 600;
      color: #fff;
      text-shadow: 0 1px 2px rgba(0, 0, 0, 0.5);
    }}
    .info-section {{
      background: rgba(15, 23, 42, 0.5);
      border: 1px solid rgba(55, 65, 81, 0.3);
      border-radius: 12px;
      padding: 1rem;
      margin-bottom: 1rem;
    }}
    .info-row {{
      display: flex;
      padding: 0.5rem 0;
      border-bottom: 1px solid rgba(55, 65, 81, 0.2);
    }}
    .info-row:last-child {{
      border-bottom: none;
    }}
    .info-label {{
      flex: 0 0 120px;
      font-size: 0.875rem;
      color: #9ca3af;
    }}
    .info-value {{
      flex: 1;
      font-size: 0.875rem;
      color: #e5e7eb;
      word-break: break-all;
    }}
    code {{
      font-family: 'Courier New', monospace;
      background: rgba(31, 41, 55, 0.5);
      padding: 0.125rem 0.375rem;
      border-radius: 4px;
      font-size: 0.875rem;
    }}
    .footer {{
      margin-top: 1.5rem;
      padding-top: 1rem;
      border-top: 1px solid rgba(55, 65, 81, 0.3);
      display: flex;
      justify-content: space-between;
      align-items: center;
      flex-wrap: wrap;
      gap: 0.5rem;
      font-size: 0.75rem;
      color: #6b7280;
    }}
    .refresh-indicator {{
      display: flex;
      align-items: center;
      gap: 0.5rem;
    }}
    .refresh-dot {{
      width: 6px;
      height: 6px;
      background: #10b981;
      border-radius: 50%;
      animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
    }}
    @media (max-width: 640px) {{
      body {{ padding: 1rem; }}
      .card {{ padding: 1.5rem; }}
      h1 {{ font-size: 1.25rem; }}
      .metrics-grid {{ grid-template-columns: repeat(2, 1fr); }}
      .info-label {{ flex: 0 0 90px; }}
    }}
  </style>
</head>
<body>
  <div class="aurora">
    <div class="aurora-blob"></div>
    <div class="aurora-blob"></div>
    <div class="aurora-blob"></div>
  </div>
  <div class="container">
    <div class="card">
      <div class="header">
        <div class="status-indicator">
          <div class="status-dot"></div>
          {label}
        </div>
        <span class="pill">{launch_source}</span>
        <span class="pill">{run_mode_label}</span>
        {keep_alive_badge}
      </div>
      
      <h1>Codex 重试进度</h1>
      <div class="run-id">Run ID: {run_id}</div>
      
      {progress_bar}
      
      <div class="metrics-grid">
        <div class="metric-item">
          <div class="metric-label">尝试次数</div>
          <div class="metric-value">{attempt}</div>
        </div>
        <div class="metric-item">
          <div class="metric-label">拥堵次数</div>
          <div class="metric-value">{high_demand_count}</div>
        </div>
        <div class="metric-item">
          <div class="metric-label">并发线程</div>
          <div class="metric-value">{active_workers}/{concurrency}</div>
        </div>
        {exit_code_display}
        <div class="metric-item">
          <div class="metric-label">最大尝试</div>
          <div class="metric-value">{max_tries_display}</div>
        </div>
        <div class="metric-item">
          <div class="metric-label">重试间隔</div>
          <div class="metric-value">{interval_seconds}s</div>
        </div>
      </div>
      
      <div class="info-section">
        <div class="info-row">
          <div class="info-label">状态消息</div>
          <div class="info-value">{message}</div>
        </div>
        <div class="info-row">
          <div class="info-label">执行命令</div>
          <div class="info-value"><code>{command}</code></div>
        </div>
        <div class="info-row">
          <div class="info-label">工作目录</div>
          <div class="info-value"><code>{work_dir}</code></div>
        </div>
        <div class="info-row">
          <div class="info-label">开始时间</div>
          <div class="info-value">{started_at}</div>
        </div>
        <div class="info-row">
          <div class="info-label">更新时间</div>
          <div class="info-value">{updated_at}</div>
        </div>
      </div>
      
      <div class="footer">
        <div class="refresh-indicator">
          <div class="refresh-dot"></div>
          <span>每 2 秒自动刷新</span>
        </div>
        <span>Codex Launcher v2.2.0</span>
      </div>
    </div>
  </div>
</body>
</html>"#,
        label = label,
        color = color,
        pulse_color = pulse_color,
        run_id = html_escape(&status.run_id),
        launch_source = launch_source_label(status.launch_source),
        run_mode_label = run_mode_label,
        keep_alive_badge = keep_alive_badge,
        progress_bar = progress_bar,
        attempt = status.attempt,
        high_demand_count = status.high_demand_count,
        concurrency = status.concurrency,
        active_workers = status.active_workers,
        exit_code_display = exit_code_display,
        max_tries_display = max_tries_display,
        interval_seconds = status.interval_seconds,
        message = html_escape(&status.message),
        command = html_escape(&status.command),
        work_dir = html_escape(&status.work_dir),
        started_at = html_escape(&status.started_at),
        updated_at = html_escape(&status.updated_at),
    )
}

fn launch_source_label(source: LaunchSource) -> &'static str {
    match source {
        LaunchSource::Unknown => "未知 / 旧版本",
        LaunchSource::Gui => "GUI 手动",
        LaunchSource::ScheduledTask => "Windows 定时任务",
        LaunchSource::HeadlessCli => "Headless CLI",
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
