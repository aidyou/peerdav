use super::*;
use std::{num::NonZeroUsize, time::Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn context() -> OperationContext {
    OperationContext {
        cancellation: Default::default(),
        deadline: Some(Instant::now() + Duration::from_secs(3)),
        budget: OperationBudget {
            max_bytes: 1024 * 1024,
            max_chunk_bytes: NonZeroUsize::new(65536).unwrap(),
            max_entries: NonZeroUsize::new(100).unwrap(),
        },
    }
}
fn client(host: String) -> Client {
    reqwest_dav::ClientBuilder::new()
        .set_host(host)
        .set_agent(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        )
        .build()
        .unwrap()
}
async fn server(response: String) -> (Client, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut data = vec![];
        loop {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            data.push(byte[0]);
            if data.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(data).unwrap()
    });
    (client(host), task)
}
struct MemorySource;
impl ReplayableSource for MemorySource {
    fn size(&self) -> u64 {
        3
    }
    fn resume_key(&self) -> &str {
        "stable-source\n3"
    }
    fn read_at(
        &self,
        offset: u64,
        length: usize,
    ) -> BoxFuture<'_, Result<bytes::Bytes, FileError>> {
        Box::pin(async move {
            Ok(bytes::Bytes::copy_from_slice(
                &b"abc"[offset as usize..offset as usize + length],
            ))
        })
    }
    fn validate(&self) -> BoxFuture<'_, Result<(), FileError>> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn resumable_write_contract_v1_v2_resume_and_unknown_cleanup() {
    for (version, fail_commit, cleanup_status) in [
        ("1", false, 204),
        ("2", false, 204),
        ("2", true, 500),
        ("2", true, 200),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let methods = if fail_commit {
                vec!["OPTIONS", "HEAD", "PUT", "DELETE"]
            } else {
                vec!["OPTIONS", "HEAD", "PUT"]
            };
            for method in methods {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = vec![];
                loop {
                    data.push(socket.read_u8().await.unwrap());
                    if data.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let headers = String::from_utf8(data).unwrap().to_ascii_lowercase();
                assert!(headers.starts_with(&method.to_ascii_lowercase()));
                if method == "HEAD" || method == "PUT" {
                    let digest = hex::encode(sha2::Sha256::digest(b"abc"));
                    let header = if method == "HEAD" {
                        "x-peerdav-part-sha256"
                    } else {
                        "x-peerdav-content-sha256"
                    };
                    assert!(headers.contains(&format!("{header}: {digest}")));
                }
                if method == "PUT" {
                    assert!(headers.contains("x-peerdav-write-condition: replace"));
                }
                if fail_commit && method == "PUT" {
                    continue;
                }
                let status = if method == "DELETE" {
                    cleanup_status
                } else {
                    204
                };
                socket.write_all(format!("HTTP/1.1 {status} Test\r\nx-peerdav-write-conditions: create-only,replace\r\nx-peerdav-upload-version: {version}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let backend =
            CorePeerDavBackend::new(client(host), "test".into(), "abc".into(), 0).unwrap();
        let result = backend
            .write_source(
                context(),
                SourceWriteRequest {
                    entry: backend.scope().entry("/file"),
                    condition: Condition::Any,
                    mode: WriteMode::Streaming,
                    source: std::sync::Arc::new(MemorySource),
                    session_id: "aa".into(),
                    progress: None,
                },
            )
            .await;
        if fail_commit && cleanup_status != 200 {
            let e = result.unwrap_err();
            assert_eq!(e.outcome, MutationOutcome::Unknown);
            assert_eq!(e.cleanup.len(), 1);
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
        server.await.unwrap();
    }
}
#[tokio::test]
async fn resumable_unsupported_conditions_rejected_before_source_or_network() {
    struct Unreadable;
    impl ReplayableSource for Unreadable {
        fn size(&self) -> u64 {
            3
        }
        fn resume_key(&self) -> &str {
            "key"
        }
        fn read_at(&self, _: u64, _: usize) -> BoxFuture<'_, Result<bytes::Bytes, FileError>> {
            panic!("source must not be read")
        }
        fn validate(&self) -> BoxFuture<'_, Result<(), FileError>> {
            panic!("source must not be validated")
        }
    }
    let backend = CorePeerDavBackend::new(
        client("http://127.0.0.1:1".into()),
        "test".into(),
        "abc".into(),
        0,
    )
    .unwrap();
    for (condition, mode) in [
        (
            Condition::Match(Revision::new("revision")),
            WriteMode::Streaming,
        ),
        (Condition::Any, WriteMode::BoundedAtomic { max_bytes: 3 }),
    ] {
        let error = backend
            .write_source(
                context(),
                SourceWriteRequest {
                    entry: backend.scope().entry("/file"),
                    condition,
                    mode,
                    source: std::sync::Arc::new(Unreadable),
                    session_id: "aa".into(),
                    progress: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, FileErrorKind::Unsupported);
        assert_eq!(error.outcome, MutationOutcome::NotApplied);
    }
}

use sha2::Digest;

#[test]
fn raw_paths_roundtrip_once_and_share_boundaries() {
    let raw = "/中文/100% #?.txt";
    let encoded = encode_share_path("abc", raw).unwrap();
    assert_eq!(href_to_relative(&encoded, "abc"), Some(raw.into()));
    assert_eq!(
        href_to_relative(&format!("https://host{encoded}"), "abc"),
        Some(raw.into())
    );
    assert_eq!(href_to_relative("/share/abcd/a", "abc"), None);
    assert_eq!(href_to_relative("/share/abc/%2e%2e/secret", "abc"), None);
    assert_eq!(href_to_relative("/share/abc/a%2fb", "abc"), None);
    assert_eq!(
        href_to_relative("/share/abc/%252f", "abc"),
        Some("/%2f".into())
    );
}
#[tokio::test]
async fn range_is_verified_not_merely_requested() {
    let (client, task) =
        server("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".into()).await;
    assert!(
        get(&client, "/share/abc/a", Some(ReadRange::new(1, 3).unwrap()))
            .await
            .is_err()
    );
    assert!(
        task.await
            .unwrap()
            .to_ascii_lowercase()
            .contains("range: bytes=1-2")
    );
    let (client, task) = server("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 1-2/3\r\nContent-Length: 2\r\nConnection: close\r\n\r\nbc".into()).await;
    let backend = CorePeerDavBackend::new(client, "test".into(), "abc".into(), 1).unwrap();
    let response = backend
        .read(
            context(),
            ReadRequest {
                entry: backend.scope().entry("/a"),
                range: Some(ReadRange::new(1, 3).unwrap()),
            },
        )
        .await
        .unwrap();
    let bytes = response.body.try_collect::<Vec<_>>().await.unwrap();
    assert_eq!(bytes.concat(), b"bc");
    task.await.unwrap();
}
use futures_util::TryStreamExt;
#[tokio::test]
async fn scope_and_unsupported_operations_fail_before_io_or_body_poll() {
    let backend = CorePeerDavBackend::new(
        client("http://127.0.0.1:1".into()),
        "test".into(),
        "abc".into(),
        2,
    )
    .unwrap();
    let old = BackendScope::new("peerdav".into(), "test".into(), "abc".into(), 1).unwrap();
    assert_eq!(
        backend
            .stat(context(), old.entry("/a"))
            .await
            .unwrap_err()
            .kind,
        FileErrorKind::ScopeMismatch
    );
    let request = ListRequest {
        parent: backend.scope().entry("/"),
        query: None,
        prefix: None,
        cursor: None,
        limit: NonZeroUsize::new(1).unwrap(),
    };
    let mut continued = request.clone();
    continued.cursor = Some(ListCursor::new(&request, "unsupported".into()).unwrap());
    assert_eq!(
        backend.list(context(), continued).await.unwrap_err().kind,
        FileErrorKind::Unsupported
    );
    let body = Box::pin(futures_util::stream::poll_fn(|_| {
        panic!("unsupported write polled body")
    }));
    assert_eq!(
        backend
            .write(
                context(),
                WriteRequest {
                    entry: backend.scope().entry("/a"),
                    condition: Condition::Any,
                    mode: WriteMode::Streaming,
                    body
                }
            )
            .await
            .unwrap_err()
            .kind,
        FileErrorKind::Unsupported
    );
    for mode in [DeleteMode::File, DeleteMode::EmptyDirectory] {
        assert_eq!(
            backend
                .delete(
                    context(),
                    DeleteRequest {
                        entry: backend.scope().entry("/a"),
                        mode,
                        condition: Condition::Any
                    }
                )
                .await
                .unwrap_err()
                .kind,
            FileErrorKind::Unsupported
        );
    }
}
#[tokio::test]
async fn recursive_delete_and_create_dir_dispatch_core_contract() {
    for (method, status) in [("DELETE", "204 No Content"), ("MKCOL", "201 Created")] {
        let (client, task) = server(format!(
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ))
        .await;
        let backend = CorePeerDavBackend::new(client, "test".into(), "abc".into(), 1).unwrap();
        let entry = backend.scope().entry("/中文%");
        if method == "DELETE" {
            backend
                .delete(
                    context(),
                    DeleteRequest {
                        entry,
                        condition: Condition::Any,
                        mode: DeleteMode::Recursive,
                    },
                )
                .await
                .unwrap();
        } else {
            backend
                .create_dir(
                    context(),
                    CreateDirRequest {
                        entry,
                        condition: Condition::CreateOnly,
                    },
                )
                .await
                .unwrap();
        }
        let request = task.await.unwrap();
        assert!(request.starts_with(&format!("{method} /share/abc/%E4%B8%AD%E6%96%87%25/ ")));
    }
}

#[tokio::test]
async fn page_truncated_never_advertises_a_cursor() {
    let item = |href: &str| {
        format!(
            r#"<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>1</D:getcontentlength><D:getcontenttype>text/plain</D:getcontenttype><D:getlastmodified>Sun, 01 Nov 2026 00:00:00 GMT</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#
        )
    };
    let xml = format!(
        r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:">{}{}</D:multistatus>"#,
        item("/share/abc/%E4%B8%AD%25.txt"),
        item("/share/abc/b.txt")
    );
    let response = format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
        xml.len()
    );
    let (client, task) = server(response).await;
    let backend = CorePeerDavBackend::new(client, "test".into(), "abc".into(), 1).unwrap();
    let request = ListRequest {
        parent: backend.scope().entry("/"),
        query: None,
        prefix: None,
        cursor: None,
        limit: NonZeroUsize::new(1).unwrap(),
    };
    let page = backend.list(context(), request).await.unwrap();
    assert_eq!(page.entries()[0].id.opaque(), "/中%.txt");
    assert!(page.entries()[0].modified.is_some());
    assert_eq!(
        page.entries()[0].content_type.as_deref(),
        Some("text/plain")
    );
    assert_eq!(page.completeness(), &Completeness::Truncated);
    assert!(page.next().is_none());
    task.await.unwrap();
}

#[tokio::test]
async fn propfind_allocation_is_bounded_and_xml_source_preserved() {
    let (client, task) = server(
        "HTTP/1.1 207 Multi-Status\r\nContent-Length: 9999999\r\nConnection: close\r\n\r\n".into(),
    )
    .await;
    assert_eq!(
        list_entities(&client, "/", Depth::Number(1), Operation::List, 12)
            .await
            .unwrap_err()
            .kind,
        FileErrorKind::BudgetExceeded
    );
    task.await.unwrap();
    let (client, task) = server(
        "HTTP/1.1 207 Multi-Status\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad".into(),
    )
    .await;
    let error = list_entities(&client, "/", Depth::Number(1), Operation::List, 100)
        .await
        .unwrap_err();
    assert!(std::error::Error::source(&error).is_some());
    task.await.unwrap();
}
