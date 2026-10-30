//! End-to-end S3 protocol coverage against an in-process HTTP fixture.
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use peerdav_lib::Vault;
use peerdav_lib::cloud::{
    S3Store,
    config_types::{AddMappingInput, SaveS3Input, ScheduleConfig},
    runtime::CloudRuntime,
    runtime_types::{CreateRepositoryInput, JoinRepositoryInput},
    traits::ObjectStore,
    types::{CloudError, ObjectStream, ProxyMode, PutCondition, S3Config},
};
use peerdav_lib::sync::{manager_types::SyncPhase, traits::NoopSyncEvents};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone, Default)]
struct FixtureState {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    delete_keys: Arc<Mutex<Vec<String>>>,
    ignore_conditions: bool,
}
struct Fixture {
    endpoint: String,
    state: FixtureState,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(ignore_conditions: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fixture");
        let addr = listener.local_addr().expect("fixture address");
        let state = FixtureState {
            objects: Arc::new(Mutex::new(BTreeMap::new())),
            delete_keys: Arc::new(Mutex::new(Vec::new())),
            ignore_conditions,
        };
        let (stop, mut shutdown) = oneshot::channel();
        let serving = state.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let state = serving.clone();
                        connections.spawn(async move {
                            let io = TokioIo::new(socket);
                            let service = service_fn(move |request| handle(request, state.clone()));
                            let _ = http1::Builder::new().serve_connection(io, service).await;
                        });
                    }
                }
            }
            connections.shutdown().await;
        });
        Self {
            endpoint: format!("http://{}", addr),
            state,
            stop: Some(stop),
            task,
        }
    }
    fn config(&self, prefix: &str) -> S3Config {
        S3Config {
            endpoint: self.endpoint.clone(),
            bucket: "fixture".into(),
            region: "us-east-1".into(),
            prefix: prefix.into(),
            path_style: true,
            access_key: "access".into(),
            secret_key: "secret".into(),
            session_token: None,
            allow_insecure_http: true,
            proxy: ProxyMode::Direct,
        }
    }
    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.expect("fixture task");
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

fn response(status: StatusCode, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(body.into()))
        .expect("response")
}
fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", hex::encode(Sha256::digest(bytes)))
}
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn key_from_path(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let bucket = parts.next()?;
    if bucket != "fixture" {
        return None;
    };
    Some(parts.next().unwrap_or_default().to_string())
}

async fn handle(
    request: Request<Incoming>,
    state: FixtureState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path().to_string();
    if request.method() == Method::GET
        && request
            .uri()
            .query()
            .is_some_and(|q| q.contains("list-type=2"))
    {
        let query: BTreeMap<_, _> =
            url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
                .into_owned()
                .collect();
        let prefix = query.get("prefix").cloned().unwrap_or_default();
        let token = query
            .get("continuation-token")
            .and_then(|x| x.parse::<usize>().ok())
            .unwrap_or(0);
        let max = query
            .get("max-keys")
            .and_then(|x| x.parse::<usize>().ok())
            .unwrap_or(1000)
            .min(2);
        let keys: Vec<String> = state
            .objects
            .lock()
            .expect("fixture lock")
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        let page = keys
            .iter()
            .skip(token)
            .take(max)
            .cloned()
            .collect::<Vec<_>>();
        let truncated = token + page.len() < keys.len();
        let next = if truncated {
            format!(
                "<NextContinuationToken>{}</NextContinuationToken>",
                token + page.len()
            )
        } else {
            String::new()
        };
        let contents = page.iter().map(|k| { let data = state.objects.lock().expect("fixture lock").get(k).cloned().unwrap_or_default(); format!("<Contents><Key>{}</Key><LastModified>2024-01-01T00:00:00.000Z</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>", xml_escape(k), etag(&data), data.len()) }).collect::<String>();
        return Ok(response(
            StatusCode::OK,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult><Name>fixture</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{}</MaxKeys><IsTruncated>{}</IsTruncated>{}{}</ListBucketResult>",
                xml_escape(&prefix),
                page.len(),
                max,
                truncated,
                next,
                contents
            ),
        ));
    }
    if request.method() == Method::POST && request.uri().query() == Some("delete") {
        let bytes = request
            .into_body()
            .collect()
            .await
            .expect("delete XML")
            .to_bytes();
        let xml = String::from_utf8(bytes.to_vec()).expect("UTF-8");
        let mut response_xml = String::from("<DeleteResult>");
        for part in xml.split("<Key>").skip(1) {
            let key = part.split("</Key>").next().expect("key");
            state.objects.lock().expect("lock").remove(key);
            state
                .delete_keys
                .lock()
                .expect("delete log")
                .push(key.to_owned());
            response_xml.push_str(&format!("<Deleted><Key>{key}</Key></Deleted>"));
        }
        response_xml.push_str("</DeleteResult>");
        return Ok(response(StatusCode::OK, response_xml));
    }
    let Some(key) = key_from_path(&path) else {
        return Ok(response(StatusCode::NOT_FOUND, "missing bucket"));
    };
    match *request.method() {
        Method::PUT => {
            let none_match = request
                .headers()
                .get("if-none-match")
                .is_some_and(|v| v == "*");
            let if_match = request
                .headers()
                .get("if-match")
                .map(|v| v.to_str().unwrap_or_default().to_owned());
            let body = request
                .into_body()
                .collect()
                .await
                .expect("request body")
                .to_bytes()
                .to_vec();
            let mut objects = state.objects.lock().expect("fixture lock");
            let condition_failed = !state.ignore_conditions
                && ((none_match && objects.contains_key(&key))
                    || if_match.is_some_and(|expected| {
                        objects.get(&key).is_none_or(|b| expected != etag(b))
                    }));
            if condition_failed {
                return Ok(response(
                    StatusCode::PRECONDITION_FAILED,
                    "condition failed",
                ));
            }
            objects.insert(key, body.clone());
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("etag", etag(&body))
                .body(Full::new(Bytes::new()))
                .expect("put response"))
        }
        Method::GET | Method::HEAD => {
            let Some(body) = state
                .objects
                .lock()
                .expect("fixture lock")
                .get(&key)
                .cloned()
            else {
                return Ok(response(StatusCode::NOT_FOUND, "missing"));
            };
            let tag = etag(&body);
            let range = request
                .headers()
                .get("range")
                .and_then(|h| h.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes="))
                .and_then(|v| v.split_once('-'))
                .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
            let (status, payload, content_range) = if let Some((start, end)) = range {
                let end = end.min(body.len().saturating_sub(1));
                let payload = if start <= end {
                    body[start..=end].to_vec()
                } else {
                    Vec::new()
                };
                (
                    StatusCode::PARTIAL_CONTENT,
                    payload,
                    Some(format!("bytes {}-{}/{}", start, end, body.len())),
                )
            } else {
                (StatusCode::OK, body, None)
            };
            let mut builder = Response::builder()
                .status(status)
                .header("etag", tag)
                .header("content-length", payload.len().to_string());
            if let Some(cr) = content_range {
                builder = builder.header("content-range", cr);
            }
            Ok(builder
                .body(if request.method() == Method::HEAD {
                    Full::new(Bytes::new())
                } else {
                    Full::new(Bytes::from(payload))
                })
                .expect("get response"))
        }
        Method::DELETE => {
            state.objects.lock().expect("fixture lock").remove(&key);
            Ok(response(StatusCode::NO_CONTENT, Bytes::new()))
        }
        _ => Ok(response(StatusCode::METHOD_NOT_ALLOWED, "method")),
    }
}

fn body(data: &'static [u8]) -> ObjectStream {
    stream::once(async move { Ok(Bytes::from_static(data)) }).boxed()
}
fn make_store(fixture: &Fixture, prefix: &str) -> S3Store {
    S3Store::new(fixture.config(prefix)).expect("store")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_crud_range_pagination_and_prefix() {
    let fixture = Fixture::start(false).await;
    let store = make_store(&fixture, "tenant/");
    let created = store
        .put("a/one", body(b"0123456789"), PutCondition::CreateOnly)
        .await
        .expect("create");
    assert!(created.etag.is_some());
    let bytes = store
        .get("a/one", Some(2..6))
        .await
        .expect("range")
        .try_collect::<Vec<_>>()
        .await
        .expect("range stream")
        .concat();
    assert_eq!(bytes, b"2345");
    assert_eq!(store.stat("a/one").await.expect("head").size, 10);
    store
        .put("b/two", body(b"two"), PutCondition::Any)
        .await
        .expect("put");
    let listed: Vec<_> = store
        .list("a/")
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|m| m.expect("list item").key)
        .collect();
    assert_eq!(listed, vec!["a/one"]);
    store.delete("a/one").await.expect("delete");
    assert!(store.stat("a/one").await.is_err());
    let other = make_store(&fixture, "other/");
    other
        .put("a/one", body(b"isolated"), PutCondition::CreateOnly)
        .await
        .expect("other put");
    let tenant_list: Vec<_> = store.list("").collect::<Vec<_>>().await;
    assert_eq!(tenant_list.len(), 1);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_create_only_stale_etag_and_concurrent_writers() {
    let fixture = Fixture::start(false).await;
    let store = Arc::new(make_store(&fixture, "cas/"));
    let first = store
        .put("key", body(b"old"), PutCondition::CreateOnly)
        .await
        .expect("initial");
    assert!(matches!(
        store
            .put("key", body(b"again"), PutCondition::CreateOnly)
            .await,
        Err(CloudError::Precondition)
    ));
    let stale = first.etag.expect("etag");
    store
        .put("key", body(b"new"), PutCondition::MatchEtag(stale.clone()))
        .await
        .expect("match");
    assert!(matches!(
        store
            .put("key", body(b"stale"), PutCondition::MatchEtag(stale))
            .await,
        Err(CloudError::Precondition)
    ));
    let current = store
        .stat("key")
        .await
        .expect("current")
        .etag
        .expect("current etag");
    let a = store.clone();
    let b = store.clone();
    let (left, right) = tokio::join!(
        a.put(
            "key",
            body(b"writer-a"),
            PutCondition::MatchEtag(current.clone())
        ),
        b.put("key", body(b"writer-b"), PutCondition::MatchEtag(current))
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert!(
        store
            .verify_capabilities()
            .await
            .expect("probe")
            .conditional_writes_verified
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_list_paginates_fixture_pages() {
    let fixture = Fixture::start(false).await;
    let store = make_store(&fixture, "page/");
    for key in ["a", "b", "c"] {
        store
            .put(key, body(b"x"), PutCondition::Any)
            .await
            .expect("put");
    }
    let listed: Vec<_> = store.list("").collect::<Vec<_>>().await;
    assert_eq!(listed.iter().filter(|item| item.is_ok()).count(), 3);
    assert_eq!(fixture.state.objects.lock().expect("state").len(), 3);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_bad_fixture_can_be_used_for_capability_probe() {
    let fixture = Fixture::start(true).await;
    let store = make_store(&fixture, "probe/");
    assert!(!store.capabilities().conditional_writes_verified);
    assert!(matches!(
        store.verify_capabilities().await,
        Err(CloudError::Unsupported)
    ));
    assert!(!store.capabilities().conditional_writes_verified);
    fixture.shutdown().await;
}

#[test]
fn cloud_endpoint_credentials_and_key_validation_are_fail_closed() {
    fn config(endpoint: &str) -> S3Config {
        S3Config {
            endpoint: endpoint.into(),
            bucket: "fixture".into(),
            region: "us-east-1".into(),
            prefix: "safe/".into(),
            path_style: true,
            access_key: "access".into(),
            secret_key: "secret-marker".into(),
            session_token: Some("token-marker".into()),
            allow_insecure_http: false,
            proxy: ProxyMode::Direct,
        }
    }
    for endpoint in [
        "http://127.0.0.1:1",
        "https://user:secret@example.org",
        "https://example.org/?secret=marker",
        "https://example.org/path",
        "file:///tmp/s3",
    ] {
        assert!(
            S3Store::new(config(endpoint)).is_err(),
            "endpoint {endpoint}"
        );
    }
    let mut cfg = config("http://203.0.113.1");
    cfg.allow_insecure_http = true;
    assert!(S3Store::new(cfg).is_err());
    let mut cfg = config("https://example.org");
    cfg.access_key.clear();
    assert!(S3Store::new(cfg).is_err());
    let cfg = config("https://example.org");
    assert!(!format!("{cfg:?}").contains("secret-marker"));
    assert!(!format!("{cfg:?}").contains("token-marker"));
}

fn runtime_for(dir: &tempfile::TempDir) -> Arc<CloudRuntime> {
    let vault = Arc::new(Vault::open(&dir.path().join("vault.pd")).expect("open vault"));
    vault
        .initialize(
            dir.path().to_str().expect("vault path"),
            "admin",
            "password 123",
        )
        .expect("initialize vault");
    CloudRuntime::new(vault, Arc::new(NoopSyncEvents), dir.path().to_owned())
}

fn runtime_connection(fixture: &Fixture) -> SaveS3Input {
    SaveS3Input {
        id: None,
        name: "fixture".into(),
        endpoint: fixture.endpoint.clone(),
        bucket: "fixture".into(),
        region: "us-east-1".into(),
        prefix: "runtime".into(),
        path_style: true,
        access_key: Some("access".into()),
        secret_key: Some("secret".into()),
        session_token: None,
        clear_credentials: false,
        clear_session_token: false,
        clear_proxy_auth: false,
        allow_insecure_http: true,
        proxy: ProxyMode::Direct,
        schedule: None,
    }
}

async fn wait_completed(runtime: &CloudRuntime, mapping_id: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = runtime
            .snapshot()
            .expect("runtime snapshot")
            .statuses
            .into_iter()
            .find(|status| status.mapping_id == mapping_id)
            .expect("mapping status");
        match status.phase {
            SyncPhase::Completed => return,
            SyncPhase::Failed | SyncPhase::PendingRecovery => {
                panic!("sync did not complete: {:?}", status)
            }
            _ if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            _ => panic!("sync timed out: {:?}", status),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_runtime_two_devices_round_trip_uses_http_fixture() {
    let fixture = Fixture::start(false).await;
    let a_dir = tempfile::tempdir().expect("A data dir");
    let b_dir = tempfile::tempdir().expect("B data dir");
    let a_root_area = tempfile::tempdir().expect("A root area");
    let b_root_area = tempfile::tempdir().expect("B root area");
    let a_root = a_root_area.path().join("root");
    let b_root = b_root_area.path().join("root");
    std::fs::create_dir(&a_root).expect("A root");
    std::fs::create_dir(&b_root).expect("B root");
    let a = runtime_for(&a_dir);
    let b = runtime_for(&b_dir);
    a.resume().expect("A resume");
    b.resume().expect("B resume");
    let a_conn = a
        .save_connection(runtime_connection(&fixture))
        .expect("A connection");
    let repo = a
        .create_repository(CreateRepositoryInput {
            connection_id: a_conn.id.clone(),
            repository_id: None,
            encrypted: true,
            password: "shared password".into(),
            password_confirmation: "shared password".into(),
        })
        .await
        .expect("create repository");
    let b_conn = b
        .save_connection(runtime_connection(&fixture))
        .expect("B connection");
    let joined = b
        .join_repository(JoinRepositoryInput {
            connection_id: b_conn.id.clone(),
            password: "shared password".into(),
        })
        .await
        .expect("join repository");
    assert_eq!(joined.id, repo.id);
    let a_mapping = a
        .save_mapping(
            None,
            AddMappingInput {
                connection_id: a_conn.id,
                repository_id: repo.id.clone(),
                local_root: a_root.clone(),
                logical_path: "docs".into(),
                schedule: ScheduleConfig {
                    enabled: false,
                    interval: 60,
                },
            },
        )
        .await
        .expect("A mapping");
    let b_mapping = b
        .save_mapping(
            None,
            AddMappingInput {
                connection_id: b_conn.id,
                repository_id: joined.id,
                local_root: b_root.clone(),
                logical_path: "docs".into(),
                schedule: ScheduleConfig {
                    enabled: false,
                    interval: 60,
                },
            },
        )
        .await
        .expect("B mapping");

    std::fs::write(a_root.join("note.txt"), b"initial").expect("initial note");
    let preview = a.preview(&a_mapping.id).await.expect("A preview");
    assert!(preview.requires_confirmation);
    let ticket = a
        .manual_sync(&a_mapping.id, &preview.preview_id)
        .await
        .expect("A sync");
    assert!(ticket.run_id > 0);
    wait_completed(&a, &a_mapping.id).await;
    let preview = b.preview(&b_mapping.id).await.expect("B preview");
    assert!(preview.requires_confirmation);
    b.manual_sync(&b_mapping.id, &preview.preview_id)
        .await
        .expect("B sync");
    wait_completed(&b, &b_mapping.id).await;
    assert_eq!(
        std::fs::read(b_root.join("note.txt")).expect("B note"),
        b"initial"
    );

    std::fs::write(b_root.join("note.txt"), b"edited").expect("edit");
    let preview = b.preview(&b_mapping.id).await.expect("B edit preview");
    b.manual_sync(&b_mapping.id, &preview.preview_id)
        .await
        .expect("B edit sync");
    wait_completed(&b, &b_mapping.id).await;
    let preview = a.preview(&a_mapping.id).await.expect("A reverse preview");
    a.manual_sync(&a_mapping.id, &preview.preview_id)
        .await
        .expect("A reverse sync");
    wait_completed(&a, &a_mapping.id).await;
    assert_eq!(
        std::fs::read(a_root.join("note.txt")).expect("A edited note"),
        b"edited"
    );

    a.validate_scheduled_root(&a_mapping)
        .expect("manual sync authorized timer");
    let moved_root = a_root_area.path().join("moved-root");
    std::fs::rename(&a_root, &moved_root).unwrap();
    assert_eq!(
        a.validate_scheduled_root(&a_mapping).unwrap_err().code,
        "CLOUD_SCHEDULE_PAUSED"
    );
    std::fs::rename(&moved_root, &a_root).unwrap();
    assert_eq!(
        a.validate_scheduled_root(&a_mapping).unwrap_err().code,
        "CLOUD_SCHEDULE_PAUSED",
        "returning inode must remain paused"
    );
    let reselected = a
        .save_mapping(
            Some(&a_mapping.id),
            AddMappingInput {
                connection_id: a_mapping.connection_id.clone(),
                repository_id: a_mapping.repository_id.clone(),
                local_root: a_root.clone(),
                logical_path: "docs".into(),
                schedule: ScheduleConfig {
                    enabled: false,
                    interval: 60,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        a.validate_scheduled_root(&reselected).unwrap_err().code,
        "CLOUD_SCHEDULE_PAUSED"
    );
    let preview = a.preview(&a_mapping.id).await.unwrap();
    a.manual_sync(&a_mapping.id, &preview.preview_id)
        .await
        .unwrap();
    wait_completed(&a, &a_mapping.id).await;
    a.validate_scheduled_root(&reselected)
        .expect("explicit reselection and preview must restore eligibility");
    std::fs::remove_file(&a_root.join("note.txt")).expect("delete A note");
    let preview = a.preview(&a_mapping.id).await.expect("A delete preview");
    a.manual_sync(&a_mapping.id, &preview.preview_id)
        .await
        .expect("A delete sync");
    wait_completed(&a, &a_mapping.id).await;
    let preview = b.preview(&b_mapping.id).await.expect("B recovery preview");
    b.manual_sync(&b_mapping.id, &preview.preview_id)
        .await
        .expect("B recovery sync");
    wait_completed(&b, &b_mapping.id).await;
    assert!(
        !b.recovery_records(&b_mapping.id)
            .await
            .expect("recovery records")
            .is_empty()
    );
    assert!(
        !fixture
            .state
            .objects
            .lock()
            .expect("fixture state")
            .is_empty()
    );
    assert!(
        fixture
            .state
            .delete_keys
            .lock()
            .expect("delete keys")
            .iter()
            .all(|key| key.contains(".probe")),
        "association and sync deletes must never remove repository objects"
    );
    let before_delete = fixture.state.delete_keys.lock().unwrap().len();
    a.delete_mapping(&a_mapping.id)
        .await
        .expect("detach mapping");
    a.delete_connection(&a.snapshot().unwrap().connections[0].id)
        .await
        .expect("detach connection");
    assert_eq!(
        fixture.state.delete_keys.lock().unwrap().len(),
        before_delete,
        "detaching must not send remote DELETE"
    );
    a.stop_and_clear().await.expect("stop A");
    b.stop_and_clear().await.expect("stop B");
    fixture.shutdown().await;
}

#[tokio::test]
async fn cloud_runtime_rejects_stale_preview_id() {
    let fixture = Fixture::start(false).await;
    let dir = tempfile::tempdir().expect("data dir");
    let root_area = tempfile::tempdir().expect("root area");
    let root = root_area.path().join("root");
    std::fs::create_dir(&root).expect("root");
    let runtime = runtime_for(&dir);
    runtime.resume().expect("resume");
    let connection = runtime
        .save_connection(runtime_connection(&fixture))
        .expect("connection");
    let repo = runtime
        .create_repository(CreateRepositoryInput {
            connection_id: connection.id.clone(),
            repository_id: None,
            encrypted: true,
            password: "shared password".into(),
            password_confirmation: "shared password".into(),
        })
        .await
        .expect("repository");
    let mapping = runtime
        .save_mapping(
            None,
            AddMappingInput {
                connection_id: connection.id,
                repository_id: repo.id,
                local_root: root,
                logical_path: "docs".into(),
                schedule: ScheduleConfig {
                    enabled: false,
                    interval: 60,
                },
            },
        )
        .await
        .expect("mapping");
    let error = runtime
        .manual_sync(&mapping.id, "invalid-preview-id")
        .await
        .expect_err("stale preview");
    assert_eq!(error.code, "CLOUD_PREVIEW_STALE");
    assert!(
        runtime
            .snapshot()
            .expect("snapshot")
            .capabilities
            .safe_apply,
        "safe apply unsupported"
    );
    runtime.stop_and_clear().await.expect("stop runtime");
    fixture.shutdown().await;
}

#[tokio::test]
async fn cloud_rejects_unsafe_keys_before_network_and_bounds_upload() {
    let fixture = Fixture::start(false).await;
    let store = make_store(&fixture, "safe");
    for key in [
        "",
        "/absolute",
        "../escape",
        "a/../escape",
        "a//b",
        "C:/drive",
        "a\\b",
        "nul\0",
    ] {
        assert!(
            store
                .put(key, body(b"x"), PutCondition::CreateOnly)
                .await
                .is_err()
        );
        assert!(store.stat(key).await.is_err());
    }
    assert_eq!(fixture.state.objects.lock().expect("state").len(), 0);
    let too_large = stream::once(async {
        Ok(Bytes::from(vec![
            0_u8;
            peerdav_lib::cloud::types::MAX_PUT_BYTES
                + 1
        ]))
    })
    .boxed();
    assert!(matches!(
        store
            .put("large", too_large, PutCondition::CreateOnly)
            .await,
        Err(CloudError::PayloadTooLarge)
    ));
    assert_eq!(fixture.state.objects.lock().expect("state").len(), 0);
    fixture.shutdown().await;
}
