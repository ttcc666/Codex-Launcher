use keyring::{Entry, Error as KeyringError};
use serde::Serialize;
use url::Url;

const CREDENTIAL_SERVICE: &str = "CodexLauncher";
const PROXY_PASSWORD_ACCOUNT: &str = "proxy-password";

const PROXY_SCHEMES: &[&str] = &["http", "https", "socks5", "socks5h"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyCredentialStatus {
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SetProxyResult {
    pub url: String,
    pub configured: bool,
}

fn proxy_entry() -> Result<Entry, String> {
    Entry::new(CREDENTIAL_SERVICE, PROXY_PASSWORD_ACCOUNT)
        .map_err(|_| "初始化代理 Credential Manager entry 失败".to_string())
}

pub fn get_proxy_password() -> Result<Option<String>, String> {
    match proxy_entry()?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(_) => Err("从 Windows Credential Manager 读取代理密码失败".to_string()),
    }
}

pub fn set_proxy_password(password: &str) -> Result<(), String> {
    proxy_entry()?
        .set_password(password)
        .map_err(|_| "写入代理密码到 Windows Credential Manager 失败".to_string())
}

pub fn delete_proxy_password() -> Result<bool, String> {
    match proxy_entry()?.delete_credential() {
        Ok(()) => Ok(true),
        Err(KeyringError::NoEntry) => Ok(false),
        Err(_) => Err("从 Windows Credential Manager 删除代理密码失败".to_string()),
    }
}

/// 校验代理 URL（可含或不含密码）。返回归一化后的 `Url`。
pub fn validate_proxy_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value.trim()).map_err(|error| format!("代理 URL 无效: {error}"))?;
    if !PROXY_SCHEMES.contains(&url.scheme()) {
        return Err(format!("代理协议必须是 {}", PROXY_SCHEMES.join("/")));
    }
    if url.host_str().is_none() {
        return Err("代理 URL 必须包含 host".to_string());
    }
    Ok(url)
}

/// 从完整 URL 抽离密码：返回去密码后的 URL 字符串与抽出的密码（如有）。
#[allow(dead_code)]
pub fn split_proxy_url(full_url: &str) -> Result<(String, Option<String>), String> {
    let mut url = validate_proxy_url(full_url)?;
    let password = url.password().map(str::to_owned).filter(|p| !p.is_empty());
    url.set_password(None)
        .map_err(|_| "剥离代理密码失败".to_string())?;
    Ok((url.to_string(), password))
}

/// 原地剥离代理密码：把 `url_field` 更新为去密码版本，返回抽出的密码（如有）。
/// 对禁用或空 URL 宽容（不报错）；仅当 scheme 合法且确含密码时才剥离并返回。
pub fn extract_proxy_password(url_field: &mut String) -> Result<Option<String>, String> {
    let trimmed = url_field.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let mut url = match Url::parse(trimmed) {
        Ok(url) => url,
        Err(_) => return Ok(None),
    };
    if !PROXY_SCHEMES.contains(&url.scheme()) {
        return Ok(None);
    }
    let password = url.password().map(str::to_owned).filter(|p| !p.is_empty());
    match password {
        Some(pw) => {
            url.set_password(None)
                .map_err(|_| "剥离代理密码失败".to_string())?;
            *url_field = url.to_string();
            Ok(Some(pw))
        }
        None => Ok(None),
    }
}

/// 拼回完整代理 URL：去密码的配置 URL + 凭据库密码。
pub fn resolve_full_proxy_url(config_url: &str) -> Result<Option<String>, String> {
    let trimmed = config_url.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let mut url = validate_proxy_url(trimmed)?;
    if let Some(password) = get_proxy_password()? {
        url.set_password(Some(&password))
            .map_err(|_| "拼接代理密码失败".to_string())?;
    }
    Ok(Some(url.to_string()))
}

/// 构造子进程环境变量（大小写双写以兼容不同工具链读取习惯）。
pub fn build_proxy_env_vars(full_url: &str) -> Vec<(&'static str, String)> {
    let url = full_url.to_string();
    vec![
        ("HTTP_PROXY", url.clone()),
        ("http_proxy", url.clone()),
        ("HTTPS_PROXY", url.clone()),
        ("https_proxy", url.clone()),
        ("ALL_PROXY", url.clone()),
        ("all_proxy", url),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_supported_schemes_with_host() {
        for ok in [
            "http://127.0.0.1:7890",
            "https://proxy.example.com:8080",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
            "http://user@127.0.0.1:7890",
            "http://user:pass@127.0.0.1:7890",
        ] {
            validate_proxy_url(ok).expect(ok);
        }
    }

    #[test]
    fn validate_rejects_unsupported_schemes_and_missing_host() {
        assert!(validate_proxy_url("ftp://127.0.0.1:7890").is_err());
        assert!(validate_proxy_url("http://").is_err());
        assert!(validate_proxy_url("not-a-url").is_err());
        assert!(validate_proxy_url("").is_err());
    }

    #[test]
    fn split_proxy_url_strips_password_keeps_user() {
        let (url, password) = split_proxy_url("http://user:pass@127.0.0.1:7890").expect("split");
        assert_eq!(url, "http://user@127.0.0.1:7890/");
        assert_eq!(password.as_deref(), Some("pass"));
    }

    #[test]
    fn split_proxy_url_without_password_is_noop() {
        let (url, password) = split_proxy_url("socks5://127.0.0.1:1080").expect("split");
        assert_eq!(url, "socks5://127.0.0.1:1080");
        assert!(password.is_none());
    }

    #[test]
    fn extract_proxy_password_strips_in_place_and_returns_secret() {
        let mut url = "http://user:secret@proxy.example.com:8080".to_string();
        let password = extract_proxy_password(&mut url).expect("extract");
        assert_eq!(password.as_deref(), Some("secret"));
        assert_eq!(url, "http://user@proxy.example.com:8080/");
    }

    #[test]
    fn extract_proxy_password_is_tolerant_of_empty_and_invalid_when_disabled() {
        let mut empty = String::new();
        assert!(extract_proxy_password(&mut empty).expect("empty").is_none());
        assert!(empty.is_empty());

        let mut invalid = "garbage".to_string();
        assert!(extract_proxy_password(&mut invalid)
            .expect("invalid")
            .is_none());
        assert_eq!(invalid, "garbage");

        let mut wrong_scheme = "ftp://user:pw@host:21".to_string();
        assert!(extract_proxy_password(&mut wrong_scheme)
            .expect("scheme")
            .is_none());
        assert_eq!(wrong_scheme, "ftp://user:pw@host:21");
    }

    #[test]
    fn build_proxy_env_vars_emits_six_keys_with_identical_value() {
        let vars = build_proxy_env_vars("socks5://127.0.0.1:1080");
        assert_eq!(vars.len(), 6);
        for (key, value) in &vars {
            assert_eq!(value, "socks5://127.0.0.1:1080", "{key}");
        }
        let keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"HTTP_PROXY"));
        assert!(keys.contains(&"http_proxy"));
        assert!(keys.contains(&"HTTPS_PROXY"));
        assert!(keys.contains(&"https_proxy"));
        assert!(keys.contains(&"ALL_PROXY"));
        assert!(keys.contains(&"all_proxy"));
    }
}
