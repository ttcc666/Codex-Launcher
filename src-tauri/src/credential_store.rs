use keyring::{Entry, Error as KeyringError};

const CREDENTIAL_SERVICE: &str = "CodexLauncher";
const SERVER_CHAN_ACCOUNT: &str = "serverchan-sendkey";

pub trait CredentialStore: Send + Sync {
    fn get_send_key(&self) -> Result<Option<String>, String>;
    fn set_send_key(&self, send_key: &str) -> Result<(), String>;
    fn delete_send_key(&self) -> Result<bool, String>;
}

#[derive(Debug, Default)]
pub struct WindowsCredentialStore;

impl WindowsCredentialStore {
    fn entry(&self) -> Result<Entry, String> {
        Entry::new(CREDENTIAL_SERVICE, SERVER_CHAN_ACCOUNT)
            .map_err(|_| "初始化 Windows Credential Manager entry 失败".to_string())
    }
}

impl CredentialStore for WindowsCredentialStore {
    fn get_send_key(&self) -> Result<Option<String>, String> {
        match self.entry()?.get_password() {
            Ok(send_key) => Ok(Some(send_key)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(_) => Err("从 Windows Credential Manager 读取 Server酱凭据失败".to_string()),
        }
    }

    fn set_send_key(&self, send_key: &str) -> Result<(), String> {
        self.entry()?
            .set_password(send_key)
            .map_err(|_| "写入 Windows Credential Manager 失败".to_string())
    }

    fn delete_send_key(&self) -> Result<bool, String> {
        match self.entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(_) => Err("从 Windows Credential Manager 删除 Server酱凭据失败".to_string()),
        }
    }
}

#[cfg(test)]
pub struct MemoryCredentialStore {
    value: std::sync::Mutex<Option<String>>,
    fail: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl Default for MemoryCredentialStore {
    fn default() -> Self {
        Self {
            value: std::sync::Mutex::new(None),
            fail: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
impl MemoryCredentialStore {
    pub fn with_send_key(send_key: &str) -> Self {
        Self {
            value: std::sync::Mutex::new(Some(send_key.to_string())),
            fail: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn set_failure(&self, fail: bool) {
        self.fail.store(fail, std::sync::atomic::Ordering::Release);
    }

    fn check_failure(&self) -> Result<(), String> {
        if self.fail.load(std::sync::atomic::Ordering::Acquire) {
            Err("fake credential backend failure".to_string())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
impl CredentialStore for MemoryCredentialStore {
    fn get_send_key(&self) -> Result<Option<String>, String> {
        self.check_failure()?;
        self.value
            .lock()
            .map_err(|_| "fake credential mutex poisoned".to_string())
            .map(|value| value.clone())
    }

    fn set_send_key(&self, send_key: &str) -> Result<(), String> {
        self.check_failure()?;
        *self
            .value
            .lock()
            .map_err(|_| "fake credential mutex poisoned".to_string())? =
            Some(send_key.to_string());
        Ok(())
    }

    fn delete_send_key(&self) -> Result<bool, String> {
        self.check_failure()?;
        Ok(self
            .value
            .lock()
            .map_err(|_| "fake credential mutex poisoned".to_string())?
            .take()
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_deletes_without_real_credentials() {
        let store = MemoryCredentialStore::default();
        assert_eq!(store.get_send_key().expect("read empty store"), None);

        store.set_send_key("SCT_TEST_KEY").expect("save fake key");
        assert_eq!(
            store.get_send_key().expect("read fake key").as_deref(),
            Some("SCT_TEST_KEY")
        );
        assert!(store.delete_send_key().expect("delete fake key"));
        assert!(!store.delete_send_key().expect("delete missing fake key"));
    }

    #[test]
    fn memory_store_surfaces_backend_failures() {
        let store = MemoryCredentialStore::default();
        store.set_failure(true);
        assert!(store.get_send_key().is_err());
        assert!(store.set_send_key("SCT_TEST_KEY").is_err());
        assert!(store.delete_send_key().is_err());
    }
}
