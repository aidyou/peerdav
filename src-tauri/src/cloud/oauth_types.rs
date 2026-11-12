use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex, time::Instant};
use zeroize::Zeroizing;

pub(crate) struct LoopbackAuthorization {
    pub listener: tokio::net::TcpListener,
    pub redirect_uri: String,
    pub created: Instant,
    pub cancel: tokio_util::sync::CancellationToken,
}

pub(crate) struct CachedAccessToken {
    pub revision: u64,
    pub token: Zeroizing<String>,
    pub expires: Instant,
}

#[derive(Default)]
pub struct OAuthAuthorizations {
    pub(crate) pending: Mutex<HashMap<String, PendingAuthorization>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfficialProvider {
    GoogleDrive,
    OneDrive,
    BaiduNetdisk,
    Dropbox,
    YandexDisk,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationStart {
    pub request_id: String,
    pub authorization_url: String,
    pub expires_in: u64,
}

/// Intentionally not Debug or Serialize: tokens must not reach logs or webviews.
pub struct OAuthTokens {
    pub access_token: Zeroizing<String>,
    pub refresh_token: Option<Zeroizing<String>>,
    pub expires_in: Option<u64>,
}

/// Provider and form stay bound to the authorization request.
pub struct TokenExchange {
    pub(crate) provider: OfficialProvider,
    pub(crate) form: Zeroizing<Vec<(String, String)>>,
}

pub(crate) struct PendingAuthorization {
    pub provider: OfficialProvider,
    pub client_id: String,
    pub redirect_uri: String,
    pub state: Zeroizing<String>,
    pub verifier: Zeroizing<String>,
    pub created: Instant,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("授权参数无效 / Invalid authorization parameters")]
    InvalidInput,
    #[error("授权已过期或不匹配 / Authorization expired or does not match")]
    InvalidState,
    #[error("待处理授权过多 / Too many pending authorizations")]
    TooManyPending,
    #[error("授权状态不可用 / Authorization state unavailable")]
    StateUnavailable,
    #[error("授权网络请求失败 / Authorization network request failed")]
    Transport(#[source] reqwest::Error),
    // Do not store response bodies: some providers echo credentials in errors.
    #[error("授权服务返回 HTTP {status}：{message}")]
    Http { status: u16, message: String },
    #[error("无法监听授权回调 / Cannot listen for authorization callback")]
    CallbackIo(#[source] std::io::Error),
    #[error("授权服务响应无效 / Invalid authorization service response")]
    InvalidResponse,
}
