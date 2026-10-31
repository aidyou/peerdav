use base64::Engine;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode, body::Incoming, server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use peerdav_lib::cloud::{
    S3Store,
    traits::ObjectStore,
    types::{CloudError, ProxyMode, S3Config},
};
use std::convert::Infallible;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Empty,
    Fail,
    Cycle,
    Outside,
    Sibling,
}
struct Fixture {
    endpoint: String,
    requests: Arc<AtomicUsize>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (stop, mut shutdown) = oneshot::channel();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_task = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let requests_for_connection = Arc::clone(&requests_for_task);
                        tokio::spawn(async move {
                            let io = TokioIo::new(socket);
                            let _ = http1::Builder::new()
                                .serve_connection(
                                    io,
                                    service_fn(move |r| {
                                        requests_for_connection.fetch_add(1, Ordering::Relaxed);
                                        handle(r, mode)
                                    }),
                                )
                                .await;
                        });
                    }
                }
            }
        });
        Self {
            endpoint: format!("http://{addr}"),
            requests,
            stop: Some(stop),
            task,
        }
    }
    fn store(&self, prefix: &str) -> S3Store {
        S3Store::new(S3Config {
            endpoint: self.endpoint.clone(),
            bucket: "fixture".into(),
            region: "us-east-1".into(),
            prefix: prefix.into(),
            path_style: true,
            access_key: "test".into(),
            secret_key: "test".into(),
            session_token: None,
            allow_insecure_http: true,
            proxy: ProxyMode::Direct,
        })
        .expect("store")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}
fn xml(token: &str, next: Option<&str>, key: &str) -> String {
    format!(
        "<ListBucketResult><Name>fixture</Name><Prefix>{token}</Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>{}</IsTruncated>{}<Contents><Key>{key}</Key><LastModified>2024-01-01T00:00:00.000Z</LastModified><ETag>\"x\"</ETag><Size>1</Size></Contents></ListBucketResult>",
        next.is_some(),
        next.map(|x| format!("<NextContinuationToken>{x}</NextContinuationToken>"))
            .unwrap_or_default()
    )
}
async fn handle(req: Request<Incoming>, mode: Mode) -> Result<Response<Full<Bytes>>, Infallible> {
    let q = req.uri().query().unwrap_or_default();
    if !q.contains("list-type=2") {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()))
            .unwrap());
    }
    let token = url::form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| k == "continuation-token")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let (status, body) = match mode {
        Mode::Normal => {
            if token.is_empty() {
                (StatusCode::OK, xml("p", Some("A"), "p/a"))
            } else {
                (StatusCode::OK, xml("p", None, "p/b"))
            }
        }
        Mode::Empty => {
            if token.is_empty() {
                (
                    StatusCode::OK,
                    "<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>A</NextContinuationToken></ListBucketResult>".into(),
                )
            } else {
                (StatusCode::OK, xml("p", None, "p/a"))
            }
        }
        Mode::Fail => {
            if token.is_empty() {
                (StatusCode::OK, xml("p", Some("A"), "p/a"))
            } else {
                (StatusCode::FORBIDDEN, "denied".into())
            }
        }
        Mode::Cycle => (
            StatusCode::OK,
            xml("p", Some(if token == "A" { "B" } else { "A" }), "p/a"),
        ),
        Mode::Outside => (StatusCode::OK, xml("p", None, "other/x")),
        Mode::Sibling => (StatusCode::OK, xml("p", None, "px/file")),
    };
    Ok(Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

#[tokio::test]
async fn multi_page_token_and_prefix_binding() {
    let f = Fixture::start(Mode::Normal).await;
    let s = f.store("");
    let p = s.list_page("p", None).await.expect("first");
    assert_eq!(p.objects[0].key, "p/a");
    let t = p.next_token.expect("token");
    assert_eq!(
        s.list_page("other", Some(&t))
            .await
            .unwrap_err()
            .to_string(),
        "object key is outside the configured prefix"
    );
    let p2 = s.list_page("p", Some(&t)).await.expect("second");
    assert_eq!(p2.objects[0].key, "p/b");
}
#[tokio::test]
async fn empty_page_with_token_continues() {
    let f = Fixture::start(Mode::Empty).await;
    let s = f.store("");
    let p = s.list_page("p", None).await.expect("first");
    let t = p.next_token.expect("token");
    let p2 = s.list_page("p", Some(&t)).await.expect("second");
    assert_eq!(p2.objects.len(), 1);
    assert_eq!(p2.objects[0].key, "p/a");
    assert!(p2.next_token.is_none());
    assert_eq!(f.requests.load(Ordering::Relaxed), 2);
}
#[tokio::test]
async fn sibling_prefix_is_rejected() {
    let f = Fixture::start(Mode::Sibling).await;
    let s = f.store("");
    assert!(matches!(
        s.list_page("p", None).await,
        Err(CloudError::PrefixViolation)
    ));
}
#[tokio::test]
async fn empty_scope_token_is_rejected() {
    let f = Fixture::start(Mode::Normal).await;
    let s = f.store("");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({"prefix":"p","provider_token":"","pages":0,"entries":0,"scope_fingerprint":""}).to_string(),
    );
    assert!(matches!(
        s.list_page("p", Some(&raw)).await,
        Err(CloudError::PrefixViolation)
    ));
}
#[tokio::test]
async fn second_page_error_is_not_complete() {
    let f = Fixture::start(Mode::Fail).await;
    let s = f.store("");
    let p = s.list_page("p", None).await.expect("first");
    let t = p.next_token.expect("token");
    assert!(s.list_page("p", Some(&t)).await.is_err());
}
#[tokio::test]
async fn provider_cycle_is_rejected() {
    let f = Fixture::start(Mode::Cycle).await;
    let s = f.store("");
    let p = s.list_page("p", None).await.expect("first");
    let t = p.next_token.expect("token");
    let p2 = s.list_page("p", Some(&t)).await.expect("second");
    let t2 = p2.next_token.expect("token");
    assert!(matches!(
        s.list_page("p", Some(&t2)).await,
        Err(CloudError::InvalidConfig(_))
    ));
}
#[tokio::test]
async fn response_key_outside_prefix_is_rejected() {
    let f = Fixture::start(Mode::Outside).await;
    let s = f.store("");
    assert!(matches!(
        s.list_page("p", None).await,
        Err(CloudError::PrefixViolation)
    ));
}
#[tokio::test]
async fn forged_budget_token_cannot_skip_scope_or_limits() {
    let f = Fixture::start(Mode::Normal).await;
    let s = f.store("");
    let p = s.list_page("p", None).await.expect("first");
    let token = p.next_token.expect("token");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .expect("decode token");
    let mut state: serde_json::Value = serde_json::from_slice(&bytes).expect("parse token");
    // This DTO is transport state, not an authenticated authorization ledger.
    // Start with a real scoped token and alter only counters to test local guards.
    let before = f.requests.load(Ordering::Relaxed);
    for (pages, entries) in [(1000, 1), (1, 100000)] {
        state["pages"] = serde_json::json!(pages);
        state["entries"] = serde_json::json!(entries);
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&state).expect("serialize token"));
        assert!(matches!(
            s.list_page("p", Some(&raw)).await,
            Err(CloudError::UploadBudgetExceeded)
        ));
        assert_eq!(f.requests.load(Ordering::Relaxed), before);
    }
    state["pages"] = serde_json::json!(999);
    state["entries"] = serde_json::json!(99999);
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&state).expect("serialize token"));
    let last = s
        .list_page("p", Some(&raw))
        .await
        .expect("last allowed page");
    assert_eq!(last.objects.len(), 1);
    assert!(last.next_token.is_none());
    assert_eq!(f.requests.load(Ordering::Relaxed), before + 1);
}

#[tokio::test]
async fn trailing_slash_prefix_matches_but_tokens_remain_exactly_bound() {
    let f = Fixture::start(Mode::Normal).await;
    let s = f.store("");
    let first = s.list_page("p/", None).await.expect("trailing prefix");
    assert_eq!(first.objects[0].key, "p/a");
    let token = first.next_token.expect("token");
    let before = f.requests.load(Ordering::Relaxed);
    assert!(matches!(
        s.list_page("p", Some(&token)).await,
        Err(CloudError::PrefixViolation)
    ));
    assert_eq!(f.requests.load(Ordering::Relaxed), before);
    let last = s.list_page("p/", Some(&token)).await.expect("second");
    assert_eq!(last.objects[0].key, "p/b");
}

#[tokio::test]
async fn empty_root_lists_children_and_still_binds_token_scope() {
    let f = Fixture::start(Mode::Normal).await;
    let s = f.store("");
    let first = s.list_page("", None).await.expect("root");
    assert_eq!(first.objects[0].key, "p/a");
    let token = first.next_token.expect("token");
    let before = f.requests.load(Ordering::Relaxed);
    assert!(matches!(
        s.list_page("p", Some(&token)).await,
        Err(CloudError::PrefixViolation)
    ));
    assert!(matches!(
        f.store("other").list_page("", Some(&token)).await,
        Err(CloudError::PrefixViolation)
    ));
    assert_eq!(f.requests.load(Ordering::Relaxed), before);
    assert!(
        s.list_page("", Some(&token))
            .await
            .expect("root second")
            .next_token
            .is_none()
    );
}
