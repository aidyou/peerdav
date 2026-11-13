use peerdav_lib::cloud::types::CloudError;
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
fn service_status_is_readable_and_diagnostics_keep_original_response() {
    let error = AppError::from(DomainError::from(CloudError::Service { status: 503, detail: "upstream maintenance".into() }));
    assert!(error.message.contains("存储服务暂时不可用"));
    assert!(!error.message.contains("503"));
    assert!(error.detail.as_deref().unwrap().contains("503"));
    assert!(error.detail.as_deref().unwrap().contains("upstream maintenance"));
    assert_eq!(error.context.as_ref().unwrap()["status"], 503);
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
    assert!(error.message.contains("存储服务暂时不可用"));
    assert!(!error.message.contains("HTTP 503"));
    assert!(error.detail.as_deref().unwrap_or_default().contains("HTTP 503"));
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

#[test]
fn physical_identity_errors_have_stable_structured_codes_at_boundary() {
    use peerdav_lib::cloud::{ObjectGuardError, ObjectServiceError};
    for (error, expected) in [
        (
            ObjectGuardError::IdentityUnconfirmed,
            "CLOUD_PHYSICAL_IDENTITY_UNCONFIRMED",
        ),
        (
            ObjectGuardError::InvalidBinding,
            "CLOUD_PHYSICAL_BINDING_INVALID",
        ),
        (ObjectGuardError::BindingBusy, "CLOUD_PHYSICAL_BINDING_BUSY"),
    ] {
        let original = error.to_string();
        let domain = DomainError::from(ObjectServiceError::Guard(error));
        assert_eq!(domain.source().unwrap().to_string(), original);
        let mapped = AppError::from(domain);
        assert_eq!(mapped.code, expected);
        assert!(mapped.message.contains("物理存储"));
        assert!(!mapped.message.contains(" / "));
        assert!(mapped.detail.as_deref().unwrap().contains(&original));
        let serialized = serde_json::to_value(&mapped).unwrap();
        assert!(serialized["code"].is_string());
        assert!(serialized["message"].is_string());
        assert!(serialized["context"].is_object());
    }
}

#[test]
fn object_service_guard_storage_source_is_retained_and_redacted_at_boundary() {
    use peerdav_lib::cloud::{ObjectGuardError, ObjectServiceError};
    let domain = DomainError::from(ObjectServiceError::Guard(ObjectGuardError::Storage {
        source: Box::new(Cause(
            "HTTP 503 secret_key=GUARD_SECRET_MARKER request https://user:PASSWORD_MARKER@example.test/?X-Amz-Signature=SIGNATURE_MARKER",
        )),
    }));
    assert!(
        domain
            .source()
            .unwrap()
            .source()
            .unwrap()
            .to_string()
            .contains("GUARD_SECRET_MARKER")
    );
    let error = AppError::from(domain);
    assert_eq!(error.code, "CLOUD_GUARD_STORAGE");
    assert!(error.detail.as_deref().unwrap().contains("HTTP 503"));
    let serialized = serde_json::to_string(&error).unwrap();
    for marker in ["GUARD_SECRET_MARKER", "PASSWORD_MARKER", "SIGNATURE_MARKER"] {
        assert!(
            !serialized.contains(marker),
            "leaked {marker}: {serialized}"
        );
    }
}

#[test]
fn object_service_cloud_wrapper_preserves_existing_cloud_boundary_semantics() {
    use peerdav_lib::cloud::ObjectServiceError;
    let error = AppError::from(DomainError::from(ObjectServiceError::Cloud(
        CloudError::CompleteUnknown {
            source: object_error("HTTP 503 response lost"),
        },
    )));
    assert_eq!(error.code, "CLOUD_UPLOAD_PENDING_RECOVERY");
    assert_eq!(error.context.as_ref().unwrap()["pendingRecovery"], true);
    assert!(
        error
            .detail
            .as_deref()
            .unwrap()
            .contains("HTTP 503 response lost")
    );
}

fn object_error(message: &'static str) -> object_store::Error {
    object_store::Error::Generic {
        store: "cloud-error-flow",
        source: Box::new(Cause(message)),
    }
}

#[test]
fn complete_unknown_maps_to_pending_recovery_not_cancellation() {
    let error = AppError::from(DomainError::from(CloudError::CompleteUnknown {
        source: object_error("HTTP 503 provider response lost"),
    }));
    assert_eq!(error.code, "CLOUD_UPLOAD_PENDING_RECOVERY");
    assert_ne!(error.code, "CLOUD_CANCELLED");
    assert!(error.message.contains("未知"));
    assert!(error.message.contains("存储服务暂时不可用"));
    assert!(!error.message.contains("HTTP 503"));
    assert!(error.detail.as_deref().unwrap_or_default().contains("HTTP 503"));
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("pendingRecovery"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("multipartUploadIdKnown"))
            .and_then(|value| value.as_bool()),
        Some(false)
    );
    let detail = error.detail.expect("detail");
    assert!(detail.contains("HTTP 503 provider response lost"));
}

#[test]
fn multipart_abort_keeps_primary_and_abort_causes_at_error_boundary() {
    let error = AppError::from(DomainError::from(CloudError::MultipartAbort {
        operation: "abort after part failure",
        primary: Box::new(CloudError::Multipart {
            operation: "upload part",
            source: object_error("part cause: connection reset"),
        }),
        abort: object_error("abort cause: timeout"),
    }));
    assert_eq!(error.code, "CLOUD_UPLOAD_PENDING_RECOVERY");
    assert!(error.message.contains("需要恢复"));
    let detail = error.detail.expect("detail");
    assert!(detail.contains("abort after part failure"));
    assert!(detail.contains("part cause: connection reset"));
    assert!(detail.contains("abort cause: timeout"));
}

#[test]
fn cancelled_abort_keeps_cancellation_semantics_and_recovery_state() {
    let error = AppError::from(DomainError::from(CloudError::MultipartAbort {
        operation: "abort after cancellation",
        primary: Box::new(CloudError::UploadCancelled),
        abort: object_error("abort cause: timeout"),
    }));
    assert_eq!(error.code, "CLOUD_UPLOAD_PENDING_RECOVERY");
    assert_ne!(error.code, "CLOUD_CANCELLED");
    assert!(error.message.contains("已取消"));
    let detail = error.detail.expect("detail");
    assert!(detail.contains("multipart upload cancelled"));
    assert!(detail.contains("abort cause: timeout"));
}
