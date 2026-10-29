//! Proxy discovery for update requests.
//!
//! The updater talks to GitHub and jsDelivr, which are unreachable from some networks unless the
//! traffic goes through a proxy. The HTTP client reads proxy variables on its own, but a stale
//! variable that points at a proxy which is no longer running makes every update check fail, so
//! the configured proxy is checked before it is used and skipped when the server does not answer.

use std::env;
use std::future::Future;

use tokio::net::TcpStream;
use tokio::time::timeout;
use url::Url;

use super::types::PROXY_CHECK_TIMEOUT;
use super::types::ProxyChoice;

/// Environment variables checked in order.
///
/// HTTPS variables come first because every update endpoint is HTTPS, `ALL_PROXY` follows as the
/// usual catch-all set by proxy clients, and the plain `HTTP_*` variables are the last resort.
const ENV_VARS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// Resolves which proxy the update requests must use.
pub(super) async fn resolve() -> ProxyChoice {
    let lookup = |name: &str| env::var(name).ok();
    if no_proxy_disables(&lookup) {
        log::info!("NO_PROXY disables proxies for update requests");
        return ProxyChoice::Direct;
    }
    let candidates = candidates_with(&lookup);
    if candidates.is_empty() {
        return ProxyChoice::Platform;
    }
    match select(candidates, reachable).await {
        Some(proxy) => {
            log::info!("update requests use the proxy {}", redacted(&proxy));
            ProxyChoice::Url(proxy)
        }
        None => {
            log::warn!("no configured proxy is reachable; update requests connect directly");
            ProxyChoice::Direct
        }
    }
}

/// Returns the first candidate whose server answers a TCP connection.
async fn select<F, Fut>(candidates: Vec<Url>, check: F) -> Option<Url>
where
    F: Fn(Url) -> Fut,
    Fut: Future<Output = bool>,
{
    for proxy in candidates {
        if check(proxy.clone()).await {
            return Some(proxy);
        }
        log::warn!(
            "ignoring the unreachable proxy {} set in the environment",
            redacted(&proxy)
        );
    }
    None
}

fn candidates_with(lookup: &impl Fn(&str) -> Option<String>) -> Vec<Url> {
    let mut candidates = Vec::new();
    for name in ENV_VARS {
        let Some(value) = lookup(name) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match parse(value) {
            Ok(proxy) => {
                log::debug!("proxy candidate from {name}: {}", redacted(&proxy));
                candidates.push(proxy);
            }
            Err(reason) => log::warn!("ignoring proxy in {name}: {reason}"),
        }
    }
    candidates
}

/// `NO_PROXY=*` is the documented way to opt out of proxy usage, so it wins over the proxy
/// variables. Individual host names are not matched here: update hosts are always public, and a
/// host list is normally about intranet addresses.
fn no_proxy_disables(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    ["NO_PROXY", "no_proxy"].iter().any(|name| {
        lookup(name).is_some_and(|value| value.split(',').any(|entry| entry.trim() == "*"))
    })
}

fn parse(value: &str) -> Result<Url, String> {
    let candidate = if value.contains("://") {
        value.to_owned()
    } else {
        // `127.0.0.1:7890` is a common way to write the variable; assume plain HTTP.
        format!("http://{value}")
    };
    let proxy = Url::parse(&candidate).map_err(|error| error.to_string())?;
    match proxy.scheme() {
        // The updater's HTTP client is built without SOCKS support, so a SOCKS URL handed to it
        // would fail every request instead of falling back to a direct connection.
        "http" | "https" => {}
        scheme => return Err(format!("unsupported proxy scheme `{scheme}`")),
    }
    if proxy.host_str().is_none_or(str::is_empty) {
        return Err("proxy URL has no host".to_owned());
    }
    Ok(proxy)
}

/// Checks that the proxy server accepts connections. Only the TCP handshake is verified: a proxy
/// that answers but cannot reach the internet is not distinguishable without sending real traffic.
async fn reachable(proxy: Url) -> bool {
    let Some(host) = proxy.host_str() else {
        return false;
    };
    let Some(port) = proxy.port_or_known_default() else {
        return false;
    };
    matches!(
        timeout(PROXY_CHECK_TIMEOUT, TcpStream::connect((host, port))).await,
        Ok(Ok(_stream))
    )
}

/// Formats a proxy URL without its credentials so logs never contain secrets.
fn redacted(proxy: &Url) -> String {
    let host = proxy.host_str().unwrap_or_default();
    match proxy.port() {
        Some(port) => format!("{}://{host}:{port}", proxy.scheme()),
        None => format!("{}://{host}", proxy.scheme()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn candidates(vars: &[(&str, &str)]) -> Vec<String> {
        candidates_with(&lookup(vars))
            .into_iter()
            .map(|url| url.to_string())
            .collect()
    }

    #[test]
    fn keeps_the_configured_proxies_in_priority_order() {
        let all = [
            ("HTTPS_PROXY", "http://https-proxy:1"),
            ("ALL_PROXY", "http://all-proxy:2"),
            ("HTTP_PROXY", "http://http-proxy:3"),
        ];
        assert_eq!(
            candidates(&all),
            [
                "http://https-proxy:1/",
                "http://all-proxy:2/",
                "http://http-proxy:3/",
            ]
        );
        assert_eq!(
            candidates(&[("http_proxy", "http://lower-proxy:4")]),
            ["http://lower-proxy:4/"]
        );
        assert!(candidates(&[]).is_empty());
        assert!(candidates(&[("HTTPS_PROXY", "   ")]).is_empty());
    }

    #[test]
    fn adds_the_scheme_when_the_variable_omits_it() {
        assert_eq!(
            candidates(&[("HTTPS_PROXY", " 127.0.0.1:7890 ")]),
            ["http://127.0.0.1:7890/"]
        );
    }

    #[test]
    fn skips_unusable_entries_and_keeps_looking() {
        let values = [
            ("HTTPS_PROXY", "socket://127.0.0.1:9"),
            ("https_proxy", "http://"),
            ("ALL_PROXY", "socks5://127.0.0.1:7891"),
            ("all_proxy", "http://127.0.0.1:7892"),
        ];
        assert_eq!(candidates(&values), ["http://127.0.0.1:7892/"]);
        assert!(candidates(&[("ALL_PROXY", "socks5://127.0.0.1:7891")]).is_empty());
    }

    #[test]
    fn no_proxy_with_a_wildcard_disables_the_configured_proxy() {
        let values = [
            ("HTTPS_PROXY", "http://127.0.0.1:7890"),
            ("NO_PROXY", "localhost, *"),
        ];
        assert!(no_proxy_disables(&lookup(&values)));
        assert!(!no_proxy_disables(&lookup(&[("no_proxy", "localhost")])));
    }

    #[tokio::test]
    async fn skips_a_proxy_whose_server_is_down() {
        let candidates: Vec<Url> = ["http://down-proxy:9", "http://up-proxy:10"]
            .iter()
            .map(|value| parse(value).expect("valid proxy"))
            .collect();
        let chosen = select(
            candidates,
            |proxy: Url| async move { proxy.port() == Some(10) },
        )
        .await;
        assert_eq!(
            chosen.map(|url| url.to_string()),
            Some("http://up-proxy:10/".to_owned())
        );
    }

    #[tokio::test]
    async fn reports_no_proxy_when_none_is_reachable() {
        let candidates = vec![parse("http://127.0.0.1:9").expect("valid proxy")];
        assert!(select(candidates, |_proxy| async { false }).await.is_none());
    }

    #[tokio::test]
    async fn reachable_follows_the_tcp_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a local listener");
        let address = listener.local_addr().expect("listener address");
        let proxy = parse(&format!("http://{address}")).expect("valid proxy");
        assert!(
            reachable(proxy.clone()).await,
            "a listening socket must be reachable"
        );
        drop(listener);
        assert!(
            !reachable(proxy).await,
            "a closed socket must not be reachable"
        );
    }

    #[test]
    fn keeps_credentials_out_of_logs() {
        let proxy = parse("http://user:secret@127.0.0.1:8080").expect("valid proxy");
        assert_eq!(redacted(&proxy), "http://127.0.0.1:8080");
    }
}
