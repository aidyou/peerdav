use std::time::Duration;

use object_store::client::{
    HttpClient, HttpConnector, HttpError, HttpRequest, HttpResponse, HttpService,
};
use reqwest::{Client, NoProxy, Proxy};
use url::Url;

use super::types::{CloudError, ProxyMode};

#[derive(Clone)]
pub struct ConnectionTransport(Client);

#[derive(Clone)]
pub(crate) struct QiniuResponseCompatibility(ConnectionTransport);

impl std::fmt::Debug for QiniuResponseCompatibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QiniuResponseCompatibility([redacted])")
    }
}

#[async_trait::async_trait]
impl HttpService for QiniuResponseCompatibility {
    async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut response = HttpService::call(&self.0.0, request).await?;
        // Qiniu emits raw UTF-8 Content-Disposition. object_store 0.14.2
        // applies HeaderValue::to_str (ASCII only) to this optional attribute,
        // failing even HEAD. We do not expose presentation attributes; discard
        // only this incompatible optional header, never integrity metadata.
        discard_non_ascii_disposition(response.headers_mut());
        Ok(response)
    }
}

fn discard_non_ascii_disposition(headers: &mut reqwest::header::HeaderMap) {
    if headers
        .get("content-disposition")
        .is_some_and(|value| value.to_str().is_err())
    {
        headers.remove("content-disposition");
    }
}

impl HttpConnector for QiniuResponseCompatibility {
    fn connect(&self, _: &object_store::ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(self.clone()))
    }
}

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
    pub(crate) fn qiniu_compatibility(self) -> QiniuResponseCompatibility {
        QiniuResponseCompatibility(self)
    }
    /// Shared client for service-level requests (for example bucket discovery)
    /// that must obey the same proxy, redirect and timeout policy as object I/O.
    pub fn client(&self) -> &Client {
        &self.0
    }

    pub fn new(mode: &ProxyMode) -> Result<Self, CloudError> {
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

    #[test]
    fn disposition_compatibility_preserves_all_other_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("last-modified", "invalid-date"),
            ("content-length", "3"),
            ("etag", "\"revision\""),
            ("content-range", "bytes 1-2/3"),
            ("date", "unchanged"),
            ("x-amz-request-id", "request"),
        ] {
            headers.insert(name, HeaderValue::from_str(value).expect("header"));
        }
        let original = headers.clone();
        headers.insert(
            "content-disposition",
            HeaderValue::from_bytes("attachment; filename=中文.png".as_bytes()).expect("UTF8"),
        );
        discard_non_ascii_disposition(&mut headers);
        assert_eq!(headers, original);
        headers.insert(
            "content-disposition",
            HeaderValue::from_static("attachment; filename=image.png"),
        );
        let ascii = headers.clone();
        discard_non_ascii_disposition(&mut headers);
        assert_eq!(headers, ascii);
    }

    #[tokio::test]
    async fn qiniu_utf8_disposition_preserves_raw_keys_and_metadata() {
        use crate::{ObjectStore, PutCondition, S3Config, S3Store};
        use bytes::Bytes;
        use futures_util::{StreamExt, TryStreamExt, stream};
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..9 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut request = Vec::new();
                loop {
                    request.push(socket.read_u8().await.expect("header"));
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8(request).expect("ASCII request");
                let mut first = text.lines().next().expect("line").split_whitespace();
                let method = first.next().expect("method");
                paths.push(first.next().expect("path").to_owned());
                let size = text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("size"))
                    })
                    .unwrap_or(0);
                let mut body = vec![0; size];
                socket.read_exact(&mut body).await.expect("body");
                if method == "PUT" {
                    assert_eq!(body, b"raw");
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"revision\"\r\nConnection: close\r\n\r\n").await.expect("put");
                } else {
                    // Qiniu returns raw UTF-8 in this optional presentation header.
                    let response = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nETag: \"revision\"\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\nContent-Disposition: attachment; filename=\"中文 图片.png\"\r\nConnection: close\r\n\r\n";
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("metadata");
                    if method == "GET" {
                        socket.write_all(b"raw").await.expect("data");
                    }
                }
            }
            paths
        });
        let store = S3Store::new(S3Config {
            endpoint: format!("http://{address}"),
            bucket: "fixture".into(),
            region: "test".into(),
            prefix: "owned/".into(),
            path_style: true,
            access_key: "fake".into(),
            secret_key: "fake".into(),
            session_token: None,
            buckets: vec![],
            allow_insecure_http: true,
            proxy: ProxyMode::Direct,
        })
        .expect("store");
        for key in [
            "图片/中文 图片.png",
            "图片/literal%2F.png",
            "图片/space image.png",
        ] {
            store
                .put(
                    key,
                    stream::once(async { Ok(Bytes::from_static(b"raw")) }).boxed(),
                    PutCondition::CreateOnly,
                )
                .await
                .expect("put");
            let meta = store.stat(key).await.expect("head with UTF-8 disposition");
            assert_eq!(meta.key, key);
            assert_eq!(meta.size, 3);
            assert_eq!(meta.etag.as_deref(), Some("\"revision\""));
            let chunks: Vec<Bytes> = store
                .get(key, None)
                .await
                .expect("get")
                .try_collect()
                .await
                .expect("body");
            assert_eq!(chunks.concat(), b"raw");
        }
        let paths = server.await.expect("server");
        for group in paths.chunks_exact(3) {
            assert!(group.iter().all(|path| path == &group[0]));
        }
        assert!(paths[0].contains("%E4%B8%AD%E6%96%87%20"));
        assert!(paths[3].contains("literal%252F.png"));
        assert!(paths[6].contains("space%20image.png"));
    }

    #[tokio::test]
    async fn conditional_delete_sends_revision_and_preserves_provider_errors() {
        use crate::{CloudError, ObjectStore, S3Config, S3Store};
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"probe\"\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nETag: \"probe\"\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\nConnection: close\r\n\r\ndenied",
            ] {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.expect("header"));
                }
                let request = String::from_utf8(bytes).expect("request");
                let length = request.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().expect("length"))
                }).unwrap_or(0);
                socket.read_exact(&mut vec![0; length]).await.expect("body");
                requests.push(request);
                socket.write_all(response.as_bytes()).await.expect("response");
            }
            requests
        });
        let store = S3Store::new(S3Config {
            endpoint: format!("http://{address}"), bucket: "fixture".into(),
            region: "test".into(), prefix: "owned/".into(), path_style: true,
            access_key: "fake".into(), secret_key: "fake".into(), session_token: None,
            buckets: vec![], allow_insecure_http: true, proxy: ProxyMode::Direct,
        }).expect("store");
        let store = crate::CoreS3ReadBridge(crate::CoreS3Backend::new(Arc::new(store), 1).expect("bridge"));
        for revision in ["", " ", "*", "bad\r\nheader"] {
            assert!(matches!(store.delete_if_match("图片/keep.png", revision).await, Err(CloudError::InvalidConfig(_))));
        }
        assert!(matches!(store.delete_if_match("../outside", "\"revision\"").await, Err(CloudError::InvalidConfig(_))));
        let result = store.delete_if_match("图片/literal%2F image.png", "\"revision\"").await;
        if result.is_err() {
            server.abort();
        }
        assert!(result.is_ok(), "production adapter refused conditional delete: {result:?}");
        assert!(matches!(store.delete_if_match("图片/literal%2F image.png", "\"stale\"").await, Err(CloudError::Precondition)));
        assert!(matches!(store.delete_if_match("图片/literal%2F image.png", "\"revision\"").await,
            Err(CloudError::Service { status: 403, detail }) if detail == "denied"));
        let requests = timeout(Duration::from_secs(5), server).await.expect("server timeout").expect("server");
        assert_eq!(requests.len(), 8);
        assert!(requests[0].starts_with("PUT /fixture/owned/probes/"));
        assert!(requests[1].contains("if-match: \"peerdav-impossible-revision\""));
        for request in requests.into_iter().skip(5) {
            assert!(request.starts_with("DELETE /fixture/owned/%E5%9B%BE%E7%89%87/literal%252F%20image.png?"));
            assert!(request.to_ascii_lowercase().contains("if-match: \""));
            assert!(request.contains("X-Amz-Signature="));
            assert!(request.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        }
    }

    #[tokio::test]
    async fn conditional_delete_refuses_provider_ignoring_precondition() {
        use crate::{CloudError, ObjectStore, S3Config, S3Store};
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            for response in [
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"probe\"\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ] {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") { bytes.push(socket.read_u8().await.expect("header")); }
                let request = String::from_utf8(bytes).expect("request");
                assert!(request.lines().next().expect("line").contains("/owned/probes/"));
                let length = request.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().expect("length"))
                }).unwrap_or(0);
                socket.read_exact(&mut vec![0; length]).await.expect("body");
                socket.write_all(response.as_bytes()).await.expect("response");
            }
            assert!(timeout(Duration::from_millis(100), listener.accept()).await.is_err(), "must not delete user object");
        });
        let store = S3Store::new(S3Config {
            endpoint: format!("http://{address}"), bucket: "fixture".into(), region: "test".into(), prefix: "owned/".into(),
            path_style: true, access_key: "fake".into(), secret_key: "fake".into(), session_token: None,
            buckets: vec![], allow_insecure_http: true, proxy: ProxyMode::Direct,
        }).expect("store");
        for key in ["图片/keep.png", "图片/keep2.png"] {
            assert!(matches!(store.delete_if_match(key, "\"revision\"").await, Err(CloudError::Unsupported)));
        }
        timeout(Duration::from_secs(5), server).await.expect("timeout").expect("server");
    }

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
