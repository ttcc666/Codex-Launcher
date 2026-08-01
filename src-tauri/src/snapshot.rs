use crate::app_storage::AppPaths;
use crate::retry_engine::{TaskStatus, MAX_LOGICAL_LINE_BYTES};
use crate::run_manager::keep_alive_override;
use crate::status_store::{read_optional_status, reconcile_stale_status};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
const INITIAL_BACKLOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_NORMALIZED_RECORD_BYTES: usize = MAX_LOGICAL_LINE_BYTES + 1024;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRequest {
    pub run_id: Option<String>,
    pub byte_offset: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResponse {
    pub run_id: Option<String>,
    pub reset: bool,
    pub log_lines: Vec<String>,
    pub new_byte_offset: u64,
    pub has_more: bool,
    pub history_truncated: bool,
    pub status: Option<TaskStatus>,
}

pub async fn read_snapshot(
    paths: &AppPaths,
    request: &SnapshotRequest,
) -> Result<SnapshotResponse, String> {
    reconcile_stale_status(paths, None)?;
    for retry in 0..=1 {
        let status_before = read_optional_status(paths)?;
        let run_id_before = status_before.as_ref().map(|status| status.run_id.clone());
        let log = read_log_chunk(
            paths,
            request,
            run_id_before.as_deref(),
            status_before
                .as_ref()
                .is_some_and(|status| status.status.is_active()),
        )
        .await?;
        let mut status_after = read_optional_status(paths)?;
        if let Some(status) = status_after.as_mut() {
            if status.status.is_active() {
                if let Some(enabled) = keep_alive_override(paths, &status.run_id)? {
                    status.keep_alive_enabled = enabled;
                }
            }
        }
        let run_id_after = status_after.as_ref().map(|status| status.run_id.clone());

        if retry == 0 && run_id_before != run_id_after {
            continue;
        }

        return Ok(SnapshotResponse {
            run_id: run_id_after,
            reset: log.reset,
            log_lines: log.log_lines,
            new_byte_offset: log.new_byte_offset,
            has_more: log.has_more,
            history_truncated: log.history_truncated,
            status: status_after,
        });
    }

    unreachable!("bounded snapshot retry always returns")
}

struct LogChunk {
    reset: bool,
    log_lines: Vec<String>,
    new_byte_offset: u64,
    has_more: bool,
    history_truncated: bool,
}

async fn read_log_chunk(
    paths: &AppPaths,
    request: &SnapshotRequest,
    current_run_id: Option<&str>,
    current_run_active: bool,
) -> Result<LogChunk, String> {
    let mut file = match tokio::fs::File::open(&paths.latest_log).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LogChunk {
                reset: request.run_id.as_deref() != current_run_id || request.byte_offset > 0,
                log_lines: Vec::new(),
                new_byte_offset: 0,
                has_more: false,
                history_truncated: false,
            });
        }
        Err(error) => {
            return Err(format!(
                "打开 latest log 失败 [{}]: {}",
                paths.latest_log.display(),
                error
            ));
        }
    };
    let file_len = file
        .metadata()
        .await
        .map_err(|error| format!("读取 latest log metadata 失败: {}", error))?
        .len();
    let mut reset = request.run_id.as_deref() != current_run_id || request.byte_offset > file_len;
    let mut start = if reset { 0 } else { request.byte_offset };
    let mut history_truncated = false;
    if request.run_id.is_none() && current_run_active && file_len > INITIAL_BACKLOG_BYTES {
        let candidate = file_len - INITIAL_BACKLOG_BYTES;
        start = align_to_next_record(&mut file, candidate, file_len).await?;
        reset = true;
        history_truncated = start > 0;
    }
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|error| format!("定位 latest log offset 失败: {}", error))?;

    let available = file_len.saturating_sub(start);
    let read_limit = available.min(MAX_SNAPSHOT_BYTES as u64);
    let mut bytes = Vec::with_capacity(read_limit as usize);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("读取 latest log 失败: {}", error))?;
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1);
    let Some(complete_len) = complete_len else {
        if read_limit < available {
            return Err(format!(
                "normalized log record 超过 snapshot 上限 {} bytes [{}]",
                MAX_SNAPSHOT_BYTES,
                paths.latest_log.display()
            ));
        }
        return Ok(LogChunk {
            reset,
            log_lines: Vec::new(),
            new_byte_offset: start,
            has_more: false,
            history_truncated,
        });
    };
    let complete = &bytes[..complete_len];
    let text = std::str::from_utf8(complete).map_err(|error| {
        format!(
            "normalized log 不是有效 UTF-8 [{} offset={}]: {}",
            paths.latest_log.display(),
            start,
            error
        )
    })?;
    let without_final_newline = &text[..text.len() - 1];
    let log_lines = without_final_newline
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    let new_byte_offset = start + complete_len as u64;

    Ok(LogChunk {
        reset,
        log_lines,
        new_byte_offset,
        has_more: new_byte_offset < file_len && read_limit < available,
        history_truncated,
    })
}

async fn align_to_next_record(
    file: &mut tokio::fs::File,
    candidate: u64,
    file_len: u64,
) -> Result<u64, String> {
    if candidate == 0 {
        return Ok(0);
    }
    file.seek(std::io::SeekFrom::Start(candidate))
        .await
        .map_err(|error| format!("定位 reconnect backlog 失败: {}", error))?;
    let available = file_len.saturating_sub(candidate);
    let read_limit = available.min(MAX_NORMALIZED_RECORD_BYTES as u64);
    let mut bytes = Vec::with_capacity(read_limit as usize);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("对齐 reconnect backlog 失败: {}", error))?;
    bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|newline| candidate + newline as u64 + 1)
        .ok_or_else(|| {
            format!(
                "无法在 {} bytes 内对齐 normalized log record boundary",
                MAX_NORMALIZED_RECORD_BYTES
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_storage::atomic_write_json;
    use crate::retry_engine::RunStatus;
    use crate::run_manager::RunManager;
    use std::fs;
    use std::sync::Arc;

    fn status(paths: &AppPaths, run_id: &str) -> TaskStatus {
        TaskStatus {
            run_id: run_id.to_string(),
            owner_pid: 1,
            child_pid: None,
            status: RunStatus::Running,
            run_mode: Default::default(),
            keep_alive_enabled: false,
            message: "running".to_string(),
            command: "echo test".to_string(),
            work_dir: paths.root_dir.to_string_lossy().to_string(),
            log_file: paths
                .logs_dir
                .join(format!("codex-retry-{run_id}.log"))
                .to_string_lossy()
                .to_string(),
            latest_log: paths.latest_log.to_string_lossy().to_string(),
            attempt: 1,
            high_demand_count: 0,
            max_tries: 1,
            interval_seconds: 1,
            progress_percent: 99.0,
            last_exit_code: None,
            last_error_snippet: String::new(),
            result_preview: String::new(),
            started_at: "2026-07-31T00:00:00Z".to_string(),
            updated_at: "2026-07-31T00:00:01Z".to_string(),
        }
    }

    #[tokio::test]
    async fn truncation_resets_to_zero_and_returns_new_file_start() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "run-a")).expect("write status");
        fs::write(&paths.latest_log, "x".repeat(2 * 1024)).expect("write long log");
        fs::write(&paths.latest_log, "new-first-line\n".repeat(12)).expect("truncate log");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: 2 * 1024,
            },
        )
        .await
        .expect("read truncated snapshot");

        assert!(response.reset);
        assert_eq!(
            response.log_lines.first().map(String::as_str),
            Some("new-first-line")
        );
        assert_eq!(
            response.new_byte_offset,
            fs::metadata(&paths.latest_log).expect("log metadata").len()
        );
    }

    #[tokio::test]
    async fn run_id_switch_resets_without_dropping_first_line() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "run-b")).expect("write status");
        fs::write(&paths.latest_log, b"run-b-first\nrun-b-second\n").expect("write log");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: 999,
            },
        )
        .await
        .expect("read switched snapshot");

        assert!(response.reset);
        assert_eq!(response.run_id.as_deref(), Some("run-b"));
        assert_eq!(response.log_lines, ["run-b-first", "run-b-second"]);
    }

    #[tokio::test]
    async fn snapshot_chunks_large_logs_with_bounded_memory() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "run-a")).expect("write status");
        let line = format!("{}\n", "x".repeat(1023));
        fs::write(
            &paths.latest_log,
            line.repeat((MAX_SNAPSHOT_BYTES / line.len()) * 2 + 2),
        )
        .expect("write large log");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: 0,
            },
        )
        .await
        .expect("read bounded snapshot");

        assert!(response.new_byte_offset <= MAX_SNAPSHOT_BYTES as u64);
        assert!(response.new_byte_offset > 0);
        assert!(response.has_more);
    }

    #[tokio::test]
    async fn stale_active_status_without_held_os_lock_is_reconciled_to_failed() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "stale-run"))
            .expect("write stale status");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: None,
                byte_offset: 0,
            },
        )
        .await
        .expect("read reconciled snapshot");

        let reconciled = response.status.expect("status remains available");
        assert_eq!(reconciled.status, RunStatus::Failed);
        assert_eq!(reconciled.child_pid, None);
        assert!(
            reconciled.message.contains("owner"),
            "{}",
            reconciled.message
        );
    }

    #[tokio::test]
    async fn active_status_with_held_os_lock_remains_active() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        let manager = Arc::new(RunManager::new());
        let _lease = manager
            .reserve(&paths, "live-run".to_string(), false)
            .expect("hold run lock");
        atomic_write_json(&paths.status_file, &status(&paths, "live-run"))
            .expect("write live status");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: None,
                byte_offset: 0,
            },
        )
        .await
        .expect("read live snapshot");

        assert_eq!(
            response.status.expect("status remains available").status,
            RunStatus::Running
        );
    }

    #[tokio::test]
    async fn snapshot_does_not_advance_past_incomplete_trailing_line() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "run-a")).expect("write status");
        fs::write(&paths.latest_log, b"complete\npartial").expect("write partial log");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: 0,
            },
        )
        .await
        .expect("read snapshot");

        assert_eq!(response.log_lines, ["complete"]);
        assert_eq!(response.new_byte_offset, b"complete\n".len() as u64);
    }

    #[tokio::test]
    async fn snapshot_preserves_blank_lines() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "run-a")).expect("write status");
        fs::write(&paths.latest_log, b"first\n\nthird\n").expect("write log");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: 0,
            },
        )
        .await
        .expect("read snapshot");

        assert_eq!(response.log_lines, ["first", "", "third"]);
    }

    #[tokio::test]
    async fn snapshot_waits_for_a_complete_utf8_record() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(&paths.status_file, &status(&paths, "run-a")).expect("write status");
        let mut bytes = b"complete\n".to_vec();
        bytes.push("中".as_bytes()[0]);
        fs::write(&paths.latest_log, &bytes).expect("write partial UTF-8 record");

        let first = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: 0,
            },
        )
        .await
        .expect("read first snapshot");
        assert_eq!(first.log_lines, ["complete"]);
        assert_eq!(first.new_byte_offset, b"complete\n".len() as u64);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&paths.latest_log)
            .expect("open partial log");
        file.write_all(&"中".as_bytes()[1..]).expect("finish UTF-8");
        file.write_all(b"\n").expect("finish record");
        let second = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: Some("run-a".to_string()),
                byte_offset: first.new_byte_offset,
            },
        )
        .await
        .expect("read completed UTF-8 record");
        assert_eq!(second.log_lines, ["中"]);
        assert!(!second
            .log_lines
            .iter()
            .any(|line| line.contains('\u{fffd}')));
    }

    #[tokio::test]
    async fn initial_reconnect_to_large_active_log_uses_bounded_tail() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        let manager = Arc::new(RunManager::new());
        let _lease = manager
            .reserve(&paths, "run-a".to_string(), false)
            .expect("hold run lock");
        atomic_write_json(&paths.status_file, &status(&paths, "run-a")).expect("write status");
        let line = format!("record-{}\n", "x".repeat(1014));
        fs::write(
            &paths.latest_log,
            line.repeat((INITIAL_BACKLOG_BYTES as usize / line.len()) + 1024),
        )
        .expect("write large active log");

        let response = read_snapshot(
            &paths,
            &SnapshotRequest {
                run_id: None,
                byte_offset: 0,
            },
        )
        .await
        .expect("read reconnect tail");

        assert!(response.reset);
        assert!(response.history_truncated);
        assert!(response.new_byte_offset > 0);
        assert!(response
            .log_lines
            .iter()
            .all(|line| line.starts_with("record-")));
    }
}
