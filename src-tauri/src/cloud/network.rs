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
        let proxy = self.official_proxy(id)?;
        let mut clients = self.official_clients.lock().map_err(|_| AppError::from(crate::DomainError::Internal { detail: "HTTP client cache lock poisoned".into() }))?;
        let key = id.unwrap_or("");
        if let Some((_, client)) = clients.get(key).filter(|(configured, _)| configured == &proxy) {
            return Ok(client.clone());
        }
        let client = super::proxy::ConnectionTransport::new(&proxy)
            .map(|transport| transport.client().clone()).map_err(|source| AppError::from(crate::DomainError::from(source)))?;
        if clients.len() >= 128 { clients.clear(); }
        clients.insert(key.to_owned(), (proxy, client.clone()));
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[tokio::test]
    async fn official_requests_reuse_proxy_connection_and_rebuild_after_proxy_change() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("proxy");
        let address = listener.local_addr().expect("address");
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = accepted.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("connection");
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut data = [0; 4096];
                for _ in 0..2 {
                    let n = socket.read(&mut data).await.expect("request");
                    if n == 0 { break; }
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok").await.expect("response");
                }
            }
        });
        let data = tempfile::tempdir().expect("data");
        let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("initialize");
        let runtime = CloudRuntime::new(vault, Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
        let proxy = ProxyMode::Custom { url: format!("http://{address}"), username: None, password: None };
        runtime.config.save_client_proxy(proxy.clone()).expect("proxy");
        for _ in 0..2 {
            let response = runtime.official_http_client(Some("connection")).expect("client").get("http://pool-test.invalid/object").send().await.expect("proxied request");
            assert_eq!(response.text().await.expect("body"), "ok");
        }
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
        runtime.config.save_client_proxy(ProxyMode::Direct).expect("direct");
        runtime.official_http_client(Some("connection")).expect("replace cached client");
        runtime.config.save_client_proxy(proxy).expect("proxy again");
        let response = runtime.official_http_client(Some("connection")).expect("client").get("http://pool-test.invalid/object").send().await.expect("new pool");
        assert_eq!(response.text().await.expect("body"), "ok");
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
        server.abort();
        assert!(server.await.expect_err("stopped server").is_cancelled());
    }

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
