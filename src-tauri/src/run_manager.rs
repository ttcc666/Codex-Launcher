use crate::app_storage::{atomic_write_json, read_json, AppPaths};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const LOCK_VERSION: u32 = 1;
const STOP_REQUEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RunLockMetadata {
    version: u32,
    run_id: String,
    owner_pid: u32,
    started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StopRequest {
    pub version: u32,
    pub run_id: String,
    pub requested_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopTarget {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownWaitResult {
    NoLocalRun,
    Completed(String),
    TimedOut(String),
}

#[derive(Debug, Clone)]
struct ActiveRun {
    run_id: String,
    cancellation: CancellationToken,
    child_pid: Option<u32>,
    completion: watch::Receiver<bool>,
}

#[derive(Debug, Default)]
pub struct RunManager {
    active: Mutex<Option<ActiveRun>>,
}

impl RunManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve(self: &Arc<Self>, paths: &AppPaths, run_id: String) -> Result<RunLease, String> {
        let process_lock = ProcessLock::try_acquire(
            &paths.run_lock,
            &RunLockMetadata {
                version: LOCK_VERSION,
                run_id: run_id.clone(),
                owner_pid: std::process::id(),
                started_at: Utc::now().to_rfc3339(),
            },
        )?;

        let cancellation = CancellationToken::new();
        let (completion_sender, completion) = watch::channel(false);
        let mut active = self
            .active
            .lock()
            .map_err(|_| "本地 run reservation mutex 已损坏".to_string())?;
        if let Some(current) = active.as_ref() {
            return Err(format!("本进程已有任务在运行: {}", current.run_id));
        }
        *active = Some(ActiveRun {
            run_id: run_id.clone(),
            cancellation: cancellation.clone(),
            child_pid: None,
            completion,
        });
        drop(active);

        Ok(RunLease {
            manager: self.clone(),
            paths: paths.clone(),
            run_id,
            cancellation,
            completion_sender,
            process_lock: Some(process_lock),
        })
    }

    pub fn request_stop(&self, paths: &AppPaths, run_id: &str) -> Result<StopTarget, String> {
        if run_id.trim().is_empty() {
            return Err("停止请求缺少 run ID".to_string());
        }

        let active = self
            .active
            .lock()
            .map_err(|_| "本地 run reservation mutex 已损坏".to_string())?;
        if let Some(local) = active.as_ref() {
            if local.run_id != run_id {
                return Err(format!(
                    "run ID 已变化，当前本地 run 为 {}，拒绝停止 {}",
                    local.run_id, run_id
                ));
            }
            local.cancellation.cancel();
            return Ok(StopTarget::Local);
        }
        drop(active);

        atomic_write_json(
            &paths.stop_request,
            &StopRequest {
                version: STOP_REQUEST_VERSION,
                run_id: run_id.to_string(),
                requested_at: Utc::now().to_rfc3339(),
            },
        )?;
        Ok(StopTarget::Remote)
    }

    pub fn local_owned_run_id(&self) -> Result<Option<String>, String> {
        let active = self
            .active
            .lock()
            .map_err(|_| "本地 run reservation mutex 已损坏".to_string())?;
        Ok(active.as_ref().map(|run| run.run_id.clone()))
    }

    pub async fn cancel_local_owned_and_wait(
        &self,
        timeout: Duration,
    ) -> Result<ShutdownWaitResult, String> {
        let (run_id, cancellation, mut completion) = {
            let active = self
                .active
                .lock()
                .map_err(|_| "本地 run reservation mutex 已损坏".to_string())?;
            let Some(local) = active.as_ref() else {
                return Ok(ShutdownWaitResult::NoLocalRun);
            };
            (
                local.run_id.clone(),
                local.cancellation.clone(),
                local.completion.clone(),
            )
        };

        cancellation.cancel();
        if *completion.borrow() {
            return Ok(ShutdownWaitResult::Completed(run_id));
        }

        let completed = tokio::time::timeout(timeout, async {
            loop {
                completion
                    .changed()
                    .await
                    .map_err(|_| "run completion channel 意外关闭".to_string())?;
                if *completion.borrow() {
                    return Ok::<(), String>(());
                }
            }
        })
        .await;

        match completed {
            Ok(Ok(())) => Ok(ShutdownWaitResult::Completed(run_id)),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(ShutdownWaitResult::TimedOut(run_id)),
        }
    }

    fn set_child_pid(&self, run_id: &str, child_pid: Option<u32>) -> Result<(), String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "本地 run reservation mutex 已损坏".to_string())?;
        let local = active
            .as_mut()
            .ok_or_else(|| format!("run reservation 已释放: {}", run_id))?;
        if local.run_id != run_id {
            return Err(format!(
                "run reservation 不匹配，当前 {}，请求 {}",
                local.run_id, run_id
            ));
        }
        local.child_pid = child_pid;
        Ok(())
    }

    fn release(&self, run_id: &str) {
        match self.active.lock() {
            Ok(mut active) if active.as_ref().is_some_and(|run| run.run_id == run_id) => {
                *active = None;
            }
            Ok(_) => {}
            Err(_) => eprintln!("释放 run reservation 时 mutex 已损坏: {}", run_id),
        }
    }
}

pub struct RunLease {
    manager: Arc<RunManager>,
    paths: AppPaths,
    run_id: String,
    cancellation: CancellationToken,
    completion_sender: watch::Sender<bool>,
    process_lock: Option<ProcessLock>,
}

impl RunLease {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn set_child_pid(&self, child_pid: Option<u32>) -> Result<(), String> {
        self.manager.set_child_pid(&self.run_id, child_pid)
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        if let Err(error) = clear_stop_request_if_matches(&self.paths, &self.run_id) {
            eprintln!("{}", error);
        }
        self.process_lock.take();
        self.manager.release(&self.run_id);
        let _ = self.completion_sender.send(true);
    }
}

pub fn is_stop_requested(paths: &AppPaths, run_id: &str) -> Result<bool, String> {
    if !paths.stop_request.exists() {
        return Ok(false);
    }
    let request: StopRequest = read_json(&paths.stop_request)?;
    if request.version != STOP_REQUEST_VERSION {
        return Err(format!(
            "不支持的 stop request 版本 {} [{}]",
            request.version,
            paths.stop_request.display()
        ));
    }
    Ok(request.run_id == run_id)
}

pub fn clear_stop_request_if_matches(paths: &AppPaths, run_id: &str) -> Result<(), String> {
    if !paths.stop_request.exists() {
        return Ok(());
    }
    let request: StopRequest = read_json(&paths.stop_request)?;
    if request.run_id != run_id {
        return Ok(());
    }
    fs::remove_file(&paths.stop_request).map_err(|error| {
        format!(
            "删除 stop request 失败 [{}]: {}",
            paths.stop_request.display(),
            error
        )
    })
}

pub(crate) struct ProcessLock {
    file: File,
    _path: PathBuf,
}

pub(crate) struct MaintenanceLease {
    file: File,
}

impl MaintenanceLease {
    pub(crate) fn acquire(path: &Path) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("maintenance lock 路径没有父目录: {}", path.display()))?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "创建 maintenance lock 目录失败 [{}]: {}",
                parent.display(),
                error
            )
        })?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                format!("打开 maintenance lock 失败 [{}]: {}", path.display(), error)
            })?;
        file.lock_exclusive().map_err(|error| {
            format!("获取 maintenance lock 失败 [{}]: {}", path.display(), error)
        })?;
        Ok(Self { file })
    }
}

impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            eprintln!("释放 maintenance lock 失败: {}", error);
        }
    }
}

impl ProcessLock {
    fn try_acquire(path: &Path, metadata: &RunLockMetadata) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("run lock 路径没有父目录: {}", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 run lock 目录失败 [{}]: {}", parent.display(), error))?;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("打开 run lock 失败 [{}]: {}", path.display(), error))?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock
                || matches!(error.raw_os_error(), Some(32 | 33))
            {
                "已有另一个 Codex Launcher run 正在执行".to_string()
            } else {
                format!("获取 run lock 失败 [{}]: {}", path.display(), error)
            }
        })?;

        let metadata = serde_json::to_vec_pretty(metadata)
            .map_err(|error| format!("序列化 run lock metadata 失败: {}", error))?;
        file.set_len(0)
            .and_then(|_| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(&metadata))
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                format!(
                    "写入 run lock metadata 失败 [{}]: {}",
                    path.display(),
                    error
                )
            })?;

        Ok(Self {
            file,
            _path: path.to_path_buf(),
        })
    }

    pub(crate) fn try_acquire_existing(path: &Path) -> Result<Option<Self>, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("run lock 路径没有父目录: {}", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 run lock 目录失败 [{}]: {}", parent.display(), error))?;

        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("打开 run lock 失败 [{}]: {}", path.display(), error))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self {
                file,
                _path: path.to_path_buf(),
            })),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || matches!(error.raw_os_error(), Some(32 | 33)) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!(
                "探测 run lock 失败 [{}]: {}",
                path.display(),
                error
            )),
        }
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            eprintln!("释放 run lock 失败: {}", error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const HELPER_ROOT_ENV: &str = "CODEX_LAUNCHER_LOCK_HELPER_ROOT";
    const HELPER_READY_ENV: &str = "CODEX_LAUNCHER_LOCK_HELPER_READY";

    #[test]
    fn two_local_reservations_cannot_overlap() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());

        let first = manager
            .reserve(&paths, "first".to_string())
            .expect("reserve first run");
        assert!(manager.reserve(&paths, "second".to_string()).is_err());
        drop(first);
        manager
            .reserve(&paths, "second".to_string())
            .expect("reserve after first releases");
    }

    #[test]
    fn stale_lock_file_without_os_lock_does_not_block_start() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        fs::write(&paths.run_lock, b"stale metadata").expect("write stale lock file");

        let manager = Arc::new(RunManager::new());
        manager
            .reserve(&paths, "fresh".to_string())
            .expect("stale file must not imply an active OS lock");
    }

    #[tokio::test]
    async fn cancellation_wait_completes_only_after_lease_cleanup() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let lease = manager
            .reserve(&paths, "run-a".to_string())
            .expect("reserve run");
        let dropper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(lease);
        });

        assert_eq!(
            manager
                .cancel_local_owned_and_wait(Duration::from_secs(1))
                .await
                .expect("cancel and wait"),
            ShutdownWaitResult::Completed("run-a".to_string())
        );
        dropper.await.expect("lease dropper");
        assert_eq!(manager.local_owned_run_id().expect("read active run"), None);
    }

    #[tokio::test]
    async fn cancellation_wait_has_a_bounded_timeout() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        let manager = Arc::new(RunManager::new());
        let lease = manager
            .reserve(&paths, "run-a".to_string())
            .expect("reserve run");

        assert_eq!(
            manager
                .cancel_local_owned_and_wait(Duration::from_millis(10))
                .await
                .expect("cancel and wait"),
            ShutdownWaitResult::TimedOut("run-a".to_string())
        );
        drop(lease);
    }

    #[test]
    fn mismatched_stop_request_does_not_cancel_current_run() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::from_root(temp.path().join("app-data"));
        paths.ensure_directories().expect("create app dirs");
        atomic_write_json(
            &paths.stop_request,
            &StopRequest {
                version: STOP_REQUEST_VERSION,
                run_id: "old-run".to_string(),
                requested_at: Utc::now().to_rfc3339(),
            },
        )
        .expect("write stop request");

        assert!(!is_stop_requested(&paths, "current-run").expect("read stop request"));
        assert!(is_stop_requested(&paths, "old-run").expect("read stop request"));
    }

    #[test]
    fn cross_process_lock_helper() {
        let Some(root) = std::env::var_os(HELPER_ROOT_ENV) else {
            return;
        };
        let ready = PathBuf::from(
            std::env::var_os(HELPER_READY_ENV).expect("helper ready path environment variable"),
        );
        let paths = AppPaths::from_root(PathBuf::from(root));
        let manager = Arc::new(RunManager::new());
        let _lease = manager
            .reserve(&paths, "child-process".to_string())
            .expect("child process acquires lock");
        fs::write(&ready, b"ready").expect("signal helper readiness");
        std::thread::sleep(Duration::from_secs(10));
    }

    #[test]
    fn independent_processes_contend_and_window_close_does_not_touch_remote_owner() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let root = temp.path().join("app-data");
        let ready = temp.path().join("ready");
        let mut child = spawn_lock_helper(&root, &ready);

        wait_for_file(&ready, &mut child);
        let paths = AppPaths::from_root(root);
        let manager = Arc::new(RunManager::new());
        assert_eq!(manager.local_owned_run_id().expect("read local run"), None);
        assert!(manager
            .reserve(&paths, "parent-process".to_string())
            .is_err());
        assert!(child.try_wait().expect("query helper").is_none());

        child.kill().expect("terminate helper");
        child.wait().expect("wait for helper");
    }

    fn spawn_lock_helper(root: &Path, ready: &Path) -> Child {
        Command::new(std::env::current_exe().expect("test executable path"))
            .args([
                "--exact",
                "run_manager::tests::cross_process_lock_helper",
                "--nocapture",
            ])
            .env(HELPER_ROOT_ENV, root)
            .env(HELPER_READY_ENV, ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lock helper")
    }

    fn wait_for_file(path: &Path, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            if let Some(status) = child.try_wait().expect("query helper") {
                panic!("lock helper exited early: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "lock helper did not become ready"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
