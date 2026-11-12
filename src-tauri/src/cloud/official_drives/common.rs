//! Shared bounded-JSON helpers for official drive adapters.
use serde_json::Value;
use zeroize::Zeroizing;

use crate::cloud::oauth_types::OfficialProvider;
use crate::cloud::official_api_types::OfficialApiError;

const JSON_LIMIT: usize = 4 * 1024 * 1024;
const ERROR_LIMIT: usize = 64 * 1024;

fn provider_reason<'a>(provider: OfficialProvider, payload: &'a Value) -> Option<&'a str> {
    match provider {
        OfficialProvider::Dropbox => payload.get("error_summary").or_else(|| payload.get("error")).and_then(Value::as_str),
        OfficialProvider::YandexDisk => payload.get("message").or_else(|| payload.get("description")).and_then(Value::as_str),
        OfficialProvider::BaiduNetdisk => payload.get("errmsg").or_else(|| payload.get("error_msg")).and_then(Value::as_str),
        _ => None,
    }
}

fn http_error(provider: OfficialProvider, reason: &str, token: &str, status: u16) -> OfficialApiError {
    let fallback = match provider {
        OfficialProvider::Dropbox => "Dropbox rejected request",
        OfficialProvider::YandexDisk => "Yandex Disk rejected request",
        OfficialProvider::BaiduNetdisk => "百度网盘拒绝请求 / Baidu Netdisk rejected request",
        _ => "Drive request rejected",
    };
    let message = if reason.trim().is_empty() { fallback } else { reason };
    OfficialApiError::Http {
        status,
        message: message.chars().take(2048).collect::<String>().replace(token, "[redacted]"),
    }
}

async fn read_body(response: &mut reqwest::Response, limit: usize) -> Result<Zeroizing<Vec<u8>>, OfficialApiError> {
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|error| OfficialApiError::Transport(error.without_url()))? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(OfficialApiError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Read a bounded JSON body and turn provider errors into original HTTP details.
pub(crate) async fn read_json(
    mut response: reqwest::Response,
    token: &str,
    provider: OfficialProvider,
) -> Result<Value, OfficialApiError> {
    let status = response.status();
    let bytes = read_body(&mut response, JSON_LIMIT).await?;
    if !status.is_success() {
        let payload: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let reason = provider_reason(provider, &payload).unwrap_or("");
        return Err(http_error(provider, reason, token, status.as_u16()));
    }
    serde_json::from_slice(&bytes).map_err(|_| OfficialApiError::InvalidResponse)
}

/// Verify a body response without consuming successful content.
pub(crate) async fn ensure_success(
    mut response: reqwest::Response,
    token: &str,
    provider: OfficialProvider,
) -> Result<reqwest::Response, OfficialApiError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let bytes = read_body(&mut response, ERROR_LIMIT).await?;
    let payload: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let reason = provider_reason(provider, &payload).unwrap_or("");
    Err(http_error(provider, reason, token, status.as_u16()))
}