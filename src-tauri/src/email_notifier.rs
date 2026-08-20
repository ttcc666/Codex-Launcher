use keyring::{Entry, Error as KeyringError};
use lettre::{
    message::header::ContentType,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde::Serialize;
use std::time::Duration;

const CREDENTIAL_SERVICE: &str = "CodexLauncher";
const EMAIL_SMTP_ACCOUNT: &str = "email-smtp-password";
const SMTP_TIMEOUT: Duration = Duration::from_secs(15);

// ── 凭据管理（直接操作 keyring，不通过 CredentialStore trait） ─────────────────

fn email_entry() -> Result<Entry, String> {
    Entry::new(CREDENTIAL_SERVICE, EMAIL_SMTP_ACCOUNT)
        .map_err(|_| "初始化邮件 Credential Manager entry 失败".to_string())
}

pub fn get_email_password() -> Result<Option<String>, String> {
    match email_entry()?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(_) => Err("从 Windows Credential Manager 读取邮件密码失败".to_string()),
    }
}

pub fn set_email_password(password: &str) -> Result<(), String> {
    email_entry()?
        .set_password(password)
        .map_err(|_| "写入邮件密码到 Windows Credential Manager 失败".to_string())
}

pub fn delete_email_password() -> Result<bool, String> {
    match email_entry()?.delete_credential() {
        Ok(()) => Ok(true),
        Err(KeyringError::NoEntry) => Ok(false),
        Err(_) => Err("从 Windows Credential Manager 删除邮件密码失败".to_string()),
    }
}

// ── 状态类型 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EmailCredentialStatus {
    pub configured: bool,
}

// ── SMTP 发送 ─────────────────────────────────────────────────────────────────

/// 通过 SMTP 发送邮件。
/// - 端口 465：使用 SSL/TLS（Wrapper）
/// - 其他端口（如 587）：使用 STARTTLS（Required）
pub async fn deliver_email(
    smtp_host: &str,
    smtp_port: u16,
    smtp_username: &str,
    smtp_password: &str,
    to_address: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    let email = Message::builder()
        .from(
            smtp_username
                .parse()
                .map_err(|e| format!("发件人地址格式错误: {e}"))?,
        )
        .to(to_address
            .parse()
            .map_err(|e| format!("收件人地址格式错误: {e}"))?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|e| format!("构建邮件失败: {e}"))?;

    let credentials = Credentials::new(smtp_username.to_string(), smtp_password.to_string());

    let tls_params = TlsParameters::new(smtp_host.to_string())
        .map_err(|e| format!("TLS 参数初始化失败: {e}"))?;

    let tls = if smtp_port == 465 {
        Tls::Wrapper(tls_params) // SSL/TLS（隐式 TLS）
    } else {
        Tls::Required(tls_params) // STARTTLS（显式 TLS）
    };

    let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
        .port(smtp_port)
        .tls(tls)
        .credentials(credentials)
        .timeout(Some(SMTP_TIMEOUT))
        .build();

    transport
        .send(email)
        .await
        .map(|_| ())
        .map_err(|e| format!("邮件发送失败: {e}"))
}
