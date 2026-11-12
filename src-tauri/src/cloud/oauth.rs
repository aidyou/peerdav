//! Official OAuth authorization requests. No proxy token-exchange services.
use super::oauth_types::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::Instant;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub use super::oauth_types::OAuthAuthorizations;

impl OfficialProvider {
    pub fn authorization_endpoint(self) -> &'static str {
        match self {
            Self::GoogleDrive => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::OneDrive => "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            Self::BaiduNetdisk => "https://openapi.baidu.com/oauth/2.0/authorize",
            Self::Dropbox => "https://www.dropbox.com/oauth2/authorize",
            Self::YandexDisk => "https://oauth.yandex.com/authorize",
        }
    }
    pub fn token_endpoint(self) -> &'static str {
        match self {
            Self::GoogleDrive => "https://oauth2.googleapis.com/token",
            Self::OneDrive => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            Self::BaiduNetdisk => "https://openapi.baidu.com/oauth/2.0/token",
            Self::Dropbox => "https://api.dropboxapi.com/oauth2/token",
            Self::YandexDisk => "https://oauth.yandex.com/token",
        }
    }
    /// Providers with an authorization-code flow that accepts PKCE
    /// (`code_challenge`/`code_verifier`), so a desktop client holds no secret.
    pub fn supports_pkce(self) -> bool {
        matches!(self, Self::GoogleDrive | Self::OneDrive | Self::Dropbox | Self::YandexDisk)
    }
    /// Dropbox only issues a refresh token when the authorization request asks
    /// for offline access.
    fn requests_offline_access(self) -> bool {
        self == Self::Dropbox
    }
    /// `None` means the provider takes permissions from the registered app and
    /// must not receive an explicit scope parameter (Yandex ID).
    fn scope(self) -> Option<&'static str> {
        match self {
            Self::GoogleDrive => Some("https://www.googleapis.com/auth/drive"),
            Self::OneDrive => Some("offline_access Files.ReadWrite"),
            Self::BaiduNetdisk => Some("basic netdisk"),
            Self::Dropbox => Some("files.metadata.read files.metadata.write files.content.read files.content.write"),
            Self::YandexDisk => None,
        }
    }
}

fn random_token() -> Result<String, OAuthError> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes).map_err(|_| OAuthError::StateUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn redirect_allowed(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else { return false; };
    url.username().is_empty() && url.password().is_none() && url.fragment().is_none() && url.query().is_none()
        && (url.scheme() == "https" || (url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))))
}

impl OAuthAuthorizations {
    pub fn begin(&self, provider: OfficialProvider, client_id: String, redirect_uri: String) -> Result<AuthorizationStart, OAuthError> {
        if client_id.trim().is_empty() || client_id.len() > 2048 || !redirect_allowed(&redirect_uri) {
            return Err(OAuthError::InvalidInput);
        }
        let request_id = random_token()?;
        let state = Zeroizing::new(random_token()?);
        let verifier = Zeroizing::new(random_token()?);
        let mut url = url::Url::parse(provider.authorization_endpoint()).map_err(|_| OAuthError::InvalidInput)?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code").append_pair("client_id", &client_id)
                .append_pair("redirect_uri", &redirect_uri).append_pair("state", state.as_str());
            if let Some(scope) = provider.scope() { query.append_pair("scope", scope); }
            if provider.supports_pkce() {
                query.append_pair("code_challenge", &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())))
                    .append_pair("code_challenge_method", "S256");
            }
            if provider == OfficialProvider::GoogleDrive {
                query.append_pair("access_type", "offline").append_pair("prompt", "consent");
            }
            if provider.requests_offline_access() {
                query.append_pair("token_access_type", "offline");
            }
        }
        let mut pending = self.pending.lock().map_err(|_| OAuthError::StateUnavailable)?;
        pending.retain(|_, item| item.created.elapsed().as_secs() < 600);
        if pending.len() >= 16 { return Err(OAuthError::TooManyPending); }
        pending.insert(request_id.clone(), PendingAuthorization { provider, client_id, redirect_uri, state, verifier, created: Instant::now() });
        Ok(AuthorizationStart { request_id, authorization_url: url.into(), expires_in: 600 })
    }

    /// Consume before exchanging: callbacks cannot be replayed, even after an error.
    pub fn complete(&self, request_id: &str, callback: &str) -> Result<TokenExchange, OAuthError> {
        let item = self.pending.lock().map_err(|_| OAuthError::StateUnavailable)?
            .remove(request_id).ok_or(OAuthError::InvalidState)?;
        if item.created.elapsed().as_secs() >= 600 { return Err(OAuthError::InvalidState); }
        if callback.len() > 16384 { return Err(OAuthError::InvalidInput); }
        let mut url = url::Url::parse(callback).map_err(|_| OAuthError::InvalidInput)?;
        if url.fragment().is_some() { return Err(OAuthError::InvalidInput); }
        let pairs: Vec<_> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let one = |key: &str| -> Option<&str> {
            let mut values = pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str());
            let first = values.next()?;
            if values.next().is_some() { None } else { Some(first) }
        };
        let state = one("state").ok_or(OAuthError::InvalidState)?;
        if !bool::from(state.as_bytes().ct_eq(item.state.as_bytes())) { return Err(OAuthError::InvalidState); }
        let code = one("code").filter(|value| !value.is_empty()).ok_or(OAuthError::InvalidState)?;
        if pairs.iter().any(|(key, _)| key == "error") { return Err(OAuthError::InvalidState); }
        url.set_query(None);
        let mut expected = url::Url::parse(&item.redirect_uri).map_err(|_| OAuthError::InvalidInput)?;
        // Registered redirects must not contain query parameters: ambiguous callbacks fail closed.
        if expected.query().is_some() { return Err(OAuthError::InvalidInput); }
        expected.set_query(None);
        if url != expected { return Err(OAuthError::InvalidState); }
        let mut form = vec![("grant_type".into(), "authorization_code".into()),
            ("client_id".into(), item.client_id), ("redirect_uri".into(), item.redirect_uri), ("code".into(), code.into())];
        if item.provider.supports_pkce() {
            form.push(("code_verifier".into(), item.verifier.to_string()));
        }
        Ok(TokenExchange { provider: item.provider, form: Zeroizing::new(form) })
    }

    pub fn clear(&self) -> Result<(), OAuthError> {
        self.pending.lock().map_err(|_| OAuthError::StateUnavailable)?.clear();
        Ok(())
    }

    pub fn cancel(&self, request_id: &str) -> Result<(), OAuthError> {
        self.pending.lock().map_err(|_| OAuthError::StateUnavailable)?.remove(request_id);
        Ok(())
    }
}

impl TokenExchange {
    pub fn refresh(provider: OfficialProvider, client_id: String, refresh_token: Zeroizing<String>) -> Result<Self, OAuthError> {
        if client_id.trim().is_empty() || refresh_token.trim().is_empty()
            || client_id.len() > 2048 || refresh_token.len() > 16384 {
            return Err(OAuthError::InvalidInput);
        }
        Ok(Self { provider, form: Zeroizing::new(vec![
            ("grant_type".into(), "refresh_token".into()), ("client_id".into(), client_id),
            ("refresh_token".into(), refresh_token.to_string()),
        ]) })
    }

    pub fn provider(&self) -> OfficialProvider { self.provider }

    pub fn client_id(&self) -> Result<&str, OAuthError> {
        self.form.iter().find(|(key, _)| key == "client_id")
            .map(|(_, value)| value.as_str()).ok_or(OAuthError::InvalidInput)
    }

    pub async fn exchange(self, client_secret: Option<Zeroizing<String>>) -> Result<OAuthTokens, OAuthError> {
        let endpoint = self.provider.token_endpoint();
        self.exchange_at(client_secret, endpoint).await
    }

    async fn exchange_at(self, client_secret: Option<Zeroizing<String>>, endpoint: &str) -> Result<OAuthTokens, OAuthError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build().map_err(OAuthError::Transport)?;
        self.exchange_using(client_secret, endpoint, client).await
    }

    pub(crate) async fn exchange_using(mut self, client_secret: Option<Zeroizing<String>>, endpoint: &str, client: reqwest::Client) -> Result<OAuthTokens, OAuthError> {
        if let Some(secret) = client_secret {
            if secret.is_empty() { return Err(OAuthError::InvalidInput); }
            self.form.push(("client_secret".into(), secret.to_string()));
        }
        let mut response = client.post(endpoint).timeout(std::time::Duration::from_secs(30))
            .form(&*self.form).send().await.map_err(|error| OAuthError::Transport(error.without_url()))?;
        let status = response.status();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(OAuthError::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > 65536 { return Err(OAuthError::InvalidResponse); }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            let mut message = payload.get("error_description").or_else(|| payload.get("error")).and_then(|v| v.as_str())
                .unwrap_or("Authorization service rejected request").chars().take(2048).collect::<String>();
            for (_, secret) in self.form.iter().filter(|(key, _)| matches!(key.as_str(), "client_secret" | "code" | "refresh_token" | "code_verifier")) {
                if !secret.is_empty() { message = message.replace(secret, "[redacted]"); }
            }
            return Err(OAuthError::Http { status: status.as_u16(), message });
        }
        let payload: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| OAuthError::InvalidResponse)?;
        let access = payload.get("access_token").and_then(|v| v.as_str().map(str::to_owned))
            .filter(|v| !v.is_empty()).ok_or(OAuthError::InvalidResponse)?;
        let refresh = payload.get("refresh_token").and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(|v| Zeroizing::new(v.to_owned()));
        Ok(OAuthTokens { access_token: Zeroizing::new(access), refresh_token: refresh,
            expires_in: payload.get("expires_in").and_then(|v| v.as_u64()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn token_exchange_uses_authenticated_proxy_without_resolving_remote_host() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("proxy");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            loop {
                request.push(socket.read_u8().await.expect("header"));
                if request.ends_with(b"\r\n\r\n") { break; }
                assert!(request.len() < 16384);
            }
            let headers = String::from_utf8(request).expect("headers");
            assert!(headers.starts_with("POST http://oauth.invalid/token "));
            assert!(headers.to_ascii_lowercase().contains("proxy-authorization: basic dXNlcjpwYXNz".to_ascii_lowercase().as_str()));
            let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).expect("length");
            let mut body = vec![0; length]; socket.read_exact(&mut body).await.expect("body");
            assert!(String::from_utf8_lossy(&body).contains("grant_type=refresh_token"));
            let body = r#"{"access_token":"access","refresh_token":"refresh"}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("reply");
        });
        let transport = super::super::proxy::ConnectionTransport::new(&super::super::types::ProxyMode::Custom { url: format!("http://{address}"), username: Some("user".into()), password: Some("pass".into()) }).expect("transport");
        let exchange = TokenExchange::refresh(OfficialProvider::GoogleDrive, "client".into(), Zeroizing::new("refresh".into())).expect("exchange");
        assert!(exchange.exchange_using(None, "http://oauth.invalid/token", transport.client().clone()).await.is_ok());
        server.await.expect("proxy task");
    }

    #[tokio::test]
    async fn token_exchange_uses_post_and_never_follows_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (status, body, accepted) in [
            ("200 OK", r#"{"access_token":"access","refresh_token":"rotated","expires_in":3600}"#, true),
            ("200 OK", r#"{"error":"invalid_grant"}"#, false),
            ("302 Found", "", false),
            ("401 Unauthorized", "private provider error", false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let address = listener.local_addr().expect("address");
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut bytes = vec![0; 8192];
                let read = socket.read(&mut bytes).await.expect("request");
                let request = String::from_utf8_lossy(&bytes[..read]);
                assert!(request.starts_with("POST /token "));
                let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: https://evil.invalid/\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.expect("response");
            });
            let exchange = TokenExchange::refresh(OfficialProvider::GoogleDrive, "client".into(), Zeroizing::new("refresh".into())).expect("refresh request");
            let result = exchange.exchange_at(None, &format!("http://{address}/token")).await;
            assert_eq!(result.is_ok(), accepted);
            if let Ok(tokens) = result {
                assert_eq!(tokens.access_token.as_str(), "access");
                assert_eq!(tokens.refresh_token.as_deref().map(String::as_str), Some("rotated"));
            }
            task.await.expect("server");
        }
    }

    #[test]
    fn provider_identifiers_match_frontend_contract() {
        assert_eq!(serde_json::to_string(&OfficialProvider::GoogleDrive).expect("serialize"), "\"google_drive\"");
        assert_eq!(serde_json::to_string(&OfficialProvider::OneDrive).expect("serialize"), "\"one_drive\"");
        assert_eq!(serde_json::to_string(&OfficialProvider::BaiduNetdisk).expect("serialize"), "\"baidu_netdisk\"");
        assert_eq!(serde_json::to_string(&OfficialProvider::Dropbox).expect("serialize"), "\"dropbox\"");
        assert_eq!(serde_json::to_string(&OfficialProvider::YandexDisk).expect("serialize"), "\"yandex_disk\"");
    }

    #[test]
    fn only_secretless_providers_add_pkce_and_yandex_omits_scope() {
        for provider in [OfficialProvider::GoogleDrive, OfficialProvider::OneDrive, OfficialProvider::Dropbox, OfficialProvider::YandexDisk] {
            assert!(provider.supports_pkce(), "{provider:?}");
        }
        assert!(!OfficialProvider::BaiduNetdisk.supports_pkce());
        let auth = OAuthAuthorizations::default();
        let start = auth.begin(OfficialProvider::YandexDisk, "client".into(), "http://127.0.0.1:7777/oauth/callback".into()).expect("begin");
        let url = url::Url::parse(&start.authorization_url).expect("url");
        assert!(!url.query_pairs().any(|(key, _)| key == "scope"));
        assert!(url.query_pairs().any(|(key, _)| key == "code_challenge"));
        let dropbox = auth.begin(OfficialProvider::Dropbox, "client".into(), "http://127.0.0.1:7777/oauth/callback".into()).expect("begin");
        let dropbox = url::Url::parse(&dropbox.authorization_url).expect("url");
        assert!(dropbox.query_pairs().any(|(key, value)| key == "token_access_type" && value == "offline"));
    }

    #[test]
    fn official_requests_bind_state_redirect_and_single_use() {
        for provider in [OfficialProvider::GoogleDrive, OfficialProvider::OneDrive, OfficialProvider::BaiduNetdisk, OfficialProvider::Dropbox, OfficialProvider::YandexDisk] {
            let auth = OAuthAuthorizations::default();
            let start = auth.begin(provider, "client".into(), "http://127.0.0.1:7777/callback".into()).expect("begin");
            let url = url::Url::parse(&start.authorization_url).expect("url");
            let state = url.query_pairs().find(|(k, _)| k == "state").expect("state").1.into_owned();
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.query_pairs().any(|(k, _)| k == "code_challenge"), provider.supports_pkce());
            let callback = format!("http://127.0.0.1:7777/callback?code=code&state={state}");
            let form = auth.complete(&start.request_id, &callback).expect("valid callback");
            assert!(form.form.iter().any(|(key, value)| key == "code" && value == "code"));
            assert_eq!(form.form.iter().any(|(key, _)| key == "code_verifier"), provider.supports_pkce());
            assert!(auth.complete(&start.request_id, &callback).is_err());
        }
    }
    #[test]
    fn expired_wrong_redirect_duplicate_code_and_pending_limit_fail_closed() {
        let auth = OAuthAuthorizations::default();
        let start = auth.begin(OfficialProvider::GoogleDrive, "id".into(), "http://127.0.0.1/callback".into()).expect("begin");
        auth.pending.lock().expect("lock").get_mut(&start.request_id).expect("pending").created = Instant::now() - std::time::Duration::from_secs(600);
        assert!(auth.complete(&start.request_id, "http://127.0.0.1/callback?code=x&state=x").is_err());
        for path in ["http://127.0.0.1/other", "http://127.0.0.2/callback"] {
            let start = auth.begin(OfficialProvider::GoogleDrive, "id".into(), "http://127.0.0.1/callback".into()).expect("begin");
            let url = url::Url::parse(&start.authorization_url).expect("url");
            let state = url.query_pairs().find(|(k, _)| k == "state").expect("state").1.into_owned();
            assert!(auth.complete(&start.request_id, &format!("{path}?code=x&state={state}")).is_err());
        }
        let start = auth.begin(OfficialProvider::GoogleDrive, "id".into(), "http://127.0.0.1/callback".into()).expect("begin");
        let url = url::Url::parse(&start.authorization_url).expect("url");
        let state = url.query_pairs().find(|(k, _)| k == "state").expect("state").1.into_owned();
        assert!(auth.complete(&start.request_id, &format!("http://127.0.0.1/callback?code=x&code=y&state={state}")).is_err());
        for _ in 0..16 {
            auth.begin(OfficialProvider::OneDrive, "id".into(), "http://127.0.0.1/callback".into()).expect("under capacity");
        }
        assert!(matches!(auth.begin(OfficialProvider::OneDrive, "id".into(), "http://127.0.0.1/callback".into()), Err(OAuthError::TooManyPending)));
        assert!(auth.begin(OfficialProvider::OneDrive, "id".into(), "http://127.0.0.1/callback?x=y".into()).is_err());
    }

    #[test]
    fn invalid_callbacks_and_cancel_never_release_a_code() {
        let auth = OAuthAuthorizations::default();
        for suffix in ["state=wrong", "state=x&state=x", "error=denied", "code=second&state=x"] {
            let start = auth.begin(OfficialProvider::GoogleDrive, "id".into(), "http://127.0.0.1/callback".into()).expect("begin");
            assert!(auth.complete(&start.request_id, &format!("http://127.0.0.1/callback?code=code&{suffix}")).is_err());
        }
        for redirect in ["http://example.org/callback", "file:///tmp/callback", "https://user:password@example.org/callback"] {
            assert!(auth.begin(OfficialProvider::OneDrive, "id".into(), redirect.into()).is_err());
        }
        let start = auth.begin(OfficialProvider::OneDrive, "id".into(), "http://127.0.0.1/callback".into()).expect("begin");
        auth.cancel(&start.request_id).expect("cancel");
        assert!(auth.complete(&start.request_id, "http://127.0.0.1/callback?code=x").is_err());
    }
}
