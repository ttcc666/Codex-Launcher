use async_trait::async_trait;
use reqwest::{redirect::Policy, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;

const SERVER_CHAN_ORIGIN: &str = "https://sctapi.ftqq.com/";
const MAX_SEND_KEY_BYTES: usize = 256;
const MAX_RESPONSE_BYTES: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(8);
const RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_DELIVERY_ATTEMPTS: usize = 2;
const UNCONFIRMED_DELIVERY_MESSAGE: &str =
    "Server酱响应确认超时；通知可能已送达，请检查微信后再决定是否重试";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub attempts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryError {
    message: String,
    retryable: bool,
}

impl DeliveryError {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }

    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
}

#[async_trait]
pub(crate) trait ServerChanTransport: Send + Sync {
    async fn send(
        &self,
        send_key: &str,
        title: &str,
        description: &str,
    ) -> Result<(), DeliveryError>;
}

#[derive(Clone)]
pub struct ServerChanClient {
    transport: Arc<dyn ServerChanTransport>,
}

impl ServerChanClient {
    pub fn production() -> Self {
        let transport: Arc<dyn ServerChanTransport> = match ReqwestServerChanTransport::new() {
            Ok(transport) => Arc::new(transport),
            Err(error) => Arc::new(UnavailableTransport { error }),
        };
        Self { transport }
    }

    #[cfg(test)]
    pub(crate) fn with_transport(transport: Arc<dyn ServerChanTransport>) -> Self {
        Self { transport }
    }

    pub async fn deliver(
        &self,
        send_key: &str,
        title: &str,
        description: &str,
    ) -> Result<DeliveryReceipt, String> {
        let send_key = validate_send_key(send_key)?;
        let delivery = self.deliver_inner(&send_key, title, description);
        tokio::time::timeout(DELIVERY_TIMEOUT, delivery)
            .await
            .map_err(|_| UNCONFIRMED_DELIVERY_MESSAGE.to_string())?
    }

    async fn deliver_inner(
        &self,
        send_key: &str,
        title: &str,
        description: &str,
    ) -> Result<DeliveryReceipt, String> {
        let mut last_error = None;
        for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
            match self.transport.send(send_key, title, description).await {
                Ok(()) => return Ok(DeliveryReceipt { attempts: attempt }),
                Err(error) if error.retryable && attempt < MAX_DELIVERY_ATTEMPTS => {
                    last_error = Some(error.message);
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Err(error) => return Err(error.message),
            }
        }
        Err(last_error.unwrap_or_else(|| "Server酱通知发送失败".to_string()))
    }
}

pub fn validate_send_key(send_key: &str) -> Result<String, String> {
    let send_key = send_key.trim();
    if send_key.is_empty() {
        return Err("Server酱 SendKey 不能为空".to_string());
    }
    if send_key.len() > MAX_SEND_KEY_BYTES {
        return Err(format!(
            "Server酱 SendKey 不能超过 {} bytes",
            MAX_SEND_KEY_BYTES
        ));
    }
    if !send_key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("Server酱 SendKey 包含不允许的字符".to_string());
    }
    Ok(send_key.to_string())
}

fn build_endpoint(send_key: &str) -> Result<Url, DeliveryError> {
    let mut url = Url::parse(SERVER_CHAN_ORIGIN)
        .map_err(|_| DeliveryError::permanent("Server酱固定 endpoint 无效"))?;
    url.path_segments_mut()
        .map_err(|_| DeliveryError::permanent("Server酱固定 endpoint 不能写入 path"))?
        .push(&format!("{send_key}.send"));
    Ok(url)
}

struct ReqwestServerChanTransport {
    client: reqwest::Client,
}

impl ReqwestServerChanTransport {
    fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .map(|client| Self { client })
            .map_err(|_| "初始化 Server酱 HTTP client 失败".to_string())
    }
}

#[async_trait]
impl ServerChanTransport for ReqwestServerChanTransport {
    async fn send(
        &self,
        send_key: &str,
        title: &str,
        description: &str,
    ) -> Result<(), DeliveryError> {
        let endpoint = build_endpoint(send_key)?;
        let mut response = self
            .client
            .post(endpoint)
            .form(&[("title", title), ("desp", description)])
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_error(status));
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(DeliveryError::permanent("Server酱响应超过 8 KiB 安全上限"));
            }
            body.extend_from_slice(&chunk);
        }
        parse_business_response(&body, send_key)
    }
}

fn map_reqwest_error(error: reqwest::Error) -> DeliveryError {
    if error.is_connect() && error.is_timeout() {
        DeliveryError::transient("连接 Server酱超时")
    } else if error.is_connect() {
        DeliveryError::transient("无法连接 Server酱服务")
    } else if error.is_timeout() {
        DeliveryError::permanent(UNCONFIRMED_DELIVERY_MESSAGE)
    } else {
        DeliveryError::transient("Server酱网络请求失败")
    }
}

fn http_error(status: StatusCode) -> DeliveryError {
    let message = format!("Server酱返回 HTTP {}", status.as_u16());
    if status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        DeliveryError::transient(message)
    } else {
        DeliveryError::permanent(message)
    }
}

fn parse_business_response(body: &[u8], send_key: &str) -> Result<(), DeliveryError> {
    let response: Value = serde_json::from_slice(body)
        .map_err(|_| DeliveryError::permanent("Server酱返回了无效 JSON"))?;
    let code = response
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| DeliveryError::permanent("Server酱响应缺少业务 code"))?;
    if code == 0 {
        return Ok(());
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .map(|value| redact_and_bound(value, send_key, 240))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "未提供错误详情".to_string());
    Err(DeliveryError::permanent(format!(
        "Server酱业务错误 code={code}: {message}"
    )))
}

fn redact_and_bound(value: &str, secret: &str, max_chars: usize) -> String {
    let redacted = value.replace(secret, "[REDACTED]");
    let mut chars = redacted.chars();
    let bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

struct UnavailableTransport {
    error: String,
}

#[async_trait]
impl ServerChanTransport for UnavailableTransport {
    async fn send(
        &self,
        _send_key: &str,
        _title: &str,
        _description: &str,
    ) -> Result<(), DeliveryError> {
        Err(DeliveryError::permanent(self.error.clone()))
    }
}

#[cfg(test)]
struct QueueTransport {
    results: Mutex<VecDeque<Result<(), DeliveryError>>>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
struct PendingTransport;

#[cfg(test)]
#[async_trait]
impl ServerChanTransport for PendingTransport {
    async fn send(
        &self,
        _send_key: &str,
        _title: &str,
        _description: &str,
    ) -> Result<(), DeliveryError> {
        std::future::pending().await
    }
}

#[cfg(test)]
impl QueueTransport {
    fn new(results: Vec<Result<(), DeliveryError>>) -> Self {
        Self {
            results: Mutex::new(results.into()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl ServerChanTransport for QueueTransport {
    async fn send(
        &self,
        _send_key: &str,
        _title: &str,
        _description: &str,
    ) -> Result<(), DeliveryError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.results
            .lock()
            .expect("queue transport mutex")
            .pop_front()
            .unwrap_or_else(|| Err(DeliveryError::permanent("no fake response")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = "SCT_TEST_KEY_123";

    #[test]
    fn send_key_validation_blocks_path_and_query_injection() {
        assert_eq!(validate_send_key(TEST_KEY).expect("valid key"), TEST_KEY);
        for invalid in ["", "SCT/key", "SCT?key", "SCT.key", "含中文"] {
            assert!(validate_send_key(invalid).is_err(), "{invalid}");
        }
        assert!(validate_send_key(&"A".repeat(MAX_SEND_KEY_BYTES + 1)).is_err());
    }

    #[test]
    fn endpoint_is_fixed_https_origin_with_one_safe_segment() {
        let endpoint = build_endpoint(TEST_KEY).expect("build endpoint");
        assert_eq!(endpoint.scheme(), "https");
        assert_eq!(endpoint.host_str(), Some("sctapi.ftqq.com"));
        assert_eq!(endpoint.path(), "/SCT_TEST_KEY_123.send");
        assert!(endpoint.query().is_none());
    }

    #[test]
    fn business_response_requires_zero_code_and_redacts_secret() {
        assert!(parse_business_response(br#"{"code":0,"message":"ok"}"#, TEST_KEY).is_ok());
        let error = parse_business_response(
            format!(r#"{{"code":40001,"message":"bad {TEST_KEY}"}}"#).as_bytes(),
            TEST_KEY,
        )
        .expect_err("business failure");
        assert!(!error.message.contains(TEST_KEY));
        assert!(error.message.contains("[REDACTED]"));
    }

    #[test]
    fn only_timeout_rate_limit_and_server_errors_are_retryable() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(http_error(status).retryable, "{status}");
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            assert!(!http_error(status).retryable, "{status}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn outer_timeout_bounds_a_hung_transport() {
        let client = ServerChanClient::with_transport(Arc::new(PendingTransport));

        let error = client
            .deliver(TEST_KEY, "title", "description")
            .await
            .expect_err("hung transport must hit the outer timeout");

        assert_eq!(error, UNCONFIRMED_DELIVERY_MESSAGE);
    }

    #[tokio::test]
    async fn transient_failure_retries_once_with_the_same_event() {
        let transport = Arc::new(QueueTransport::new(vec![
            Err(DeliveryError::transient("temporary")),
            Ok(()),
        ]));
        let client = ServerChanClient::with_transport(transport.clone());

        let receipt = client
            .deliver(TEST_KEY, "title", "description")
            .await
            .expect("second attempt succeeds");

        assert_eq!(receipt.attempts, 2);
        assert_eq!(
            transport.calls.load(std::sync::atomic::Ordering::Acquire),
            2
        );
    }

    #[tokio::test]
    async fn permanent_failure_does_not_retry() {
        let transport = Arc::new(QueueTransport::new(vec![Err(DeliveryError::permanent(
            "permanent",
        ))]));
        let client = ServerChanClient::with_transport(transport.clone());

        assert!(client
            .deliver(TEST_KEY, "title", "description")
            .await
            .is_err());
        assert_eq!(
            transport.calls.load(std::sync::atomic::Ordering::Acquire),
            1
        );
    }
}
