use std::time::Duration;

use object_store::client::{HttpClient, HttpConnector};
use reqwest::{Client, NoProxy, Proxy};
use url::Url;

use super::types::{CloudError, ProxyMode};

#[derive(Clone)]
pub(super) struct ConnectionTransport(Client);

impl std::fmt::Debug for ConnectionTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionTransport([redacted])")
    }
}

impl HttpConnector for ConnectionTransport {
    fn connect(&self, _: &object_store::ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(self.0.clone()))
    }
}

fn proxy_url(raw: &str) -> Result<Url, CloudError> {
    let mut url =
        Url::parse(raw).map_err(|_| CloudError::InvalidConfig("invalid proxy URL".into()))?;
    if url.scheme() == "socks5" {
        url.set_scheme("socks5h")
            .map_err(|_| CloudError::InvalidConfig("invalid proxy scheme".into()))?;
    }
    if !matches!(url.scheme(), "http" | "https" | "socks5h")
        || url.host_str().is_none()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CloudError::InvalidConfig(
            "proxy must be HTTP, HTTPS or SOCKS5 with a host and no path".into(),
        ));
    }
    Ok(url)
}

impl ConnectionTransport {
    /// Shared client for service-level requests (for example bucket discovery)
    /// that must obey the same proxy, redirect and timeout policy as object I/O.
    pub(super) fn client(&self) -> &Client {
        &self.0
    }

    pub(super) fn new(mode: &ProxyMode) -> Result<Self, CloudError> {
        Self::with_environment(mode, |name| std::env::var(name).ok())
    }

    fn with_environment(
        mode: &ProxyMode,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, CloudError> {
        // Never inherit an implicit system/PAC configuration or a fallback route.
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120));
        match mode {
            ProxyMode::Direct => {}
            ProxyMode::Custom {
                url,
                username,
                password,
            } => {
                let mut parsed = proxy_url(url)?;
                if let Some(username) = username {
                    parsed
                        .set_username(username)
                        .map_err(|_| CloudError::InvalidConfig("invalid proxy username".into()))?;
                }
                if let Some(password) = password {
                    parsed
                        .set_password(Some(password))
                        .map_err(|_| CloudError::InvalidConfig("invalid proxy password".into()))?;
                }
                let proxy = Proxy::all(parsed)
                    .map_err(|source| CloudError::Transport {
                        source: source.without_url(),
                    })?
                    .no_proxy(None);
                builder = builder.proxy(proxy);
            }
            ProxyMode::Environment => {
                let read = |upper: &str, lower: &str| {
                    lookup(upper)
                        .or_else(|| lookup(lower))
                        .filter(|v| !v.trim().is_empty())
                };
                let all = read("ALL_PROXY", "all_proxy");
                let http = read("HTTP_PROXY", "http_proxy").or_else(|| all.clone());
                let https = read("HTTPS_PROXY", "https_proxy").or(all);
                let exclusions =
                    read("NO_PROXY", "no_proxy").and_then(|s| NoProxy::from_string(&s));
                if let Some(raw) = http {
                    builder = builder.proxy(
                        Proxy::http(proxy_url(&raw)?)
                            .map_err(|source| CloudError::Transport {
                                source: source.without_url(),
                            })?
                            .no_proxy(exclusions.clone()),
                    );
                }
                if let Some(raw) = https {
                    builder = builder.proxy(
                        Proxy::https(proxy_url(&raw)?)
                            .map_err(|source| CloudError::Transport {
                                source: source.without_url(),
                            })?
                            .no_proxy(exclusions),
                    );
                }
            }
        }
        builder
            .build()
            .map(Self)
            .map_err(|source| CloudError::Transport {
                source: source.without_url(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::generate_simple_self_signed;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};

    async fn http_proxy(
        require_auth: bool,
    ) -> (SocketAddr, tokio::task::JoinHandle<(bool, String)>) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind proxy");
        let address = listener.local_addr().expect("proxy address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("proxy accept");
            let mut request = Vec::new();
            loop {
                let mut byte = [0; 1];
                stream.read_exact(&mut byte).await.expect("proxy request");
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let auth = text
                .lines()
                .any(|line| line.eq_ignore_ascii_case("proxy-authorization: Basic dXNlcjpwYXNz"));
            let ok = !require_auth || auth;
            let status = if ok {
                "200 Connection established"
            } else {
                "407 Proxy Authentication Required"
            };
            stream
                .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                .await
                .expect("proxy response");
            (ok, text)
        });
        (address, task)
    }

    async fn socks_proxy() -> (
        SocketAddr,
        tokio::task::JoinHandle<(String, String, String)>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind socks");
        let address = listener.local_addr().expect("socks address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("socks accept");
            let mut greeting = [0; 2];
            stream
                .read_exact(&mut greeting)
                .await
                .expect("socks greeting");
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0; greeting[1] as usize];
            stream
                .read_exact(&mut methods)
                .await
                .expect("socks methods");
            assert!(methods.contains(&2));
            stream.write_all(&[5, 2]).await.expect("socks method");
            let mut auth_header = [0; 2];
            stream
                .read_exact(&mut auth_header)
                .await
                .expect("socks auth header");
            assert_eq!(&auth_header, &[1, 4]);
            let mut username = vec![0; auth_header[1] as usize];
            stream
                .read_exact(&mut username)
                .await
                .expect("socks username");
            let mut password_len = [0; 1];
            stream
                .read_exact(&mut password_len)
                .await
                .expect("socks password length");
            let mut password = vec![0; password_len[0] as usize];
            stream
                .read_exact(&mut password)
                .await
                .expect("socks password");
            assert_eq!(username, b"user");
            assert_eq!(password, b"pass");
            stream.write_all(&[1, 0]).await.expect("socks auth");
            let mut header = [0; 4];
            stream.read_exact(&mut header).await.expect("socks connect");
            assert_eq!(&header[..4], &[5, 1, 0, 3]);
            let mut domain_len = [0; 1];
            stream
                .read_exact(&mut domain_len)
                .await
                .expect("socks domain length");
            let mut domain = vec![0; domain_len[0] as usize];
            stream.read_exact(&mut domain).await.expect("socks domain");
            let mut port = [0; 2];
            stream.read_exact(&mut port).await.expect("socks port");
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .await
                .expect("socks reply");
            let mut request = Vec::new();
            loop {
                let mut byte = [0; 1];
                stream.read_exact(&mut byte).await.expect("socks payload");
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("socks response");
            (
                String::from_utf8_lossy(&domain).into_owned(),
                String::from_utf8_lossy(&port).into_owned(),
                String::from_utf8_lossy(&request).into_owned(),
            )
        });
        (address, task)
    }

    #[tokio::test]
    async fn http_proxy_authentication_is_sent_over_real_request() {
        let (address, server) = http_proxy(true).await;
        let mode = ProxyMode::Custom {
            url: format!("http://{}", address),
            username: Some("user".into()),
            password: Some("pass".into()),
        };
        let client = ConnectionTransport::with_environment(&mode, |_| None)
            .expect("client")
            .0;
        let response = client
            .get("http://unresolvable.invalid/")
            .send()
            .await
            .expect("proxy response");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let (ok, request) = server.await.expect("server");
        assert!(ok && request.contains("http://unresolvable.invalid/"));
    }

    #[tokio::test]
    async fn socks5h_authentication_sends_unresolvable_domain_to_proxy() {
        let (address, server) = socks_proxy().await;
        let mode = ProxyMode::Custom {
            url: format!("socks5h://{}", address),
            username: Some("user".into()),
            password: Some("pass".into()),
        };
        let client = ConnectionTransport::with_environment(&mode, |_| None)
            .expect("client")
            .0;
        let response = client
            .get("http://unresolvable.invalid/")
            .send()
            .await
            .expect("socks response");
        assert_eq!(response.text().await.expect("body"), "ok");
        let (domain, _, request) = server.await.expect("server");
        assert!(domain.ends_with("unresolvable.invalid"));
        assert!(request.starts_with("GET / HTTP/1.1"));
    }

    #[tokio::test]
    async fn environment_all_proxy_fallback_routes_real_request() {
        let (address, server) = http_proxy(false).await;
        let client = ConnectionTransport::with_environment(&ProxyMode::Environment, |name| {
            (name == "ALL_PROXY").then(|| format!("http://{address}"))
        })
        .expect("client")
        .0;
        assert!(
            client
                .get("http://unresolvable.invalid/")
                .send()
                .await
                .expect("response")
                .status()
                .is_success()
        );
        assert!(
            server
                .await
                .expect("proxy task")
                .1
                .contains("http://unresolvable.invalid/")
        );
    }

    #[tokio::test]
    async fn environment_http_proxy_routes_real_request_and_no_proxy_bypasses_it() {
        let (address, server) = http_proxy(false).await;
        let mut values = HashMap::new();
        values.insert("HTTP_PROXY", format!("http://{address}"));
        values.insert("NO_PROXY", "not-this-host.invalid".into());
        let mode = ProxyMode::Environment;
        let client = ConnectionTransport::with_environment(&mode, |name| values.get(name).cloned())
            .expect("client")
            .0;
        let response = client
            .get("http://unresolvable.invalid/")
            .send()
            .await
            .expect("proxy response");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let (ok, request) = server.await.expect("server");
        assert!(ok && request.contains("http://unresolvable.invalid/"));

        let (address, mut server) = http_proxy(false).await;
        values.insert("NO_PROXY", "unresolvable.invalid".into());
        values.insert("HTTP_PROXY", format!("http://{address}"));
        let client = ConnectionTransport::with_environment(&mode, |name| values.get(name).cloned())
            .expect("client")
            .0;
        assert!(
            client
                .get("http://unresolvable.invalid/")
                .send()
                .await
                .is_err()
        );
        assert!(
            timeout(Duration::from_millis(400), &mut server)
                .await
                .is_err()
        );
        server.abort();
        assert!(
            server
                .await
                .expect_err("aborted unused proxy")
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn environment_https_proxy_routes_connect_and_reports_proxy_authentication() {
        let (address, server) = http_proxy(true).await;
        let mut values = HashMap::new();
        values.insert("HTTPS_PROXY", format!("http://{address}"));
        let client = ConnectionTransport::with_environment(&ProxyMode::Environment, |name| {
            values.get(name).cloned()
        })
        .expect("client")
        .0;
        let error = client
            .get("https://unresolvable.invalid/")
            .send()
            .await
            .expect_err("407 must fail");
        assert!(!error.to_string().is_empty());
        let (ok, request) = server.await.expect("server");
        assert!(!ok && request.starts_with("CONNECT unresolvable.invalid:443 HTTP/1.1"));
    }

    #[tokio::test]
    async fn custom_proxy_is_not_bypassed_by_no_proxy_and_direct_can_reach_same_target() {
        let target = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("target bind");
        let target_address = target.local_addr().expect("target address");
        let mut target_task = tokio::spawn(async move {
            let (mut stream, _) = target.accept().await.expect("target accept");
            let mut request = [0; 64];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("target response");
        });
        let (proxy_address, proxy_task) = http_proxy(false).await;
        let mode = ProxyMode::Custom {
            url: format!("http://{proxy_address}"),
            username: None,
            password: None,
        };
        let custom = ConnectionTransport::with_environment(&mode, |name| {
            (name == "NO_PROXY").then(|| "127.0.0.1".into())
        })
        .expect("client")
        .0;
        let response = custom
            .get(format!("http://{target_address}/"))
            .send()
            .await
            .expect("proxy response");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(
            timeout(Duration::from_millis(400), &mut target_task)
                .await
                .is_err()
        );
        let (ok, request) = proxy_task.await.expect("proxy task");
        assert!(ok && request.contains(&format!("http://{target_address}/")));
        let direct = ConnectionTransport::with_environment(&ProxyMode::Direct, |_| None)
            .expect("direct")
            .0;
        let direct_response = direct
            .get(format!("http://{target_address}/"))
            .send()
            .await
            .expect("direct response");
        assert_eq!(direct_response.status(), reqwest::StatusCode::OK);
        target_task.await.expect("target task");
    }

    #[tokio::test]
    async fn https_proxy_uses_tls_and_rejects_untrusted_self_signed_certificate() {
        let certified = generate_simple_self_signed(vec!["localhost".into()]).expect("certificate");
        let certs = vec![rustls::pki_types::CertificateDer::from(
            certified.cert.der().to_vec(),
        )];
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()),
        );
        let provider = rustls::crypto::ring::default_provider();
        let config = rustls::ServerConfig::builder_with_provider(provider.into())
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("tls config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("tls proxy bind");
        let address = listener.local_addr().expect("tls proxy address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("tls proxy accept");
            acceptor.accept(stream).await.is_ok()
        });
        let mut values = HashMap::new();
        values.insert("HTTPS_PROXY", format!("https://{address}"));
        let client = ConnectionTransport::with_environment(&ProxyMode::Environment, |name| {
            values.get(name).cloned()
        })
        .expect("client")
        .0;
        assert!(
            client
                .get("https://unresolvable.invalid/")
                .send()
                .await
                .is_err()
        );
        assert!(!server.await.expect("tls proxy task"));
    }

    #[tokio::test]
    async fn https_proxy_authentication_works_with_a_trusted_test_root() {
        let certified = generate_simple_self_signed(vec!["localhost".into()]).expect("certificate");
        let certificate = certified.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()),
        );
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)
        .expect("server TLS");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("proxy");
        let port = listener.local_addr().expect("address").port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let mut tls = acceptor.accept(socket).await.expect("TLS handshake");
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                tls.read_exact(&mut byte).await.expect("request");
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() < 16384);
            }
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("response");
            String::from_utf8(request).expect("UTF8")
        });
        // This root is injected ONLY in this test. Production keeps the system
        // trust store and cannot disable certificate validation.
        let client = Client::builder()
            .no_proxy()
            .add_root_certificate(
                reqwest::Certificate::from_der(certificate.as_ref()).expect("root"),
            )
            .proxy(
                Proxy::all(format!("https://localhost:{port}"))
                    .expect("proxy")
                    .basic_auth("user", "pass"),
            )
            .build()
            .expect("client");
        assert_eq!(
            client
                .get("http://unresolvable.invalid/")
                .send()
                .await
                .expect("HTTPS proxy")
                .text()
                .await
                .expect("body"),
            "ok"
        );
        let request = server.await.expect("server");
        assert!(request.starts_with("GET http://unresolvable.invalid/ HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("proxy-authorization: basic dxnlcjpwyxnz")
        );
    }

    #[tokio::test]
    async fn bad_proxy_fails_without_direct_connection_and_clients_are_isolated() {
        let target = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("target bind");
        let target_address = target.local_addr().expect("target address");
        let target_task = tokio::spawn(async move {
            timeout(Duration::from_millis(400), target.accept())
                .await
                .is_ok()
        });
        let mode = ProxyMode::Custom {
            url: "http://127.0.0.1:1".into(),
            username: None,
            password: None,
        };
        let client = ConnectionTransport::with_environment(&mode, |_| None)
            .expect("client")
            .0;
        assert!(
            client
                .get(format!("http://{target_address}/"))
                .send()
                .await
                .is_err()
        );
        assert!(!target_task.await.expect("target task"));
        let direct_target = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("direct target bind");
        let direct_address = direct_target.local_addr().expect("direct target address");
        let direct_task = tokio::spawn(async move {
            let (mut stream, _) = direct_target.accept().await.expect("direct target accept");
            let mut request = [0; 64];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("direct response");
        });
        let direct = ConnectionTransport::with_environment(&ProxyMode::Direct, |_| {
            Some("http://127.0.0.1:1".into())
        })
        .expect("direct")
        .0;
        assert_eq!(
            direct
                .get(format!("http://{direct_address}/"))
                .send()
                .await
                .expect("direct response")
                .status(),
            reqwest::StatusCode::OK
        );
        direct_task.await.expect("direct task");
    }

    #[test]
    fn custom_and_environment_invalid_proxy_fail_closed_without_secrets() {
        let marker = "secret-marker";
        let custom = ProxyMode::Custom {
            url: format!("ftp://user:{marker}@localhost"),
            username: None,
            password: None,
        };
        let error = ConnectionTransport::with_environment(&custom, |_| None)
            .err()
            .expect("reject");
        assert!(!format!("{custom:?} {error:?}").contains(marker));
        assert!(
            ConnectionTransport::with_environment(&ProxyMode::Environment, |name| (name
                == "ALL_PROXY")
                .then(|| "bad proxy".into()))
            .is_err()
        );
        assert!(
            ConnectionTransport::with_environment(&ProxyMode::Direct, |_| Some("bad proxy".into()))
                .is_ok()
        );
    }

    #[test]
    fn socks5_is_always_proxy_dns() {
        assert_eq!(
            proxy_url("socks5://user:pass@localhost:1080")
                .expect("parse")
                .scheme(),
            "socks5h"
        );
    }
}
