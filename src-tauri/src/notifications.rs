use crate::app_storage::{append_bounded_text_log, AppPaths};
use crate::config_manager::ServerChanConfig;
use crate::credential_store::CredentialStore;
use crate::retry_engine::{RunMode, RunStatus, TaskStatus};
use crate::server_chan::{validate_send_key, DeliveryReceipt, ServerChanClient};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::Arc;

const NOTIFICATION_LOG_MAX_BYTES: usize = 1024 * 1024;
const MAX_EVENT_MESSAGE_CHARS: usize = 800;

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

    fn test() -> Self {
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
            message: "Server酱个人微信通知配置有效".to_string(),
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

#[derive(Clone)]
pub struct NotificationService {
    paths: AppPaths,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServerChanClient>,
}

impl NotificationService {
    pub fn production(paths: AppPaths, credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            paths,
            credentials,
            client: Arc::new(ServerChanClient::production()),
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
        }
    }

    pub fn run_sink(&self, settings: ServerChanConfig) -> Arc<dyn NotificationSink> {
        Arc::new(ServerChanRunSink {
            settings,
            paths: self.paths.clone(),
            credentials: self.credentials.clone(),
            client: self.client.clone(),
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
        let event = NotificationEvent::test();
        let send_key = read_send_key(self.credentials.clone())
            .await?
            .ok_or_else(|| "尚未配置 Server酱 SendKey".to_string())?;
        match deliver_event(&self.client, &send_key, &event).await {
            Ok(receipt) => {
                record_delivery(&self.paths, &event, &Ok(receipt.clone()));
                Ok(format!(
                    "测试通知发送成功（HTTP 尝试 {} 次）",
                    receipt.attempts
                ))
            }
            Err(error) => {
                record_delivery(&self.paths, &event, &Err(error.clone()));
                Err(error)
            }
        }
    }

    pub async fn notify_start_failure(&self, settings: ServerChanConfig, message: &str) {
        let sink = self.run_sink(settings);
        let _ = sink.notify(NotificationEvent::start_failed(message)).await;
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
                record_delivery(&self.paths, &event, &Err(error.clone()));
                return Err(error);
            }
            Err(error) => {
                record_delivery(&self.paths, &event, &Err(error.clone()));
                return Err(error);
            }
        };

        let result = deliver_event(&self.client, &send_key, &event).await;
        record_delivery(&self.paths, &event, &result);
        result.map(|_| ())
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

fn record_delivery(
    paths: &AppPaths,
    event: &NotificationEvent,
    result: &Result<DeliveryReceipt, String>,
) {
    let outcome = match result {
        Ok(receipt) => format!("success attempts={}", receipt.attempts),
        Err(error) => format!("failed error={}", bound_text(error, 240)),
    };
    let entry = format!(
        "[{}] eventId={} type={} {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
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

    fn terminal_status(root: &std::path::Path, status: RunStatus) -> TaskStatus {
        TaskStatus {
            run_id: "run-123".to_string(),
            owner_pid: 1,
            child_pid: None,
            status,
            run_mode: RunMode::Retry,
            keep_alive_enabled: false,
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
        let sink = service.run_sink(settings);

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
        assert!(service.run_sink(settings).notify(event).await.is_err());

        let log = std::fs::read_to_string(&paths.notifications_log).expect("notification log");
        assert!(log.contains("尚未配置 Server酱 SendKey"));
    }
}
