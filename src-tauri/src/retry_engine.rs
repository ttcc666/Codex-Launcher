use crate::app_storage::{read_json, AppPaths};
use crate::config_manager::{
    get_codex_base_url, is_base_url_allowed, MAX_INTERVAL_SECONDS, MAX_TRIES_LIMIT,
};
use crate::run_manager::{
    is_stop_requested, keep_alive_override, MaintenanceLease, RunLease, RunManager,
};
use crate::status_store::write_status;
use crate::windows_text::decode_output_text;
use chrono::{DateTime, Local, Utc};
use command_group::{AsyncCommandGroup, AsyncGroupChild};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const HIGH_DEMAND_MSG: &str =
    "We're currently experiencing high demand, which may cause temporary errors.";
const OUTPUT_CHUNK_BYTES: usize = 8 * 1024;
const OUTPUT_CHANNEL_CAPACITY: usize = 32;
const OUTPUT_TAIL_MAX_BYTES: usize = 64 * 1024;
const OUTPUT_TAIL_MAX_CHUNKS: usize = 512;
const LOG_FLUSH_BYTES: usize = 32 * 1024;
const REMOTE_STOP_POLL_MS: u64 = 250;
const LOG_NOTICE_INTERVAL: Duration = Duration::from_millis(100);
const CONTINUATION_MARKER: &str = " … [logical line continues]";
pub(crate) const MAX_LOGICAL_LINE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Starting,
    Running,
    Success,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RunMode {
    #[default]
    Retry,
    ManualKeepAlive,
}

impl RunStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub run_id: String,
    pub owner_pid: u32,
    pub child_pid: Option<u32>,
    pub status: RunStatus,
    #[serde(default)]
    pub run_mode: RunMode,
    #[serde(default)]
    pub keep_alive_enabled: bool,
    pub message: String,
    pub command: String,
    pub work_dir: String,
    pub log_file: String,
    pub latest_log: String,
    pub attempt: u64,
    pub high_demand_count: u64,
    pub max_tries: u64,
    pub interval_seconds: u64,
    pub progress_percent: f64,
    pub last_exit_code: Option<i32>,
    pub last_error_snippet: String,
    pub result_preview: String,
    pub started_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub run_id: String,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub command: String,
    pub work_dir: PathBuf,
    pub interval_seconds: u64,
    pub max_tries: u64,
    pub allowed_base_urls: String,
    pub keep_alive: bool,
    pub keep_alive_interval: Duration,
    pub run_mode: RunMode,
    shell_program: OsString,
}

impl RunOptions {
    pub fn new(
        command: String,
        work_dir: PathBuf,
        interval_seconds: u64,
        max_tries: u64,
        allowed_base_urls: String,
    ) -> Self {
        Self {
            command,
            work_dir,
            interval_seconds,
            max_tries,
            allowed_base_urls,
            keep_alive: false,
            keep_alive_interval: Duration::from_secs(300),
            run_mode: RunMode::Retry,
            shell_program: std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe")),
        }
    }

    pub fn with_keep_alive(mut self, keep_alive: bool, keep_alive_interval: Duration) -> Self {
        self.keep_alive = keep_alive;
        self.keep_alive_interval = keep_alive_interval;
        self
    }

    pub fn with_run_mode(mut self, run_mode: RunMode) -> Self {
        self.run_mode = run_mode;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("command 不能为空".to_string());
        }
        if !self.work_dir.is_dir() {
            return Err(format!(
                "工作目录不存在或不是目录: {}",
                self.work_dir.display()
            ));
        }
        if !(1..=MAX_INTERVAL_SECONDS).contains(&self.interval_seconds) {
            return Err(format!(
                "重试间隔必须在 1..={} 秒之间",
                MAX_INTERVAL_SECONDS
            ));
        }
        if self.max_tries > MAX_TRIES_LIMIT {
            return Err(format!("最大尝试次数不能超过 {}", MAX_TRIES_LIMIT));
        }
        if self.keep_alive && self.keep_alive_interval.is_zero() {
            return Err("保活间隔必须大于 0".to_string());
        }
        Ok(())
    }

    #[cfg(test)]
    fn with_shell_program(mut self, shell_program: impl Into<OsString>) -> Self {
        self.shell_program = shell_program.into();
        self
    }
}

pub struct StartedRun {
    run_id: String,
    completion: JoinHandle<Result<TaskStatus, String>>,
}

impl StartedRun {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn detach(self) {
        drop(self.completion);
    }

    pub async fn wait(self) -> Result<TaskStatus, String> {
        self.completion
            .await
            .map_err(|error| format!("run task join 失败: {}", error))?
    }
}

#[derive(Clone)]
struct RunContext {
    app: Option<AppHandle>,
    paths: AppPaths,
    options: RunOptions,
}

#[derive(Clone)]
struct StatusBuilder {
    run_id: String,
    owner_pid: u32,
    child_pid: Option<u32>,
    run_mode: RunMode,
    keep_alive_enabled: bool,
    command: String,
    work_dir: String,
    log_file: String,
    latest_log: String,
    max_tries: u64,
    interval_seconds: u64,
    attempt: u64,
    high_demand_count: u64,
    last_exit_code: Option<i32>,
    last_error_snippet: String,
    result_preview: String,
    started_at: DateTime<Utc>,
}

impl StatusBuilder {
    fn new(run_id: String, options: &RunOptions, paths: &AppPaths, log_file: &Path) -> Self {
        Self {
            run_id,
            owner_pid: std::process::id(),
            child_pid: None,
            run_mode: options.run_mode,
            keep_alive_enabled: options.keep_alive,
            command: options.command.clone(),
            work_dir: options.work_dir.to_string_lossy().to_string(),
            log_file: log_file.to_string_lossy().to_string(),
            latest_log: paths.latest_log.to_string_lossy().to_string(),
            max_tries: options.max_tries,
            interval_seconds: options.interval_seconds,
            attempt: 0,
            high_demand_count: 0,
            last_exit_code: None,
            last_error_snippet: String::new(),
            result_preview: String::new(),
            started_at: Utc::now(),
        }
    }

    fn build(&self, status: RunStatus, message: impl Into<String>) -> TaskStatus {
        let progress_percent = if status.is_active() && self.max_tries > 0 {
            ((self.attempt as f64 / self.max_tries as f64) * 100.0).min(99.0)
        } else if status.is_active() {
            0.0
        } else {
            100.0
        };
        TaskStatus {
            run_id: self.run_id.clone(),
            owner_pid: self.owner_pid,
            child_pid: self.child_pid,
            status,
            run_mode: self.run_mode,
            keep_alive_enabled: self.keep_alive_enabled,
            message: message.into(),
            command: self.command.clone(),
            work_dir: self.work_dir.clone(),
            log_file: self.log_file.clone(),
            latest_log: self.latest_log.clone(),
            attempt: self.attempt,
            high_demand_count: self.high_demand_count,
            max_tries: self.max_tries,
            interval_seconds: self.interval_seconds,
            progress_percent,
            last_exit_code: self.last_exit_code,
            last_error_snippet: self.last_error_snippet.clone(),
            result_preview: self.result_preview.clone(),
            started_at: self.started_at.to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }
}

pub async fn start_run(
    app: Option<AppHandle>,
    manager: Arc<RunManager>,
    paths: AppPaths,
    options: RunOptions,
) -> Result<StartedRun, String> {
    options.validate()?;
    validate_base_url_preflight(&options.allowed_base_urls).await?;
    paths.ensure_directories()?;

    let run_id = format!(
        "{}-{}",
        Local::now().format("%Y%m%d-%H%M%S"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let lease = manager.reserve(&paths, run_id.clone(), options.keep_alive)?;
    let maintenance_lease = MaintenanceLease::acquire(&paths.maintenance_lock)?;
    let log_file = paths.logs_dir.join(format!("codex-retry-{run_id}.log"));
    let mut status_builder = StatusBuilder::new(run_id.clone(), &options, &paths, &log_file);
    let mut log_sink = match LogSink::open(&run_id, &log_file, &paths.latest_log, app.clone()).await
    {
        Ok(log_sink) => log_sink,
        Err(error) => {
            let status = status_builder.build(RunStatus::Failed, error.clone());
            write_status(&paths, &status, app.as_ref())?;
            return Err(error);
        }
    };

    log_sink
        .write_system(&format!("开始执行: {}", options.command))
        .await?;
    log_sink
        .write_system(&format!("工作目录: {}", options.work_dir.display()))
        .await?;
    if options.run_mode == RunMode::ManualKeepAlive {
        log_sink.write_system("运行模式: 手动保活").await?;
    }
    log_sink.flush().await?;
    let starting = status_builder.build(RunStatus::Starting, "正在启动第一次尝试");
    write_status(&paths, &starting, app.as_ref())?;
    drop(maintenance_lease);

    let first_child = match spawn_child(&options) {
        Ok(child) => child,
        Err(error) => {
            let message = format!("启动子进程失败: {}", error);
            status_builder.last_error_snippet = get_snippet(&message, 280);
            status_builder.result_preview = message.clone();
            log_sink.write_system(&message).await?;
            log_sink.flush().await?;
            let failed = status_builder.build(RunStatus::Failed, message.clone());
            write_status(&paths, &failed, app.as_ref())?;
            return Err(message);
        }
    };

    status_builder.child_pid = first_child.id();
    lease.set_child_pid(first_child.id())?;

    let context = RunContext {
        app: app.clone(),
        paths: paths.clone(),
        options,
    };
    let panic_builder = status_builder.clone();
    let panic_paths = paths;
    let panic_app = app;
    let run_future = run_retry_loop(context, lease, log_sink, first_child, status_builder);
    let completion = tokio::spawn(async move {
        match std::panic::AssertUnwindSafe(run_future)
            .catch_unwind()
            .await
        {
            Ok(result) => result,
            Err(payload) => {
                let message = format!("run task panic: {}", panic_message(payload));
                record_panic_failure(&panic_paths, &panic_builder, panic_app.as_ref(), &message);
                Err(message)
            }
        }
    });

    Ok(StartedRun { run_id, completion })
}

async fn run_retry_loop(
    context: RunContext,
    lease: RunLease,
    mut log_sink: LogSink,
    first_child: AsyncGroupChild,
    mut status_builder: StatusBuilder,
) -> Result<TaskStatus, String> {
    let outcome = execute_attempts(
        &context,
        &lease,
        &mut log_sink,
        first_child,
        &mut status_builder,
    )
    .await;

    status_builder.child_pid = None;
    let (terminal_status, terminal_message) = match outcome {
        LoopOutcome::Success(message) => (RunStatus::Success, message),
        LoopOutcome::Failed(message) => (RunStatus::Failed, message),
        LoopOutcome::Stopped(message) => (RunStatus::Stopped, message),
    };
    let terminal = status_builder.build(terminal_status, terminal_message.clone());

    let log_result = log_sink.write_system(&terminal_message).await;
    let flush_result = log_sink.flush().await;
    let status_result = write_status(&context.paths, &terminal, context.app.as_ref());
    status_result?;
    log_result?;
    flush_result?;
    Ok(terminal)
}

async fn execute_attempts(
    context: &RunContext,
    lease: &RunLease,
    log_sink: &mut LogSink,
    first_child: AsyncGroupChild,
    status_builder: &mut StatusBuilder,
) -> LoopOutcome {
    let mut next_child = Some(first_child);
    loop {
        if lease.cancellation_token().is_cancelled() {
            return LoopOutcome::Stopped("用户停止了任务".to_string());
        }
        match is_stop_requested(&context.paths, lease.run_id()) {
            Ok(true) => return LoopOutcome::Stopped("收到匹配 run ID 的远程停止请求".to_string()),
            Ok(false) => {}
            Err(error) => return LoopOutcome::Failed(error),
        }

        let mut child = match next_child.take() {
            Some(child) => child,
            None => match spawn_child(&context.options) {
                Ok(child) => child,
                Err(error) => {
                    return LoopOutcome::Failed(format!("启动子进程失败: {}", error));
                }
            },
        };
        let child_pid = child.id();
        if let Err(error) = lease.set_child_pid(child_pid) {
            let _ = child.kill().await;
            return LoopOutcome::Failed(error);
        }

        status_builder.attempt += 1;
        status_builder.child_pid = child_pid;
        let attempt_message = format!("第 {} 次尝试执行中", status_builder.attempt);
        if let Err(error) = log_sink.write_system(&attempt_message).await {
            let _ = child.kill().await;
            return LoopOutcome::Failed(error);
        }
        let running = status_builder.build(RunStatus::Running, attempt_message);
        if let Err(error) = log_sink.flush().await {
            let _ = child.kill().await;
            return LoopOutcome::Failed(error);
        }
        if let Err(error) = write_status(&context.paths, &running, context.app.as_ref()) {
            let _ = child.kill().await;
            return LoopOutcome::Failed(error);
        }

        let attempt_result = match run_child(context, lease, log_sink, child).await {
            Ok(result) => result,
            Err(LoopOutcome::Stopped(message)) => return LoopOutcome::Stopped(message),
            Err(LoopOutcome::Failed(message)) => return LoopOutcome::Failed(message),
            Err(LoopOutcome::Success(message)) => return LoopOutcome::Success(message),
        };
        status_builder.child_pid = None;
        if let Err(error) = lease.set_child_pid(None) {
            return LoopOutcome::Failed(error);
        }

        let Some(exit_code) = attempt_result.exit_code else {
            status_builder.last_error_snippet =
                "子进程异常终止，Windows 未提供 exit code".to_string();
            status_builder.result_preview = attempt_result.preview;
            return LoopOutcome::Failed(status_builder.last_error_snippet.clone());
        };
        status_builder.last_exit_code = Some(exit_code);
        status_builder.result_preview = attempt_result.preview.clone();

        let should_retry = exit_code != 0 || attempt_result.high_demand;
        if !should_retry {
            let keep_alive_enabled = match effective_keep_alive_enabled(context, lease) {
                Ok(enabled) => enabled,
                Err(outcome) => return outcome,
            };
            status_builder.keep_alive_enabled = keep_alive_enabled;
            if keep_alive_enabled {
                status_builder.last_error_snippet = String::new();
                let keep_alive_seconds = context.options.keep_alive_interval.as_secs();
                let keep_alive_message = format!(
                    "命令成功完成 (exit={exit_code})，保活已开启，{} 秒后再次执行",
                    keep_alive_seconds
                );
                if let Err(error) = log_sink.write_system(&keep_alive_message).await {
                    return LoopOutcome::Failed(error);
                }
                if let Err(error) = log_sink.flush().await {
                    return LoopOutcome::Failed(error);
                }
                let waiting = status_builder.build(RunStatus::Running, keep_alive_message);
                if let Err(error) = write_status(&context.paths, &waiting, context.app.as_ref()) {
                    return LoopOutcome::Failed(error);
                }
                match wait_for_keep_alive(context, lease).await {
                    Ok(()) => continue,
                    Err(LoopOutcome::Success(message)) => {
                        status_builder.keep_alive_enabled = false;
                        return LoopOutcome::Success(message);
                    }
                    Err(outcome) => return outcome,
                }
            }
            return LoopOutcome::Success(format!("命令成功完成 (exit={exit_code})"));
        }

        if attempt_result.high_demand {
            status_builder.high_demand_count += 1;
            status_builder.last_error_snippet = get_snippet(HIGH_DEMAND_MSG, 280);
        } else {
            status_builder.last_error_snippet = attempt_result.preview;
        }

        if has_reached_attempt_limit(status_builder.attempt, context.options.max_tries) {
            return LoopOutcome::Failed(format!(
                "命令未成功且已达到最大尝试次数 {} (exit={exit_code})",
                context.options.max_tries
            ));
        }

        let retry_message = if attempt_result.high_demand {
            format!(
                "检测到高负载，{} 秒后重试",
                context.options.interval_seconds
            )
        } else {
            format!(
                "命令异常退出 (exit={exit_code})，{} 秒后重试",
                context.options.interval_seconds
            )
        };
        if let Err(error) = log_sink.write_system(&retry_message).await {
            return LoopOutcome::Failed(error);
        }
        match wait_for_retry(context, lease).await {
            Ok(()) => {}
            Err(outcome) => return outcome,
        }
    }
}

fn has_reached_attempt_limit(attempt: u64, max_tries: u64) -> bool {
    max_tries > 0 && attempt >= max_tries
}

async fn run_child(
    context: &RunContext,
    lease: &RunLease,
    log_sink: &mut LogSink,
    mut child: AsyncGroupChild,
) -> Result<AttemptResult, LoopOutcome> {
    let stdout = child
        .inner()
        .stdout
        .take()
        .ok_or_else(|| LoopOutcome::Failed("子进程 stdout pipe 缺失".to_string()))?;
    let stderr = child
        .inner()
        .stderr
        .take()
        .ok_or_else(|| LoopOutcome::Failed("子进程 stderr pipe 缺失".to_string()))?;

    let (sender, mut receiver) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(pump_stream(stdout, StreamSource::Stdout, sender.clone()));
    tokio::spawn(pump_stream(stderr, StreamSource::Stderr, sender));

    let cancellation = lease.cancellation_token();
    let mut remote_poll = tokio::time::interval(Duration::from_millis(REMOTE_STOP_POLL_MS));
    remote_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut capture = AttemptCapture::default();

    while !(stdout_done && stderr_done) {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                return Err(LoopOutcome::Stopped("用户停止了任务".to_string()));
            }
            _ = remote_poll.tick() => {
                match is_stop_requested(&context.paths, lease.run_id()) {
                    Ok(true) => {
                        let _ = child.kill().await;
                        lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Stopped("收到匹配 run ID 的远程停止请求".to_string()));
                    }
                    Ok(false) => {}
                    Err(error) => {
                        let _ = child.kill().await;
                        lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Failed(error));
                    }
                }
            }
            event = receiver.recv() => {
                match event {
                    Some(StreamEvent::Chunk(source, bytes)) => {
                        capture.push_raw(source, &bytes);
                        match log_sink.write_output(source, &bytes).await {
                            Ok(fragments) => capture.push_normalized(&fragments),
                            Err(error) => {
                                let _ = child.kill().await;
                                lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                                return Err(LoopOutcome::Failed(error));
                            }
                        }
                    }
                    Some(StreamEvent::Done(source)) => {
                        match log_sink.finish_output(source).await {
                            Ok(fragments) => capture.push_normalized(&fragments),
                            Err(error) => {
                                let _ = child.kill().await;
                                lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                                return Err(LoopOutcome::Failed(error));
                            }
                        }
                        match source {
                            StreamSource::Stdout => stdout_done = true,
                            StreamSource::Stderr => stderr_done = true,
                        }
                    }
                    Some(StreamEvent::Error(source, error)) => {
                        let _ = child.kill().await;
                        lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Failed(format!(
                            "读取 {} 失败: {}",
                            source.label(), error
                        )));
                    }
                    None => {
                        let _ = child.kill().await;
                        lease.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Failed(
                            "stdout/stderr reader channel 意外关闭".to_string(),
                        ));
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|error| LoopOutcome::Failed(format!("等待子进程组退出失败: {}", error)))?;
    Ok(AttemptResult {
        exit_code: status.code(),
        high_demand: capture.high_demand,
        preview: capture.tail.preview(),
    })
}

async fn wait_for_retry(context: &RunContext, lease: &RunLease) -> Result<(), LoopOutcome> {
    let cancellation = lease.cancellation_token();
    let delay = tokio::time::sleep(Duration::from_secs(context.options.interval_seconds));
    tokio::pin!(delay);
    let mut remote_poll = tokio::time::interval(Duration::from_millis(REMOTE_STOP_POLL_MS));
    remote_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(LoopOutcome::Stopped("用户在重试等待期间停止了任务".to_string()));
            }
            _ = &mut delay => return Ok(()),
            _ = remote_poll.tick() => {
                match is_stop_requested(&context.paths, lease.run_id()) {
                    Ok(true) => {
                        return Err(LoopOutcome::Stopped(
                            "收到匹配 run ID 的远程停止请求".to_string(),
                        ));
                    }
                    Ok(false) => {}
                    Err(error) => return Err(LoopOutcome::Failed(error)),
                }
            }
        }
    }
}

async fn wait_for_keep_alive(context: &RunContext, lease: &RunLease) -> Result<(), LoopOutcome> {
    let cancellation = lease.cancellation_token();
    let mut keep_alive = lease.keep_alive_receiver();
    if !effective_keep_alive_enabled(context, lease)? {
        return Err(LoopOutcome::Success(
            "保活已关闭，当前 run 正常结束".to_string(),
        ));
    }
    let delay = tokio::time::sleep(context.options.keep_alive_interval);
    tokio::pin!(delay);
    let mut remote_poll = tokio::time::interval(Duration::from_millis(REMOTE_STOP_POLL_MS));
    remote_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(LoopOutcome::Stopped("用户在保活等待期间停止了任务".to_string()));
            }
            changed = keep_alive.changed() => {
                match changed {
                    Ok(()) if !*keep_alive.borrow_and_update() => {
                        return Err(LoopOutcome::Success(
                            "保活已关闭，当前 run 正常结束".to_string(),
                        ));
                    }
                    Ok(()) => {}
                    Err(_) => {
                        return Err(LoopOutcome::Failed(
                            "运行时保活控制通道意外关闭".to_string(),
                        ));
                    }
                }
            }
            _ = &mut delay => {
                if effective_keep_alive_enabled(context, lease)? {
                    return Ok(());
                }
                return Err(LoopOutcome::Success(
                    "保活已关闭，当前 run 正常结束".to_string(),
                ));
            },
            _ = remote_poll.tick() => {
                match is_stop_requested(&context.paths, lease.run_id()) {
                    Ok(true) => {
                        return Err(LoopOutcome::Stopped(
                            "收到匹配 run ID 的远程停止请求".to_string(),
                        ));
                    }
                    Ok(false) => {}
                    Err(error) => return Err(LoopOutcome::Failed(error)),
                }
                if !effective_keep_alive_enabled(context, lease)? {
                    return Err(LoopOutcome::Success(
                        "保活已关闭，当前 run 正常结束".to_string(),
                    ));
                }
            }
        }
    }
}

fn effective_keep_alive_enabled(
    context: &RunContext,
    lease: &RunLease,
) -> Result<bool, LoopOutcome> {
    match keep_alive_override(&context.paths, lease.run_id()) {
        Ok(Some(enabled)) => Ok(enabled),
        Ok(None) => Ok(lease.is_keep_alive_enabled()),
        Err(error) => Err(LoopOutcome::Failed(error)),
    }
}

fn spawn_child(options: &RunOptions) -> Result<AsyncGroupChild, String> {
    let mut command = Command::new(&options.shell_program);
    command
        .args(["/D", "/S", "/C", &options.command])
        .current_dir(&options.work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut group = command.group();
    group.kill_on_drop(true);
    #[cfg(target_os = "windows")]
    group.creation_flags(CREATE_NO_WINDOW);
    group.spawn().map_err(|error| {
        format!(
            "无法启动 shell [{}]: {}",
            PathBuf::from(&options.shell_program).display(),
            error
        )
    })
}

async fn validate_base_url_preflight(allowed_base_urls: &str) -> Result<(), String> {
    if allowed_base_urls.trim().is_empty() {
        return Ok(());
    }
    match get_codex_base_url().await? {
        Some(current) if is_base_url_allowed(&current, allowed_base_urls)? => Ok(()),
        Some(current) => Err(format!(
            "当前 Codex base_url 不在允许列表中。当前: {}；允许: {}",
            current, allowed_base_urls
        )),
        None => Err("无法读取当前 Codex provider 的 base_url，已阻止启动".to_string()),
    }
}

#[derive(Debug)]
enum LoopOutcome {
    Success(String),
    Failed(String),
    Stopped(String),
}

struct AttemptResult {
    exit_code: Option<i32>,
    high_demand: bool,
    preview: String,
}

#[derive(Debug, Clone, Copy)]
enum StreamSource {
    Stdout,
    Stderr,
}

impl StreamSource {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

enum StreamEvent {
    Chunk(StreamSource, Vec<u8>),
    Done(StreamSource),
    Error(StreamSource, std::io::Error),
}

async fn pump_stream<R>(mut reader: R, source: StreamSource, sender: mpsc::Sender<StreamEvent>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0_u8; OUTPUT_CHUNK_BYTES];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => {
                let _ = sender.send(StreamEvent::Done(source)).await;
                return;
            }
            Ok(read) => {
                if sender
                    .send(StreamEvent::Chunk(source, buffer[..read].to_vec()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(StreamEvent::Error(source, error)).await;
                return;
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FramedLine {
    bytes: Vec<u8>,
    continuation: bool,
}

#[derive(Default)]
struct LineFramer {
    pending: Vec<u8>,
}

impl LineFramer {
    fn push(&mut self, bytes: &[u8]) -> Vec<FramedLine> {
        let mut framed = Vec::new();
        let mut remaining = bytes;

        while let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') {
            let mut segment = &remaining[..newline];
            if segment.ends_with(b"\r") {
                segment = &segment[..segment.len() - 1];
            } else if segment.is_empty() && self.pending.last() == Some(&b'\r') {
                self.pending.pop();
            }
            self.extend_bounded(segment, &mut framed);
            framed.push(FramedLine {
                bytes: std::mem::take(&mut self.pending),
                continuation: false,
            });
            remaining = &remaining[newline + 1..];
        }

        self.extend_bounded(remaining, &mut framed);
        framed
    }

    fn finish(&mut self) -> Option<FramedLine> {
        (!self.pending.is_empty()).then(|| FramedLine {
            bytes: std::mem::take(&mut self.pending),
            continuation: false,
        })
    }

    fn extend_bounded(&mut self, mut bytes: &[u8], framed: &mut Vec<FramedLine>) {
        while !bytes.is_empty() {
            let available = MAX_LOGICAL_LINE_BYTES.saturating_sub(self.pending.len());
            if bytes.len() <= available {
                self.pending.extend_from_slice(bytes);
                return;
            }

            self.pending.extend_from_slice(&bytes[..available]);
            bytes = &bytes[available..];
            framed.push(FramedLine {
                bytes: std::mem::take(&mut self.pending),
                continuation: true,
            });
        }
    }
}

#[derive(Debug)]
struct NormalizedFragment {
    text: String,
    continuation: bool,
}

#[derive(Default)]
struct AttemptCapture {
    tail: OutputTail,
    stdout_scanner: ByteMarkerScanner,
    stderr_scanner: ByteMarkerScanner,
    high_demand: bool,
}

impl AttemptCapture {
    fn push_raw(&mut self, source: StreamSource, bytes: &[u8]) {
        let scanner = match source {
            StreamSource::Stdout => &mut self.stdout_scanner,
            StreamSource::Stderr => &mut self.stderr_scanner,
        };
        scanner.push(bytes);
        self.high_demand = self.stdout_scanner.found || self.stderr_scanner.found;
    }

    fn push_normalized(&mut self, fragments: &[NormalizedFragment]) {
        for fragment in fragments {
            self.tail.push(&fragment.text);
            if !fragment.continuation {
                self.tail.push("\n");
            }
        }
    }
}

#[derive(Default)]
struct ByteMarkerScanner {
    suffix: Vec<u8>,
    found: bool,
}

impl ByteMarkerScanner {
    fn push(&mut self, bytes: &[u8]) {
        if self.found {
            return;
        }

        let marker = HIGH_DEMAND_MSG.as_bytes();
        let mut window = Vec::with_capacity(self.suffix.len() + bytes.len());
        window.extend_from_slice(&self.suffix);
        window.extend_from_slice(bytes);
        self.found = window
            .windows(marker.len())
            .any(|candidate| candidate == marker);

        let keep = marker.len().saturating_sub(1).min(window.len());
        self.suffix.clear();
        self.suffix
            .extend_from_slice(&window[window.len().saturating_sub(keep)..]);
    }
}

#[derive(Default)]
struct OutputTail {
    chunks: VecDeque<String>,
    bytes: usize,
}

impl OutputTail {
    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut owned = text.to_string();
        if owned.len() > OUTPUT_TAIL_MAX_BYTES {
            let start = floor_char_boundary(&owned, owned.len() - OUTPUT_TAIL_MAX_BYTES);
            owned = owned[start..].to_string();
        }
        self.bytes += owned.len();
        self.chunks.push_back(owned);
        while self.bytes > OUTPUT_TAIL_MAX_BYTES || self.chunks.len() > OUTPUT_TAIL_MAX_CHUNKS {
            if let Some(removed) = self.chunks.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            } else {
                break;
            }
        }
    }

    fn preview(&self) -> String {
        get_snippet(
            &self.chunks.iter().map(String::as_str).collect::<String>(),
            280,
        )
    }
}

struct LogSink {
    run_id: String,
    full: BufWriter<tokio::fs::File>,
    latest: BufWriter<tokio::fs::File>,
    app: Option<AppHandle>,
    pending_bytes: usize,
    stdout_framer: LineFramer,
    stderr_framer: LineFramer,
    notice_throttle: LogNoticeThrottle,
}

impl LogSink {
    async fn open(
        run_id: &str,
        full_path: &Path,
        latest_path: &Path,
        app: Option<AppHandle>,
    ) -> Result<Self, String> {
        let full = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(full_path)
            .await
            .map_err(|error| format!("创建 run log 失败 [{}]: {}", full_path.display(), error))?;
        let latest = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(latest_path)
            .await
            .map_err(|error| {
                format!(
                    "初始化 latest log 失败 [{}]: {}",
                    latest_path.display(),
                    error
                )
            })?;
        Ok(Self {
            run_id: run_id.to_string(),
            full: BufWriter::new(full),
            latest: BufWriter::new(latest),
            app,
            pending_bytes: 0,
            stdout_framer: LineFramer::default(),
            stderr_framer: LineFramer::default(),
            notice_throttle: LogNoticeThrottle::default(),
        })
    }

    async fn write_system(&mut self, message: &str) -> Result<(), String> {
        let line = format!(
            "[{}] [launcher] {}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            message
        );
        self.write_record(line.as_bytes()).await
    }

    async fn write_output(
        &mut self,
        source: StreamSource,
        bytes: &[u8],
    ) -> Result<Vec<NormalizedFragment>, String> {
        let framed = self.framer_mut(source).push(bytes);
        self.write_framed(source, framed).await
    }

    async fn finish_output(
        &mut self,
        source: StreamSource,
    ) -> Result<Vec<NormalizedFragment>, String> {
        let framed = self.framer_mut(source).finish().into_iter().collect();
        self.write_framed(source, framed).await
    }

    fn framer_mut(&mut self, source: StreamSource) -> &mut LineFramer {
        match source {
            StreamSource::Stdout => &mut self.stdout_framer,
            StreamSource::Stderr => &mut self.stderr_framer,
        }
    }

    async fn write_framed(
        &mut self,
        source: StreamSource,
        framed: Vec<FramedLine>,
    ) -> Result<Vec<NormalizedFragment>, String> {
        let mut normalized = Vec::with_capacity(framed.len());
        for line in framed {
            let text = decode_output_text(&line.bytes);
            let continuation = if line.continuation {
                CONTINUATION_MARKER
            } else {
                ""
            };
            let record = format!(
                "[{}] [{}] {}{}\n",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                source.label(),
                text,
                continuation
            );
            self.write_record(record.as_bytes()).await?;
            normalized.push(NormalizedFragment {
                text,
                continuation: line.continuation,
            });
        }
        Ok(normalized)
    }

    async fn write_record(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.full
            .write_all(bytes)
            .await
            .map_err(|error| format!("写入 full log 失败: {}", error))?;
        self.latest
            .write_all(bytes)
            .await
            .map_err(|error| format!("写入 latest log 失败: {}", error))?;
        self.pending_bytes += bytes.len();
        if let Some(app) = &self.app {
            if self
                .notice_throttle
                .should_emit(tokio::time::Instant::now())
            {
                let _ = app.emit(
                    "log-line",
                    LogEvent {
                        run_id: self.run_id.clone(),
                    },
                );
            }
        }
        if self.pending_bytes >= LOG_FLUSH_BYTES {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), String> {
        self.full
            .flush()
            .await
            .map_err(|error| format!("flush full log 失败: {}", error))?;
        self.latest
            .flush()
            .await
            .map_err(|error| format!("flush latest log 失败: {}", error))?;
        self.pending_bytes = 0;
        Ok(())
    }
}

#[derive(Default)]
struct LogNoticeThrottle {
    last_emitted: Option<tokio::time::Instant>,
}

impl LogNoticeThrottle {
    fn should_emit(&mut self, now: tokio::time::Instant) -> bool {
        if self
            .last_emitted
            .is_some_and(|last| now.duration_since(last) < LOG_NOTICE_INTERVAL)
        {
            return false;
        }
        self.last_emitted = Some(now);
        true
    }
}

pub fn clear_history_logs(paths: &AppPaths) -> Result<usize, String> {
    let _maintenance_lease = MaintenanceLease::acquire(&paths.maintenance_lock)?;
    clear_history_logs_locked(paths)
}

fn clear_history_logs_locked(paths: &AppPaths) -> Result<usize, String> {
    let active_log = if paths.status_file.exists() {
        let status: TaskStatus = read_json(&paths.status_file)?;
        status
            .status
            .is_active()
            .then(|| PathBuf::from(status.log_file))
    } else {
        None
    };

    let mut candidates = Vec::new();
    for entry in fs::read_dir(&paths.logs_dir)
        .map_err(|error| format!("读取日志目录失败 [{}]: {}", paths.logs_dir.display(), error))?
    {
        let entry = entry.map_err(|error| format!("读取日志目录项失败: {}", error))?;
        let path = entry.path();
        let is_run_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("codex-retry-") && name.ends_with(".log"));
        if !is_run_log {
            continue;
        }
        if path == paths.latest_log || active_log.as_ref().is_some_and(|active| active == &path) {
            continue;
        }
        candidates.push(path);
    }

    let mut deleted = 0;
    for path in candidates {
        match fs::remove_file(&path) {
            Ok(()) => deleted += 1,
            Err(error) if is_sharing_violation(&error) => continue,
            Err(error) => {
                return Err(format!("删除历史日志失败 [{}]: {}", path.display(), error));
            }
        }
    }
    Ok(deleted)
}

fn is_sharing_violation(error: &std::io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

fn record_panic_failure(
    paths: &AppPaths,
    fallback: &StatusBuilder,
    app: Option<&AppHandle>,
    message: &str,
) {
    let mut status = read_json::<TaskStatus>(&paths.status_file)
        .unwrap_or_else(|_| fallback.build(RunStatus::Failed, message));
    status.status = RunStatus::Failed;
    status.child_pid = None;
    status.message = message.to_string();
    status.last_error_snippet = get_snippet(message, 280);
    status.updated_at = Utc::now().to_rfc3339();
    if let Err(error) = write_status(paths, &status, app) {
        eprintln!("记录 panic terminal status 失败: {}", error);
    }
    append_emergency_log(&PathBuf::from(&status.log_file), message);
    append_emergency_log(&paths.latest_log, message);
}

fn append_emergency_log(path: &Path, message: &str) {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            let _ = writeln!(
                file,
                "[{}] [launcher] {}",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                message
            );
            let _ = file.flush();
        }
        Err(error) => eprintln!("写入 emergency log 失败 [{}]: {}", path.display(), error),
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub fn get_snippet(text: &str, max_len: usize) -> String {
    let clean = text
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.chars().count() <= max_len {
        clean
    } else {
        format!("{}...", clean.chars().take(max_len).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_storage::atomic_write_json;
    use crate::run_manager::StopTarget;
    use std::process::{Child, Command as StdCommand, Stdio as StdStdio};
    use std::time::Instant;

    const MAINTENANCE_HELPER_ROOT_ENV: &str = "CODEX_LAUNCHER_MAINTENANCE_HELPER_ROOT";
    const MAINTENANCE_HELPER_RESERVED_ENV: &str = "CODEX_LAUNCHER_MAINTENANCE_RESERVED";
    const MAINTENANCE_HELPER_GO_ENV: &str = "CODEX_LAUNCHER_MAINTENANCE_GO";
    const MAINTENANCE_HELPER_CREATED_ENV: &str = "CODEX_LAUNCHER_MAINTENANCE_CREATED";

    fn options(work_dir: &Path, command: &str, max_tries: u64) -> RunOptions {
        RunOptions::new(
            command.to_string(),
            work_dir.to_path_buf(),
            1,
            max_tries,
            String::new(),
        )
    }

    #[tokio::test]
    async fn success_writes_stdout_and_stderr_to_full_log() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let started = start_run(
            None,
            manager,
            paths.clone(),
            options(
                temp.path(),
                "echo stdout-marker & echo stderr-marker 1>&2 & exit /b 0",
                1,
            ),
        )
        .await
        .expect("start success command");

        let status = started.wait().await.expect("wait for success run");
        assert_eq!(status.status, RunStatus::Success, "{}", status.message);
        let full_log = fs::read_to_string(&status.log_file).expect("read full log");
        assert!(full_log.contains("stdout-marker"));
        assert!(full_log.contains("stderr-marker"));
    }

    #[tokio::test]
    async fn one_read_chunk_with_multiple_lines_prefixes_each_logical_line_once() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let full_path = temp.path().join("full.log");
        let latest_path = temp.path().join("latest.log");
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None)
            .await
            .expect("open log sink");

        let _ = sink
            .write_output(StreamSource::Stdout, b"first\nsecond\n")
            .await
            .expect("write output");
        sink.flush().await.expect("flush output");

        let log = fs::read_to_string(full_path).expect("read normalized log");
        assert_eq!(log.matches("[stdout]").count(), 2, "{log}");
        assert_eq!(log.lines().count(), 2, "{log}");
    }

    #[tokio::test]
    async fn utf8_character_split_across_reads_is_preserved() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let full_path = temp.path().join("full.log");
        let latest_path = temp.path().join("latest.log");
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None)
            .await
            .expect("open log sink");
        let chinese = "中文".as_bytes();

        let _ = sink
            .write_output(StreamSource::Stdout, &chinese[..1])
            .await
            .expect("write first byte chunk");
        let _ = sink
            .write_output(StreamSource::Stdout, &chinese[1..])
            .await
            .expect("write remaining byte chunk");
        let _ = sink
            .finish_output(StreamSource::Stdout)
            .await
            .expect("flush final partial line");
        sink.flush().await.expect("flush output");

        let log = fs::read_to_string(full_path).expect("log must remain valid UTF-8");
        assert!(log.contains("中文"), "{log}");
        assert!(!log.contains('\u{fffd}'), "{log}");
        assert_eq!(log.matches("[stdout]").count(), 1, "{log}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn oem_936_output_is_normalized_to_utf8() {
        use windows_sys::Win32::Globalization::GetOEMCP;

        if unsafe { GetOEMCP() } != 936 {
            return;
        }

        let temp = tempfile::tempdir().expect("create temp dir");
        let full_path = temp.path().join("full.log");
        let latest_path = temp.path().join("latest.log");
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None)
            .await
            .expect("open log sink");

        let _ = sink
            .write_output(StreamSource::Stdout, &[0xD6, 0xD0, b'\n'])
            .await
            .expect("write GBK-compatible OEM bytes");
        sink.flush().await.expect("flush output");

        let log = fs::read_to_string(full_path).expect("log must remain valid UTF-8");
        assert!(log.contains('中'), "{log}");
        assert!(!log.contains('\u{fffd}'), "{log}");
    }

    #[tokio::test]
    async fn nonzero_exit_and_high_demand_produce_failed_terminal_status() {
        let temp = tempfile::tempdir().expect("create temp dir");

        for command in [
            "exit /b 7",
            "echo We're currently experiencing high demand, which may cause temporary errors. 1>&2 & exit /b 0",
        ] {
            let paths = AppPaths::from_root(
                temp.path()
                    .join(uuid::Uuid::new_v4().simple().to_string()),
            );
            let manager = Arc::new(RunManager::new());
            let status = start_run(
                None,
                manager,
                paths,
                options(temp.path(), command, 1),
            )
            .await
            .expect("start failing command")
            .wait()
            .await
            .expect("wait for failed run");
            assert_eq!(status.status, RunStatus::Failed);
        }
    }

    #[tokio::test]
    async fn max_tries_counts_total_attempts() {
        let temp = tempfile::tempdir().expect("create temp dir");

        for max_tries in [1, 3] {
            let paths = AppPaths::from_root(temp.path().join(format!("attempt-limit-{max_tries}")));
            let status = start_run(
                None,
                Arc::new(RunManager::new()),
                paths,
                options(temp.path(), "exit /b 7", max_tries),
            )
            .await
            .expect("start failing command")
            .wait()
            .await
            .expect("wait for attempt limit");

            assert_eq!(status.status, RunStatus::Failed);
            assert_eq!(status.attempt, max_tries);
            assert!(status.message.contains("最大尝试次数"));
        }
    }

    #[tokio::test]
    async fn spawn_failure_is_returned_and_persisted_as_failed() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let invalid_shell = temp.path().join("missing-cmd.exe");
        let result = start_run(
            None,
            manager,
            paths.clone(),
            options(temp.path(), "echo never", 1)
                .with_shell_program(invalid_shell.into_os_string()),
        )
        .await;

        assert!(result.is_err());
        let status: TaskStatus = read_json(&paths.status_file).expect("read failed status");
        assert_eq!(status.status, RunStatus::Failed);
        assert!(status.message.contains("启动子进程失败"));
    }

    #[tokio::test]
    async fn local_cancellation_produces_stopped_terminal_status() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let started = start_run(
            None,
            manager.clone(),
            paths.clone(),
            options(temp.path(), "ping -n 30 127.0.0.1 >nul", 1),
        )
        .await
        .expect("start long command");
        let run_id = started.run_id().to_string();

        assert_eq!(
            manager
                .request_stop(&paths, &run_id)
                .expect("request local stop"),
            StopTarget::Local
        );
        let status = started.wait().await.expect("wait for stopped run");
        assert_eq!(status.status, RunStatus::Stopped);
    }

    #[tokio::test]
    async fn disabling_keep_alive_ends_the_current_wait_as_success() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let started = start_run(
            None,
            manager.clone(),
            paths.clone(),
            options(temp.path(), "echo keep-alive-success", 0)
                .with_keep_alive(true, Duration::from_secs(60)),
        )
        .await
        .expect("start keep-alive run");
        let run_id = started.run_id().to_string();
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            if let Ok(status) = read_json::<TaskStatus>(&paths.status_file) {
                if status.message.contains("保活已开启") {
                    assert!(status.keep_alive_enabled);
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "run did not enter keep-alive wait"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        manager
            .set_keep_alive(&paths, &run_id, false)
            .expect("disable keep-alive");
        let status = tokio::time::timeout(Duration::from_secs(2), started.wait())
            .await
            .expect("keep-alive wait should end immediately")
            .expect("wait for terminal status");

        assert_eq!(status.status, RunStatus::Success);
        assert_eq!(status.attempt, 1);
        assert!(!status.keep_alive_enabled);
        assert!(status.message.contains("保活已关闭"));
    }

    #[test]
    fn output_tail_remains_bounded_under_large_output() {
        let mut tail = OutputTail::default();
        for index in 0..10_000 {
            tail.push(&format!("line-{index}-{}\n", "x".repeat(128)));
        }
        assert!(tail.bytes <= OUTPUT_TAIL_MAX_BYTES);
        assert!(tail.chunks.len() <= OUTPUT_TAIL_MAX_CHUNKS);
        assert!(tail.preview().chars().count() <= 283);
    }

    #[test]
    fn line_framer_preserves_blank_lines_and_bounds_oversized_lines() {
        let mut framer = LineFramer::default();
        let framed = framer.push(b"first\n\nthird\n");
        assert_eq!(
            framed,
            [
                FramedLine {
                    bytes: b"first".to_vec(),
                    continuation: false,
                },
                FramedLine {
                    bytes: Vec::new(),
                    continuation: false,
                },
                FramedLine {
                    bytes: b"third".to_vec(),
                    continuation: false,
                },
            ]
        );

        let oversized = vec![b'x'; MAX_LOGICAL_LINE_BYTES * 2 + 7];
        let fragments = framer.push(&oversized);
        assert_eq!(fragments.len(), 2);
        assert!(fragments.iter().all(|fragment| fragment.continuation));
        assert!(fragments
            .iter()
            .all(|fragment| fragment.bytes.len() <= MAX_LOGICAL_LINE_BYTES));
        let final_fragment = framer.finish().expect("final bounded fragment");
        assert_eq!(final_fragment.bytes.len(), 7);
        assert!(!final_fragment.continuation);
    }

    #[test]
    fn high_demand_marker_is_detected_across_raw_chunk_boundaries() {
        let mut scanner = ByteMarkerScanner::default();
        let split = HIGH_DEMAND_MSG.len() / 2;
        scanner.push(&HIGH_DEMAND_MSG.as_bytes()[..split]);
        assert!(!scanner.found);
        scanner.push(&HIGH_DEMAND_MSG.as_bytes()[split..]);
        assert!(scanner.found);
    }

    #[test]
    fn log_event_payload_contains_only_the_run_id() {
        let value = serde_json::to_value(LogEvent {
            run_id: "run-a".to_string(),
        })
        .expect("serialize log event");
        assert_eq!(value, serde_json::json!({ "runId": "run-a" }));
    }

    #[tokio::test(start_paused = true)]
    async fn log_notification_throttle_has_a_minimum_interval() {
        let mut throttle = LogNoticeThrottle::default();
        assert!(throttle.should_emit(tokio::time::Instant::now()));
        assert!(!throttle.should_emit(tokio::time::Instant::now()));
        tokio::time::advance(LOG_NOTICE_INTERVAL - Duration::from_millis(1)).await;
        assert!(!throttle.should_emit(tokio::time::Instant::now()));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(throttle.should_emit(tokio::time::Instant::now()));
    }

    #[tokio::test]
    async fn clear_history_preserves_active_run_log_and_latest_log() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let started = start_run(
            None,
            manager.clone(),
            paths.clone(),
            options(temp.path(), "ping -n 30 127.0.0.1 >nul", 1),
        )
        .await
        .expect("start active run");
        let run_id = started.run_id().to_string();
        let active: TaskStatus = read_json(&paths.status_file).expect("read active status");
        let active_log = PathBuf::from(&active.log_file);
        let old_log = paths.logs_dir.join("codex-retry-old.log");
        fs::write(&old_log, b"old").expect("write old log");

        assert_eq!(clear_history_logs(&paths).expect("clear history"), 1);
        assert!(active_log.exists());
        assert!(paths.latest_log.exists());
        assert!(!old_log.exists());

        manager
            .request_stop(&paths, &run_id)
            .expect("stop active run");
        assert_eq!(
            started.wait().await.expect("wait for stopped run").status,
            RunStatus::Stopped
        );
    }

    #[test]
    fn maintenance_creation_helper() {
        let Some(root) = std::env::var_os(MAINTENANCE_HELPER_ROOT_ENV) else {
            return;
        };
        let reserved = PathBuf::from(
            std::env::var_os(MAINTENANCE_HELPER_RESERVED_ENV).expect("maintenance reserved path"),
        );
        let go = PathBuf::from(
            std::env::var_os(MAINTENANCE_HELPER_GO_ENV).expect("maintenance go path"),
        );
        let created = PathBuf::from(
            std::env::var_os(MAINTENANCE_HELPER_CREATED_ENV).expect("maintenance created path"),
        );
        let paths = AppPaths::from_root(PathBuf::from(root));
        paths.ensure_directories().expect("create helper app dirs");
        let manager = Arc::new(RunManager::new());
        let _run_lease = manager
            .reserve(&paths, "maintenance-race".to_string(), false)
            .expect("helper reserves run lock");
        fs::write(&reserved, b"reserved").expect("signal run reservation");
        wait_for_signal(&go, None);

        let _maintenance = MaintenanceLease::acquire(&paths.maintenance_lock)
            .expect("helper acquires maintenance lock");
        let active_log = paths.logs_dir.join("codex-retry-maintenance-race.log");
        fs::write(&active_log, b"active\n").expect("create active log");
        let helper_options = options(&paths.root_dir, "echo helper", 1);
        let status = StatusBuilder::new(
            "maintenance-race".to_string(),
            &helper_options,
            &paths,
            &active_log,
        )
        .build(RunStatus::Running, "running");
        atomic_write_json(&paths.status_file, &status).expect("write active helper status");
        fs::write(&created, b"created").expect("signal active log creation");
        std::thread::sleep(Duration::from_secs(10));
    }

    #[test]
    fn concurrent_log_creation_and_cleanup_never_delete_active_log() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let root = temp.path().join("app-data");
        let paths = AppPaths::from_root(root.clone());
        paths.ensure_directories().expect("create app dirs");
        let reserved = temp.path().join("reserved");
        let go = temp.path().join("go");
        let created = temp.path().join("created");
        let mut child = StdCommand::new(std::env::current_exe().expect("test executable path"))
            .args([
                "--exact",
                "retry_engine::tests::maintenance_creation_helper",
                "--nocapture",
            ])
            .env(MAINTENANCE_HELPER_ROOT_ENV, &root)
            .env(MAINTENANCE_HELPER_RESERVED_ENV, &reserved)
            .env(MAINTENANCE_HELPER_GO_ENV, &go)
            .env(MAINTENANCE_HELPER_CREATED_ENV, &created)
            .stdout(StdStdio::null())
            .stderr(StdStdio::null())
            .spawn()
            .expect("spawn maintenance helper");
        wait_for_signal(&reserved, Some(&mut child));

        let maintenance = MaintenanceLease::acquire(&paths.maintenance_lock)
            .expect("cleanup acquires maintenance lock first");
        let old_log = paths.logs_dir.join("codex-retry-old.log");
        fs::write(&old_log, b"old\n").expect("write old log");
        fs::write(&go, b"go").expect("allow helper to attempt creation");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!created.exists(), "run-log creation must wait for cleanup");
        assert_eq!(clear_history_logs_locked(&paths).expect("clear history"), 1);
        drop(maintenance);

        wait_for_signal(&created, Some(&mut child));
        assert!(
            paths
                .logs_dir
                .join("codex-retry-maintenance-race.log")
                .exists(),
            "active log created after cleanup must remain"
        );
        child.kill().expect("terminate maintenance helper");
        child.wait().expect("wait for maintenance helper");
    }

    fn wait_for_signal(path: &Path, mut child: Option<&mut Child>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            if let Some(child) = child.as_deref_mut() {
                if let Some(status) = child.try_wait().expect("query helper") {
                    panic!("maintenance helper exited early: {status}");
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
