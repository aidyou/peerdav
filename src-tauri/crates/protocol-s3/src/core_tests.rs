use crate::{CoreS3Backend, ProxyMode, S3Config, S3Store};
use peerdav_file_core::*;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

struct Fixture {
    endpoint: String,
    calls: Arc<AtomicUsize>,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => accepted.expect("accept"),
                };
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0u8; 1024];
                    let n = socket.read(&mut buffer).await.expect("request");
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    if request.windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                observed.fetch_add(1, Ordering::SeqCst);
                let text = String::from_utf8(request).expect("http request");
                let uri = text.split_whitespace().nth(1).expect("uri");
                let url = url::Url::parse(&format!("http://localhost{uri}")).expect("url");
                let query: std::collections::BTreeMap<_, _> =
                    url.query_pairs().into_owned().collect();
                let (headers, body) = if query.get("list-type").is_some() {
                    assert_eq!(
                        query.get("prefix").map(String::as_str),
                        Some("资料/100%2F/图片/")
                    );
                    assert!(matches!(
                        query.get("max-keys").map(String::as_str),
                        Some("1" | "1000")
                    ));
                    let (key, next) = if query.get("continuation-token").is_some() {
                        assert_eq!(
                            query.get("continuation-token").map(String::as_str),
                            Some("next-literal%")
                        );
                        (
                            "资料/100%2F/图片/b%2F",
                            "<IsTruncated>false</IsTruncated>".to_owned(),
                        )
                    } else {
                        ("资料/100%2F/图片/a 中文%", "<IsTruncated>true</IsTruncated><NextContinuationToken>next-literal%</NextContinuationToken>".to_owned())
                    };
                    (
                        "Content-Type: application/xml\r\n".to_owned(),
                        format!(
                            "<ListBucketResult><Name>fixture</Name><Prefix>资料/100%2F/图片/</Prefix><MaxKeys>1</MaxKeys>{next}<Contents><Key>{key}</Key><LastModified>2024-01-01T00:00:00.000Z</LastModified><ETag>etag</ETag><Size>4</Size></Contents></ListBucketResult>"
                        ),
                    )
                } else {
                    // Deliberately ignore Range and return a full 200 response.
                    assert!(text.to_ascii_lowercase().contains("range: bytes=1-2"));
                    (
                        "ETag: etag\r\nLast-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\n".to_owned(),
                        "full".to_owned(),
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("respond");
            }
        });
        Self {
            endpoint,
            calls,
            stop,
            task,
        }
    }
    fn store(&self) -> Arc<S3Store> {
        Arc::new(
            S3Store::new(S3Config {
                endpoint: self.endpoint.clone(),
                bucket: "fixture".into(),
                region: "test".into(),
                prefix: "资料/100%2F/".into(),
                path_style: true,
                access_key: "test".into(),
                secret_key: "test".into(),
                session_token: None,
                buckets: vec![],
                allow_insecure_http: true,
                proxy: ProxyMode::Direct,
            })
            .expect("store"),
        )
    }
    async fn shutdown(self) {
        self.stop.send(()).expect("stop");
        self.task.await.expect("fixture joined");
    }
}
fn context() -> OperationContext {
    OperationContext {
        cancellation: Default::default(),
        deadline: None,
        budget: OperationBudget {
            max_bytes: 1024,
            max_chunk_bytes: NonZeroUsize::new(1024).expect("chunk"),
            max_entries: NonZeroUsize::new(10).expect("entries"),
        },
    }
}

#[tokio::test]
async fn core_http_pagination_keeps_raw_prefix_limit_provider_token_and_generation() {
    let fixture = Fixture::start().await;
    let low = fixture.store();
    let adapter = CoreS3Backend::new(low.clone(), 1).expect("adapter");
    let mut request = ListRequest {
        parent: adapter.scope().entry("图片/"),
        query: None,
        prefix: None,
        cursor: None,
        limit: NonZeroUsize::new(1).expect("limit"),
    };
    let page = adapter
        .list(context(), request.clone())
        .await
        .expect("first");
    assert_eq!(page.entries()[0].id.opaque(), "图片/a 中文%");
    assert_eq!(page.entries()[0].modified, Some(1704067200));
    request.cursor = page.next().cloned();
    assert!(request.cursor.is_some());
    let page = adapter
        .list(context(), request.clone())
        .await
        .expect("second");
    assert_eq!(page.entries()[0].id.opaque(), "图片/b%2F");
    assert_eq!(page.completeness(), &Completeness::Complete);
    let other = CoreS3Backend::new(low, 2).expect("new generation");
    assert_eq!(
        other
            .list(context(), request)
            .await
            .expect_err("stale generation")
            .kind,
        FileErrorKind::ScopeMismatch
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    fixture.shutdown().await;
}

#[tokio::test]
async fn core_http_application_bridge_preserves_pagination_and_errors() {
    use crate::{CoreS3ReadBridge, ObjectStore};
    let fixture = Fixture::start().await;
    let bridge = CoreS3ReadBridge(CoreS3Backend::new(fixture.store(), 7).expect("backend"));
    let first = ObjectStore::list_page(&bridge, "图片/", None)
        .await
        .expect("first");
    assert_eq!(first.objects[0].key, "图片/a 中文%");
    let second = ObjectStore::list_page(&bridge, "图片/", first.next_token.as_deref())
        .await
        .expect("next");
    assert_eq!(second.objects[0].key, "图片/b%2F");
    assert!(second.next_token.is_none());
    let error = ObjectStore::get(&bridge, "file", Some(1..3))
        .await
        .err()
        .expect("range rejected");
    assert!(matches!(error, crate::CloudError::Storage { .. }));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    fixture.shutdown().await;
}

#[tokio::test]
async fn core_http_bridge_unverified_conditions_never_poll_or_perform_io() {
    use crate::{CloudError, CoreS3ReadBridge, ObjectStore, PutCondition};
    use futures_util::StreamExt;
    let fixture = Fixture::start().await;
    let bridge = CoreS3ReadBridge(CoreS3Backend::new(fixture.store(), 7).expect("backend"));
    let capabilities = ObjectStore::capabilities(&bridge);
    assert!(!capabilities.create_only);
    assert!(!capabilities.conditional_replace);
    assert!(!capabilities.conditional_writes_verified);
    for condition in [PutCondition::CreateOnly, PutCondition::MatchEtag("etag".into())] {
        let polls = Arc::new(AtomicUsize::new(0));
        let observed = polls.clone();
        let body = futures_util::stream::poll_fn(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::Ready(Some(Err(CloudError::UploadCancelled)))
        }).boxed();
        let error = ObjectStore::put(&bridge, "file", body, condition)
            .await
            .expect_err("unverified conditional write");
        assert_eq!(polls.load(Ordering::SeqCst), 0, "body must remain unpolled");
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0, "no provider I/O");
        let CloudError::Storage { source: object_store::Error::Generic { source, .. } } = error else {
            panic!("expected preserved core preflight error");
        };
        let error = source.downcast_ref::<FileError>().expect("core error");
        assert_eq!(error.kind, FileErrorKind::Unsupported);
        assert_eq!(error.outcome, MutationOutcome::NotApplied);
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn core_http_ignored_range_is_rejected_not_relabelled() {
    let fixture = Fixture::start().await;
    let adapter = CoreS3Backend::new(fixture.store(), 1).expect("adapter");
    let result = adapter
        .read(
            context(),
            ReadRequest {
                entry: adapter.scope().entry("file"),
                range: Some(ReadRange::new(1, 3).expect("range")),
            },
        )
        .await;
    assert!(
        result.is_err(),
        "ignored Range must never succeed as a partial response"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.shutdown().await;
}

#[tokio::test]
async fn private_endpoint_never_drops_bucket_from_request_path() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = vec![0u8; 8192];
        let n = socket.read(&mut request).await.expect("read");
        let request = String::from_utf8_lossy(&request[..n]).into_owned();
        socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        request
    });
    let store = S3Store::new(S3Config {
        endpoint,
        bucket: "bucket".into(),
        region: "us-east-1".into(),
        prefix: String::new(),
        // Older MinIO/self-hosted connections may still persist virtual-host
        // addressing even though the endpoint cannot support it.
        path_style: false,
        access_key: "a".into(),
        secret_key: "b".into(),
        session_token: None,
        buckets: vec![],
        allow_insecure_http: true,
        proxy: ProxyMode::Direct,
    })
    .expect("store");
    assert!(store.core_stat("a.txt").await.is_err());
    let request = server.await.expect("server");
    assert!(
        request.starts_with("HEAD /bucket/a.txt HTTP/1.1\r\n"),
        "bucket must stay in the request path: {request}"
    );
}
