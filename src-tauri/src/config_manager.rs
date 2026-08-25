use crate::app_storage::{atomic_write_bytes, atomic_write_json, AppPaths};
use crate::schedule::ScheduleConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

pub const CURRENT_CONFIG_VERSION: u32 = 2;
pub const MAX_INTERVAL_SECONDS: u64 = 86_400;
pub const MAX_TRIES_LIMIT: u64 = 100_000;
pub const MAX_KEEP_ALIVE_INTERVAL_MINUTES: u64 = 24 * 60;
pub const MAX_CONCURRENCY: u64 = 16;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerChanConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DesktopNotificationConfig {
    pub enabled: bool,
}

impl Default for DesktopNotificationConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct EmailNotificationConfig {
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub to_address: String,
}

impl EmailNotificationConfig {
    pub fn default_port() -> u16 {
        465
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    pub config_version: u32,
    pub command: String,
    pub work_dir: String,
    pub interval: u64,
    pub max_tries: u64,
    pub concurrency: u64,
    pub task_name: String,
    pub schedule: ScheduleConfig,
    pub allowed_base_urls: String,
    pub keep_alive: bool,
    pub keep_alive_interval_minutes: u64,
    pub desktop_notification: DesktopNotificationConfig,
    pub server_chan: ServerChanConfig,
    pub email: EmailNotificationConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            command: String::new(),
            work_dir: String::new(),
            interval: 10,
            max_tries: 0,
            concurrency: 1,
            task_name: "CodexDailyRetry0840".to_string(),
            schedule: ScheduleConfig::default(),
            allowed_base_urls: String::new(),
            keep_alive: false,
            keep_alive_interval_minutes: 5,
            desktop_notification: DesktopNotificationConfig::default(),
            server_chan: ServerChanConfig::default(),
            email: EmailNotificationConfig {
                smtp_port: EmailNotificationConfig::default_port(),
                ..Default::default()
            },
        }
    }
}

impl AppConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.config_version != CURRENT_CONFIG_VERSION {
            return Err(format!(
                "不支持的配置版本 {}，当前支持版本为 {}",
                self.config_version, CURRENT_CONFIG_VERSION
            ));
        }
        if self.command.trim().is_empty() {
            return Err("command 不能为空".to_string());
        }

        let work_dir = Path::new(self.work_dir.trim());
        if self.work_dir.trim().is_empty() {
            return Err("工作目录不能为空".to_string());
        }
        if !work_dir.is_dir() {
            return Err(format!("工作目录不存在或不是目录: {}", work_dir.display()));
        }

        if !(1..=MAX_INTERVAL_SECONDS).contains(&self.interval) {
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
        if !(1..=MAX_KEEP_ALIVE_INTERVAL_MINUTES).contains(&self.keep_alive_interval_minutes) {
            return Err(format!(
                "保活间隔必须在 1..={} 分钟之间",
                MAX_KEEP_ALIVE_INTERVAL_MINUTES
            ));
        }

        validate_task_name(&self.task_name)?;
        self.schedule.validate()?;

        for candidate in split_allowed_urls(&self.allowed_base_urls) {
            parse_base_url(candidate)?;
        }

        Ok(())
    }
}

pub fn validate_task_name(task_name: &str) -> Result<(), String> {
    let task_name = task_name.trim();
    if task_name.is_empty() {
        return Err("计划任务名称不能为空".to_string());
    }
    if task_name.chars().count() > 238 {
        return Err("计划任务名称不能超过 238 个字符".to_string());
    }
    if task_name.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '&' | '^' | '%' | '$' | ';'
            )
    }) {
        return Err("计划任务名称包含不允许的字符".to_string());
    }
    Ok(())
}

pub fn get_codex_config_path() -> PathBuf {
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        if !codex_home.trim().is_empty() {
            return PathBuf::from(codex_home).join("config.toml");
        }
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".codex").join("config.toml");
    }
    PathBuf::from("config.toml")
}

pub async fn get_codex_base_url() -> Result<Option<String>, String> {
    let path = get_codex_config_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|error| format!("读取 Codex 配置失败 [{}]: {}", path.display(), error))?;
    let value: toml::Value = toml::from_str(&content)
        .map_err(|error| format!("解析 Codex 配置失败 [{}]: {}", path.display(), error))?;

    let active_provider = value
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);

    let mut provider_urls = HashMap::new();
    if let Some(providers) = value.get("model_providers").and_then(toml::Value::as_table) {
        for (name, item) in providers {
            if let Some(url) = item.get("base_url").and_then(toml::Value::as_str) {
                provider_urls.insert(name.clone(), url.trim().to_string());
            }
        }
    }

    if let Some(provider) = active_provider {
        if let Some(url) = provider_urls.get(&provider) {
            return Ok(Some(url.clone()));
        }
    }
    if provider_urls.len() == 1 {
        return Ok(provider_urls.into_values().next());
    }

    Ok(None)
}

pub fn is_base_url_allowed(current_url: &str, allowed_urls: &str) -> Result<bool, String> {
    let candidates = split_allowed_urls(allowed_urls);
    if candidates.is_empty() {
        return Ok(true);
    }

    let current = parse_base_url(current_url)?;
    Ok(candidates
        .into_iter()
        .map(parse_base_url)
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|candidate| base_urls_equal(&current, candidate)))
}

pub async fn load_config(paths: &AppPaths, default_work_dir: &Path) -> Result<AppConfig, String> {
    paths.ensure_directories()?;
    let persisted = paths.config_file.exists();
    let mut config = if persisted {
        load_persisted_config(&paths.config_file)?
    } else {
        AppConfig::default()
    };

    if config.work_dir.trim().is_empty() {
        config.work_dir = default_work_dir.to_string_lossy().to_string();
    }
    if config.allowed_base_urls.trim().is_empty() {
        config.allowed_base_urls = get_codex_base_url().await?.unwrap_or_default();
    }
    if config.concurrency == 0 {
        config.concurrency = 1;
    }

    if persisted {
        config.validate().map_err(|error| {
            format!("配置校验失败 [{}]: {}", paths.config_file.display(), error)
        })?;
    }
    Ok(config)
}

pub async fn save_config(paths: &AppPaths, config: &AppConfig) -> Result<(), String> {
    config.validate()?;
    let path = paths.config_file.clone();
    let backup = paths.config_v1_backup.clone();
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        backup_v1_config_if_needed(&path, &backup)?;
        atomic_write_json(&path, &config)
    })
    .await
    .map_err(|error| format!("配置写入任务失败: {}", error))?
}

fn load_persisted_config(path: &Path) -> Result<AppConfig, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("读取 JSON 失败 [{}]: {}", path.display(), error))?;
    let mut value: Value = serde_json::from_str(&content)
        .map_err(|error| format!("解析 JSON 失败 [{}]: {}", path.display(), error))?;
    let version = config_version(&value)?;
    if version > CURRENT_CONFIG_VERSION {
        return Err(format!(
            "配置校验失败 [{}]: 不支持的配置版本 {}，当前支持版本为 {}",
            path.display(),
            version,
            CURRENT_CONFIG_VERSION
        ));
    }
    if version == 1 {
        migrate_v1_value(&mut value)?;
    }
    serde_json::from_value(value)
        .map_err(|error| format!("解析 JSON 失败 [{}]: {}", path.display(), error))
}

fn config_version(value: &Value) -> Result<u32, String> {
    let version = value
        .get("configVersion")
        .map(|version| {
            version
                .as_u64()
                .and_then(|version| u32::try_from(version).ok())
                .ok_or_else(|| "configVersion 必须是非负整数".to_string())
        })
        .transpose()?
        .unwrap_or(1);
    if version == 0 {
        return Err("configVersion 不能为 0".to_string());
    }
    Ok(version)
}

fn migrate_v1_value(value: &mut Value) -> Result<(), String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "配置根节点必须是 JSON object".to_string())?;
    let daily_at = object
        .get("dailyAt")
        .and_then(Value::as_str)
        .unwrap_or("08:40")
        .trim()
        .to_string();
    object.insert(
        "schedule".to_string(),
        serde_json::json!({
            "kind": "daily",
            "time": daily_at,
            "everyDays": 1
        }),
    );
    object.insert(
        "configVersion".to_string(),
        Value::from(CURRENT_CONFIG_VERSION),
    );
    object.remove("dailyAt");
    Ok(())
}

fn backup_v1_config_if_needed(path: &Path, backup: &Path) -> Result<(), String> {
    if backup.exists() || !path.exists() {
        return Ok(());
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("读取旧配置失败 [{}]: {}", path.display(), error))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析旧配置失败 [{}]: {}", path.display(), error))?;
    if config_version(&value)? == 1 {
        atomic_write_bytes(backup, &bytes)?;
    }
    Ok(())
}

fn split_allowed_urls(allowed_urls: &str) -> Vec<&str> {
    allowed_urls
        .split([';', '；'])
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .collect()
}

fn parse_base_url(value: &str) -> Result<Url, String> {
    let mut url = Url::parse(value.trim())
        .map_err(|error| format!("base URL 无效 [{}]: {}", value.trim(), error))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!(
            "base URL 必须是带 host 的 http/https URL: {}",
            value
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("base URL 不能包含用户名或密码: {}", value));
    }
    url.set_fragment(None);

    let normalized_path = if url.path() == "/" {
        "/".to_string()
    } else {
        url.path().trim_end_matches('/').to_string()
    };
    url.set_path(&normalized_path);
    Ok(url)
}

fn base_urls_equal(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left_host, right_host)| left_host.eq_ignore_ascii_case(right_host))
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path() == right.path()
        && left.query() == right.query()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn valid_config(work_dir: &Path) -> AppConfig {
        AppConfig {
            command: "echo valid".to_string(),
            work_dir: work_dir.to_string_lossy().to_string(),
            ..AppConfig::default()
        }
    }

    #[test]
    fn keep_alive_interval_is_validated_for_manual_mode_too() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let mut config = valid_config(temp.path());
        config.keep_alive = false;
        config.keep_alive_interval_minutes = 0;

        assert!(config
            .validate()
            .expect_err("zero keep-alive interval must fail")
            .contains("保活间隔"));
    }

    #[tokio::test]
    async fn old_config_missing_new_fields_loads_with_defaults() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        let old_json = serde_json::json!({
            "command": "cmd /c exit 0",
            "workDir": temp.path().to_string_lossy(),
            "interval": 5,
            "maxTries": 3,
            "notify": true,
            "openDashboard": true,
            "taskName": "Legacy Task",
            "dailyAt": "09:30"
        });
        fs::write(
            &paths.config_file,
            serde_json::to_vec_pretty(&old_json).expect("serialize old config"),
        )
        .expect("write old config");

        let loaded = load_config(&paths, temp.path())
            .await
            .expect("load old config");

        assert_eq!(loaded.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(loaded.command, "cmd /c exit 0");
        assert_eq!(
            loaded.concurrency, 1,
            "旧配置缺少 concurrency 时必须回落到单线程"
        );
        assert_eq!(
            loaded.desktop_notification,
            DesktopNotificationConfig::default()
        );
        assert!(loaded.desktop_notification.enabled);
        assert_eq!(loaded.server_chan, ServerChanConfig::default());
        assert_eq!(
            loaded.schedule,
            ScheduleConfig::Daily {
                time: "09:30".to_string(),
                every_days: 1,
            }
        );
        assert_eq!(loaded.config_version, 2);
        assert!(!paths.config_v1_backup.exists());
    }

    #[tokio::test]
    async fn explicit_zero_concurrency_is_normalized_to_single_thread() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        let mut config = valid_config(temp.path());
        config.concurrency = 0;
        fs::write(
            &paths.config_file,
            serde_json::to_vec_pretty(&config).expect("serialize zero-concurrency config"),
        )
        .expect("write zero-concurrency config");

        let loaded = load_config(&paths, temp.path())
            .await
            .expect("hand-edited zero concurrency must not break loading");

        assert_eq!(loaded.concurrency, 1);
    }

    #[test]
    fn concurrency_is_bounded_to_the_supported_range() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let mut config = valid_config(temp.path());

        for invalid in [0, MAX_CONCURRENCY + 1] {
            config.concurrency = invalid;
            assert!(config
                .validate()
                .expect_err("out-of-range concurrency must fail")
                .contains("并发线程数"));
        }

        for valid in [1, MAX_CONCURRENCY] {
            config.concurrency = valid;
            config.validate().expect("in-range concurrency is accepted");
        }
    }

    #[test]
    fn serialized_config_never_contains_a_server_chan_send_key() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let mut config = valid_config(temp.path());
        config.server_chan.enabled = true;

        let serialized = serde_json::to_string(&config).expect("serialize config");

        assert!(serialized.contains("serverChan"));
        assert!(!serialized.to_ascii_lowercase().contains("sendkey"));
        assert!(!serialized.to_ascii_lowercase().contains("send_key"));
    }

    #[tokio::test]
    async fn corrupt_json_returns_path_and_preserves_original_file() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        let corrupt = b"{ definitely-not-json";
        fs::write(&paths.config_file, corrupt).expect("write corrupt config");

        let error = load_config(&paths, temp.path())
            .await
            .expect_err("corrupt JSON must be visible");

        assert!(error.contains(&paths.config_file.display().to_string()));
        assert_eq!(
            fs::read(&paths.config_file).expect("read original"),
            corrupt
        );
    }

    #[tokio::test]
    async fn fresh_missing_config_returns_empty_unsaved_draft() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));

        let loaded = load_config(&paths, temp.path())
            .await
            .expect("load fresh draft");

        assert!(loaded.command.is_empty());
        assert_eq!(loaded.work_dir, temp.path().to_string_lossy());
        assert!(
            !paths.config_file.exists(),
            "fresh draft must not be persisted"
        );
        assert!(
            loaded.validate().is_err(),
            "fresh draft stays intentionally incomplete"
        );
    }

    #[test]
    fn base_url_host_is_case_insensitive_but_path_is_case_sensitive() {
        assert!(
            is_base_url_allowed("HTTPS://EXAMPLE.COM/API/", "https://example.com/API")
                .expect("compare valid URLs")
        );
        assert!(
            !is_base_url_allowed("https://example.com/API", "https://EXAMPLE.com/api")
                .expect("compare valid URLs")
        );
    }

    #[test]
    fn validation_rejects_invalid_ranges_and_paths() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let mut config = valid_config(temp.path());
        config.interval = 0;
        assert!(config.validate().is_err());

        config.interval = 1;
        config.max_tries = MAX_TRIES_LIMIT + 1;
        assert!(config.validate().is_err());

        config.max_tries = 1;
        config.schedule = ScheduleConfig::Daily {
            time: "24:00".to_string(),
            every_days: 1,
        };
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn first_v2_save_creates_one_copy_only_v1_backup() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        let original = serde_json::to_vec_pretty(&serde_json::json!({
            "configVersion": 1,
            "command": "echo old",
            "workDir": temp.path().to_string_lossy(),
            "interval": 5,
            "maxTries": 1,
            "taskName": "Legacy Task",
            "dailyAt": "07:30"
        }))
        .expect("serialize v1");
        fs::write(&paths.config_file, &original).expect("write v1");

        let mut loaded = load_config(&paths, temp.path())
            .await
            .expect("migrate in memory");
        loaded.command = "echo new".to_string();
        save_config(&paths, &loaded).await.expect("save v2");

        assert_eq!(
            fs::read(&paths.config_v1_backup).expect("read backup"),
            original
        );
        let first_backup = fs::read(&paths.config_v1_backup).expect("read first backup");
        loaded.command = "echo newer".to_string();
        save_config(&paths, &loaded).await.expect("save v2 again");
        assert_eq!(
            fs::read(&paths.config_v1_backup).expect("read stable backup"),
            first_backup
        );
    }
}
