use peerdav_lib::{AppError, DomainError};
use std::error::Error;
use std::fmt;

#[derive(Debug)]
struct Cause(&'static str);
impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for Cause {}

fn cloud(code: &'static str, reason: &'static str, cause: &'static str) -> DomainError {
    DomainError::Cloud {
        code,
        user_reason: reason,
        source: Box::new(Cause(cause)),
    }
}

#[test]
fn stable_cloud_code_and_structured_shape() {
    let error = AppError::from(cloud(
        "CLOUD_NOT_FOUND",
        "云端对象不存在",
        "provider: missing",
    ));
    assert_eq!(error.code, "CLOUD_NOT_FOUND");
    assert!(error.message.contains("云存储操作失败"));
    assert!(error.detail.is_some());
    assert!(error.context.is_some());
}

#[test]
fn source_chain_is_retained_in_safe_detail() {
    let error = AppError::from(cloud(
        "CLOUD_PRECONDITION",
        "云端前置条件失败",
        "transport: timeout",
    ));
    let detail = error.detail.expect("cloud detail");
    assert!(detail.contains("云端前置条件失败"));
    assert!(detail.contains("transport: timeout"));
}

#[test]
fn signed_urls_are_not_exposed() {
    let error = AppError::from(cloud(
        "CLOUD_CONFIG_INVALID",
        "配置无效",
        "request https://user:password@example.test/object?X-Amz-Signature=secret123",
    ));
    assert!(!error.message.contains("https://"));
    assert!(!error.message.contains("secret123"));
    assert!(
        !error
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("secret123")
    );
}

#[test]
fn credential_markers_are_redacted() {
    let error = AppError::from(cloud(
        "CLOUD_CONFIG_INVALID",
        "配置无效",
        "secret_key=secret-value session_token=token-value Authorization=Bearer-secret",
    ));
    let text = format!("{} {}", error.message, error.detail.unwrap_or_default());
    assert!(!text.contains("secret-value"));
    assert!(!text.contains("token-value"));
    assert!(!text.contains("Bearer-secret"));
}

#[test]
fn status_like_transport_reason_is_not_generic() {
    let error = AppError::from(cloud(
        "CLOUD_PRECONDITION",
        "云存储传输失败",
        "HTTP 503 Service Unavailable",
    ));
    assert!(error.message.contains("HTTP 503"));
    assert!(!error.message.contains("未知错误"));
}

#[test]
fn split_headers_json_and_multiline_secrets_never_cross_ipc() {
    for cause in [
        "HTTP 407 Proxy-Authorization: Basic PROXY_CREDENTIAL_MARKER",
        "HTTP 401 authorization: Bearer BEARER_CREDENTIAL_MARKER",
        "HTTP 403 {\"secretKey\": \"JSON_CREDENTIAL_MARKER\"}",
        "HTTP 503 password:\n MULTILINE_CREDENTIAL_MARKER",
        "HTTP 400 request https://username:URL_CREDENTIAL_MARKER@example.test/?x-amz-signature=SIGNATURE_MARKER",
    ] {
        let error = AppError::from(cloud("CLOUD_STORAGE", "云存储请求失败", cause));
        let output = serde_json::to_string(&error).unwrap();
        for marker in [
            "PROXY_CREDENTIAL_MARKER",
            "BEARER_CREDENTIAL_MARKER",
            "JSON_CREDENTIAL_MARKER",
            "MULTILINE_CREDENTIAL_MARKER",
            "URL_CREDENTIAL_MARKER",
            "SIGNATURE_MARKER",
        ] {
            assert!(!output.contains(marker), "leaked marker in {output}");
        }
    }
}

#[test]
fn domain_source_chain_remains_available_before_ipc_mapping() {
    let domain = cloud(
        "CLOUD_UNSUPPORTED",
        "云服务不支持",
        "provider capability mismatch",
    );
    let source = (&domain as &dyn Error).source().expect("source retained");
    assert_eq!(source.to_string(), "provider capability mismatch");
}
