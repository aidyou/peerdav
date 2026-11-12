use super::{oauth_types::*, official_api::OfficialApi, official_api_types::OfficialPage, runtime::CloudRuntime};
use crate::{AppError, DomainError};
use zeroize::Zeroizing;

pub(crate) fn oauth_error(error: OAuthError) -> AppError {
    AppError::from(DomainError::Cloud { code: "CLOUD_OAUTH_FAILED", user_reason: "网盘授权失败 / Drive authorization failed", source: Box::new(error) })
}

pub(crate) fn official_endpoint(provider: OfficialProvider) -> &'static str {
    match provider {
        OfficialProvider::GoogleDrive => "https://www.googleapis.com/drive/v3",
        OfficialProvider::OneDrive => "https://graph.microsoft.com/v1.0",
        OfficialProvider::BaiduNetdisk => "https://pan.baidu.com/rest/2.0/xpan",
        OfficialProvider::Dropbox => "https://api.dropboxapi.com/2",
        OfficialProvider::YandexDisk => "https://cloud-api.yandex.net/v1",
    }
}

impl CloudRuntime {
    /// Object-store view used by append repositories and mirror sync. Google
    /// keeps its dedicated adapter; the other official providers share the
    /// file-API adapter.
    pub(crate) fn official_store(&self, connection: &str) -> Result<Option<std::sync::Arc<dyn super::traits::ObjectStore>>, AppError> {
        self.admit()?;
        let snapshot = self.config.snapshot().map_err(super::runtime::config_error)?;
        let Some(entry) = snapshot.official_connections.iter().find(|entry| entry.id == connection) else { return Ok(None); };
        if entry.provider == OfficialProvider::GoogleDrive { return self.google_store(connection); }
        if entry.provider == OfficialProvider::YandexDisk {
            return Err(AppError { code: "CLOUD_UNSUPPORTED".into(), message: "此协议的同步尚未实现 / Sync is not implemented for this protocol".into(), detail: None, context: None });
        }
        Ok(Some(std::sync::Arc::new(super::official_store::OfficialObjectStore::new(
            self.weak_self.clone(),
            entry.id.clone(),
            entry.revision,
            entry.root_id.clone(),
            entry.provider,
            official_endpoint(entry.provider).into(),
        ))))
    }
}

impl CloudRuntime {
    pub fn begin_official_authorization(&self, provider: OfficialProvider, client_id: String, redirect_uri: String) -> Result<AuthorizationStart, AppError> {
        self.admit()?;
        self.official_authorizations.begin(provider, client_id, redirect_uri).map_err(oauth_error)
    }

    pub async fn begin_loopback_authorization(&self, provider: OfficialProvider, client_id: String, redirect_uri: String) -> Result<AuthorizationStart, AppError> {
        self.admit()?;
        if provider == OfficialProvider::BaiduNetdisk { return Err(oauth_error(OAuthError::InvalidInput)); }
        let cancel = self.vault.transfer_cancellation().map_err(AppError::from)?.child_token();
        let pending = super::oauth_loopback::bind(&redirect_uri, cancel.clone()).await.map_err(oauth_error)?;
        let mut callbacks = self.official_callbacks.lock().await;
        callbacks.retain(|_, pending| {
            let keep = pending.created.elapsed().as_secs() < 600 && !pending.cancel.is_cancelled();
            if !keep { pending.cancel.cancel(); }
            keep
        });
        if callbacks.len() >= 4 { return Err(oauth_error(OAuthError::TooManyPending)); }
        let mut cancels = self.official_callback_cancels.lock().await;
        cancels.retain(|id, token| callbacks.contains_key(id) || !token.is_cancelled());
        if cancels.len() >= 16 { return Err(oauth_error(OAuthError::TooManyPending)); }
        let start = self.begin_official_authorization(provider, client_id, pending.redirect_uri.clone())?;
        cancels.insert(start.request_id.clone(), cancel);
        callbacks.insert(start.request_id.clone(), pending);
        Ok(start)
    }

    pub async fn wait_loopback_authorization(&self, request_id: &str, name: String, client_secret: Option<String>, root_id: String) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
        self.wait_loopback_authorization_with_proxy(request_id, name, client_secret, root_id, false).await
    }

    pub async fn wait_loopback_authorization_with_proxy(&self, request_id: &str, name: String, client_secret: Option<String>, root_id: String, direct: bool) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
        self.admit()?;
        let pending = self.official_callbacks.lock().await.remove(request_id).ok_or_else(|| oauth_error(OAuthError::InvalidState))?;
        let cancel = pending.cancel.clone();
        let received = super::oauth_loopback::receive(pending).await;
        let callback = match received {
            Ok(callback) => callback,
            Err(error) => {
                self.official_callback_cancels.lock().await.remove(request_id);
                self.official_authorizations.cancel(request_id).map_err(oauth_error)?;
                return Err(oauth_error(error));
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(oauth_error(OAuthError::InvalidState)),
            result = self.complete_official_authorization_with_proxy(request_id, callback.as_str(), name, client_secret, root_id, direct) => result,
        };
        self.official_callback_cancels.lock().await.remove(request_id);
        self.official_authorizations.cancel(request_id).map_err(oauth_error)?;
        result
    }

    pub async fn cancel_official_authorization(&self, request_id: &str) -> Result<(), AppError> {
        if let Some(cancel) = self.official_callback_cancels.lock().await.remove(request_id) { cancel.cancel(); }
        self.official_callbacks.lock().await.remove(request_id);
        self.official_authorizations.cancel(request_id).map_err(oauth_error)
    }

    pub async fn complete_official_authorization(
        &self, request_id: &str, callback: &str, name: String, client_secret: Option<String>, root_id: String,
    ) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
        self.complete_official_authorization_with_proxy(request_id, callback, name, client_secret, root_id, false).await
    }

    pub async fn complete_official_authorization_with_proxy(
        &self, request_id: &str, callback: &str, name: String, client_secret: Option<String>, root_id: String, direct: bool,
    ) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
        self.admit()?;
        let _network = self.network.read().await;
        let _refresh = self.official_refresh.lock().await;
        self.admit()?;
        if name.trim().is_empty() || name.len() > 256 || root_id.trim().is_empty() || root_id.len() > 4096
            || client_secret.as_ref().is_some_and(|value| value.len() > 16384) {
            return Err(oauth_error(OAuthError::InvalidInput));
        }
        let exchange = self.official_authorizations.complete(request_id, callback).map_err(oauth_error)?;
        let provider = exchange.provider();
        let client_id = exchange.client_id().map_err(oauth_error)?.to_owned();
        let proxy = if direct { super::types::ProxyMode::Direct } else { self.official_proxy(None)? };
        let client = super::proxy::ConnectionTransport::new(&proxy).map_err(|source| AppError::from(crate::DomainError::from(source)))?.client().clone();
        let tokens = exchange.exchange_using(client_secret.clone().map(Zeroizing::new), provider.token_endpoint(), client).await.map_err(oauth_error)?;
        self.admit()?;
        let refresh = tokens.refresh_token.ok_or_else(|| oauth_error(OAuthError::InvalidResponse))?;
        self.config.save_official_connection_with_proxy(super::official_types::OfficialConnection {
            id: String::new(), name, provider, config_version: 1, revision: 0, client_id,
            client_secret, refresh_token: refresh.to_string(), root_id,
        }, Some(direct)).map_err(super::runtime::config_error)
    }

    pub(crate) async fn official_api(&self, connection_id: &str) -> Result<(super::official_types::OfficialConnection, OfficialApi), AppError> {
        self.admit()?;
        let mut cache = self.official_refresh.lock().await;
        self.admit()?;
        let connection = self.config.snapshot().map_err(super::runtime::config_error)?.official_connections
            .into_iter().find(|entry| entry.id == connection_id)
            .ok_or_else(|| super::runtime::config_error(super::config_types::ConfigError::NotFound("official connection".into())))?;
        cache.retain(|_, entry| entry.expires > std::time::Instant::now());
        let access_token = if let Some(cached) = cache.get(connection_id).filter(|cached| cached.revision == connection.revision) {
            cached.token.clone()
        } else {
            let tokens = TokenExchange::refresh(connection.provider, connection.client_id.clone(), Zeroizing::new(connection.refresh_token.clone()))
                .map_err(oauth_error)?.exchange_using(connection.client_secret.clone().map(Zeroizing::new), connection.provider.token_endpoint(), self.official_http_client(Some(connection_id))?).await.map_err(oauth_error)?;
            self.admit()?;
            if let Some(refresh) = tokens.refresh_token {
                self.config.rotate_official_token(&connection.id, connection.revision, refresh.as_str()).map_err(super::runtime::config_error)?;
            }
            let lifetime = tokens.expires_in.unwrap_or(60).min(86400).saturating_sub(30);
            if lifetime > 0 && cache.len() < 128 {
                cache.insert(connection.id.clone(), CachedAccessToken { revision: connection.revision, token: tokens.access_token.clone(),
                    expires: std::time::Instant::now() + std::time::Duration::from_secs(lifetime) });
            }
            tokens.access_token
        };
        drop(cache);
        let client = self.official_http_client(Some(connection_id))?;
        let drive = super::official_drives::create(connection.provider, client.clone(), access_token.clone());
        let api = OfficialApi { #[cfg(test)] fixture: self.official_fixture.lock().ok().and_then(|value| value.clone()), client, provider: connection.provider, access_token, drive };
        Ok((connection, api))
    }

    pub async fn list_official(&self, connection_id: &str, parent: &str, cursor: Option<&str>) -> Result<OfficialPage, AppError> {
        self.admit()?;
        let _network = self.network.read().await;
        let (connection, api) = self.official_api(connection_id).await?;
        let result = api.list(if parent.is_empty() { &connection.root_id } else { parent }, cursor).await;
        if matches!(&result, Err(super::official_api_types::OfficialApiError::Http { status: 401, .. })) {
            self.official_refresh.lock().await.remove(connection_id);
        }
        let page = result.map_err(api_error)?;
        self.admit()?;
        let current = self.config.snapshot().map_err(super::runtime::config_error)?;
        if !current.official_connections.iter().any(|entry| entry.id == connection.id && entry.revision == connection.revision) {
            return Err(crate::file_manager::service::invalid());
        }
        Ok(page)
    }
}

pub(crate) fn api_error(error: super::official_api_types::OfficialApiError) -> AppError {
    AppError::from(DomainError::Cloud { code: "CLOUD_OFFICIAL_API", user_reason: "网盘请求失败 / Drive request failed", source: Box::new(error) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn official_store_exposes_non_google_providers_without_network() {
        let data = tempfile::tempdir().expect("data");
        let vault = std::sync::Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("init");
        let runtime = CloudRuntime::new(vault, std::sync::Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
        runtime.resume().expect("resume");
        for provider in [OfficialProvider::BaiduNetdisk, OfficialProvider::Dropbox, OfficialProvider::OneDrive] {
            let connection = runtime.config.save_official_connection(crate::cloud::official_types::OfficialConnection {
                id: String::new(), name: format!("{provider:?}"), provider,
                config_version: 1, revision: 0, client_id: "client".into(), client_secret: None,
                refresh_token: "offline".into(), root_id: "/".into(),
            }).expect("connection");
            let store = runtime.official_store(&connection.id).expect("store").expect("official provider");
            let scope = store.scope().expect("scope");
            assert_eq!(scope.endpoint, official_endpoint(provider));
            assert_eq!(scope.bucket, "/");
        }
        let yandex = runtime.config.save_official_connection(crate::cloud::official_types::OfficialConnection {
            id: String::new(), name: "Yandex".into(), provider: OfficialProvider::YandexDisk,
            config_version: 1, revision: 0, client_id: "client".into(), client_secret: None,
            refresh_token: "offline".into(), root_id: "disk:/".into(),
        }).expect("yandex connection");
        assert!(runtime.official_store(&yandex.id).is_err());
    }
}
