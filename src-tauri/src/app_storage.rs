use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub root_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub config_file: PathBuf,
    pub config_v1_backup: PathBuf,
    pub scheduler_state: PathBuf,
    pub status_file: PathBuf,
    pub status_html: PathBuf,
    pub latest_log: PathBuf,
    pub run_lock: PathBuf,
    pub maintenance_lock: PathBuf,
    pub stop_request: PathBuf,
    pub keep_alive_request: PathBuf,
    pub crash_log: PathBuf,
    pub headless_log: PathBuf,
    pub notifications_log: PathBuf,
    pub uninstall_log: PathBuf,
    pub webview_data_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, String> {
        let local_app_data = dirs::data_local_dir()
            .ok_or_else(|| "无法解析 Windows LOCALAPPDATA 目录".to_string())?;
        Ok(Self::from_root(local_app_data.join("CodexLauncher")))
    }

    pub fn from_root(root_dir: PathBuf) -> Self {
        let logs_dir = root_dir.join("logs");
        Self {
            config_file: root_dir.join("launcher-config.json"),
            config_v1_backup: root_dir.join("launcher-config.v1.backup.json"),
            scheduler_state: root_dir.join("scheduler-state.json"),
            status_file: root_dir.join("status.json"),
            status_html: root_dir.join("status.html"),
            run_lock: root_dir.join("run.lock"),
            maintenance_lock: root_dir.join("maintenance.lock"),
            stop_request: root_dir.join("stop-request.json"),
            keep_alive_request: root_dir.join("keep-alive-request.json"),
            crash_log: logs_dir.join("crash.log"),
            headless_log: logs_dir.join("headless.log"),
            notifications_log: logs_dir.join("notifications.log"),
            uninstall_log: logs_dir.join("uninstall.log"),
            latest_log: logs_dir.join("latest.log"),
            webview_data_dir: root_dir.join("webview2"),
            logs_dir,
            root_dir,
        }
    }

    pub fn ensure_directories(&self) -> Result<(), String> {
        fs::create_dir_all(&self.logs_dir)
            .map_err(|error| path_error("创建日志目录", &self.logs_dir, error))?;
        fs::create_dir_all(&self.webview_data_dir)
            .map_err(|error| path_error("创建 WebView2 数据目录", &self.webview_data_dir, error))?;
        Ok(())
    }

    pub fn migrate_legacy_config<I>(&self, legacy_log_dirs: I) -> Result<Option<PathBuf>, String>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        if self.config_file.exists() {
            return Ok(None);
        }

        self.ensure_directories()?;
        let mut visited = Vec::new();
        for legacy_dir in legacy_log_dirs {
            if visited.iter().any(|seen: &PathBuf| seen == &legacy_dir) {
                continue;
            }
            visited.push(legacy_dir.clone());

            let legacy_config = legacy_dir.join("launcher-config.json");
            if !legacy_config.is_file() {
                continue;
            }

            let bytes = fs::read(&legacy_config)
                .map_err(|error| path_error("读取 legacy 配置", &legacy_config, error))?;
            atomic_write_bytes(&self.config_file, &bytes)?;
            return Ok(Some(legacy_config));
        }

        Ok(None)
    }
}

pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("序列化 JSON 失败 [{}]: {}", path.display(), error))?;
    atomic_write_bytes(path, &bytes)
}

pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("目标路径没有父目录: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| path_error("创建目录", parent, error))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("目标文件名不是有效 Unicode: {}", path.display()))?;

    let temp_path = loop {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = fs::remove_file(&candidate);
                    return Err(path_error("写入临时文件", &candidate, error));
                }
                drop(file);
                break candidate;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(path_error("创建临时文件", &candidate, error)),
        }
    };

    if let Err(error) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(path_error("原子替换文件", path, error));
    }

    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let content = fs::read_to_string(path).map_err(|error| path_error("读取 JSON", path, error))?;
    serde_json::from_str(&content)
        .map_err(|error| format!("解析 JSON 失败 [{}]: {}", path.display(), error))
}

pub fn append_bounded_text_log(path: &Path, entry: &str, max_bytes: usize) -> Result<(), String> {
    if max_bytes == 0 {
        return Err("bounded log 最大字节数必须大于 0".to_string());
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("日志路径没有父目录: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| path_error("创建日志目录", parent, error))?;

    let mut record = entry.to_string();
    if !record.ends_with('\n') {
        record.push('\n');
    }
    if record.len() > max_bytes {
        let mut start = record.len() - max_bytes;
        while start < record.len() && !record.is_char_boundary(start) {
            start += 1;
        }
        record = record[start..].to_string();
    }

    let existing_len = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if existing_len.saturating_add(record.len() as u64) > max_bytes as u64 {
        let full_marker = "[older diagnostic entries truncated]\n";
        let marker = if full_marker.len() <= max_bytes {
            full_marker
        } else {
            ""
        };
        let available = max_bytes.saturating_sub(marker.len());
        if record.len() > available {
            let mut start = record.len() - available;
            while start < record.len() && !record.is_char_boundary(start) {
                start += 1;
            }
            record = record[start..].to_string();
        }
        let mut replacement = String::with_capacity(marker.len() + record.len());
        replacement.push_str(marker);
        replacement.push_str(&record);
        return atomic_write_bytes(path, replacement.as_bytes());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| path_error("打开诊断日志", path, error))?;
    file.write_all(record.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| path_error("追加诊断日志", path, error))
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    for attempt in 0..=40_u64 {
        let result = unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result != 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();
        let is_transient_share_conflict = matches!(error.raw_os_error(), Some(5 | 32 | 33));
        if !is_transient_share_conflict || attempt == 40 {
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis((attempt + 1).min(10)));
    }

    unreachable!("bounded retry loop always returns")
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn path_error(action: &str, path: &Path, error: io::Error) -> String {
    format!("{}失败 [{}]: {}", action, path.display(), error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestDocument {
        generation: u64,
        payload: String,
    }

    #[test]
    fn legacy_migration_is_copy_only_and_never_overwrites_new_config() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("new-root"));
        let legacy_dir = temp.path().join("legacy").join("logs");
        fs::create_dir_all(&legacy_dir).expect("create legacy dir");
        let legacy_file = legacy_dir.join("launcher-config.json");
        fs::write(&legacy_file, br#"{"command":"legacy"}"#).expect("write legacy config");

        let migrated = paths
            .migrate_legacy_config([legacy_dir.clone()])
            .expect("migrate legacy config");

        assert_eq!(migrated.as_deref(), Some(legacy_file.as_path()));
        assert!(legacy_file.exists(), "legacy file must remain in place");
        assert_eq!(
            fs::read(&paths.config_file).expect("read migrated config"),
            br#"{"command":"legacy"}"#
        );

        fs::write(&legacy_file, br#"{"command":"changed legacy"}"#).expect("update legacy config");
        assert_eq!(
            paths
                .migrate_legacy_config([legacy_dir])
                .expect("skip second migration"),
            None
        );
        assert_eq!(
            fs::read(&paths.config_file).expect("read new config"),
            br#"{"command":"legacy"}"#
        );
    }

    #[test]
    fn concurrent_atomic_writes_are_always_complete_json_documents() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let target = Arc::new(temp.path().join("status.json"));
        atomic_write_json(
            &target,
            &TestDocument {
                generation: 0,
                payload: "initial".repeat(256),
            },
        )
        .expect("write initial document");

        let writer_path = Arc::clone(&target);
        let writer = std::thread::spawn(move || {
            for generation in 1..=100 {
                atomic_write_json(
                    &writer_path,
                    &TestDocument {
                        generation,
                        payload: format!("generation-{generation}").repeat(256),
                    },
                )
                .expect("write complete document");
            }
        });

        while !writer.is_finished() {
            let document: TestDocument = read_json(&target).expect("read complete JSON document");
            assert!(!document.payload.is_empty());
            std::thread::yield_now();
        }
        writer.join().expect("writer thread");

        let final_document: TestDocument = read_json(&target).expect("read final document");
        assert_eq!(final_document.generation, 100);
    }

    #[test]
    fn bounded_text_log_never_exceeds_limit() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = temp.path().join("logs").join("headless.log");

        for index in 0..20 {
            append_bounded_text_log(&path, &format!("entry-{index}-{}", "x".repeat(32)), 128)
                .expect("append bounded log");
        }

        let bytes = fs::read(&path).expect("read bounded log");
        assert!(bytes.len() <= 128);
        assert!(std::str::from_utf8(&bytes).is_ok());

        append_bounded_text_log(&path, "中文-overflow", 8).expect("append tiny bounded log");
        let tiny = fs::read(&path).expect("read tiny bounded log");
        assert!(tiny.len() <= 8);
        assert!(std::str::from_utf8(&tiny).is_ok());
    }
}
