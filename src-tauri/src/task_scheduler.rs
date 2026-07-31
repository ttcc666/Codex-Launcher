use crate::config_manager::{validate_daily_at, validate_task_name};
use crate::windows_text::decode_output_text;
use async_trait::async_trait;
use tokio::process::Command;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub async fn install_daily_task(
    task_name: &str,
    daily_at: &str,
    exe_path: &str,
) -> Result<String, String> {
    install_daily_task_with(&SystemSchedulerRunner, task_name, daily_at, exe_path).await
}

pub async fn uninstall_daily_task(task_name: &str) -> Result<String, String> {
    uninstall_daily_task_with(&SystemSchedulerRunner, task_name).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCleanupOutcome {
    Removed,
    NotFound,
}

pub async fn cleanup_daily_task(task_name: &str) -> Result<TaskCleanupOutcome, String> {
    cleanup_daily_task_with(&SystemSchedulerRunner, task_name).await
}

pub async fn check_task_status(task_name: &str) -> Result<bool, String> {
    check_task_status_with(&SystemSchedulerRunner, task_name).await
}

pub async fn get_task_detail(task_name: &str) -> Result<String, String> {
    get_task_detail_with(&SystemSchedulerRunner, task_name).await
}

async fn install_daily_task_with<R: SchedulerRunner>(
    runner: &R,
    task_name: &str,
    daily_at: &str,
    exe_path: &str,
) -> Result<String, String> {
    validate_task_name(task_name)?;
    validate_daily_at(daily_at)?;
    let exe_path = exe_path.trim();
    if exe_path.is_empty() {
        return Err("计划任务 executable path 不能为空".to_string());
    }
    if exe_path.contains('"') {
        return Err("计划任务 executable path 不能包含双引号".to_string());
    }

    let task_name = task_name.trim();
    let daily_at = daily_at.trim();
    let task_command = format!("\"{}\" --headless", exe_path);
    let args = strings([
        "/Create",
        "/TN",
        task_name,
        "/TR",
        &task_command,
        "/SC",
        "DAILY",
        "/ST",
        daily_at,
        "/F",
    ]);
    let output = runner.run(&args).await?;
    if output.success {
        Ok(format!(
            "每日计划任务 [{}] 已成功创建或更新（每日 {} 触发）",
            task_name, daily_at
        ))
    } else {
        Err(command_failure("创建或更新计划任务", &output))
    }
}

async fn uninstall_daily_task_with<R: SchedulerRunner>(
    runner: &R,
    task_name: &str,
) -> Result<String, String> {
    match cleanup_daily_task_with(runner, task_name).await? {
        TaskCleanupOutcome::Removed => Ok(format!("计划任务 [{}] 已成功删除", task_name.trim())),
        TaskCleanupOutcome::NotFound => {
            Ok(format!("计划任务 [{}] 不存在，无需删除", task_name.trim()))
        }
    }
}

async fn cleanup_daily_task_with<R: SchedulerRunner>(
    runner: &R,
    task_name: &str,
) -> Result<TaskCleanupOutcome, String> {
    validate_task_name(task_name)?;
    let task_name = task_name.trim();
    let args = strings(["/Delete", "/TN", task_name, "/F"]);
    let output = runner.run(&args).await?;
    if output.success {
        Ok(TaskCleanupOutcome::Removed)
    } else if task_not_found(&output) {
        Ok(TaskCleanupOutcome::NotFound)
    } else {
        Err(command_failure("删除计划任务", &output))
    }
}

async fn check_task_status_with<R: SchedulerRunner>(
    runner: &R,
    task_name: &str,
) -> Result<bool, String> {
    validate_task_name(task_name)?;
    let args = strings(["/Query", "/TN", task_name.trim()]);
    let output = runner.run(&args).await?;
    if output.success {
        return Ok(true);
    }
    if task_not_found(&output) {
        return Ok(false);
    }
    Err(command_failure("查询计划任务", &output))
}

async fn get_task_detail_with<R: SchedulerRunner>(
    runner: &R,
    task_name: &str,
) -> Result<String, String> {
    validate_task_name(task_name)?;
    let args = strings(["/Query", "/TN", task_name.trim(), "/FO", "LIST", "/V"]);
    let output = runner.run(&args).await?;
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

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
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

#[derive(Debug, Clone)]
struct SchedulerOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[async_trait]
trait SchedulerRunner: Sync {
    async fn run(&self, args: &[String]) -> Result<SchedulerOutput, String>;
}

struct SystemSchedulerRunner;

#[async_trait]
impl SchedulerRunner for SystemSchedulerRunner {
    async fn run(&self, args: &[String]) -> Result<SchedulerOutput, String> {
        let mut command = Command::new("schtasks");
        command.args(args);
        #[cfg(target_os = "windows")]
        command.creation_flags(CREATE_NO_WINDOW);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        output: SchedulerOutput,
    }

    impl FakeRunner {
        fn new(output: SchedulerOutput) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                output,
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls mutex").clone()
        }
    }

    #[async_trait]
    impl SchedulerRunner for FakeRunner {
        async fn run(&self, args: &[String]) -> Result<SchedulerOutput, String> {
            self.calls.lock().expect("calls mutex").push(args.to_vec());
            Ok(self.output.clone())
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

    #[tokio::test]
    async fn invalid_time_fails_before_schtasks_is_called() {
        let runner = FakeRunner::new(failed_output("must not run"));

        let error = install_daily_task_with(
            &runner,
            "Valid Task",
            "25:99",
            r"C:\Program Files\Codex\launcher.exe",
        )
        .await
        .expect_err("invalid time must fail");

        assert!(error.contains("00:00 到 23:59"));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn create_failure_never_deletes_existing_task() {
        let runner = FakeRunner::new(failed_output("access denied"));

        let error = install_daily_task_with(
            &runner,
            "Valid Task",
            "08:40",
            r"C:\Program Files\Codex\launcher.exe",
        )
        .await
        .expect_err("create failure must be returned");

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].first().map(String::as_str), Some("/Create"));
        assert!(!calls[0].iter().any(|argument| argument == "/Delete"));
        assert!(error.contains("exit code: 5"));
        assert!(error.contains("partial stdout"));
        assert!(error.contains("access denied"));
    }

    #[tokio::test]
    async fn shell_metacharacters_never_reach_a_command_line() {
        let runner = FakeRunner::new(failed_output("must not run"));

        assert!(install_daily_task_with(
            &runner,
            "task & whoami",
            "08:40",
            r"C:\Codex\launcher.exe",
        )
        .await
        .is_err());
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn query_distinguishes_not_found_from_permission_failure() {
        let missing = FakeRunner::new(SchedulerOutput {
            success: false,
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: "ERROR: The system cannot find the file specified."
                .as_bytes()
                .to_vec(),
        });
        assert!(!check_task_status_with(&missing, "Valid Task")
            .await
            .expect("missing task is not an error"));

        let denied = FakeRunner::new(failed_output("Access is denied."));
        assert!(check_task_status_with(&denied, "Valid Task")
            .await
            .expect_err("permission failure must remain visible")
            .contains("Access is denied"));
    }

    #[tokio::test]
    async fn cleanup_mapping_is_idempotent_and_preserves_real_failures() {
        let removed = FakeRunner::new(SchedulerOutput {
            success: true,
            exit_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
        assert_eq!(
            cleanup_daily_task_with(&removed, "Valid Task")
                .await
                .expect("removed task"),
            TaskCleanupOutcome::Removed
        );

        let missing = FakeRunner::new(SchedulerOutput {
            success: false,
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: b"ERROR: The system cannot find the file specified.".to_vec(),
        });
        assert_eq!(
            cleanup_daily_task_with(&missing, "Valid Task")
                .await
                .expect("missing task is successful cleanup"),
            TaskCleanupOutcome::NotFound
        );

        let denied = FakeRunner::new(failed_output("Access is denied."));
        assert!(cleanup_daily_task_with(&denied, "Valid Task")
            .await
            .expect_err("permission failure remains visible")
            .contains("Access is denied"));
    }
}
