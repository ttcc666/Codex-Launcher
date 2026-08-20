use crate::app_storage::{append_bounded_text_log, AppPaths};
use crate::config_manager::{DesktopNotificationConfig, EmailNotificationConfig, ServerChanConfig};
use crate::credential_store::CredentialStore;
use crate::email_notifier::{deliver_email, get_email_password};
use crate::retry_engine::{RunMode, RunStatus, TaskStatus};
use crate::server_chan::{validate_send_key, DeliveryReceipt, ServerChanClient};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
#[cfg(windows)]
use std::path::Path;
use std::sync::Arc;

const NOTIFICATION_LOG_MAX_BYTES: usize = 1024 * 1024;
const MAX_EVENT_MESSAGE_CHARS: usize = 800;
const MAX_DESKTOP_BODY_CHARS: usize = 360;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerChanCredentialStatus {
    pub configured: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NotificationEventType {
    RunSucceeded,
    RunFailed,
    RunStopped,
    StartFailed,
    Test,
}

impl NotificationEventType {
    fn key(self) -> &'static str {
        match self {
            Self::RunSucceeded => "runSucceeded",
            Self::RunFailed => "runFailed",
            Self::RunStopped => "runStopped",
            Self::StartFailed => "startFailed",
            Self::Test => "test",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::RunSucceeded => "[Codex Launcher] 重试流程首次成功",
            Self::RunFailed => "[Codex Launcher] 任务失败",
            Self::RunStopped => "[Codex Launcher] 任务已停止",
            Self::StartFailed => "[Codex Launcher] 任务启动失败",
            Self::Test => "[Codex Launcher] 测试通知",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEvent {
    pub schema_version: u32,
    pub event_id: String,
    pub event_type: NotificationEventType,
    pub occurred_at: String,
    pub run_id: String,
    pub run_mode: Option<RunMode>,
    pub status: RunStatus,
    pub attempt: u64,
    pub retry_count: u64,
    pub max_tries: u64,
    pub exit_code: Option<i32>,
    pub high_demand_count: u64,
    pub duration_seconds: u64,
    pub message: String,
}

impl NotificationEvent {
    pub fn from_terminal(status: &TaskStatus) -> Self {
        let event_type = match status.status {
            RunStatus::Success => NotificationEventType::RunSucceeded,
            RunStatus::Failed => NotificationEventType::RunFailed,
            RunStatus::Stopped => NotificationEventType::RunStopped,
            RunStatus::Starting | RunStatus::Running => NotificationEventType::RunFailed,
        };
        let duration_seconds = elapsed_seconds(&status.started_at, &status.updated_at);
        Self {
            schema_version: 1,
            event_id: format!("{}:{}", status.run_id, event_type.key()),
            event_type,
            occurred_at: status.updated_at.clone(),
            run_id: status.run_id.clone(),
            run_mode: Some(status.run_mode),
            status: status.status,
            attempt: status.attempt,
            retry_count: status.retry_count,
            max_tries: status.max_tries,
            exit_code: status.last_exit_code,
            high_demand_count: status.high_demand_count,
            duration_seconds,
            message: bound_text(&status.message, MAX_EVENT_MESSAGE_CHARS),
        }
    }

    pub fn start_failed(message: &str) -> Self {
        let occurred_at = Utc::now().to_rfc3339();
        Self {
            schema_version: 1,
            event_id: format!("start:{}", uuid::Uuid::new_v4().simple()),
            event_type: NotificationEventType::StartFailed,
            occurred_at,
            run_id: "not-started".to_string(),
            run_mode: None,
            status: RunStatus::Failed,
            attempt: 0,
            retry_count: 0,
            max_tries: 0,
            exit_code: None,
            high_demand_count: 0,
            duration_seconds: 0,
            message: bound_text(message, MAX_EVENT_MESSAGE_CHARS),
        }
    }

    fn test(message: &str) -> Self {
        let occurred_at = Utc::now().to_rfc3339();
        Self {
            schema_version: 1,
            event_id: format!("test:{}", uuid::Uuid::new_v4().simple()),
            event_type: NotificationEventType::Test,
            occurred_at,
            run_id: "test".to_string(),
            run_mode: None,
            status: RunStatus::Success,
            attempt: 0,
            retry_count: 0,
            max_tries: 0,
            exit_code: None,
            high_demand_count: 0,
            duration_seconds: 0,
            message: bound_text(message, MAX_EVENT_MESSAGE_CHARS),
        }
    }

    fn title(&self) -> &'static str {
        self.event_type.title()
    }

    fn markdown_description(&self) -> String {
        let run_mode = self.run_mode.map(run_mode_label).unwrap_or("尚未启动");
        let exit_code = self
            .exit_code
            .map_or_else(|| "N/A".to_string(), |code| code.to_string());
        format!(
            "**状态**：{}\n\n- Run ID：`{}`\n- 运行模式：{}\n- Attempt：{} / {}\n- Retry count：{}\n- Exit code：{}\n- High-demand：{}\n- 耗时：{} 秒\n- 时间：{}\n\n**消息**\n\n{}",
            run_status_label(self.status),
            self.run_id,
            run_mode,
            self.attempt,
            max_tries_label(self.max_tries),
            self.retry_count,
            exit_code,
            self.high_demand_count,
            self.duration_seconds,
            self.occurred_at,
            self.message
        )
    }

    fn desktop_description(&self) -> String {
        if self.event_type == NotificationEventType::Test {
            return bound_text(&self.message, MAX_DESKTOP_BODY_CHARS);
        }

        let body = format!(
            "Attempt {} / {} · Retry {} · 耗时 {} 秒\n{}",
            self.attempt,
            max_tries_label(self.max_tries),
            self.retry_count,
            self.duration_seconds,
            self.message
        );
        bound_text(&body, MAX_DESKTOP_BODY_CHARS)
    }
}

#[async_trait]
pub trait NotificationSink: Send + Sync {
    async fn notify(&self, event: NotificationEvent) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct NoopNotificationSink;

#[async_trait]
impl NotificationSink for NoopNotificationSink {
    async fn notify(&self, _event: NotificationEvent) -> Result<(), String> {
        Ok(())
    }
}

#[async_trait]
trait DesktopNotificationTransport: Send + Sync {
    async fn show(&self, title: &str, body: &str) -> Result<(), String>;
}

struct NativeDesktopNotificationTransport {
    app_identifier: String,
}

impl NativeDesktopNotificationTransport {
    fn new(app_identifier: String) -> Self {
        Self { app_identifier }
    }
}

#[async_trait]
impl DesktopNotificationTransport for NativeDesktopNotificationTransport {
    async fn show(&self, title: &str, body: &str) -> Result<(), String> {
        let app_identifier = self.app_identifier.clone();
        let title = title.to_string();
        let body = body.to_string();
        tokio::task::spawn_blocking(move || {
            show_native_desktop_notification(&app_identifier, &title, &body)
        })
        .await
        .map_err(|error| format!("桌面通知 blocking task 失败: {error}"))?
    }
}

#[cfg(test)]
struct DisabledDesktopNotificationTransport;

#[cfg(test)]
#[async_trait]
impl DesktopNotificationTransport for DisabledDesktopNotificationTransport {
    async fn show(&self, _title: &str, _body: &str) -> Result<(), String> {
        Err("测试未配置 desktop notification transport".to_string())
    }
}

#[derive(Clone)]
pub struct NotificationService {
    paths: AppPaths,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServerChanClient>,
    desktop: Arc<dyn DesktopNotificationTransport>,
}

impl NotificationService {
    pub fn production(
        paths: AppPaths,
        credentials: Arc<dyn CredentialStore>,
        app_identifier: String,
    ) -> Self {
        Self {
            paths,
            credentials,
            client: Arc::new(ServerChanClient::production()),
            desktop: Arc::new(NativeDesktopNotificationTransport::new(app_identifier)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_client(
        paths: AppPaths,
        credentials: Arc<dyn CredentialStore>,
        client: Arc<ServerChanClient>,
    ) -> Self {
        Self {
            paths,
            credentials,
            client,
            desktop: Arc::new(DisabledDesktopNotificationTransport),
        }
    }

    #[cfg(test)]
    fn with_transports(
        paths: AppPaths,
        credentials: Arc<dyn CredentialStore>,
        client: Arc<ServerChanClient>,
        desktop: Arc<dyn DesktopNotificationTransport>,
    ) -> Self {
        Self {
            paths,
            credentials,
            client,
            desktop,
        }
    }

    pub fn run_sink(
        &self,
        server_chan_settings: ServerChanConfig,
        desktop_settings: DesktopNotificationConfig,
        email_settings: EmailNotificationConfig,
    ) -> Arc<dyn NotificationSink> {
        Arc::new(NotificationRunSink {
            server_chan: ServerChanRunSink {
                settings: server_chan_settings,
                paths: self.paths.clone(),
                credentials: self.credentials.clone(),
                client: self.client.clone(),
            },
            desktop: DesktopRunSink {
                settings: desktop_settings,
                paths: self.paths.clone(),
                transport: self.desktop.clone(),
            },
            email: EmailRunSink {
                settings: email_settings,
                paths: self.paths.clone(),
            },
        })
    }

    pub async fn credential_status(&self) -> Result<ServerChanCredentialStatus, String> {
        let configured = read_send_key(self.credentials.clone()).await?.is_some();
        Ok(ServerChanCredentialStatus { configured })
    }

    pub async fn set_send_key(
        &self,
        send_key: String,
    ) -> Result<ServerChanCredentialStatus, String> {
        let send_key = validate_send_key(&send_key)?;
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.set_send_key(&send_key))
            .await
            .map_err(|_| "保存 Server酱凭据的 blocking task 失败".to_string())??;
        Ok(ServerChanCredentialStatus { configured: true })
    }

    pub async fn delete_send_key(&self) -> Result<ServerChanCredentialStatus, String> {
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.delete_send_key())
            .await
            .map_err(|_| "删除 Server酱凭据的 blocking task 失败".to_string())??;
        Ok(ServerChanCredentialStatus { configured: false })
    }

    pub async fn test_notification(&self) -> Result<String, String> {
        let event = NotificationEvent::test("Server酱个人微信通知配置有效");
        let send_key = read_send_key(self.credentials.clone())
            .await?
            .ok_or_else(|| "尚未配置 Server酱 SendKey".to_string())?;
        match deliver_event(&self.client, &send_key, &event).await {
            Ok(receipt) => {
                record_server_chan_delivery(&self.paths, &event, &Ok(receipt.clone()));
                Ok(format!(
                    "测试通知发送成功（HTTP 尝试 {} 次）",
                    receipt.attempts
                ))
            }
            Err(error) => {
                record_server_chan_delivery(&self.paths, &event, &Err(error.clone()));
                Err(error)
            }
        }
    }

    pub async fn test_desktop_notification(&self) -> Result<String, String> {
        let event = NotificationEvent::test("桌面通知配置有效，后续首次成功时会自动提醒。");
        let result = self
            .desktop
            .show(event.title(), &event.desktop_description())
            .await;
        record_desktop_delivery(&self.paths, &event, &result);
        result.map(|_| "测试通知已提交至 Windows 通知中心".to_string())
    }

    pub async fn notify_start_failure(&self, settings: ServerChanConfig, message: &str) {
        let sink = self.run_sink(
            settings,
            DesktopNotificationConfig { enabled: false },
            EmailNotificationConfig {
                enabled: false,
                ..Default::default()
            },
        );
        let _ = sink.notify(NotificationEvent::start_failed(message)).await;
    }

    pub async fn test_email_notification(
        &self,
        email_settings: EmailNotificationConfig,
    ) -> Result<String, String> {
        if email_settings.smtp_host.trim().is_empty() {
            return Err("SMTP 服务器地址不能为空".to_string());
        }
        if email_settings.to_address.trim().is_empty() {
            return Err("收件人地址不能为空".to_string());
        }
        let password = tokio::task::spawn_blocking(get_email_password)
            .await
            .map_err(|_| "读取邮件密码 blocking task 失败".to_string())??
            .ok_or_else(|| "尚未配置 SMTP 密码".to_string())?;
        let event = NotificationEvent::test("邮件通知配置有效，后续首次成功时会自动发送邮件");
        let result = deliver_email(
            &email_settings.smtp_host,
            email_settings.smtp_port,
            &email_settings.smtp_username,
            &password,
            &email_settings.to_address,
            event.title(),
            &event.markdown_description(),
        )
        .await;
        record_email_delivery(&self.paths, &event, &result);
        result.map(|_| "测试邮件发送成功".to_string())
    }
}

struct NotificationRunSink {
    server_chan: ServerChanRunSink,
    desktop: DesktopRunSink,
    email: EmailRunSink,
}

#[async_trait]
impl NotificationSink for NotificationRunSink {
    async fn notify(&self, event: NotificationEvent) -> Result<(), String> {
        let server_chan_event = event.clone();
        let email_event = event.clone();
        let (server_chan_result, desktop_result, email_result) = tokio::join!(
            self.server_chan.notify(server_chan_event),
            self.desktop.notify(event),
            self.email.notify(email_event),
        );
        merge_channel_results(server_chan_result, desktop_result, email_result)
    }
}

struct ServerChanRunSink {
    settings: ServerChanConfig,
    paths: AppPaths,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServerChanClient>,
}

#[async_trait]
impl NotificationSink for ServerChanRunSink {
    async fn notify(&self, event: NotificationEvent) -> Result<(), String> {
        if !self.settings.enabled || !should_notify(&event) {
            return Ok(());
        }
        let send_key = match read_send_key(self.credentials.clone()).await {
            Ok(Some(send_key)) => send_key,
            Ok(None) => {
                let error = "通知已启用，但尚未配置 Server酱 SendKey".to_string();
                record_server_chan_delivery(&self.paths, &event, &Err(error.clone()));
                return Err(error);
            }
            Err(error) => {
                record_server_chan_delivery(&self.paths, &event, &Err(error.clone()));
                return Err(error);
            }
        };

        let result = deliver_event(&self.client, &send_key, &event).await;
        record_server_chan_delivery(&self.paths, &event, &result);
        result.map(|_| ())
    }
}

struct DesktopRunSink {
    settings: DesktopNotificationConfig,
    paths: AppPaths,
    transport: Arc<dyn DesktopNotificationTransport>,
}

#[async_trait]
impl NotificationSink for DesktopRunSink {
    async fn notify(&self, event: NotificationEvent) -> Result<(), String> {
        if !self.settings.enabled || !should_notify(&event) {
            return Ok(());
        }

        let result = self
            .transport
            .show(event.title(), &event.desktop_description())
            .await;
        record_desktop_delivery(&self.paths, &event, &result);
        result
    }
}

struct EmailRunSink {
    settings: EmailNotificationConfig,
    paths: AppPaths,
}

#[async_trait]
impl NotificationSink for EmailRunSink {
    async fn notify(&self, event: NotificationEvent) -> Result<(), String> {
        if !self.settings.enabled || !should_notify(&event) {
            return Ok(());
        }
        if self.settings.smtp_host.trim().is_empty()
            || self.settings.smtp_username.trim().is_empty()
            || self.settings.to_address.trim().is_empty()
        {
            let error = "邮件通知已启用，但 SMTP 配置不完整".to_string();
            record_email_delivery(&self.paths, &event, &Err(error.clone()));
            return Err(error);
        }

        let password = match tokio::task::spawn_blocking(get_email_password).await {
            Ok(Ok(Some(pw))) => pw,
            Ok(Ok(None)) => {
                let error = "邮件通知已启用，但尚未配置 SMTP 密码".to_string();
                record_email_delivery(&self.paths, &event, &Err(error.clone()));
                return Err(error);
            }
            Ok(Err(e)) => {
                record_email_delivery(&self.paths, &event, &Err(e.clone()));
                return Err(e);
            }
            Err(_) => {
                let error = "读取邮件密码 blocking task 失败".to_string();
                record_email_delivery(&self.paths, &event, &Err(error.clone()));
                return Err(error);
            }
        };

        let result = deliver_email(
            &self.settings.smtp_host,
            self.settings.smtp_port,
            &self.settings.smtp_username,
            &password,
            &self.settings.to_address,
            event.title(),
            &event.markdown_description(),
        )
        .await;
        record_email_delivery(&self.paths, &event, &result);
        result
    }
}

async fn read_send_key(credentials: Arc<dyn CredentialStore>) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || credentials.get_send_key())
        .await
        .map_err(|_| "读取 Server酱凭据的 blocking task 失败".to_string())?
}

async fn deliver_event(
    client: &ServerChanClient,
    send_key: &str,
    event: &NotificationEvent,
) -> Result<DeliveryReceipt, String> {
    client
        .deliver(send_key, event.title(), &event.markdown_description())
        .await
}

fn should_notify(event: &NotificationEvent) -> bool {
    event.event_type == NotificationEventType::RunSucceeded
        && event.run_mode == Some(RunMode::Retry)
        && event.status == RunStatus::Success
}

fn record_server_chan_delivery(
    paths: &AppPaths,
    event: &NotificationEvent,
    result: &Result<DeliveryReceipt, String>,
) {
    let outcome = match result {
        Ok(receipt) => format!("success attempts={}", receipt.attempts),
        Err(error) => format!("failed error={}", bound_text(error, 240)),
    };
    record_delivery(paths, event, "serverChan", &outcome);
}

fn record_desktop_delivery(
    paths: &AppPaths,
    event: &NotificationEvent,
    result: &Result<(), String>,
) {
    let outcome = match result {
        Ok(()) => "success".to_string(),
        Err(error) => format!("failed error={}", bound_text(error, 240)),
    };
    record_delivery(paths, event, "desktop", &outcome);
}

fn record_email_delivery(paths: &AppPaths, event: &NotificationEvent, result: &Result<(), String>) {
    let outcome = match result {
        Ok(()) => "success".to_string(),
        Err(error) => format!("failed error={}", bound_text(error, 240)),
    };
    record_delivery(paths, event, "email", &outcome);
}

fn record_delivery(paths: &AppPaths, event: &NotificationEvent, channel: &str, outcome: &str) {
    let entry = format!(
        "[{}] channel={} eventId={} type={} {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        channel,
        event.event_id,
        event.event_type.key(),
        outcome
    );
    if let Err(error) =
        append_bounded_text_log(&paths.notifications_log, &entry, NOTIFICATION_LOG_MAX_BYTES)
    {
        eprintln!("写入 notification log 失败: {}", error);
    }
}

fn merge_channel_results(
    server_chan_result: Result<(), String>,
    desktop_result: Result<(), String>,
    email_result: Result<(), String>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = server_chan_result {
        errors.push(format!("Server酱: {error}"));
    }
    if let Err(error) = desktop_result {
        errors.push(format!("桌面通知: {error}"));
    }
    if let Err(error) = email_result {
        errors.push(format!("邮件: {error}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(windows)]
fn show_native_desktop_notification(
    app_identifier: &str,
    title: &str,
    body: &str,
) -> Result<(), String> {
    let mut notification = notify_rust::Notification::new();
    notification.summary(title).body(body);
    if should_use_registered_app_identifier() {
        notification.app_id(app_identifier);
    }
    notification
        .show()
        .map(|_| ())
        .map_err(|error| format!("Windows 桌面通知发送失败: {error}"))
}

#[cfg(not(windows))]
fn show_native_desktop_notification(
    _app_identifier: &str,
    _title: &str,
    _body: &str,
) -> Result<(), String> {
    Err("当前版本仅支持 Windows 桌面通知".to_string())
}

#[cfg(windows)]
fn should_use_registered_app_identifier() -> bool {
    let Some(directory) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return false;
    };
    let debug_directory = Path::new("target").join("debug");
    let release_directory = Path::new("target").join("release");
    !directory.ends_with(debug_directory) && !directory.ends_with(release_directory)
}

fn elapsed_seconds(started_at: &str, updated_at: &str) -> u64 {
    let started = DateTime::parse_from_rfc3339(started_at);
    let updated = DateTime::parse_from_rfc3339(updated_at);
    match (started, updated) {
        (Ok(started), Ok(updated)) => (updated - started).num_seconds().max(0) as u64,
        _ => 0,
    }
}

fn bound_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn run_mode_label(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Retry => "普通重试",
        RunMode::ManualKeepAlive => "手动保活",
    }
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Starting => "启动中",
        RunStatus::Running => "运行中",
        RunStatus::Success => "成功",
        RunStatus::Failed => "失败",
        RunStatus::Stopped => "已停止",
    }
}

fn max_tries_label(max_tries: u64) -> String {
    if max_tries == 0 {
        "无限".to_string()
    } else {
        max_tries.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_store::MemoryCredentialStore;
    use crate::server_chan::{DeliveryError, ServerChanTransport};
    use std::sync::Mutex;

    struct RecordingTransport {
        calls: Mutex<Vec<(String, String, String)>>,
    }

    struct RecordingDesktopTransport {
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl ServerChanTransport for RecordingTransport {
        async fn send(
            &self,
            send_key: &str,
            title: &str,
            description: &str,
        ) -> Result<(), DeliveryError> {
            self.calls.lock().expect("recording calls").push((
                send_key.to_string(),
                title.to_string(),
                description.to_string(),
            ));
            Ok(())
        }
    }

    #[async_trait]
    impl DesktopNotificationTransport for RecordingDesktopTransport {
        async fn show(&self, title: &str, body: &str) -> Result<(), String> {
            self.calls
                .lock()
                .expect("recording desktop calls")
                .push((title.to_string(), body.to_string()));
            Ok(())
        }
    }

    fn terminal_status(root: &std::path::Path, status: RunStatus) -> TaskStatus {
        TaskStatus {
            run_id: "run-123".to_string(),
            owner_pid: 1,
            child_pid: None,
            child_pids: Vec::new(),
            status,
            run_mode: RunMode::Retry,
            keep_alive_enabled: false,
            concurrency: 1,
            active_workers: 0,
            message: "terminal message".to_string(),
            command: "secret command".to_string(),
            work_dir: root.to_string_lossy().to_string(),
            log_file: root.join("secret.log").to_string_lossy().to_string(),
            latest_log: root.join("latest.log").to_string_lossy().to_string(),
            attempt: 3,
            retry_count: 2,
            high_demand_count: 1,
            max_tries: 3,
            interval_seconds: 10,
            progress_percent: 100.0,
            last_exit_code: Some(1),
            last_error_snippet: "secret preview".to_string(),
            result_preview: "secret output".to_string(),
            started_at: "2026-08-01T10:00:00Z".to_string(),
            updated_at: "2026-08-01T10:01:30Z".to_string(),
        }
    }

    fn retry_success_status(root: &std::path::Path, attempt: u64) -> TaskStatus {
        let mut status = terminal_status(root, RunStatus::Success);
        status.attempt = attempt;
        status.retry_count = attempt.saturating_sub(1);
        status.high_demand_count = 0;
        status.last_exit_code = Some(0);
        status
    }

    #[test]
    fn terminal_event_is_stable_and_excludes_command_paths_and_output() {
        let temp = tempfile::tempdir().expect("temp dir");
        let event =
            NotificationEvent::from_terminal(&terminal_status(temp.path(), RunStatus::Failed));
        let json = serde_json::to_string(&event).expect("serialize event");

        assert_eq!(event.event_id, "run-123:runFailed");
        assert_eq!(event.duration_seconds, 90);
        assert_eq!(event.retry_count, 2);
        assert!(!json.contains("secret command"));
        assert!(!json.contains("secret.log"));
        assert!(!json.contains("secret output"));
        let desktop_body = event.desktop_description();
        assert!(!desktop_body.contains("secret command"));
        assert!(!desktop_body.contains("secret.log"));
        assert!(!desktop_body.contains("secret output"));
    }

    #[tokio::test]
    async fn retry_flow_success_is_sent_regardless_of_attempt_number() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("app dirs");
        let credentials = Arc::new(MemoryCredentialStore::with_send_key("SCT_TEST_KEY"));
        let transport = Arc::new(RecordingTransport {
            calls: Mutex::new(Vec::new()),
        });
        let client = Arc::new(ServerChanClient::with_transport(transport.clone()));
        let service = NotificationService::with_client(paths, credentials, client);
        let settings = ServerChanConfig { enabled: true };
        let sink = service.run_sink(
            settings,
            DesktopNotificationConfig { enabled: false },
            EmailNotificationConfig::default(),
        );

        for attempt in [1, 2, 3] {
            let mut success = retry_success_status(temp.path(), attempt);
            success.run_id = format!("run-{attempt}");
            sink.notify(NotificationEvent::from_terminal(&success))
                .await
                .expect("retry flow success notification is sent");
        }

        let mut manual_keep_alive_success = retry_success_status(temp.path(), 1);
        manual_keep_alive_success.run_mode = RunMode::ManualKeepAlive;

        for skipped in [
            manual_keep_alive_success,
            terminal_status(temp.path(), RunStatus::Failed),
            terminal_status(temp.path(), RunStatus::Stopped),
        ] {
            sink.notify(NotificationEvent::from_terminal(&skipped))
                .await
                .expect("non-matching notification is skipped");
        }

        let calls = transport.calls.lock().expect("recorded calls");
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "SCT_TEST_KEY");
        for call in calls.iter() {
            assert!(call.1.contains("重试流程首次成功"));
            assert!(!call.2.contains("secret command"));
        }
    }

    #[tokio::test]
    async fn missing_credential_is_diagnostic_not_a_secret_leak() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("app dirs");
        let transport = Arc::new(RecordingTransport {
            calls: Mutex::new(Vec::new()),
        });
        let client = Arc::new(ServerChanClient::with_transport(transport));
        let service = NotificationService::with_client(
            paths.clone(),
            Arc::new(MemoryCredentialStore::default()),
            client,
        );
        let settings = ServerChanConfig { enabled: true };

        let event = NotificationEvent::from_terminal(&retry_success_status(temp.path(), 1));
        assert!(service
            .run_sink(
                settings,
                DesktopNotificationConfig { enabled: false },
                EmailNotificationConfig::default(),
            )
            .notify(event)
            .await
            .is_err());

        let log = std::fs::read_to_string(&paths.notifications_log).expect("notification log");
        assert!(log.contains("尚未配置 Server酱 SendKey"));
    }

    #[tokio::test]
    async fn desktop_retry_success_is_independent_and_sent_for_any_success_attempt() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("app dirs");
        let server_transport = Arc::new(RecordingTransport {
            calls: Mutex::new(Vec::new()),
        });
        let client = Arc::new(ServerChanClient::with_transport(server_transport.clone()));
        let desktop_transport = Arc::new(RecordingDesktopTransport {
            calls: Mutex::new(Vec::new()),
        });
        let service = NotificationService::with_transports(
            paths,
            Arc::new(MemoryCredentialStore::default()),
            client,
            desktop_transport.clone(),
        );
        let sink = service.run_sink(
            ServerChanConfig { enabled: false },
            DesktopNotificationConfig { enabled: true },
            EmailNotificationConfig::default(),
        );

        for attempt in [1, 2, 3] {
            let mut success = retry_success_status(temp.path(), attempt);
            success.run_id = format!("desktop-run-{attempt}");
            sink.notify(NotificationEvent::from_terminal(&success))
                .await
                .expect("desktop retry success notification is sent");
        }

        let mut manual_keep_alive_success = retry_success_status(temp.path(), 1);
        manual_keep_alive_success.run_mode = RunMode::ManualKeepAlive;
        for skipped in [
            manual_keep_alive_success,
            terminal_status(temp.path(), RunStatus::Failed),
            terminal_status(temp.path(), RunStatus::Stopped),
        ] {
            sink.notify(NotificationEvent::from_terminal(&skipped))
                .await
                .expect("non-matching desktop notification is skipped");
        }

        assert!(server_transport
            .calls
            .lock()
            .expect("server calls")
            .is_empty());
        let calls = desktop_transport.calls.lock().expect("desktop calls");
        assert_eq!(calls.len(), 3);
        for (title, body) in calls.iter() {
            assert!(title.contains("重试流程首次成功"));
            assert!(body.contains("Attempt"));
            assert!(!body.contains("secret command"));
        }
    }

    #[tokio::test]
    async fn desktop_delivery_still_runs_when_server_chan_is_misconfigured() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("app dirs");
        let server_transport = Arc::new(RecordingTransport {
            calls: Mutex::new(Vec::new()),
        });
        let desktop_transport = Arc::new(RecordingDesktopTransport {
            calls: Mutex::new(Vec::new()),
        });
        let service = NotificationService::with_transports(
            paths,
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(ServerChanClient::with_transport(server_transport)),
            desktop_transport.clone(),
        );
        let event = NotificationEvent::from_terminal(&retry_success_status(temp.path(), 1));

        let result = service
            .run_sink(
                ServerChanConfig { enabled: true },
                DesktopNotificationConfig { enabled: true },
                EmailNotificationConfig::default(),
            )
            .notify(event)
            .await;

        assert!(result.is_err());
        assert_eq!(
            desktop_transport.calls.lock().expect("desktop calls").len(),
            1
        );
    }
}
