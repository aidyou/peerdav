use super::{runtime::{CloudRuntime, config_error}, types::ProxyMode};
use crate::AppError;

impl CloudRuntime {
    pub(crate) fn resolved_proxy(&self, proxy: &ProxyMode) -> Result<ProxyMode, AppError> {
        match proxy {
            ProxyMode::Client => Ok(self.config.snapshot().map_err(config_error)?.client_proxy),
            other => Ok(other.clone()),
        }
    }

    pub(crate) fn official_proxy(&self, id: Option<&str>) -> Result<ProxyMode, AppError> {
        let snapshot = self.config.snapshot().map_err(config_error)?;
        if id.is_some_and(|id| snapshot.drive_proxies.get(id) == Some(&true)) { Ok(ProxyMode::Direct) }
        else { Ok(snapshot.client_proxy) }
    }

    pub(crate) fn official_http_client(&self, id: Option<&str>) -> Result<reqwest::Client, AppError> {
        super::proxy::ConnectionTransport::new(&self.official_proxy(id)?)
            .map(|transport| transport.client().clone()).map_err(|source| AppError::from(crate::DomainError::from(source)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn proxy_resolution_preserves_direct_and_inherits_client_configuration() {
        let data = tempfile::tempdir().expect("data");
        let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("initialize");
        let runtime = CloudRuntime::new(vault, Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
        assert!(matches!(runtime.resolved_proxy(&ProxyMode::Client).expect("default"), ProxyMode::Environment));
        runtime.config.save_client_proxy(ProxyMode::Custom { url: "http://127.0.0.1:7890".into(), username: None, password: None }).expect("save");
        assert!(matches!(runtime.resolved_proxy(&ProxyMode::Client).expect("inherit"), ProxyMode::Custom { .. }));
        assert!(matches!(runtime.resolved_proxy(&ProxyMode::Direct).expect("direct"), ProxyMode::Direct));
        assert!(matches!(runtime.official_proxy(None).expect("authorization"), ProxyMode::Custom { .. }));
    }
}
