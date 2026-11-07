//! End-to-end S3 protocol coverage against an in-process HTTP fixture.

/// Production-adapter acceptance. Credentials remain in process memory; errors
/// expose stable codes only. Cleanup is confined to this run's random prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live writes: explicit PEERDAV_QINIU_LIVE=1 and Cargo-injected credentials required"]
async fn qiniu_production_crud_mirror_append_acceptance() {
    use peerdav_lib::cloud::{ObjectGuard, ObjectService, PhysicalRoot, ProtectedIdentity, StorageIdentityAssociation};
    use peerdav_lib::cloud::runtime_types::{CreateAppendRepositoryInput, SaveAppendMappingInput};
    use peerdav_lib::sync::append_types::AppendMode;
    use std::sync::atomic::AtomicBool;
    assert_eq!(std::env::var("PEERDAV_QINIU_LIVE").ok().as_deref(), Some("1"), "live authorization required");
    let access = std::env::var("PEERDAV_QINIU_ACCESS_KEY").expect("Cargo-injected access key missing");
    let secret = std::env::var("PEERDAV_QINIU_SECRET_KEY").expect("Cargo-injected secret key missing");
    let prefix = format!("peerdav-contract/{}", hex::encode(rand::random::<[u8; 16]>()));
    let config = S3Config {
        endpoint: "https://peerdav.s3.cn-east-1.qiniucs.com".into(), bucket: "peerdav".into(),
        region: "cn-east-1".into(), prefix: prefix.clone(), path_style: false,
        access_key: access.clone(), secret_key: secret.clone(), session_token: None,
        buckets: vec![], allow_insecure_http: false, proxy: ProxyMode::Direct,
    };
    let raw = Arc::new(S3Store::new(config).map_err(|_| "store configuration").unwrap());
    let data = tempfile::tempdir().unwrap();
    let runtime = runtime_for(&data);
    runtime.resume().map_err(|e| e.code).unwrap();
    let connection = runtime.save_connection(SaveS3Input {
        id: None, name: "isolated live acceptance".into(), endpoint: "https://peerdav.s3.cn-east-1.qiniucs.com".into(),
        bucket: "peerdav".into(), buckets: vec!["peerdav".into()], region: "cn-east-1".into(), prefix: prefix.clone(),
        path_style: false, access_key: Some(access), secret_key: Some(secret), session_token: None,
        clear_credentials: false, clear_session_token: false, clear_proxy_auth: false,
        allow_insecure_http: false, proxy: ProxyMode::Direct, schedule: None,
    }).map_err(|e| e.code).unwrap();
    eprintln!("qiniu production prefix={prefix}/");
    let work = async {
        let identity = ProtectedIdentity { endpoint: "https://peerdav.s3.cn-east-1.qiniucs.com".into(), bucket: "peerdav".into(), prefix: prefix.clone(), alias: String::new() };
        let guard = ObjectGuard::open(data.path().join("live-guard"), &[9; 32]).map_err(|_| "guard")?;
        guard.register_storage_identity(&identity, StorageIdentityAssociation::New).map_err(|_| "identity")?;
        let bridge = Arc::new(peerdav_protocol_s3::CoreS3ReadBridge(peerdav_protocol_s3::CoreS3Backend::new(raw.clone(), 1).map_err(|_| "bridge")?));
        let downloads = tempfile::tempdir().map_err(|_| "downloads")?;
        let service = ObjectService::try_new(bridge, guard, identity, "live").map_err(|_| "service")?
            .with_physical_root(PhysicalRoot::open(downloads.path()).map_err(|_| "download root")?);
        service.discover("").await.map_err(|_| "discovery")?;
        let keys = ["普通/中文 100%.txt", "普通/literal%2F.txt", "普通/space file.txt", "普通邻居/keep.txt"];
        for key in keys {
            service.upload_stream_with_control(key, body(b"first"), Arc::new(AtomicBool::new(false)), None).await.map_err(|_| "upload")?;
        }
        let old = raw.stat(keys[0]).await.map_err(|_| "stat")?;
        if service.upload_stream_with_control(keys[0], body(b"bad"), Arc::new(AtomicBool::new(false)), None).await.is_ok() { return Err("overwrite guard"); }
        service.upload_stream_with_control(keys[0], body(b"replacement"), Arc::new(AtomicBool::new(false)), old.etag).await.map_err(|_| "overwrite")?;
        service.download_to(keys[0], "download.txt", None, false, None).await.map_err(|_| "download")?;
        if std::fs::read(downloads.path().join("download.txt")).map_err(|_| "read download")? != b"replacement" { return Err("download bytes"); }
        let expiry = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| "clock")?.as_secs() + 120;
        let plan = service.delete_plan("普通/", expiry).await.map_err(|_| "delete plan")?;
        if plan.entries.len() != 3 { return Err("directory boundary"); }
        let deleted = service.execute_delete(&plan, true).await.map_err(|_| "delete")?;
        if deleted.iter().any(Result::is_err) || raw.stat(keys[3]).await.is_err() { return Err("delete sibling"); }
        eprintln!("qiniu production stage=crud passed");
        let local = tempfile::tempdir().map_err(|_| "mirror local")?;
        std::fs::write(local.path().join("正式 100%.txt"), b"mirror").map_err(|_| "write mirror")?;
        std::fs::write(local.path().join(".notes.swp"), b"temporary").map_err(|_| "write temporary")?;
        raw.put("_peerdav_mirror/docs/remote.part", body(b"partial"), PutCondition::Any).await.map_err(|_| "seed temporary")?;
        let mapping = runtime.save_mirror_mapping(serde_json::from_value(serde_json::json!({
            "id": null, "connectionId": connection.id, "bucket": "peerdav", "localRoot": local.path(),
            "cloudPrefix": "docs", "schedule": { "enabled": false, "interval": 900 }
        })).map_err(|_| "mirror input")?).map_err(|_| "mirror mapping")?;
        for _ in 0..2 {
            let result = match runtime.mirror_sync(&mapping.id, false, None).await {
                Ok(result) => result,
                Err(error) if error.code == "MIRROR_CONFIRM_MERGE" => {
                    let plan = error.context.as_ref().and_then(|c| c["planId"].as_str()).ok_or("mirror confirmation plan")?;
                    runtime.mirror_sync(&mapping.id, false, Some(plan.into())).await.map_err(|_| "mirror confirmed sync")?
                }
                Err(error) => { eprintln!("qiniu mirror error code={}", error.code); return Err("mirror sync"); }
            };
            if !result.failures.is_empty() || local.path().join("remote.part").exists() { return Err("mirror exclusion"); }
        }
        if raw.stat("_peerdav_mirror/docs/.notes.swp").await.is_ok() { return Err("mirror temporary upload"); }
        eprintln!("qiniu production stage=mirror passed");
        let root = tempfile::tempdir().map_err(|_| "append root")?;
        std::fs::write(root.path().join("正式.txt"), b"encrypted final").map_err(|_| "write append")?;
        std::fs::write(root.path().join("video.part"), b"temporary").map_err(|_| "write append temporary")?;
        let repo = runtime.create_sync_drive(CreateAppendRepositoryInput {
            connection_id: connection.id.clone(), bucket: Some("peerdav".into()), mode: AppendMode::Encrypted,
            password: "acceptance-only-password".into(), password_confirmation: "acceptance-only-password".into(),
        }).await.map_err(|_| "create append")?;
        let mapping = runtime.save_append_mapping(SaveAppendMappingInput {
            mapping_id: None, selected_root: "docs".into(), mapping: AddMappingInput {
                connection_id: connection.id.clone(), repository_id: repo.binding.repo_id, local_root: root.path().into(), logical_path: "docs".into(), schedule: ScheduleConfig::default(),
            },
        }).await.map_err(|_| "append mapping")?;
        let preview = runtime.preview(&mapping.id).await.map_err(|_| "append preview")?;
        if preview.summary.uploads != 1 { return Err("append temporary preview"); }
        runtime.manual_sync(&mapping.id, &preview.preview_id).await.map_err(|_| "append start")?;
        // Avoid panic paths so every remote failure reaches cleanup.
        let mut completed = false;
        for _ in 0..600 {
            let phase = runtime.snapshot().map_err(|_| "snapshot")?.statuses.into_iter().find(|s| s.mapping_id == mapping.id).map(|s| s.phase);
            match phase {
                Some(SyncPhase::Completed) => { completed = true; break; },
                Some(SyncPhase::Failed | SyncPhase::PendingRecovery) => return Err("append worker"),
                _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        if !completed { return Err("append timeout"); }
        let preview = runtime.preview(&mapping.id).await.map_err(|_| "append steady preview")?;
        if preview.summary.uploads != 0 || preview.summary.deletes != 0 { return Err("append steady"); }
        if runtime.object_delete_plan(&connection.id, "_peerdav_mirror/").await.is_ok() { return Err("mirror protection"); }
        eprintln!("qiniu production stage=append passed");
        Ok::<(), &'static str>(())
    };
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), work).await;
    let stopped = runtime.stop_and_clear().await.is_ok();
    // List only this unpredictable run-owned prefix; collect before deleting so
    // pagination cannot skip objects as its underlying keyset changes.
    let mut token = None;
    let mut keys = Vec::new();
    let mut cleanup_ok = true;
    loop {
        match raw.list_page("", token.as_deref()).await {
            Ok(page) => { keys.extend(page.objects.into_iter().map(|m| m.key)); token = page.next_token; if token.is_none() { break; } }
            Err(_) => { cleanup_ok = false; break; }
        }
        if keys.len() > 1000 { cleanup_ok = false; break; }
    }
    let count = keys.len();
    for key in keys { if raw.delete(&key).await.is_err() { cleanup_ok = false; } }
    cleanup_ok &= raw.list_page("", None).await.is_ok_and(|p| p.objects.is_empty() && p.next_token.is_none());
    eprintln!("qiniu production cleanup objects={count} confirmed_empty={cleanup_ok}");
    assert!(stopped && cleanup_ok, "cleanup or worker stop failed");
    assert!(matches!(outcome, Ok(Ok(()))), "production acceptance failed stage={:?}", outcome);
}
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
    requests: Arc<Mutex<Vec<(Method, String)>>>,
    conditional_puts: Arc<Mutex<Vec<String>>>,
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
            requests: Arc::new(Mutex::new(Vec::new())),
            conditional_puts: Arc::new(Mutex::new(Vec::new())),
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
            buckets: Vec::new(),
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
    // HTTP paths are encoded once; storage keys (including literal %2F) are raw.
    Some(percent_encoding::percent_decode_str(parts.next().unwrap_or_default())
        .decode_utf8().ok()?.into_owned())
}

async fn handle(
    request: Request<Incoming>,
    state: FixtureState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path().to_string();
    state
        .requests
        .lock()
        .expect("request log")
        .push((request.method().clone(), path.clone()));
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
            if request.headers().contains_key("if-match")
                || request.headers().contains_key("if-none-match")
            {
                state
                    .conditional_puts
                    .lock()
                    .expect("conditional PUT log")
                    .push(path);
            }
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
            let mut objects = state.objects.lock().expect("fixture lock");
            if !state.ignore_conditions && request.headers().get("if-match").is_some_and(|expected| {
                objects.get(&key).is_none_or(|body| expected.to_str().ok() != Some(etag(body).as_str()))
            }) {
                return Ok(response(StatusCode::PRECONDITION_FAILED, "condition failed"));
            }
            objects.remove(&key);
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
            buckets: Vec::new(),
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

#[tokio::test]
async fn file_browser_backend_cloud_contract() {
    use peerdav_lib::file_manager::{cloud_list, cloud_preview};

    let fixture = Fixture::start(false).await;
    let data = tempfile::tempdir().expect("data dir");
    let runtime = runtime_for(&data);
    assert_eq!(
        cloud_list(&runtime, "missing", "", None).await.unwrap_err().code,
        "CLOUD_GATE_LOCKED"
    );
    assert_eq!(
        cloud_preview(&runtime, "missing", "notes.txt").await.unwrap_err().code,
        "CLOUD_GATE_LOCKED"
    );
    runtime.resume().expect("resume");
    let connection = runtime
        .save_connection(runtime_connection(&fixture))
        .expect("connection");
    fixture
        .state
        .objects
        .lock()
        .expect("objects")
        .insert("runtime/docs/notes.txt".into(), b"adapter text\n".to_vec());
    let browse = |prefix, cursor| cloud_list(&runtime, &connection.id, prefix, cursor);
    let preview = |key| cloud_preview(&runtime, &connection.id, key);
    let page = browse("docs/", None)
        .await
        .expect("adapter browse");
    assert_eq!(page.prefix, "docs/");
    assert!(page.complete);
    assert!(page.next.is_none());
    assert!(!page.prefix_protected);
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].key, "docs/notes.txt");
    assert!(!page.objects[0].protected);
    {
        let mut objects = fixture.state.objects.lock().expect("objects");
        for key in ["a.txt", "b.txt", "c.txt"] {
            objects.insert(format!("runtime/pages/{key}"), b"page".to_vec());
        }
    }
    let first = browse("pages/", None).await.expect("first page");
    assert!(!first.complete);
    assert_eq!(first.objects.len(), 2);
    let token = first.next.as_deref().expect("continuation");
    let second = browse("pages/", Some(token))
        .await
        .expect("second page");
    assert!(second.complete);
    assert!(second.next.is_none());
    assert_eq!(second.objects[0].key, "pages/c.txt");
    let retry = browse("pages/", Some(token))
        .await
        .expect("same-token retry");
    assert_eq!(
        serde_json::to_value(retry).expect("retry page"),
        serde_json::to_value(second).expect("second page")
    );
    let payload = preview("docs/notes.txt")
        .await
        .expect("adapter preview");
    assert_eq!(payload.file_name, "notes.txt");
    assert_eq!(payload.encoding, "utf8");
    assert_eq!(payload.data, "adapter text\n");
    let direct = runtime
        .object_list(&connection.id, "unseen/", Some("forged"))
        .await
        .unwrap_err();
    let adapted = browse("unseen/", Some("forged"))
        .await
        .unwrap_err();
    assert_eq!(
        serde_json::to_value(adapted).expect("adapted error"),
        serde_json::to_value(direct).expect("direct error")
    );
    let direct = runtime
        .object_preview(&connection.id, "missing.txt")
        .await
        .unwrap_err();
    let adapted = preview("missing.txt").await.unwrap_err();
    assert_eq!(
        serde_json::to_value(adapted).expect("adapted error"),
        serde_json::to_value(direct).expect("direct error")
    );
    assert!(
        fixture
            .state
            .requests
            .lock()
            .expect("requests")
            .iter()
            .all(|(method, _)| *method == Method::GET || *method == Method::HEAD)
    );
    runtime.stop_and_clear().await.expect("stop runtime");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_directory_delete_uses_production_bridge_and_preserves_siblings() {
    for ignore_conditions in [false, true] {
        let fixture = Fixture::start(ignore_conditions).await;
        let data = tempfile::tempdir().expect("data");
        let vault = Arc::new(Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("initialize");
        let config = peerdav_lib::cloud::config::CloudConfigStore::new(vault.clone());
        let guard = peerdav_lib::cloud::ObjectGuard::open(data.path().join("cloud-object-guard.sqlite"),
            config.get_or_create_index_key().expect("key").as_ref()).expect("guard");
        guard.register_storage_identity(&peerdav_lib::cloud::ProtectedIdentity {
            endpoint: fixture.endpoint.clone(), bucket: "fixture".into(), prefix: "runtime/".into(), alias: String::new(),
        }, peerdav_lib::cloud::StorageIdentityAssociation::New).expect("confirm identity");
        drop(guard);
        let runtime = CloudRuntime::new(vault, Arc::new(NoopSyncEvents), data.path().to_owned());
        runtime.resume().expect("resume");
        let connection = runtime.save_connection(runtime_connection(&fixture)).expect("connection");
        {
            let mut objects = fixture.state.objects.lock().expect("objects");
            for index in 0..8 {
                objects.insert(format!("runtime/图片/{index}.png"), b"image".to_vec());
            }
            objects.insert("runtime/图片-backup/keep.png".into(), b"keep".to_vec());
        }
        runtime.object_discover(&connection.id, "").await.expect("discover protection roots");
        let plan = runtime.object_delete_plan(&connection.id, "图片/").await.expect("plan");
        assert_eq!(plan.object_count, 8);
        let result = runtime.object_delete_execute(&connection.id, &plan.plan_id, true).await.expect("execute");
        assert_eq!(result.deleted.len(), 8);
        assert!(result.failed.is_empty());
        {
            let objects = fixture.state.objects.lock().expect("objects");
            assert!(objects.contains_key("runtime/图片-backup/keep.png"));
            for index in 0..8 {
                assert!(!objects.contains_key(&format!("runtime/图片/{index}.png")));
            }
        }
        runtime.stop_and_clear().await.expect("stop");
        fixture.shutdown().await;
    }
}

fn runtime_connection(fixture: &Fixture) -> SaveS3Input {
    SaveS3Input {
        id: None,
        name: "fixture".into(),
        endpoint: fixture.endpoint.clone(),
        bucket: "fixture".into(),
        buckets: Vec::new(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mirror_runtime_works_without_conditional_writes_and_recovers_baseline() {
    let fixture = Fixture::start(true).await;
    let data = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    std::fs::write(local.path().join("中文 100%.txt"), b"local first").unwrap();
    let runtime = runtime_for(&data);
    runtime.resume().unwrap();
    let mut input = runtime_connection(&fixture);
    input.bucket = "other".into();
    input.buckets = vec!["other".into(), "fixture".into()];
    let connection = runtime.save_connection(input).unwrap();
    fixture.state.objects.lock().unwrap().insert("runtime/_peerdav_mirror/docs/remote.txt".into(), b"remote first".to_vec());
    let folders = runtime.mirror_folders(&connection.id, Some("fixture"), "docs", None).await.unwrap();
    assert_eq!(folders.objects.len(), 1);
    assert_eq!(runtime.mirror_folders(&connection.id, Some("unknown"), "", None).await.unwrap_err().code, "CLOUD_CONFIG_INVALID");
    assert!(runtime.mirror_folders(&connection.id, Some("fixture"), "../escape", None).await.is_err());
    let mapping = runtime.save_mirror_mapping(serde_json::from_value(serde_json::json!({
        "id": null, "connectionId": connection.id, "bucket": "fixture", "localRoot": local.path(),
        "cloudPrefix": "docs", "schedule": { "enabled": false, "interval": 900 }
    })).unwrap()).unwrap();
    let first = runtime.mirror_sync(&mapping.id, false, None).await.unwrap_err();
    assert_eq!(first.code, "MIRROR_CONFIRM_MERGE");
    let plan = first.context.unwrap()["planId"].as_str().unwrap().to_owned();
    let merged = runtime.mirror_sync(&mapping.id, false, Some(plan)).await.unwrap();
    assert!(merged.failures.is_empty());
    assert_eq!((merged.uploaded, merged.downloaded), (1, 1));
    assert_eq!(std::fs::read(local.path().join("remote.txt")).unwrap(), b"remote first");
    std::fs::write(local.path().join("中文 100%.txt"), b"local replacement longer").unwrap();
    let replaced = runtime.mirror_sync(&mapping.id, false, None).await.unwrap();
    assert!(replaced.failures.is_empty());
    assert_eq!(replaced.uploaded, 1);
    runtime.stop_and_clear().await.unwrap();
    runtime.resume().unwrap();
    let steady = runtime.mirror_sync(&mapping.id, false, None).await.unwrap();
    assert!(steady.failures.is_empty());
    assert_eq!((steady.uploaded, steady.downloaded), (0, 0));
    std::fs::remove_file(local.path().join("中文 100%.txt")).unwrap();
    let deletion = runtime.mirror_sync(&mapping.id, false, None).await;
    let deleted = match deletion {
        Ok(result) => result,
        Err(error) => {
            assert_eq!(error.code, "MIRROR_MASS_DELETE");
            let plan = error.context.unwrap()["planId"].as_str().unwrap().to_owned();
            runtime.mirror_sync(&mapping.id, true, Some(plan)).await.unwrap()
        }
    };
    assert!(deleted.failures.is_empty());
    assert_eq!(deleted.deleted_cloud, 1);
    assert!(!fixture.state.objects.lock().unwrap().contains_key("runtime/_peerdav_mirror/docs/中文 100%.txt"));
    assert!(fixture.state.conditional_puts.lock().unwrap().is_empty());
    assert!(fixture.state.requests.lock().unwrap().iter().all(|(_, key)| !key.contains(".probe")));
    runtime.stop_and_clear().await.unwrap();
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_drive_defaults_to_append_preserves_cas_and_deletes_only_local_associations() {
    use peerdav_lib::cloud::config_types::StorageProtocol;
    use peerdav_lib::cloud::runtime_types::{CreateAppendRepositoryInput, SaveAppendMappingInput};
    use peerdav_lib::sync::append_types::APPEND_ANCHORS;

    let fixture = Fixture::start(false).await;
    let data = tempfile::tempdir().expect("data dir");
    let runtime = runtime_for(&data);
    runtime.resume().expect("resume");
    let connection = runtime
        .save_connection(runtime_connection(&fixture))
        .expect("connection");
    let cas = runtime
        .create_repository(CreateRepositoryInput {
            connection_id: connection.id.clone(),
            repository_id: None,
            encrypted: true,
            password: "CAS password".into(),
            password_confirmation: "CAS password".into(),
        })
        .await
        .expect("legacy CAS creation remains supported");
    let cas_root = tempfile::tempdir().expect("CAS root");
    let cas_mapping = runtime
        .save_mapping(
            None,
            AddMappingInput {
                connection_id: connection.id.clone(),
                repository_id: cas.id.clone(),
                local_root: cas_root.path().to_owned(),
                logical_path: "legacy".into(),
                schedule: ScheduleConfig::default(),
            },
        )
        .await
        .expect("CAS mapping");
    assert_eq!(cas_mapping.protocol, StorageProtocol::CasV1);
    assert!(
        !fixture
            .state
            .conditional_puts
            .lock()
            .expect("log")
            .is_empty()
    );
    let cas_objects = fixture.state.objects.lock().expect("objects").clone();
    fixture.state.requests.lock().expect("requests").clear();
    fixture.state.conditional_puts.lock().expect("log").clear();

    // This is exactly the camelCase input object accepted by the Tauri command.
    let input = |password: &str, confirmation: &str| {
        serde_json::from_value::<CreateAppendRepositoryInput>(serde_json::json!({
            "connectionId": connection.id,
            "mode": "encrypted",
            "password": password,
            "passwordConfirmation": confirmation,
        }))
        .expect("sync-drive input contract")
    };
    for (password, confirmation) in [
        ("", ""), ("Ab1!x", "Ab1!x"), ("😀😀😀", "😀😀😀"),
        ("密码不足六", "密码不足六"), ("password", "different"),
    ] {
        let error = runtime
            .create_sync_drive(input(password, confirmation))
            .await
            .err()
            .expect("invalid passwords rejected");
        assert_eq!(error.code, "CLOUD_APPEND_PASSWORD");
    }
    assert!(fixture.state.requests.lock().expect("requests").is_empty());
    let append = runtime
        .create_sync_drive(input("Ab1!xy", "Ab1!xy"))
        .await
        .expect("default Append creation");
    let requests = fixture.state.requests.lock().expect("requests").clone();
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _)| *method == Method::PUT)
            .count(),
        1
    );
    assert!(
        requests
            .iter()
            .all(|(_, path)| path.contains(APPEND_ANCHORS)),
        "{requests:?}"
    );
    assert!(
        fixture
            .state
            .conditional_puts
            .lock()
            .expect("log")
            .is_empty()
    );
    for (key, bytes) in &cas_objects {
        assert_eq!(
            fixture.state.objects.lock().expect("objects").get(key),
            Some(bytes)
        );
    }
    let append_root = tempfile::tempdir().expect("Append root");
    let append_mapping = runtime
        .save_append_mapping(SaveAppendMappingInput {
            mapping_id: None,
            selected_root: "docs".into(),
            mapping: AddMappingInput {
                connection_id: connection.id.clone(),
                repository_id: append.binding.repo_id.clone(),
                local_root: append_root.path().to_owned(),
                logical_path: "docs".into(),
                schedule: ScheduleConfig::default(),
            },
        })
        .await
        .expect("Append mapping");
    assert_eq!(append_mapping.protocol, StorageProtocol::AppendV2);
    let snapshot = runtime.snapshot().expect("snapshot");
    assert_eq!(snapshot.repositories.len(), 1);
    assert_eq!(snapshot.repositories[0].id, cas.id);
    assert_eq!(
        snapshot
            .mappings
            .iter()
            .find(|m| m.id == cas_mapping.id)
            .expect("CAS mapping preserved")
            .protocol,
        StorageProtocol::CasV1
    );
    assert_eq!(snapshot.append_repositories.len(), 1);

    let remote_before_delete = fixture.state.objects.lock().expect("objects").clone();
    fixture.state.requests.lock().expect("requests").clear();
    runtime
        .delete_connection(&connection.id)
        .await
        .expect("local cascade");
    let snapshot = runtime.snapshot().expect("snapshot after deletion");
    assert!(snapshot.connections.is_empty());
    assert!(snapshot.repositories.is_empty());
    assert!(snapshot.append_repositories.is_empty());
    assert!(snapshot.mappings.is_empty());
    assert!(fixture.state.requests.lock().expect("requests").is_empty());
    assert_eq!(
        *fixture.state.objects.lock().expect("objects"),
        remote_before_delete
    );
    runtime.stop_and_clear().await.expect("stop runtime");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_drive_create_join_sync_both_modes_without_cas_probe_or_control_head() {
    use peerdav_lib::cloud::runtime_types::{
        CreateAppendRepositoryInput, JoinAppendRepositoryInput, SaveAppendMappingInput,
    };
    use peerdav_lib::sync::append_types::{APPEND_ROOT, AppendMode};
    for mode in [AppendMode::Encrypted, AppendMode::AuthenticatedPlain] {
        let fixture = Fixture::start(true).await; // Deliberately cannot pass CAS probing.
        let data = tempfile::tempdir().unwrap();
        let runtime = runtime_for(&data);
        runtime.resume().unwrap();
        let connection = runtime
            .save_connection(runtime_connection(&fixture))
            .unwrap();
        let created = runtime
            .create_sync_drive(CreateAppendRepositoryInput {
                bucket: None,
                connection_id: connection.id.clone(),
                mode: mode.clone(),
                password: "append password".into(),
                password_confirmation: "append password".into(),
            })
            .await
            .unwrap();
        let candidates = runtime
            .discover_append_repositories(&connection.id)
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].fingerprint,
            created.binding.anchor_fingerprint
        );
        let join = |password: &str, accepted: bool, fingerprint: Option<String>| {
            JoinAppendRepositoryInput {
                bucket: None,
                connection_id: connection.id.clone(),
                anchor_key: created.binding.anchor_key.clone(),
                anchor_fingerprint: created.binding.anchor_fingerprint.clone(),
                repository_id: created.binding.repo_id.clone(),
                mode: mode.clone(),
                accept_tofu: accepted,
                trusted_fingerprint: fingerprint,
                password: password.into(),
            }
        };
        let before = fixture.state.objects.lock().unwrap().clone();
        assert!(
            runtime
                .join_append_repository(join("append password", false, None))
                .await
                .is_err()
        );
        assert!(
            runtime
                .join_append_repository(join("wrong", true, None))
                .await
                .is_err()
        );
        assert!(
            runtime
                .join_append_repository(join("append password", true, Some("0".repeat(64))))
                .await
                .is_err()
        );
        assert_eq!(*fixture.state.objects.lock().unwrap(), before);
        let joined = runtime
            .join_append_repository(join("append password", true, None))
            .await
            .unwrap();
        assert!(joined.binding.selected_root.is_none());
        let local = tempfile::tempdir().unwrap();
        std::fs::write(local.path().join("file.txt"), b"append runtime payload").unwrap();
        let mapping = runtime
            .save_append_mapping(SaveAppendMappingInput {
                mapping_id: None,
                selected_root: "docs".into(),
                mapping: AddMappingInput {
                    connection_id: connection.id.clone(),
                    repository_id: created.binding.repo_id.clone(),
                    local_root: local.path().to_owned(),
                    logical_path: "docs".into(),
                    schedule: ScheduleConfig::default(),
                },
            })
            .await
            .unwrap();
        let before_sync = runtime.local_sync_entries(&mapping.id).await.unwrap();
        assert!(before_sync.iter().any(|e| e.path == "file.txt" && !e.synced));
        let preview = runtime.preview(&mapping.id).await.unwrap();
        assert!(preview.counts.uploads >= 1);
        runtime
            .manual_sync(&mapping.id, &preview.preview_id)
            .await
            .unwrap();
        wait_completed(&runtime, &mapping.id).await;
        let after_sync = runtime.local_sync_entries(&mapping.id).await.unwrap();
        assert!(after_sync.iter().any(|e| e.path == "file.txt" && e.synced));
        std::fs::create_dir(local.path().join("pending")).unwrap();
        std::fs::write(local.path().join("pending/new.txt"), b"new").unwrap();
        let pending = runtime.local_sync_entries(&mapping.id).await.unwrap();
        assert!(pending.iter().filter(|e| e.path.starts_with("pending")).all(|e| !e.synced));
        std::fs::remove_file(local.path().join("pending/new.txt")).unwrap();
        std::fs::remove_dir(local.path().join("pending")).unwrap();
        let second = runtime.preview(&mapping.id).await.unwrap();
        assert_eq!(second.counts.uploads, 0);
        assert_eq!(second.counts.downloads, 0);
        let joined = runtime
            .join_append_repository(join("append password", true, None))
            .await
            .unwrap();
        assert_eq!(joined.binding.root_candidates, vec!["docs"]);
        let reader_data = tempfile::tempdir().unwrap();
        let reader = runtime_for(&reader_data);
        reader.resume().unwrap();
        let reader_connection = reader
            .save_connection(runtime_connection(&fixture))
            .unwrap();
        let mut reader_join = join("append password", true, None);
        reader_join.connection_id = reader_connection.id.clone();
        reader.join_append_repository(reader_join).await.unwrap();
        let destination = tempfile::tempdir().unwrap();
        let reader_mapping = reader
            .save_append_mapping(SaveAppendMappingInput {
                mapping_id: None,
                selected_root: "docs".into(),
                mapping: AddMappingInput {
                    connection_id: reader_connection.id,
                    repository_id: created.binding.repo_id.clone(),
                    local_root: destination.path().to_owned(),
                    logical_path: "docs".into(),
                    schedule: ScheduleConfig::default(),
                },
            })
            .await
            .unwrap();
        // Browsing an existing drive and planning sync must not read payloads,
        // even when the remote blocks are temporarily unavailable.
        let blocks: Vec<_> = fixture.state.objects.lock().unwrap().iter()
            .filter(|(key, _)| key.contains("/blocks/"))
            .map(|(key, bytes)| (key.clone(), bytes.clone())).collect();
        assert!(!blocks.is_empty());
        for (key, _) in &blocks { fixture.state.objects.lock().unwrap().remove(key); }
        fixture.state.requests.lock().unwrap().clear();
        let preview = reader.preview(&reader_mapping.id).await.unwrap();
        assert_eq!(preview.counts.downloads, 1);
        assert!(fixture.state.requests.lock().unwrap().iter().all(|(_, path)| !path.contains("/blocks/")));
        for (key, bytes) in blocks { fixture.state.objects.lock().unwrap().insert(key, bytes); }
        fixture.state.requests.lock().unwrap().clear();
        reader
            .manual_sync(&reader_mapping.id, &preview.preview_id)
            .await
            .unwrap();
        wait_completed(&reader, &reader_mapping.id).await;
        assert!(fixture.state.requests.lock().unwrap().iter().any(|(method, path)| *method == Method::GET && path.contains("/blocks/")));
        assert_eq!(
            std::fs::read(destination.path().join("file.txt")).unwrap(),
            b"append runtime payload"
        );
        let initial_records = reader.recovery_records(&reader_mapping.id).await.unwrap();
        std::fs::write(local.path().join("file.txt"), b"updated payload").unwrap();
        let preview = runtime.preview(&mapping.id).await.unwrap();
        runtime
            .manual_sync(&mapping.id, &preview.preview_id)
            .await
            .unwrap();
        wait_completed(&runtime, &mapping.id).await;
        let preview = reader.preview(&reader_mapping.id).await.unwrap();
        reader
            .manual_sync(&reader_mapping.id, &preview.preview_id)
            .await
            .unwrap();
        wait_completed(&reader, &reader_mapping.id).await;
        let history = reader.recovery_records(&reader_mapping.id).await.unwrap();
        let record = history
            .iter()
            .find(|record| {
                record.relative_path == "file.txt"
                    && record.phase == "installed"
                    && !initial_records.iter().any(|old| old.id == record.id)
            })
            .unwrap();
        reader
            .restore(&reader_mapping.id, &record.id, "restored.txt".into())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(destination.path().join("restored.txt")).unwrap(),
            b"append runtime payload"
        );
        assert!(
            reader
                .restore(&reader_mapping.id, &record.id, "file.txt".into())
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(destination.path().join("file.txt")).unwrap(),
            b"updated payload"
        );
        reader.stop_and_clear().await.unwrap();
        let mut missing = join("append password", true, None);
        missing.anchor_key = format!("v2/anchors/{}-{}", "a".repeat(32), "b".repeat(64));
        missing.repository_id = "a".repeat(32);
        missing.anchor_fingerprint = "b".repeat(64);
        let before_missing = fixture.state.objects.lock().unwrap().clone();
        assert!(runtime.join_append_repository(missing).await.is_err());
        assert_eq!(*fixture.state.objects.lock().unwrap(), before_missing);
        assert!(runtime.snapshot().unwrap().append_repositories.len() == 1);
        assert!(
            runtime
                .recovery_records(&mapping.id)
                .await
                .unwrap()
                .iter()
                .all(|r| !r.id.is_empty())
        );
        let requests = fixture.state.requests.lock().unwrap().clone();
        assert!(
            !requests
                .iter()
                .any(|(_, path)| path.contains("probe") || path.ends_with("/HEAD")),
            "{requests:?}"
        );
        assert!(
            fixture
                .state
                .objects
                .lock()
                .unwrap()
                .keys()
                .all(|key| key.starts_with(&format!("runtime/{APPEND_ROOT}/")))
        );
        runtime.stop_and_clear().await.unwrap();
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_drive_storage_stays_bound_after_connection_changes() {
    use peerdav_lib::cloud::runtime_types::{CreateAppendRepositoryInput, SaveAppendMappingInput};
    use peerdav_lib::sync::append_types::{APPEND_ROOT, AppendMode};
    let fixture = Fixture::start(true).await;
    let data = tempfile::tempdir().expect("data");
    let runtime = runtime_for(&data);
    runtime.resume().expect("resume");
    let mut input = runtime_connection(&fixture);
    input.bucket = "other".into();
    input.buckets = vec!["other".into(), "fixture".into()];
    let connection = runtime.save_connection(input).expect("connection");
    let created = runtime.create_sync_drive(CreateAppendRepositoryInput {
        connection_id: connection.id.clone(), bucket: Some("fixture".into()),
        mode: AppendMode::Encrypted, password: "bound password".into(),
        password_confirmation: "bound password".into(),
    }).await.expect("create in selected bucket");
    let storage = created.binding.storage.as_ref().expect("durable binding");
    assert_eq!(storage.bucket, "fixture");
    assert_eq!(storage.prefix, "runtime");
    assert_eq!(runtime.discover_append_in_bucket(&connection.id, Some("fixture")).await.expect("discover").len(), 1);
    assert_eq!(runtime.discover_append_in_bucket(&connection.id, Some("unknown")).await.expect_err("unconfigured bucket").code, "CLOUD_CONFIG_INVALID");
    let local = tempfile::tempdir().expect("root");
    std::fs::create_dir(local.path().join("docs")).expect("folder");
    let payload = b"private payload must never be stored as plaintext";
    std::fs::write(local.path().join("docs/private.txt"), payload).expect("file");
    let mapping = runtime.save_append_mapping(SaveAppendMappingInput {
        mapping_id: None, selected_root: "root".into(), mapping: AddMappingInput {
            connection_id: connection.id.clone(), repository_id: created.binding.repo_id.clone(),
            local_root: local.path().to_owned(), logical_path: "root".into(),
            schedule: ScheduleConfig { enabled: true, interval: 60 },
        },
    }).await.expect("mapping");
    assert!(runtime.validate_scheduled_root(&mapping).is_err(), "automatic sync cannot bypass first confirmation");
    let mut updated = runtime_connection(&fixture);
    updated.id = Some(connection.id.clone());
    updated.bucket = "other".into();
    updated.buckets = vec!["other".into(), "fixture".into()];
    updated.prefix = "unrelated-prefix".into();
    runtime.save_connection(updated).expect("change default storage");
    fixture.state.requests.lock().expect("requests").clear();
    let preview = runtime.preview(&mapping.id).await.expect("bound preview");
    runtime.manual_sync(&mapping.id, &preview.preview_id).await.expect("confirmed sync");
    wait_completed(&runtime, &mapping.id).await;
    runtime.validate_scheduled_root(&mapping).expect("baseline permits later automatic ticks");
    assert!(runtime.local_sync_entries(&mapping.id).await.expect("synced entries").iter().all(|entry| entry.synced));
    let requests = fixture.state.requests.lock().expect("requests").clone();
    assert!(requests.iter().all(|(_, path)| path.starts_with("/fixture/") || path == "/fixture"), "{requests:?}");
    let objects = fixture.state.objects.lock().expect("objects").clone();
    assert!(objects.keys().all(|key| key.starts_with(&format!("runtime/{APPEND_ROOT}/"))));
    assert!(objects.values().all(|bytes| !bytes.windows(payload.len()).any(|window| window == payload)));
    std::fs::remove_file(local.path().join("docs/private.txt")).expect("delete child");
    let entries = runtime.local_sync_entries(&mapping.id).await.expect("pending delete");
    assert!(entries.iter().any(|entry| entry.path == "docs" && !entry.synced));
    runtime.stop_and_clear().await.expect("stop");
    fixture.shutdown().await;
}

async fn wait_completed(runtime: &CloudRuntime, mapping_id: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
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
