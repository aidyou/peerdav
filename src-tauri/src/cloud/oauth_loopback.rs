//! Loopback-only OAuth callback receiver; no background tasks or token logging.
use super::oauth_types::{LoopbackAuthorization, OAuthError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub(crate) async fn bind(redirect: &str, cancel: CancellationToken) -> Result<LoopbackAuthorization, OAuthError> {
    let url = url::Url::parse(redirect).map_err(|_| OAuthError::InvalidInput)?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.query().is_some()
        || url.fragment().is_some() || !url.username().is_empty() || url.password().is_some()
        || url.path() != "/oauth/callback" {
        return Err(OAuthError::InvalidInput);
    }
    let port = url.port().ok_or(OAuthError::InvalidInput)?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await.map_err(OAuthError::CallbackIo)?;
    let address = listener.local_addr().map_err(OAuthError::CallbackIo)?;
    let redirect_uri = format!("http://127.0.0.1:{}/oauth/callback", address.port());
    Ok(LoopbackAuthorization { listener, redirect_uri, created: Instant::now(), cancel })
}

pub(crate) async fn receive(pending: LoopbackAuthorization) -> Result<Zeroizing<String>, OAuthError> {
    let remaining = Duration::from_secs(600).checked_sub(pending.created.elapsed()).ok_or(OAuthError::InvalidState)?;
    tokio::select! {
        biased;
        _ = pending.cancel.cancelled() => Err(OAuthError::InvalidState),
        _ = tokio::time::sleep(remaining) => Err(OAuthError::InvalidState),
        result = receive_inner(&pending) => result,
    }
}

async fn receive_inner(pending: &LoopbackAuthorization) -> Result<Zeroizing<String>, OAuthError> {
    // Noise requests do not consume the OAuth state; bounded by count and deadline.
    for _ in 0..32 {
        let (mut socket, address) = pending.listener.accept().await.map_err(OAuthError::CallbackIo)?;
        if !address.ip().is_loopback() { continue; }
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = Zeroizing::new(Vec::new());
            let mut chunk = [0; 1024];
            while !bytes.windows(4).any(|value| value == b"\r\n\r\n") {
                let length = socket.read(&mut chunk).await.map_err(OAuthError::CallbackIo)?;
                if length == 0 || bytes.len() + length > 16384 { return Err(OAuthError::InvalidInput); }
                bytes.extend_from_slice(&chunk[..length]);
            }
            let request = std::str::from_utf8(&bytes).map_err(|_| OAuthError::InvalidInput)?;
            let mut parts = request.lines().next().ok_or(OAuthError::InvalidInput)?.split_whitespace();
            if parts.next() != Some("GET") { return Err(OAuthError::InvalidInput); }
            let target = parts.next().ok_or(OAuthError::InvalidInput)?;
            if !target.starts_with("/oauth/callback?") || target.contains('#') { return Err(OAuthError::InvalidInput); }
            let host = url::Url::parse(&pending.redirect_uri).map_err(|_| OAuthError::InvalidInput)?;
            let expected = format!("127.0.0.1:{}", host.port().ok_or(OAuthError::InvalidInput)?);
            let hosts: Vec<_> = request.lines().skip(1).filter_map(|line| line.split_once(':'))
                .filter(|(name, _)| name.eq_ignore_ascii_case("host")).map(|(_, value)| value.trim()).collect();
            if hosts != [expected.as_str()] { return Err(OAuthError::InvalidInput); }
            Ok(Zeroizing::new(format!("http://{expected}{target}")))
        }).await;
        let callback = match request { Ok(Ok(value)) => Some(value), _ => None };
        let body = if callback.is_some() { "Authorization received. Return to PeerDav. / 授权已接收，请返回 PeerDav。" } else { "Invalid callback request." };
        let status = if callback.is_some() { "200 OK" } else { "400 Bad Request" };
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        // A browser disconnect must not discard an already received authorization code.
        if let Err(error) = socket.write_all(response.as_bytes()).await { log::debug!("OAuth callback response write failed: {error}"); }
        if let Some(callback) = callback { return Ok(callback); }
    }
    Err(OAuthError::InvalidState)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_wrong_host_and_stalled_or_expired_callbacks() {
        let pending = bind("http://127.0.0.1:0/oauth/callback", CancellationToken::new()).await.expect("bind");
        let port = pending.listener.local_addr().expect("address").port();
        let client = tokio::spawn(async move {
            for host in ["evil.invalid".to_owned(), format!("127.0.0.1:{port}")] {
                let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.expect("connect");
                socket.write_all(format!("GET /oauth/callback?code=code&state=state HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).await.expect("request");
                let mut response = Vec::new(); socket.read_to_end(&mut response).await.expect("response");
                assert!(String::from_utf8_lossy(&response).starts_with(if host == "evil.invalid" { "HTTP/1.1 400" } else { "HTTP/1.1 200" }));
            }
        });
        assert!(receive(pending).await.is_ok());
        client.await.expect("client");
        let mut expired = bind("http://127.0.0.1:0/oauth/callback", CancellationToken::new()).await.expect("bind");
        expired.created = Instant::now() - Duration::from_secs(601);
        assert!(receive(expired).await.is_err());
    }

    #[tokio::test]
    async fn receives_loopback_callback_and_cancels_without_background_tasks() {
        let pending = bind("http://127.0.0.1:0/oauth/callback", CancellationToken::new()).await.expect("bind");
        let port = pending.listener.local_addr().expect("address").port();
        let client = tokio::spawn(async move {
            let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.expect("connect");
            socket.write_all(format!("GET /oauth/callback?code=private&state=state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).await.expect("write");
            let mut response = Vec::new(); socket.read_to_end(&mut response).await.expect("response");
            assert!(!String::from_utf8_lossy(&response).contains("private"));
        });
        let callback = receive(pending).await.expect("callback");
        assert!(callback.contains("code=private"));
        client.await.expect("client");
        let cancel = CancellationToken::new();
        let pending = bind("http://127.0.0.1:0/oauth/callback", cancel.clone()).await.expect("bind");
        cancel.cancel(); assert!(receive(pending).await.is_err());
        assert!(bind("http://0.0.0.0:8000/oauth/callback", CancellationToken::new()).await.is_err());
        assert!(bind("https://example.com/oauth/callback", CancellationToken::new()).await.is_err());
    }
}
