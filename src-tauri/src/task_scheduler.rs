use crate::app_storage::{atomic_write_json, read_json, AppPaths};
use crate::config_manager::validate_task_name;
use crate::schedule::{compile_schedule, ScheduleConfig};
use crate::windows_text::decode_output_text;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tokio::process::Command;

const SCHEDULER_STATE_VERSION: u32 = 1;
const TASK_NAME_ENV: &str = "CODEX_LAUNCHER_TASK_NAME";

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const HEALTH_QUERY_SCRIPT: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$name = [Environment]::GetEnvironmentVariable('CODEX_LAUNCHER_TASK_NAME')
$task = Get-ScheduledTask -TaskName $name -TaskPath '\' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($null -eq $task) {
  [pscustomobject]@{ installed = $false } | ConvertTo-Json -Compress
  exit 0
}
$info = $task | Get-ScheduledTaskInfo -ErrorAction Stop
$action = $task.Actions | Select-Object -First 1
function Format-Time($value) {
  if ($null -eq $value -or $value -le [DateTime]::MinValue) { return $null }
  return $value.ToString('o')
}
[pscustomobject]@{
  installed = $true
  state = [string]$task.State
  enabled = ([string]$task.State -ne 'Disabled')
  nextRunTime = Format-Time $info.NextRunTime
  lastRunTime = Format-Time $info.LastRunTime
  lastResult = [int64]$info.LastTaskResult
  missedRuns = [uint32]$info.NumberOfMissedRuns
  actionPath = if ($null -eq $action) { $null } else { [string]$action.Execute }
  actionArguments = if ($null -eq $action) { $null } else { [string]$action.Arguments }
} | ConvertTo-Json -Compress
"#;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ScheduledTaskState {
    Ready,
    Running,
    Disabled,
    Queued,
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskHealth {
    pub installed: bool,
    pub state: Option<ScheduledTaskState>,
    pub state_label: Option<String>,
    pub enabled: bool,
    pub next_run_time: Option<String>,
    pub last_run_time: Option<String>,
    pub last_result: Option<i64>,
    pub last_result_hex: Option<String>,
    pub last_result_label: Option<String>,
    pub missed_runs: u32,
    pub action_path: Option<String>,
    pub action_arguments: Option<String>,
    pub action_matches_app: bool,
    pub managed: bool,
    pub config_drift: bool,
    pub applied_schedule_summary: Option<String>,
    pub desired_schedule_summary: String,
    pub stale_managed_tasks: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ManagedTaskRegistration {
    task_name: String,
    executable_path: String,
    applied_schedule: ScheduleConfig,
    registered_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
struct SchedulerState {
    version: u32,
    registrations: Vec<ManagedTaskRegistration>,
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self {
            version: SCHEDULER_STATE_VERSION,
            registrations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
struct TaskProbe {
    installed: bool,
    state: Option<String>,
    enabled: bool,
    next_run_time: Option<String>,
    last_run_time: Option<String>,
    last_result: Option<i64>,
    missed_runs: u32,
    action_path: Option<String>,
    action_arguments: Option<String>,
}

#[derive(Debug, Clone)]
struct SchedulerOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagedCleanupOutcome {
    Removed,
    NotFound,
    NotOwned,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: u32,
    pub not_found: u32,
    pub skipped: Vec<String>,
}

pub async fn install_task(
    paths: &AppPaths,
    task_name: &str,
    schedule: &ScheduleConfig,
    exe_path: &str,
) -> Result<String, String> {
    install_task_with(&SystemSchedulerRunner, paths, task_name, schedule, exe_path).await
}

pub async fn uninstall_task(
    paths: &AppPaths,
    task_name: &str,
    current_exe_path: &str,
) -> Result<String, String> {
    match cleanup_managed_task_with(&SystemSchedulerRunner, paths, task_name, current_exe_path)
        .await?
    {
        ManagedCleanupOutcome::Removed => Ok(format!("计划任务 [{}] 已成功删除", task_name.trim())),
        ManagedCleanupOutcome::NotFound => {
            Ok(format!("计划任务 [{}] 不存在，无需删除", task_name.trim()))
        }
        ManagedCleanupOutcome::NotOwned => Err(format!(
            "计划任务 [{}] 不属于 Codex Launcher，拒绝删除",
            task_name.trim()
        )),
    }
}

pub async fn cleanup_all_managed_tasks(
    paths: &AppPaths,
    fallback_task_name: Option<&str>,
    current_exe_path: &str,
) -> Result<CleanupReport, String> {
    let state = load_scheduler_state(paths)?;
    let mut names = state
        .registrations
        .iter()
        .map(|registration| registration.task_name.clone())
        .collect::<BTreeSet<_>>();
    if let Some(fallback) = fallback_task_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        validate_task_name(fallback)?;
        names.insert(fallback.to_string());
    }

    let mut report = CleanupReport::default();
    let mut failures = Vec::new();
    for task_name in names {
        match cleanup_managed_task_with(&SystemSchedulerRunner, paths, &task_name, current_exe_path)
            .await
        {
            Ok(ManagedCleanupOutcome::Removed) => report.removed += 1,
            Ok(ManagedCleanupOutcome::NotFound) => report.not_found += 1,
            Ok(ManagedCleanupOutcome::NotOwned) => report
                .skipped
                .push(format!("任务 [{}] ownership 不匹配，已跳过", task_name)),
            Err(error) => failures.push(format!("[{}] {}", task_name, error)),
        }
    }
    if failures.is_empty() {
        Ok(report)
    } else {
        Err(format!(
            "清理 managed 计划任务失败: {}",
            failures.join("; ")
        ))
    }
}

pub async fn check_task_status(task_name: &str) -> Result<bool, String> {
    validate_task_name(task_name)?;
    Ok(SystemSchedulerRunner
        .query_task(task_name.trim())
        .await?
        .installed)
}

pub async fn get_task_detail(task_name: &str) -> Result<String, String> {
    get_task_detail_with(&SystemSchedulerRunner, task_name).await
}

pub async fn get_task_health(
    paths: &AppPaths,
    task_name: &str,
    desired_schedule: &ScheduleConfig,
    current_exe_path: &str,
) -> Result<TaskHealth, String> {
    get_task_health_with(
        &SystemSchedulerRunner,
        paths,
        task_name,
        desired_schedule,
        current_exe_path,
    )
    .await
}

async fn install_task_with<R: SchedulerRunner>(
    runner: &R,
    paths: &AppPaths,
    task_name: &str,
    schedule: &ScheduleConfig,
    exe_path: &str,
) -> Result<String, String> {
    validate_task_name(task_name)?;
    let compiled = compile_schedule(schedule)?;
    let exe_path = validate_executable_path(exe_path)?;
    let task_name = task_name.trim();
    let mut state = load_scheduler_state(paths)?;
    let target_registration = state
        .registrations
        .iter()
        .find(|registration| registration.task_name == task_name)
        .cloned();
    let existing = runner.query_task(task_name).await?;
    let adopted_legacy = existing.installed
        && target_registration.is_none()
        && probe_matches_app(&existing, &exe_path);
    if existing.installed {
        let owned = target_registration
            .as_ref()
            .is_some_and(|registration| probe_matches_registration(&existing, registration))
            || (target_registration.is_none() && probe_matches_app(&existing, &exe_path));
        if !owned {
            return Err(format!(
                "计划任务 [{}] 已存在但不属于 Codex Launcher，拒绝覆盖",
                task_name
            ));
        }
    }

    let task_command = format!("\"{}\" --scheduled", exe_path);
    let mut args = strings(["/Create", "/TN", task_name, "/TR", &task_command]);
    args.extend(compiled.schtasks_args.clone());
    args.push("/F".to_string());
    let output = runner.run_schtasks(&args).await?;
    if !output.success {
        return Err(command_failure("创建或更新计划任务", &output));
    }

    let verified = runner.query_task(task_name).await?;
    if !verified.installed || !probe_matches_app(&verified, &exe_path) {
        return Err(format!(
            "计划任务 [{}] 创建后验证失败：Action 未指向当前 Codex Launcher --scheduled",
            task_name
        ));
    }

    state
        .registrations
        .retain(|registration| registration.task_name != task_name);
    state.registrations.push(ManagedTaskRegistration {
        task_name: task_name.to_string(),
        executable_path: exe_path.clone(),
        applied_schedule: schedule.clone(),
        registered_at: Utc::now().to_rfc3339(),
    });
    save_scheduler_state(paths, &state)?;

    let old_names = state
        .registrations
        .iter()
        .filter(|registration| registration.task_name != task_name)
        .map(|registration| registration.task_name.clone())
        .collect::<Vec<_>>();
    let mut notices = Vec::new();
    if adopted_legacy && probe_uses_legacy_headless(&existing) {
        notices.push("已接管 legacy --headless 任务并升级为 --scheduled".to_string());
    }
    for old_name in old_names {
        match cleanup_managed_task_with(runner, paths, &old_name, &exe_path).await {
            Ok(ManagedCleanupOutcome::Removed | ManagedCleanupOutcome::NotFound) => {}
            Ok(ManagedCleanupOutcome::NotOwned) => notices.push(format!(
                "旧任务 [{}] ownership 已变化，未自动删除",
                old_name
            )),
            Err(error) => notices.push(format!("旧任务 [{}] 清理失败: {}", old_name, error)),
        }
    }

    let mut message = format!(
        "计划任务 [{}] 已成功创建或更新（{}）",
        task_name, compiled.summary
    );
    if !notices.is_empty() {
        message.push_str(&format!("；{}", notices.join("；")));
    }
    Ok(message)
}

async fn cleanup_managed_task_with<R: SchedulerRunner>(
    runner: &R,
    paths: &AppPaths,
    task_name: &str,
    current_exe_path: &str,
) -> Result<ManagedCleanupOutcome, String> {
    validate_task_name(task_name)?;
    let task_name = task_name.trim();
    let mut state = load_scheduler_state(paths)?;
    let registration = state
        .registrations
        .iter()
        .find(|registration| registration.task_name == task_name)
        .cloned();
    let probe = runner.query_task(task_name).await?;
    if !probe.installed {
        remove_registration(&mut state, task_name);
        save_scheduler_state(paths, &state)?;
        return Ok(ManagedCleanupOutcome::NotFound);
    }

    let owned = registration
        .as_ref()
        .is_some_and(|registration| probe_matches_registration(&probe, registration))
        || (registration.is_none() && probe_matches_app(&probe, current_exe_path));
    if !owned {
        return Ok(ManagedCleanupOutcome::NotOwned);
    }

    let output = runner
        .run_schtasks(&strings(["/Delete", "/TN", task_name, "/F"]))
        .await?;
    let outcome = if output.success {
        ManagedCleanupOutcome::Removed
    } else if task_not_found(&output) {
        ManagedCleanupOutcome::NotFound
    } else {
        return Err(command_failure("删除计划任务", &output));
    };
    remove_registration(&mut state, task_name);
    save_scheduler_state(paths, &state)?;
    Ok(outcome)
}

async fn get_task_health_with<R: SchedulerRunner>(
    runner: &R,
    paths: &AppPaths,
    task_name: &str,
    desired_schedule: &ScheduleConfig,
    current_exe_path: &str,
) -> Result<TaskHealth, String> {
    validate_task_name(task_name)?;
    let desired_schedule_summary = desired_schedule.summary()?;
    let state_store = load_scheduler_state(paths)?;
    let task_name = task_name.trim();
    let registration = state_store
        .registrations
        .iter()
        .find(|registration| registration.task_name == task_name);
    let probe = runner.query_task(task_name).await?;
    let action_matches_app = probe_matches_app(&probe, current_exe_path);
    let applied_schedule_summary = registration
        .map(|registration| registration.applied_schedule.summary())
        .transpose()?;
    let config_drift = !probe.installed
        || !action_matches_app
        || registration
            .is_none_or(|registration| registration.applied_schedule != *desired_schedule);
    let stale_managed_tasks = state_store
        .registrations
        .iter()
        .filter(|registration| registration.task_name != task_name)
        .map(|registration| registration.task_name.clone())
        .collect::<Vec<_>>();
    let state = probe.state.as_deref().map(parse_task_state);
    let mut warnings = Vec::new();
    if probe.installed && !action_matches_app {
        warnings.push("同名任务的 Action 不属于当前 Codex Launcher".to_string());
    }
    if probe.installed && !probe.enabled {
        warnings.push("Windows 计划任务已禁用".to_string());
    }
    if probe.missed_runs > 0 {
        warnings.push(format!("Windows 记录了 {} 次错过运行", probe.missed_runs));
    }
    if let Some(result) = probe.last_result.filter(|result| *result != 0) {
        warnings.push(format!("上次运行结果为 {}", result_code_label(result)));
    }
    if config_drift {
        warnings.push("当前配置尚未应用到 Windows 计划任务".to_string());
    }
    if !stale_managed_tasks.is_empty() {
        warnings.push(format!(
            "仍记录旧 managed 任务：{}",
            stale_managed_tasks.join("、")
        ));
    }

    Ok(TaskHealth {
        installed: probe.installed,
        state,
        state_label: probe.state,
        enabled: probe.enabled,
        next_run_time: probe.next_run_time,
        last_run_time: probe.last_run_time,
        last_result: probe.last_result,
        last_result_hex: probe.last_result.map(format_result_hex),
        last_result_label: probe.last_result.map(result_code_label),
        missed_runs: probe.missed_runs,
        action_path: probe.action_path,
        action_arguments: probe.action_arguments,
        action_matches_app,
        managed: registration.is_some(),
        config_drift,
        applied_schedule_summary,
        desired_schedule_summary,
        stale_managed_tasks,
        warnings,
    })
}

async fn get_task_detail_with<R: SchedulerRunner>(
    runner: &R,
    task_name: &str,
) -> Result<String, String> {
    validate_task_name(task_name)?;
    let output = runner
        .run_schtasks(&strings([
            "/Query",
            "/TN",
            task_name.trim(),
            "/FO",
            "LIST",
            "/V",
        ]))
        .await?;
    if output.success {
        let stdout = decode_output_text(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            Err("schtasks 查询成功但未返回任务详情".to_string())
        } else {
            Ok(stdout)
        }
    } else {
        Err(command_failure("获取计划任务详情", &output))
    }
}

fn load_scheduler_state(paths: &AppPaths) -> Result<SchedulerState, String> {
    if !paths.scheduler_state.exists() {
        return Ok(SchedulerState::default());
    }
    let state: SchedulerState = read_json(&paths.scheduler_state)?;
    if state.version != SCHEDULER_STATE_VERSION {
        return Err(format!(
            "不支持的 scheduler state 版本 {} [{}]",
            state.version,
            paths.scheduler_state.display()
        ));
    }
    Ok(state)
}

fn save_scheduler_state(paths: &AppPaths, state: &SchedulerState) -> Result<(), String> {
    atomic_write_json(&paths.scheduler_state, state)
}

fn remove_registration(state: &mut SchedulerState, task_name: &str) {
    state
        .registrations
        .retain(|registration| registration.task_name != task_name);
}

fn validate_executable_path(exe_path: &str) -> Result<String, String> {
    let exe_path = exe_path.trim();
    if exe_path.is_empty() {
        return Err("计划任务 executable path 不能为空".to_string());
    }
    if exe_path.contains('"') {
        return Err("计划任务 executable path 不能包含双引号".to_string());
    }
    Ok(exe_path.to_string())
}

fn probe_matches_registration(probe: &TaskProbe, registration: &ManagedTaskRegistration) -> bool {
    probe_matches_app(probe, &registration.executable_path)
}

fn probe_matches_app(probe: &TaskProbe, executable_path: &str) -> bool {
    if !probe.installed {
        return false;
    }
    let Some(action_path) = probe.action_path.as_deref() else {
        return false;
    };
    let arguments = probe.action_arguments.as_deref().unwrap_or_default().trim();
    windows_paths_equal(action_path, executable_path)
        && matches!(arguments, "--scheduled" | "--headless")
}

fn probe_uses_legacy_headless(probe: &TaskProbe) -> bool {
    probe
        .action_arguments
        .as_deref()
        .is_some_and(|arguments| arguments.trim() == "--headless")
}

fn windows_paths_equal(left: &str, right: &str) -> bool {
    normalize_windows_path(left) == normalize_windows_path(right)
}

fn normalize_windows_path(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn parse_task_state(value: &str) -> ScheduledTaskState {
    match value.to_ascii_lowercase().as_str() {
        "ready" => ScheduledTaskState::Ready,
        "running" => ScheduledTaskState::Running,
        "disabled" => ScheduledTaskState::Disabled,
        "queued" => ScheduledTaskState::Queued,
        _ => ScheduledTaskState::Unknown,
    }
}

fn format_result_hex(value: i64) -> String {
    format!("0x{:X}", value as u64)
}

fn result_code_label(value: i64) -> String {
    match value {
        0 => "成功".to_string(),
        1 => "应用返回失败".to_string(),
        2 => "应用收到停止请求".to_string(),
        0x41300 => "Ready".to_string(),
        0x41301 => "Running".to_string(),
        0x41302 => "Disabled".to_string(),
        0x41303 => "尚未运行".to_string(),
        other => format!("未知结果 {}", format_result_hex(other)),
    }
}

fn task_not_found(output: &SchedulerOutput) -> bool {
    if output.exit_code != Some(1) {
        return false;
    }
    let combined = format!(
        "{}\n{}",
        decode_output_text(&output.stdout),
        decode_output_text(&output.stderr)
    )
    .to_lowercase();
    [
        "cannot find the file specified",
        "cannot find the specified file",
        "the system cannot find",
        "系统找不到指定的文件",
        "找不到指定的文件",
        "找不到该任务",
    ]
    .iter()
    .any(|marker| combined.contains(marker))
}

fn command_failure(action: &str, output: &SchedulerOutput) -> String {
    let stdout = decode_output_text(&output.stdout).trim().to_string();
    let stderr = decode_output_text(&output.stderr).trim().to_string();
    format!(
        "{}失败 (exit code: {}): stdout=[{}] stderr=[{}]",
        action,
        output
            .exit_code
            .map_or_else(|| "unknown".to_string(), |code| code.to_string()),
        stdout,
        stderr
    )
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

#[async_trait]
trait SchedulerRunner: Sync {
    async fn run_schtasks(&self, args: &[String]) -> Result<SchedulerOutput, String>;
    async fn query_task(&self, task_name: &str) -> Result<TaskProbe, String>;
}

struct SystemSchedulerRunner;

#[async_trait]
impl SchedulerRunner for SystemSchedulerRunner {
    async fn run_schtasks(&self, args: &[String]) -> Result<SchedulerOutput, String> {
        let mut command = Command::new("schtasks");
        command.args(args);
        apply_no_window(&mut command);
        let output = command
            .output()
            .await
            .map_err(|error| format!("执行 schtasks 失败: {}", error))?;
        Ok(SchedulerOutput {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    async fn query_task(&self, task_name: &str) -> Result<TaskProbe, String> {
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                HEALTH_QUERY_SCRIPT,
            ])
            .env(TASK_NAME_ENV, task_name);
        apply_no_window(&mut command);
        let output = command
            .output()
            .await
            .map_err(|error| format!("执行 ScheduledTasks 健康查询失败: {}", error))?;
        if !output.status.success() {
            return Err(command_failure(
                "ScheduledTasks 健康查询",
                &SchedulerOutput {
                    success: false,
                    exit_code: output.status.code(),
                    stdout: output.stdout,
                    stderr: output.stderr,
                },
            ));
        }
        let json = decode_output_text(&output.stdout).trim().to_string();
        if json.is_empty() {
            return Err("ScheduledTasks 健康查询成功但未返回 JSON".to_string());
        }
        serde_json::from_str(&json)
            .map_err(|error| format!("解析 ScheduledTasks 健康 JSON 失败: {}: [{}]", error, json))
    }
}

fn apply_no_window(command: &mut Command) {
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        outputs: Mutex<VecDeque<SchedulerOutput>>,
        probes: Mutex<VecDeque<TaskProbe>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<SchedulerOutput>, probes: Vec<TaskProbe>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outputs: Mutex::new(outputs.into()),
                probes: Mutex::new(probes.into()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls mutex").clone()
        }
    }

    #[async_trait]
    impl SchedulerRunner for FakeRunner {
        async fn run_schtasks(&self, args: &[String]) -> Result<SchedulerOutput, String> {
            self.calls.lock().expect("calls mutex").push(args.to_vec());
            self.outputs
                .lock()
                .expect("outputs mutex")
                .pop_front()
                .ok_or_else(|| "missing fake scheduler output".to_string())
        }

        async fn query_task(&self, _task_name: &str) -> Result<TaskProbe, String> {
            self.probes
                .lock()
                .expect("probes mutex")
                .pop_front()
                .ok_or_else(|| "missing fake task probe".to_string())
        }
    }

    fn success_output() -> SchedulerOutput {
        SchedulerOutput {
            success: true,
            exit_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn failed_output(stderr: &str) -> SchedulerOutput {
        SchedulerOutput {
            success: false,
            exit_code: Some(5),
            stdout: b"partial stdout".to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn app_probe(path: &str, arguments: &str) -> TaskProbe {
        TaskProbe {
            installed: true,
            state: Some("Ready".to_string()),
            enabled: true,
            action_path: Some(path.to_string()),
            action_arguments: Some(arguments.to_string()),
            ..TaskProbe::default()
        }
    }

    #[tokio::test]
    async fn invalid_cron_fails_before_external_commands_are_called() {
        let temp = tempfile::tempdir().expect("temp");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let runner = FakeRunner::new(Vec::new(), Vec::new());
        let error = install_task_with(
            &runner,
            &paths,
            "Valid Task",
            &ScheduleConfig::Cron {
                expression: "0,30 9 * * *".to_string(),
            },
            r"C:\Codex\launcher.exe",
        )
        .await
        .expect_err("unsupported cron must fail");
        assert!(error.contains("单一数值"));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn create_failure_never_deletes_existing_task() {
        let temp = tempfile::tempdir().expect("temp");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let exe = r"C:\Program Files\Codex\launcher.exe";
        let runner = FakeRunner::new(
            vec![failed_output("access denied")],
            vec![app_probe(exe, "--headless")],
        );
        let error = install_task_with(
            &runner,
            &paths,
            "Valid Task",
            &ScheduleConfig::default(),
            exe,
        )
        .await
        .expect_err("create failure");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], "/Create");
        assert!(!calls[0].iter().any(|argument| argument == "/Delete"));
        assert!(error.contains("access denied"));
    }

    #[tokio::test]
    async fn same_name_unowned_task_is_never_overwritten() {
        let temp = tempfile::tempdir().expect("temp");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let runner = FakeRunner::new(Vec::new(), vec![app_probe(r"C:\Other\app.exe", "--run")]);
        let error = install_task_with(
            &runner,
            &paths,
            "Valid Task",
            &ScheduleConfig::default(),
            r"C:\Codex\launcher.exe",
        )
        .await
        .expect_err("foreign task must be preserved");
        assert!(error.contains("拒绝覆盖"));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn successful_install_records_state_and_uses_scheduled_source() {
        let temp = tempfile::tempdir().expect("temp");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("dirs");
        let exe = r"C:\Codex\launcher.exe";
        let runner = FakeRunner::new(
            vec![success_output()],
            vec![TaskProbe::default(), app_probe(exe, "--scheduled")],
        );
        install_task_with(
            &runner,
            &paths,
            "Valid Task",
            &ScheduleConfig::default(),
            exe,
        )
        .await
        .expect("install");
        let calls = runner.calls();
        assert!(calls[0]
            .iter()
            .any(|argument| argument.contains("--scheduled")));
        let state = load_scheduler_state(&paths).expect("state");
        assert_eq!(state.registrations.len(), 1);
        assert_eq!(state.registrations[0].task_name, "Valid Task");
    }

    #[tokio::test]
    async fn health_reports_drift_and_structured_result() {
        let temp = tempfile::tempdir().expect("temp");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let mut probe = app_probe(r"C:\Codex\launcher.exe", "--scheduled");
        probe.last_result = Some(1);
        probe.missed_runs = 2;
        let runner = FakeRunner::new(Vec::new(), vec![probe]);
        let health = get_task_health_with(
            &runner,
            &paths,
            "Valid Task",
            &ScheduleConfig::default(),
            r"C:\Codex\launcher.exe",
        )
        .await
        .expect("health");
        assert!(health.installed);
        assert!(health.config_drift);
        assert_eq!(health.last_result_hex.as_deref(), Some("0x1"));
        assert!(health
            .warnings
            .iter()
            .any(|warning| warning.contains("错过")));
    }

    #[test]
    fn not_found_detection_preserves_permission_failures() {
        let missing = SchedulerOutput {
            success: false,
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: b"ERROR: The system cannot find the file specified.".to_vec(),
        };
        assert!(task_not_found(&missing));
        assert!(!task_not_found(&failed_output("Access is denied.")));
    }

    #[test]
    fn ownership_path_comparison_is_case_and_separator_insensitive() {
        let probe = app_probe(r"C:/CODEX/Launcher.exe", "--scheduled");
        assert!(probe_matches_app(&probe, r"c:\codex\launcher.exe"));
    }
}
