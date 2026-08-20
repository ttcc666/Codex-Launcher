use crate::app_storage::{read_json, AppPaths};
use crate::config_manager::{
    get_codex_base_url, is_base_url_allowed, MAX_CONCURRENCY, MAX_INTERVAL_SECONDS, MAX_TRIES_LIMIT,
};
use crate::notifications::{NoopNotificationSink, NotificationEvent, NotificationSink};
use crate::run_manager::{
    is_stop_requested, keep_alive_override, MaintenanceLease, RunLease, RunManager,
};
use crate::status_store::write_status;
use crate::windows_text::decode_output_text;
use chrono::{DateTime, Local, Utc};
use command_group::{AsyncCommandGroup, AsyncGroupChild};
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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
/// worker 编号从 1 开始，日志里显示为 `[w1]`。
const FIRST_WORKER_ID: usize = 1;

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
    #[serde(default)]
    pub child_pids: Vec<u32>,
    pub status: RunStatus,
    #[serde(default)]
    pub run_mode: RunMode,
    #[serde(default)]
    pub keep_alive_enabled: bool,
    #[serde(default = "default_concurrency")]
    pub concurrency: u64,
    #[serde(default)]
    pub active_workers: u64,
    pub message: String,
    pub command: String,
    pub work_dir: String,
    pub log_file: String,
    pub latest_log: String,
    pub attempt: u64,
    #[serde(default)]
    pub retry_count: u64,
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

fn default_concurrency() -> u64 {
    1
}

#[derive(Clone)]
pub struct RunOptions {
    pub command: String,
    pub work_dir: PathBuf,
    pub interval_seconds: u64,
    pub max_tries: u64,
    pub concurrency: u64,
    pub allowed_base_urls: String,
    pub keep_alive: bool,
    pub keep_alive_interval: Duration,
    pub run_mode: RunMode,
    notification_sink: Arc<dyn NotificationSink>,
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
            concurrency: 1,
            allowed_base_urls,
            keep_alive: false,
            keep_alive_interval: Duration::from_secs(300),
            run_mode: RunMode::Retry,
            notification_sink: Arc::new(NoopNotificationSink),
            shell_program: std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe")),
        }
    }

    pub fn with_keep_alive(mut self, keep_alive: bool, keep_alive_interval: Duration) -> Self {
        self.keep_alive = keep_alive;
        self.keep_alive_interval = keep_alive_interval;
        self
    }

    pub fn with_concurrency(mut self, concurrency: u64) -> Self {
        self.concurrency = concurrency;
        self
    }

    pub fn with_run_mode(mut self, run_mode: RunMode) -> Self {
        self.run_mode = run_mode;
        self
    }

    pub fn with_notification_sink(mut self, notification_sink: Arc<dyn NotificationSink>) -> Self {
        self.notification_sink = notification_sink;
        self
    }

    /// 保活流程固定单线程：手动保活模式全程 1 个 worker，普通重试的保活周期同样降为 1。
    fn effective_concurrency(&self) -> u64 {
        if self.run_mode == RunMode::ManualKeepAlive {
            1
        } else {
            self.concurrency.max(1)
        }
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
        if !(1..=MAX_CONCURRENCY).contains(&self.concurrency) {
            return Err(format!("并发线程数必须在 1..={} 之间", MAX_CONCURRENCY));
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
    child_pids: BTreeMap<usize, u32>,
    run_mode: RunMode,
    concurrency: u64,
    active_workers: u64,
    keep_alive_enabled: bool,
    command: String,
    work_dir: String,
    log_file: String,
    latest_log: String,
    max_tries: u64,
    interval_seconds: u64,
    attempt: u64,
    retry_count: u64,
    success_notification_attempted: bool,
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
            child_pids: BTreeMap::new(),
            run_mode: options.run_mode,
            concurrency: options.effective_concurrency(),
            active_workers: 0,
            keep_alive_enabled: options.keep_alive,
            command: options.command.clone(),
            work_dir: options.work_dir.to_string_lossy().to_string(),
            log_file: log_file.to_string_lossy().to_string(),
            latest_log: paths.latest_log.to_string_lossy().to_string(),
            max_tries: options.max_tries,
            interval_seconds: options.interval_seconds,
            attempt: 0,
            retry_count: 0,
            success_notification_attempted: false,
            high_demand_count: 0,
            last_exit_code: None,
            last_error_snippet: String::new(),
            result_preview: String::new(),
            started_at: Utc::now(),
        }
    }

    /// 全局尝试额度：`max_tries == 0` 为无限，否则所有 worker 累计不超过 `max_tries`。
    fn has_remaining_attempts(&self) -> bool {
        self.max_tries == 0 || self.attempt < self.max_tries
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
            child_pid: self.child_pids.values().next().copied(),
            child_pids: self.child_pids.values().copied().collect(),
            status,
            run_mode: self.run_mode,
            keep_alive_enabled: self.keep_alive_enabled,
            concurrency: self.concurrency,
            active_workers: self.active_workers,
            message: message.into(),
            command: self.command.clone(),
            work_dir: self.work_dir.clone(),
            log_file: self.log_file.clone(),
            latest_log: self.latest_log.clone(),
            attempt: self.attempt,
            retry_count: self.retry_count,
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

/// 并发 worker 共享的状态聚合器。所有计数、pid 记账与 status 持久化都经由它串行化，
/// 因此 N 个 worker 写出的 `status.json` 始终自洽。锁内没有 `.await`。
struct StatusPublisher {
    paths: AppPaths,
    app: Option<AppHandle>,
    state: Mutex<StatusBuilder>,
}

impl StatusPublisher {
    fn new(paths: AppPaths, app: Option<AppHandle>, state: StatusBuilder) -> Self {
        Self {
            paths,
            app,
            state: Mutex::new(state),
        }
    }

    fn update<R>(&self, mutate: impl FnOnce(&mut StatusBuilder) -> R) -> Result<R, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "run status mutex 已损坏".to_string())?;
        Ok(mutate(&mut state))
    }

    fn snapshot(
        &self,
        status: RunStatus,
        message: impl Into<String>,
    ) -> Result<TaskStatus, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "run status mutex 已损坏".to_string())?;
        Ok(state.build(status, message))
    }

    fn publish(&self, status: RunStatus, message: impl Into<String>) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "run status mutex 已损坏".to_string())?;
        let snapshot = state.build(status, message);
        write_status(&self.paths, &snapshot, self.app.as_ref())
    }

    /// 抢跑阶段的尝试额度预留：返回 `None` 表示全局尝试次数已用尽。
    fn reserve_attempt(&self) -> Result<Option<u64>, String> {
        self.update(|state| {
            if !state.has_remaining_attempts() {
                return None;
            }
            state.attempt += 1;
            Some(state.attempt)
        })
    }

    /// 保活阶段的尝试计数：与改造前一致，成功的保活循环不受 `max_tries` 限制。
    fn bump_attempt(&self) -> Result<u64, String> {
        self.update(|state| {
            state.attempt += 1;
            state.attempt
        })
    }

    fn has_remaining_attempts(&self) -> Result<bool, String> {
        self.update(|state| state.has_remaining_attempts())
    }

    fn set_worker_pid(&self, worker_id: usize, child_pid: Option<u32>) -> Result<(), String> {
        self.update(|state| match child_pid {
            Some(child_pid) => {
                state.child_pids.insert(worker_id, child_pid);
            }
            None => {
                state.child_pids.remove(&worker_id);
            }
        })
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
    let concurrency = options.effective_concurrency();
    let mut status_builder = StatusBuilder::new(run_id.clone(), &options, &paths, &log_file);
    let mut log_sink = match LogSink::open(
        &run_id,
        &log_file,
        &paths.latest_log,
        app.clone(),
        concurrency > 1,
    )
    .await
    {
        Ok(log_sink) => log_sink,
        Err(error) => {
            let status = status_builder.build(RunStatus::Failed, error.clone());
            write_status(&paths, &status, app.as_ref())?;
            return Err(error);
        }
    };

    log_sink
        .write_system(None, &format!("开始执行: {}", options.command))
        .await?;
    log_sink
        .write_system(None, &format!("工作目录: {}", options.work_dir.display()))
        .await?;
    if concurrency > 1 {
        log_sink
            .write_system(
                None,
                &format!("并发线程数: {concurrency}（任一线程成功即终止其余线程）"),
            )
            .await?;
    }
    if options.run_mode == RunMode::ManualKeepAlive {
        log_sink.write_system(None, "运行模式: 手动保活").await?;
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
            log_sink.write_system(None, &message).await?;
            log_sink.flush().await?;
            let failed = status_builder.build(RunStatus::Failed, message.clone());
            write_status(&paths, &failed, app.as_ref())?;
            return Err(message);
        }
    };

    if let Some(child_pid) = first_child.id() {
        status_builder.child_pids.insert(FIRST_WORKER_ID, child_pid);
    }
    lease.set_worker_child_pid(FIRST_WORKER_ID, first_child.id())?;

    let context = RunContext {
        app: app.clone(),
        paths: paths.clone(),
        options,
    };
    let panic_builder = status_builder.clone();
    let panic_paths = paths.clone();
    let panic_app = app.clone();
    let publisher = Arc::new(StatusPublisher::new(paths, app, status_builder));
    let log = Arc::new(AsyncMutex::new(log_sink));
    let run_future = run_retry_loop(context, lease, log, first_child, publisher);
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
    log: Arc<AsyncMutex<LogSink>>,
    first_child: AsyncGroupChild,
    publisher: Arc<StatusPublisher>,
) -> Result<TaskStatus, String> {
    let outcome = match race_phase(&context, &lease, &publisher, &log, first_child).await {
        RaceOutcome::Succeeded(exit_code) => {
            finish_or_keep_alive(&context, &lease, &publisher, &log, exit_code).await
        }
        RaceOutcome::Stopped(message) => LoopOutcome::Stopped(message),
        RaceOutcome::Failed(message) => LoopOutcome::Failed(message),
    };

    publisher.update(|state| {
        state.child_pids.clear();
        state.active_workers = 0;
    })?;
    let (terminal_status, terminal_message) = match outcome {
        LoopOutcome::Success(message) => (RunStatus::Success, message),
        LoopOutcome::Failed(message) => (RunStatus::Failed, message),
        LoopOutcome::Stopped(message) => (RunStatus::Stopped, message),
    };
    let terminal = publisher.snapshot(terminal_status, terminal_message.clone())?;

    {
        let mut log_sink = log.lock().await;
        log_sink.write_system(None, &terminal_message).await?;
        log_sink.flush().await?;
    }
    write_status(&context.paths, &terminal, context.app.as_ref())?;
    if terminal_status == RunStatus::Success && context.options.run_mode == RunMode::Retry {
        notify_retry_success_once(&context, &publisher, &terminal).await;
    } else {
        let _ = context
            .options
            .notification_sink
            .notify(NotificationEvent::from_terminal(&terminal))
            .await;
    }
    Ok(terminal)
}

async fn notify_retry_success_once(
    context: &RunContext,
    publisher: &StatusPublisher,
    success_status: &TaskStatus,
) {
    if context.options.run_mode != RunMode::Retry {
        return;
    }
    let already_attempted = publisher
        .update(|state| std::mem::replace(&mut state.success_notification_attempted, true));
    if !matches!(already_attempted, Ok(false)) {
        return;
    }

    let _ = context
        .options
        .notification_sink
        .notify(NotificationEvent::from_terminal(success_status))
        .await;
}

/// 抢跑阶段：N 个 worker 并行执行同一条命令，首个成功者胜出并立即取消其余 worker。
async fn race_phase(
    context: &RunContext,
    lease: &RunLease,
    publisher: &StatusPublisher,
    log: &AsyncMutex<LogSink>,
    first_child: AsyncGroupChild,
) -> RaceOutcome {
    let race_token = lease.cancellation_token().child_token();
    let concurrency = context.options.effective_concurrency().max(1) as usize;
    if let Err(error) = publisher.update(|state| state.active_workers = concurrency as u64) {
        return RaceOutcome::Failed(error);
    }

    let mut seed = Some(first_child);
    let mut workers = FuturesUnordered::new();
    for worker_id in FIRST_WORKER_ID..FIRST_WORKER_ID + concurrency {
        let worker = WorkerContext {
            context,
            lease,
            publisher,
            log,
            token: race_token.clone(),
            worker_id,
        };
        workers.push(race_worker(worker, seed.take()));
    }

    let mut succeeded: Option<i32> = None;
    let mut stopped: Option<String> = None;
    let mut failed: Option<String> = None;
    let mut exhausted: Option<String> = None;
    while let Some(outcome) = workers.next().await {
        let _ = publisher.update(|state| {
            state.active_workers = state.active_workers.saturating_sub(1);
        });
        match outcome {
            WorkerOutcome::Succeeded(exit_code) => {
                if succeeded.is_none() {
                    succeeded = Some(exit_code);
                    // 胜者已产生：取消其余 worker，但继续 drain 以确保子进程被回收、日志顺序确定。
                    race_token.cancel();
                }
            }
            WorkerOutcome::Stopped(message) => {
                if stopped.is_none() {
                    stopped = Some(message);
                }
            }
            WorkerOutcome::Failed(message) => {
                if failed.is_none() {
                    failed = Some(message);
                }
            }
            WorkerOutcome::Exhausted(message) => {
                if exhausted.is_none() {
                    exhausted = Some(message);
                }
            }
            WorkerOutcome::Superseded => {}
        }
    }

    if let Some(exit_code) = succeeded {
        return RaceOutcome::Succeeded(exit_code);
    }
    if let Some(message) = stopped {
        return RaceOutcome::Stopped(message);
    }
    if let Some(message) = failed.or(exhausted) {
        return RaceOutcome::Failed(message);
    }
    // 只有在全部 worker 都被取消却没有胜者时才会走到这里，等价于外部停止。
    RaceOutcome::Stopped("用户停止了任务".to_string())
}

async fn race_worker(
    worker: WorkerContext<'_>,
    next_child: Option<AsyncGroupChild>,
) -> WorkerOutcome {
    let outcome = race_worker_attempts(&worker, next_child).await;
    if matches!(outcome, WorkerOutcome::Superseded) {
        // 尽力而为的收尾提示：写失败不影响已经产生的抢跑结果。
        let _ = worker
            .write_system("已被其它并发线程抢先完成，本线程已取消")
            .await;
    }
    outcome
}

async fn race_worker_attempts(
    worker: &WorkerContext<'_>,
    mut next_child: Option<AsyncGroupChild>,
) -> WorkerOutcome {
    loop {
        if let Some(outcome) = check_interrupts(worker) {
            return worker.worker_outcome(outcome);
        }

        let attempt = match worker.publisher.reserve_attempt() {
            Ok(Some(attempt)) => attempt,
            // 全局尝试额度被兄弟 worker 用尽：优先级最低，让带 exit code 的失败消息胜出。
            Ok(None) => {
                return WorkerOutcome::Exhausted(format!(
                    "命令未成功且已达到最大尝试次数 {}",
                    worker.context.options.max_tries
                ));
            }
            Err(error) => return WorkerOutcome::Failed(error),
        };

        match execute_attempt(worker, next_child.take(), attempt).await {
            AttemptFlow::Succeeded(exit_code) => return WorkerOutcome::Succeeded(exit_code),
            AttemptFlow::Retry => continue,
            AttemptFlow::Terminal(outcome) => return worker.worker_outcome(outcome),
        }
    }
}

/// 抢跑成功后的收尾：保活关闭时直接结束，保活开启时进入固定单线程的保活阶段。
async fn finish_or_keep_alive(
    context: &RunContext,
    lease: &RunLease,
    publisher: &StatusPublisher,
    log: &AsyncMutex<LogSink>,
    exit_code: i32,
) -> LoopOutcome {
    let keep_alive_enabled = match effective_keep_alive_enabled(context, lease) {
        Ok(enabled) => enabled,
        Err(outcome) => return outcome,
    };
    if let Err(error) = publisher.update(|state| state.keep_alive_enabled = keep_alive_enabled) {
        return LoopOutcome::Failed(error);
    }
    if !keep_alive_enabled {
        return LoopOutcome::Success(format!("命令成功完成 (exit={exit_code})"));
    }
    keep_alive_phase(context, lease, publisher, log, exit_code).await
}

/// 保活阶段固定单线程：只有 worker#1 继续循环，抢跑并发不再参与。
async fn keep_alive_phase(
    context: &RunContext,
    lease: &RunLease,
    publisher: &StatusPublisher,
    log: &AsyncMutex<LogSink>,
    initial_exit_code: i32,
) -> LoopOutcome {
    let worker = WorkerContext {
        context,
        lease,
        publisher,
        log,
        token: lease.cancellation_token(),
        worker_id: FIRST_WORKER_ID,
    };
    if let Err(error) = publisher.update(|state| state.active_workers = 1) {
        return LoopOutcome::Failed(error);
    }

    let mut pending_success = Some(initial_exit_code);
    loop {
        let exit_code = match pending_success.take() {
            Some(exit_code) => exit_code,
            None => {
                if let Some(outcome) = check_interrupts(&worker) {
                    return outcome;
                }
                let attempt = match publisher.bump_attempt() {
                    Ok(attempt) => attempt,
                    Err(error) => return LoopOutcome::Failed(error),
                };
                match execute_attempt(&worker, None, attempt).await {
                    AttemptFlow::Succeeded(exit_code) => exit_code,
                    AttemptFlow::Retry => continue,
                    AttemptFlow::Terminal(outcome) => return outcome,
                }
            }
        };

        let keep_alive_enabled = match effective_keep_alive_enabled(context, lease) {
            Ok(enabled) => enabled,
            Err(outcome) => return outcome,
        };
        if let Err(error) = publisher.update(|state| state.keep_alive_enabled = keep_alive_enabled)
        {
            return LoopOutcome::Failed(error);
        }
        if !keep_alive_enabled {
            return LoopOutcome::Success(format!("命令成功完成 (exit={exit_code})"));
        }

        if let Err(error) = publisher.update(|state| state.last_error_snippet = String::new()) {
            return LoopOutcome::Failed(error);
        }
        let keep_alive_message = format!(
            "命令成功完成 (exit={exit_code})，保活已开启，{} 秒后再次执行",
            context.options.keep_alive_interval.as_secs()
        );
        if let Err(error) = worker.write_system(&keep_alive_message).await {
            return LoopOutcome::Failed(error);
        }
        if let Err(error) = worker.flush().await {
            return LoopOutcome::Failed(error);
        }
        if let Err(error) = publisher.publish(RunStatus::Running, keep_alive_message) {
            return LoopOutcome::Failed(error);
        }

        let first_success = match publisher.snapshot(
            RunStatus::Success,
            format!("命令首次成功完成 (exit={exit_code})，保活继续运行"),
        ) {
            Ok(status) => status,
            Err(error) => return LoopOutcome::Failed(error),
        };
        notify_retry_success_once(context, publisher, &first_success).await;

        match wait_for_keep_alive(&worker).await {
            Ok(()) => continue,
            Err(LoopOutcome::Success(message)) => {
                let _ = publisher.update(|state| state.keep_alive_enabled = false);
                return LoopOutcome::Success(message);
            }
            Err(outcome) => return outcome,
        }
    }
}

/// 单次尝试：spawn → 采集输出 → 记账。失败时完成重试等待并返回 `Retry`。
async fn execute_attempt(
    worker: &WorkerContext<'_>,
    next_child: Option<AsyncGroupChild>,
    attempt: u64,
) -> AttemptFlow {
    let mut child = match next_child {
        Some(child) => child,
        None => match spawn_child(&worker.context.options) {
            Ok(child) => child,
            Err(error) => {
                return AttemptFlow::Terminal(LoopOutcome::Failed(format!(
                    "启动子进程失败: {}",
                    error
                )));
            }
        },
    };
    let child_pid = child.id();
    if let Err(error) = worker.set_child_pid(child_pid) {
        let _ = child.kill().await;
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }

    let attempt_message = format!("第 {} 次尝试执行中", attempt);
    if let Err(error) = worker.write_system(&attempt_message).await {
        let _ = child.kill().await;
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }
    if let Err(error) = worker.flush().await {
        let _ = child.kill().await;
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }
    if let Err(error) = worker
        .publisher
        .publish(RunStatus::Running, attempt_message)
    {
        let _ = child.kill().await;
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }

    let attempt_result = match run_child(worker, child).await {
        Ok(result) => result,
        Err(outcome) => return AttemptFlow::Terminal(outcome),
    };
    if let Err(error) = worker.set_child_pid(None) {
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }

    let Some(exit_code) = attempt_result.exit_code else {
        let message = "子进程异常终止，Windows 未提供 exit code".to_string();
        let recorded = worker.publisher.update(|state| {
            state.last_error_snippet = message.clone();
            state.result_preview = attempt_result.preview;
        });
        if let Err(error) = recorded {
            return AttemptFlow::Terminal(LoopOutcome::Failed(error));
        }
        return AttemptFlow::Terminal(LoopOutcome::Failed(message));
    };
    let recorded = worker.publisher.update(|state| {
        state.last_exit_code = Some(exit_code);
        state.result_preview = attempt_result.preview.clone();
    });
    if let Err(error) = recorded {
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }

    if exit_code == 0 && !attempt_result.high_demand {
        return AttemptFlow::Succeeded(exit_code);
    }

    let recorded = worker.publisher.update(|state| {
        if attempt_result.high_demand {
            state.high_demand_count += 1;
            state.last_error_snippet = get_snippet(HIGH_DEMAND_MSG, 280);
        } else {
            state.last_error_snippet = attempt_result.preview;
        }
    });
    if let Err(error) = recorded {
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }

    match worker.publisher.has_remaining_attempts() {
        Ok(false) => {
            return AttemptFlow::Terminal(LoopOutcome::Failed(format!(
                "命令未成功且已达到最大尝试次数 {} (exit={exit_code})",
                worker.context.options.max_tries
            )));
        }
        Ok(true) => {}
        Err(error) => return AttemptFlow::Terminal(LoopOutcome::Failed(error)),
    }

    if let Err(error) = worker.publisher.update(|state| state.retry_count += 1) {
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }
    let retry_message = if attempt_result.high_demand {
        format!(
            "检测到高负载，{} 秒后重试",
            worker.context.options.interval_seconds
        )
    } else {
        format!(
            "命令异常退出 (exit={exit_code})，{} 秒后重试",
            worker.context.options.interval_seconds
        )
    };
    if let Err(error) = worker.write_system(&retry_message).await {
        return AttemptFlow::Terminal(LoopOutcome::Failed(error));
    }
    match wait_for_retry(worker).await {
        Ok(()) => AttemptFlow::Retry,
        Err(outcome) => AttemptFlow::Terminal(outcome),
    }
}

fn check_interrupts(worker: &WorkerContext<'_>) -> Option<LoopOutcome> {
    if worker.token.is_cancelled() {
        return Some(LoopOutcome::Stopped("用户停止了任务".to_string()));
    }
    match is_stop_requested(&worker.context.paths, worker.lease.run_id()) {
        Ok(true) => Some(LoopOutcome::Stopped(
            "收到匹配 run ID 的远程停止请求".to_string(),
        )),
        Ok(false) => None,
        Err(error) => Some(LoopOutcome::Failed(error)),
    }
}

async fn run_child(
    worker: &WorkerContext<'_>,
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

    let cancellation = worker.token.clone();
    let mut remote_poll = tokio::time::interval(Duration::from_millis(REMOTE_STOP_POLL_MS));
    remote_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut capture = AttemptCapture::default();

    while !(stdout_done && stderr_done) {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                return Err(LoopOutcome::Stopped("用户停止了任务".to_string()));
            }
            _ = remote_poll.tick() => {
                match is_stop_requested(&worker.context.paths, worker.lease.run_id()) {
                    Ok(true) => {
                        let _ = child.kill().await;
                        worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Stopped("收到匹配 run ID 的远程停止请求".to_string()));
                    }
                    Ok(false) => {}
                    Err(error) => {
                        let _ = child.kill().await;
                        worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Failed(error));
                    }
                }
            }
            event = receiver.recv() => {
                match event {
                    Some(StreamEvent::Chunk(source, bytes)) => {
                        capture.push_raw(source, &bytes);
                        match worker.write_output(source, &bytes).await {
                            Ok(fragments) => capture.push_normalized(&fragments),
                            Err(error) => {
                                let _ = child.kill().await;
                                worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                                return Err(LoopOutcome::Failed(error));
                            }
                        }
                    }
                    Some(StreamEvent::Done(source)) => {
                        match worker.finish_output(source).await {
                            Ok(fragments) => capture.push_normalized(&fragments),
                            Err(error) => {
                                let _ = child.kill().await;
                                worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
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
                        worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
                        return Err(LoopOutcome::Failed(format!(
                            "读取 {} 失败: {}",
                            source.label(), error
                        )));
                    }
                    None => {
                        let _ = child.kill().await;
                        worker.set_child_pid(None).map_err(LoopOutcome::Failed)?;
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

async fn wait_for_retry(worker: &WorkerContext<'_>) -> Result<(), LoopOutcome> {
    let cancellation = worker.token.clone();
    let delay = tokio::time::sleep(Duration::from_secs(worker.context.options.interval_seconds));
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
                match is_stop_requested(&worker.context.paths, worker.lease.run_id()) {
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

async fn wait_for_keep_alive(worker: &WorkerContext<'_>) -> Result<(), LoopOutcome> {
    let context = worker.context;
    let lease = worker.lease;
    let cancellation = worker.token.clone();
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

/// 抢跑阶段的汇总结果。
#[derive(Debug)]
enum RaceOutcome {
    Succeeded(i32),
    Failed(String),
    Stopped(String),
}

/// 单个并发 worker 的结束原因。
#[derive(Debug)]
enum WorkerOutcome {
    Succeeded(i32),
    Failed(String),
    Stopped(String),
    /// 全局尝试额度已被兄弟 worker 用尽，本 worker 一次都没跑成。
    Exhausted(String),
    /// 被抢先完成的兄弟 worker 取消，不参与终态判定。
    Superseded,
}

/// 单次尝试的走向。
#[derive(Debug)]
enum AttemptFlow {
    Succeeded(i32),
    /// 已完成失败记账与重试等待，调用方应继续循环。
    Retry,
    Terminal(LoopOutcome),
}

/// 一个 worker 的执行上下文。`token` 在抢跑阶段是 lease token 的 child token，
/// 因此用户停止会级联到全部 worker，而胜者只取消 child token。
struct WorkerContext<'a> {
    context: &'a RunContext,
    lease: &'a RunLease,
    publisher: &'a StatusPublisher,
    log: &'a AsyncMutex<LogSink>,
    token: CancellationToken,
    worker_id: usize,
}

impl WorkerContext<'_> {
    async fn write_system(&self, message: &str) -> Result<(), String> {
        self.log
            .lock()
            .await
            .write_system(Some(self.worker_id), message)
            .await
    }

    async fn write_output(
        &self,
        source: StreamSource,
        bytes: &[u8],
    ) -> Result<Vec<NormalizedFragment>, String> {
        self.log
            .lock()
            .await
            .write_output(self.worker_id, source, bytes)
            .await
    }

    async fn finish_output(&self, source: StreamSource) -> Result<Vec<NormalizedFragment>, String> {
        self.log
            .lock()
            .await
            .finish_output(self.worker_id, source)
            .await
    }

    async fn flush(&self) -> Result<(), String> {
        self.log.lock().await.flush().await
    }

    fn set_child_pid(&self, child_pid: Option<u32>) -> Result<(), String> {
        self.lease.set_worker_child_pid(self.worker_id, child_pid)?;
        self.publisher.set_worker_pid(self.worker_id, child_pid)
    }

    /// 抢跑阶段的取消可能来自用户停止，也可能来自兄弟 worker 胜出。
    /// 只有 lease token 被取消才是真正的“已停止”。
    fn worker_outcome(&self, outcome: LoopOutcome) -> WorkerOutcome {
        match outcome {
            // 抢跑阶段不会进入保活等待，因此这里拿不到 Success；保留为可见的诊断而非静默丢弃。
            LoopOutcome::Success(message) => {
                WorkerOutcome::Failed(format!("抢跑线程收到意外的成功信号: {message}"))
            }
            LoopOutcome::Failed(message) => WorkerOutcome::Failed(message),
            LoopOutcome::Stopped(message) => {
                if self.lease.cancellation_token().is_cancelled() {
                    WorkerOutcome::Stopped(message)
                } else {
                    WorkerOutcome::Superseded
                }
            }
        }
    }
}

struct AttemptResult {
    exit_code: Option<i32>,
    high_demand: bool,
    preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    label_workers: bool,
    framers: HashMap<(usize, StreamSource), LineFramer>,
    notice_throttle: LogNoticeThrottle,
}

impl LogSink {
    async fn open(
        run_id: &str,
        full_path: &Path,
        latest_path: &Path,
        app: Option<AppHandle>,
        label_workers: bool,
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
            label_workers,
            framers: HashMap::new(),
            notice_throttle: LogNoticeThrottle::default(),
        })
    }

    /// 单线程运行时保持改造前的记录格式；并发运行时才加 `[wN]` 线程标记。
    fn worker_prefix(&self, worker_id: Option<usize>) -> String {
        match worker_id {
            Some(worker_id) if self.label_workers => format!("[w{worker_id}] "),
            _ => String::new(),
        }
    }

    async fn write_system(
        &mut self,
        worker_id: Option<usize>,
        message: &str,
    ) -> Result<(), String> {
        let line = format!(
            "[{}] {}[launcher] {}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            self.worker_prefix(worker_id),
            message
        );
        self.write_record(line.as_bytes()).await
    }

    async fn write_output(
        &mut self,
        worker_id: usize,
        source: StreamSource,
        bytes: &[u8],
    ) -> Result<Vec<NormalizedFragment>, String> {
        let framed = self.framer_mut(worker_id, source).push(bytes);
        self.write_framed(worker_id, source, framed).await
    }

    async fn finish_output(
        &mut self,
        worker_id: usize,
        source: StreamSource,
    ) -> Result<Vec<NormalizedFragment>, String> {
        let framed = self
            .framer_mut(worker_id, source)
            .finish()
            .into_iter()
            .collect();
        self.write_framed(worker_id, source, framed).await
    }

    /// 每个 (worker, 流) 各自持有 framer，避免并发 worker 的半行互相污染。
    fn framer_mut(&mut self, worker_id: usize, source: StreamSource) -> &mut LineFramer {
        self.framers.entry((worker_id, source)).or_default()
    }

    async fn write_framed(
        &mut self,
        worker_id: usize,
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
                "[{}] {}[{}] {}{}\n",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                self.worker_prefix(Some(worker_id)),
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
    use crate::notifications::{NotificationEvent, NotificationEventType, NotificationSink};
    use crate::run_manager::StopTarget;
    use std::process::{Child, Command as StdCommand, Stdio as StdStdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
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

    struct FailingNotificationSink {
        status_file: PathBuf,
        observed_status: Arc<Mutex<Option<RunStatus>>>,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl NotificationSink for FailingNotificationSink {
        async fn notify(&self, event: NotificationEvent) -> Result<(), String> {
            assert_eq!(event.event_type, NotificationEventType::RunSucceeded);
            let status: TaskStatus = read_json(&self.status_file).expect("read terminal status");
            *self.observed_status.lock().expect("observed status mutex") = Some(status.status);
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err("intentional notification delivery failure".to_string())
        }
    }

    #[derive(Default)]
    struct RecordingNotificationSink {
        events: Mutex<Vec<NotificationEvent>>,
    }

    #[async_trait::async_trait]
    impl NotificationSink for RecordingNotificationSink {
        async fn notify(&self, event: NotificationEvent) -> Result<(), String> {
            self.events
                .lock()
                .expect("recorded notification events")
                .push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn notification_failure_does_not_change_terminal_status_and_sees_persisted_status() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let observed_status = Arc::new(Mutex::new(None));
        let sink = Arc::new(FailingNotificationSink {
            status_file: paths.status_file.clone(),
            observed_status: observed_status.clone(),
            calls: AtomicUsize::new(0),
        });

        let started = start_run(
            None,
            Arc::new(RunManager::new()),
            paths.clone(),
            options(temp.path(), "exit /b 0", 1).with_notification_sink(sink.clone()),
        )
        .await
        .expect("start successful run");
        let status = started.wait().await.expect("wait for terminal status");

        assert_eq!(status.status, RunStatus::Success);
        assert_eq!(
            read_json::<TaskStatus>(&paths.status_file)
                .expect("read persisted terminal status")
                .status,
            RunStatus::Success
        );
        assert_eq!(
            *observed_status.lock().expect("observed status mutex"),
            Some(RunStatus::Success)
        );
        assert_eq!(sink.calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn retry_keep_alive_notifies_only_on_the_first_successful_attempt() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let sink = Arc::new(RecordingNotificationSink::default());
        let started = start_run(
            None,
            manager.clone(),
            paths.clone(),
            options(temp.path(), "exit /b 0", 0)
                .with_keep_alive(true, Duration::from_millis(40))
                .with_notification_sink(sink.clone()),
        )
        .await
        .expect("start retry keep-alive run");
        let run_id = started.run_id().to_string();
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            if let Ok(status) = read_json::<TaskStatus>(&paths.status_file) {
                if status.attempt >= 3 {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "run did not complete multiple successful keep-alive attempts"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        manager
            .set_keep_alive(&paths, &run_id, false)
            .expect("disable retry keep-alive");
        let status = tokio::time::timeout(Duration::from_secs(2), started.wait())
            .await
            .expect("retry keep-alive should stop promptly")
            .expect("wait for terminal status");

        assert_eq!(status.status, RunStatus::Success);
        let events = sink.events.lock().expect("recorded notification events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, NotificationEventType::RunSucceeded);
        assert_eq!(events[0].run_mode, Some(RunMode::Retry));
        assert_eq!(events[0].attempt, 1);
    }

    #[tokio::test]
    async fn first_retry_success_records_exactly_one_retry() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let command = "if exist first-attempt-failed.marker (exit /b 0) else (echo failed>first-attempt-failed.marker & exit /b 1)";

        let status = start_run(
            None,
            Arc::new(RunManager::new()),
            paths,
            options(temp.path(), command, 2),
        )
        .await
        .expect("start retrying command")
        .wait()
        .await
        .expect("wait for first retry success");

        assert_eq!(status.status, RunStatus::Success, "{}", status.message);
        assert_eq!(status.attempt, 2);
        assert_eq!(status.retry_count, 1);
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
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None, false)
            .await
            .expect("open log sink");

        let _ = sink
            .write_output(FIRST_WORKER_ID, StreamSource::Stdout, b"first\nsecond\n")
            .await
            .expect("write output");
        sink.flush().await.expect("flush output");

        let log = fs::read_to_string(full_path).expect("read normalized log");
        assert_eq!(log.matches("[stdout]").count(), 2, "{log}");
        assert_eq!(log.lines().count(), 2, "{log}");
        assert!(!log.contains("[w"), "单线程日志必须保持改造前的格式: {log}");
    }

    #[tokio::test]
    async fn utf8_character_split_across_reads_is_preserved() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let full_path = temp.path().join("full.log");
        let latest_path = temp.path().join("latest.log");
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None, false)
            .await
            .expect("open log sink");
        let chinese = "中文".as_bytes();

        let _ = sink
            .write_output(FIRST_WORKER_ID, StreamSource::Stdout, &chinese[..1])
            .await
            .expect("write first byte chunk");
        let _ = sink
            .write_output(FIRST_WORKER_ID, StreamSource::Stdout, &chinese[1..])
            .await
            .expect("write remaining byte chunk");
        let _ = sink
            .finish_output(FIRST_WORKER_ID, StreamSource::Stdout)
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
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None, false)
            .await
            .expect("open log sink");

        let _ = sink
            .write_output(FIRST_WORKER_ID, StreamSource::Stdout, &[0xD6, 0xD0, b'\n'])
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
    async fn race_first_success_wins_and_cancels_the_remaining_workers() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        // 第一个抢到 marker 的线程立刻成功；其余线程会挂住 30 秒，只能靠抢跑取消收敛。
        let command = "if exist winner.marker (ping -n 30 127.0.0.1 >nul & exit /b 1) \
             else (echo won>winner.marker & exit /b 0)";
        let started = start_run(
            None,
            Arc::new(RunManager::new()),
            paths,
            options(temp.path(), command, 0).with_concurrency(3),
        )
        .await
        .expect("start racing run");

        let status = tokio::time::timeout(Duration::from_secs(20), started.wait())
            .await
            .expect("抢跑胜者必须立即终止其余线程，而不是等待 30 秒")
            .expect("wait for terminal status");

        assert_eq!(status.status, RunStatus::Success, "{}", status.message);
        assert_eq!(status.concurrency, 3);
        assert_eq!(status.active_workers, 0);
        assert!(status.child_pids.is_empty());
        let log = fs::read_to_string(&status.log_file).expect("read run log");
        assert!(log.contains("[w1]"), "并发日志必须带线程标记: {log}");
    }

    #[tokio::test]
    async fn race_success_notifies_exactly_once_even_when_workers_tie() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let sink = Arc::new(RecordingNotificationSink::default());
        let status = start_run(
            None,
            Arc::new(RunManager::new()),
            paths,
            options(temp.path(), "exit /b 0", 0)
                .with_concurrency(4)
                .with_notification_sink(sink.clone()),
        )
        .await
        .expect("start racing run")
        .wait()
        .await
        .expect("wait for terminal status");

        assert_eq!(status.status, RunStatus::Success, "{}", status.message);
        let events = sink.events.lock().expect("recorded notification events");
        assert_eq!(events.len(), 1, "同时成功的线程不能重复发送通知");
        assert_eq!(events[0].event_type, NotificationEventType::RunSucceeded);
    }

    #[tokio::test]
    async fn attempt_budget_is_shared_across_concurrent_workers() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let status = start_run(
            None,
            Arc::new(RunManager::new()),
            paths,
            options(temp.path(), "exit /b 7", 4).with_concurrency(3),
        )
        .await
        .expect("start failing racing run")
        .wait()
        .await
        .expect("wait for attempt limit");

        assert_eq!(status.status, RunStatus::Failed);
        assert_eq!(
            status.attempt, 4,
            "最大尝试次数是所有线程累计，而不是每个线程各算一份"
        );
        assert!(status.message.contains("最大尝试次数"));
    }

    #[tokio::test]
    async fn keep_alive_phase_falls_back_to_a_single_worker() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let started = start_run(
            None,
            manager.clone(),
            paths.clone(),
            options(temp.path(), "echo keep-alive-success", 0)
                .with_concurrency(3)
                .with_keep_alive(true, Duration::from_secs(60)),
        )
        .await
        .expect("start racing keep-alive run");
        let run_id = started.run_id().to_string();
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            if let Ok(status) = read_json::<TaskStatus>(&paths.status_file) {
                if status.message.contains("保活已开启") {
                    assert_eq!(
                        status.active_workers, 1,
                        "保活阶段必须固定单线程: {}",
                        status.message
                    );
                    assert_eq!(status.concurrency, 3);
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "run did not enter the keep-alive wait"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        manager
            .set_keep_alive(&paths, &run_id, false)
            .expect("disable keep-alive");
        let status = tokio::time::timeout(Duration::from_secs(5), started.wait())
            .await
            .expect("keep-alive wait should end immediately")
            .expect("wait for terminal status");
        assert_eq!(status.status, RunStatus::Success);
    }

    #[test]
    fn manual_keep_alive_ignores_the_configured_concurrency() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let run_options = options(temp.path(), "echo manual", 0)
            .with_concurrency(8)
            .with_run_mode(RunMode::ManualKeepAlive);

        assert_eq!(run_options.effective_concurrency(), 1);
        assert_eq!(
            options(temp.path(), "echo retry", 0)
                .with_concurrency(8)
                .effective_concurrency(),
            8
        );
    }

    #[test]
    fn concurrency_is_validated_against_the_supported_range() {
        let temp = tempfile::tempdir().expect("create temp dir");

        for invalid in [0, MAX_CONCURRENCY + 1] {
            assert!(options(temp.path(), "echo x", 1)
                .with_concurrency(invalid)
                .validate()
                .expect_err("out-of-range concurrency must fail")
                .contains("并发线程数"));
        }
        options(temp.path(), "echo x", 1)
            .with_concurrency(MAX_CONCURRENCY)
            .validate()
            .expect("upper bound concurrency is accepted");
    }

    #[tokio::test]
    async fn interleaved_worker_output_keeps_logical_lines_separate() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let full_path = temp.path().join("full.log");
        let latest_path = temp.path().join("latest.log");
        let mut sink = LogSink::open("run-a", &full_path, &latest_path, None, true)
            .await
            .expect("open concurrent log sink");

        let _ = sink
            .write_output(1, StreamSource::Stdout, b"worker-one-")
            .await
            .expect("write w1 partial line");
        let _ = sink
            .write_output(2, StreamSource::Stdout, b"worker-two-")
            .await
            .expect("write w2 partial line");
        let _ = sink
            .write_output(2, StreamSource::Stdout, b"tail\n")
            .await
            .expect("finish w2 line");
        let _ = sink
            .write_output(1, StreamSource::Stdout, b"tail\n")
            .await
            .expect("finish w1 line");
        sink.flush().await.expect("flush output");

        let log = fs::read_to_string(&full_path).expect("read concurrent log");
        assert!(log.contains("[w1] [stdout] worker-one-tail"), "{log}");
        assert!(log.contains("[w2] [stdout] worker-two-tail"), "{log}");
        assert_eq!(log.lines().count(), 2, "{log}");
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
