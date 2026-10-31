//! Opt-in AppendV2 acceptance; never runs during ordinary `cargo test`.
//!
//! Real AppendRepository/AppendPublisher/AppendIndex/discovery, with a test-only
//! CloudStore adapter: production S3Store does not expose connector injection.
//! No CAS probe, v1 HEAD, conditional headers, DELETE, retries or cleanup writes.
//! Objects intentionally remain under the printed, fresh validation prefix.
//! Budgets count reserved application body bytes, not TLS/billing bytes.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, future::BoxFuture, stream};
use http_body_util::{BodyExt, Limited};
use hyper::body::Body;
use object_store::{
    ObjectStore as Backend, ObjectStoreExt, PutPayload, RetryConfig,
    aws::{AmazonS3, AmazonS3Builder},
    client::{
        HttpClient, HttpConnector, HttpError, HttpErrorKind, HttpRequest, HttpResponse,
        HttpResponseBody, HttpService,
    },
    path::Path,
};
use peerdav_lib::{
    cloud::{
        traits::ObjectStore as CloudStore,
        types::{
            CloudError, ObjectListing, ObjectMetadata, ObjectPage, ObjectStream, PutCondition,
            StorageCapabilities,
        },
    },
    sync::{
        append_discovery, append_index::AppendIndex, append_publisher::AppendPublisher,
        append_repository::AppendRepository, append_types::*,
    },
};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::{
    ops::Range,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const ENDPOINT: &str = "https://s3.cn-east-1.qiniucs.com";
const MAX_REQUESTS: usize = 500;
const MAX_BYTES: usize = 100 * 1024 * 1024;
const MAX_BODY: usize = 64 * 1024;
const MAX_TIME: Duration = Duration::from_secs(180);
const REQUEST_TIME: Duration = Duration::from_secs(10);

// Deliberately never Debug/Display arbitrary SDK errors or signed requests.
fn checked<T, E>(result: Result<T, E>, stage: &str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{stage} failed (sensitive error details suppressed)"),
    }
}
fn denied(reason: &'static str) -> HttpError {
    HttpError::new(HttpErrorKind::Unknown, std::io::Error::other(reason))
}
fn random_hex<const N: usize>() -> String {
    let mut bytes = [0; N];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_".contains(&b))
        && key.split('/').all(|part| part != "." && part != "..")
}
#[derive(Default)]
struct Budget {
    requests: usize,
    bytes: usize,
}
#[derive(Clone)]
struct Guard {
    client: reqwest::Client,
    prefix: String,
    state: Arc<Mutex<Budget>>,
    started: Instant,
}
impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RestrictedQiniuTransport")
    }
}
impl Guard {
    fn validate(&self, req: &HttpRequest) -> Result<(), HttpError> {
        if req.uri().scheme_str() != Some("https")
            || req.uri().authority().map(|a| a.as_str()) != Some("s3.cn-east-1.qiniucs.com")
            || !matches!(req.method().as_str(), "GET" | "PUT")
            || req.headers().contains_key(http::header::IF_MATCH)
            || req.headers().contains_key(http::header::IF_NONE_MATCH)
        {
            return Err(denied("origin, method or conditional header denied"));
        }
        let url =
            url::Url::parse(&req.uri().to_string()).map_err(|_| denied("invalid request URL"))?;
        if req.uri().path() == "/peerdav" || req.uri().path() == "/peerdav/" {
            if req.method() != http::Method::GET {
                return Err(denied("bucket mutation denied"));
            }
            let mut prefix_seen = false;
            let mut list_seen = false;
            let mut seen = std::collections::BTreeSet::new();
            for (key, value) in url.query_pairs() {
                if !seen.insert(key.to_string()) {
                    return Err(denied("duplicate query denied"));
                }
                match key.as_ref() {
                    "prefix"
                        if value.starts_with(&format!("{}v2/", self.prefix))
                            && safe_key(&value) =>
                    {
                        prefix_seen = true
                    }
                    "list-type" if value == "2" => list_seen = true,
                    "delimiter" if value == "/" => {}
                    "max-keys" if value.parse::<usize>().is_ok_and(|n| n > 0 && n <= 1000) => {}
                    "continuation-token" if value.len() <= 4096 => {}
                    _ => return Err(denied("listing query outside isolated prefix")),
                }
            }
            if !prefix_seen || !list_seen {
                return Err(denied("unscoped listing denied"));
            }
        } else {
            let expected = format!("/peerdav/{}v2/", self.prefix);
            if !req.uri().path().starts_with(&expected)
                || !safe_key(req.uri().path())
                || req.uri().query().is_some()
            {
                return Err(denied("object path or query outside isolated prefix"));
            }
        }
        // Fail closed for unknown-length/chunked request bodies, even if a forged
        // Content-Length claims otherwise. Small fixture PUT bodies have exact hints.
        if req
            .body()
            .size_hint()
            .upper()
            .is_none_or(|n| n > MAX_BODY as u64)
        {
            return Err(denied("request body exceeds bounded size hint"));
        }
        Ok(())
    }
}
#[async_trait]
impl HttpService for Guard {
    async fn call(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.validate(&req)?;
        {
            let mut budget = self
                .state
                .lock()
                .map_err(|_| denied("budget lock poisoned"))?;
            if self.started.elapsed() >= MAX_TIME
                || budget.requests >= MAX_REQUESTS
                || budget.bytes > MAX_BYTES - 2 * MAX_BODY
            {
                return Err(denied("acceptance budget exceeded"));
            }
            budget.requests += 1;
            budget.bytes += 2 * MAX_BODY;
        }
        let response = tokio::time::timeout(REQUEST_TIME, HttpService::call(&self.client, req))
            .await
            .map_err(|_| denied("request timed out"))??;
        eprintln!("Qiniu AppendV2 HTTP status={}", response.status().as_u16());
        let (parts, body) = response.into_parts();
        let body = Limited::new(body, MAX_BODY)
            .map_err(|e| HttpError::new_boxed(HttpErrorKind::Decode, e));
        Ok(HttpResponse::from_parts(parts, HttpResponseBody::new(body)))
    }
}
impl HttpConnector for Guard {
    fn connect(&self, _: &object_store::ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(self.clone()))
    }
}

// Narrow adapter only; no production S3Store capability verification is exercised.
// SDK errors retain their sources internally, but checked() never renders them.
struct Remote {
    backend: AmazonS3,
    prefix: String,
}
fn storage(source: object_store::Error) -> CloudError {
    match source {
        object_store::Error::NotFound { .. } => CloudError::NotFound,
        source => CloudError::Storage { source },
    }
}
impl Remote {
    fn path(&self, key: &str) -> Result<Path, CloudError> {
        if !key.starts_with("v2/") || !safe_key(key) {
            return Err(CloudError::PrefixViolation);
        }
        Ok(Path::from(format!("{}{key}", self.prefix)))
    }
    fn metadata(&self, meta: object_store::ObjectMeta) -> Result<ObjectMetadata, CloudError> {
        let key = meta
            .location
            .as_ref()
            .strip_prefix(&self.prefix)
            .ok_or(CloudError::PrefixViolation)?;
        if !key.starts_with("v2/") || !safe_key(key) {
            return Err(CloudError::PrefixViolation);
        }
        Ok(ObjectMetadata {
            key: key.into(),
            size: meta.size,
            etag: meta.e_tag,
        })
    }
}
impl CloudStore for Remote {
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::default()
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        stream::once(async move { self.list_page(prefix, None).await })
            .map_ok(|page| stream::iter(page.objects.into_iter().map(Ok)))
            .try_flatten()
            .boxed()
    }
    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move {
            if token.is_some() {
                return Err(CloudError::Unsupported);
            }
            let path = self.path(prefix)?;
            let listed = self
                .backend
                .list_with_delimiter(Some(&path))
                .await
                .map_err(storage)?;
            if !listed.common_prefixes.is_empty() {
                return Err(CloudError::Unsupported);
            }
            let objects = listed
                .objects
                .into_iter()
                .map(|m| self.metadata(m))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ObjectPage {
                objects,
                next_token: None,
            })
        })
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            // Discovery needs stat; use bounded GET metadata instead of HEAD so
            // the transport never permits a capability probe or v1 HEAD.
            let result = self.backend.get(&self.path(key)?).await.map_err(storage)?;
            self.metadata(result.meta)
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            if range.is_some() {
                return Err(CloudError::Unsupported);
            }
            let result = self.backend.get(&self.path(key)?).await.map_err(storage)?;
            let bytes = result.bytes().await.map_err(storage)?;
            if bytes.len() > MAX_BODY {
                return Err(CloudError::PayloadTooLarge);
            }
            Ok(stream::once(async move { Ok(bytes) }).boxed())
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            if !matches!(condition, PutCondition::Any) {
                return Err(CloudError::Unsupported);
            }
            let path = self.path(key)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                if chunk.len() > MAX_BODY - bytes.len() {
                    return Err(CloudError::PayloadTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            let size = bytes.len() as u64;
            let result = self
                .backend
                .put(&path, PutPayload::from(Bytes::from(bytes)))
                .await
                .map_err(storage)?;
            Ok(ObjectMetadata {
                key: key.into(),
                size,
                etag: result.e_tag,
            })
        })
    }
    fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }
}
async fn read(remote: &Remote, key: &str) -> Vec<u8> {
    let mut body = checked(remote.get(key, None).await, "read object");
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = checked(chunk, "read bounded body");
        assert!(bytes.len() + chunk.len() <= MAX_BODY, "body limit");
        bytes.extend_from_slice(&chunk);
    }
    bytes
}
fn descriptor(mode: AppendMode) -> Descriptor {
    Descriptor {
        version: 2,
        repo_id: random_hex::<16>(),
        mode,
        chunker: Chunker {
            name: "fastcdc".into(),
            version: "5.0.0".into(),
            normalization: "level1".into(),
            level: 1,
            min: 1_048_576,
            average: 4_194_304,
            max: 8_388_608,
            seed: 0,
        },
    }
}
async fn exercise(remote: Arc<Remote>, mode: AppendMode, budget: &Arc<Mutex<Budget>>) {
    let password = Zeroizing::new(random_hex::<32>());
    let wrong_password = Zeroizing::new(random_hex::<32>());
    let (anchor, mut binding) = checked(
        AppendRepository::prepare_anchor(descriptor(mode), password.as_bytes()),
        "prepare anchor",
    );
    binding.source = TrustSource::TrustedFingerprint;
    checked(
        remote
            .put(
                anchor.key(),
                stream::iter([Ok(Bytes::copy_from_slice(anchor.bytes()))]).boxed(),
                PutCondition::Any,
            )
            .await,
        "publish anchor",
    );
    assert!(
        read(&remote, anchor.key()).await == anchor.bytes(),
        "anchor bytes changed"
    );
    let candidates = checked(
        AppendRepository::discover_anchors(remote.as_ref()).await,
        "discover anchors",
    );
    assert!(
        candidates.iter().any(|c| c.key == binding.key
            && c.fingerprint == binding.fingerprint
            && c.repo_id == binding.repo_id),
        "anchor not discovered"
    );
    assert!(
        matches!(
            AppendRepository::open_selected(
                remote.clone(),
                binding.clone(),
                wrong_password.as_bytes()
            )
            .await,
            Err(AppendError::Authentication)
        ),
        "wrong password was not rejected"
    );
    let mut bad_binding = binding.clone();
    bad_binding.fingerprint = "00".repeat(32);
    assert!(
        matches!(
            AppendRepository::open_selected(remote.clone(), bad_binding, password.as_bytes()).await,
            Err(AppendError::FingerprintMismatch)
        ),
        "wrong fingerprint was not rejected"
    );
    let (repo, _) = checked(
        AppendRepository::open_selected(remote.clone(), binding.clone(), password.as_bytes()).await,
        "open anchor",
    );
    let payload = b"Qiniu AppendV2 publication payload / encrypted and authenticated plain";
    let (block_key, block) = checked(repo.seal_block(payload), "seal block");
    assert!(checked(repo.open_block(&block), "open sealed block").as_slice() == payload);
    let envelope: Envelope = checked(serde_json::from_slice(&block), "decode block envelope");
    let binding_block = BlockBinding {
        block_id: envelope.logical_id,
        len: envelope.plaintext_len,
        object: ObjectRef {
            kind: "block".into(),
            digest: hex::encode(Sha256::digest(&block)),
        },
    };
    let root_revision = checked(
        peerdav_lib::sync::merge::revision(peerdav_lib::sync::merge_types::LocalChange {
            entity: peerdav_lib::sync::merge_types::EntityId("root".into()),
            change_id: peerdav_lib::sync::merge_types::ChangeId("root-create".into()),
            parents: vec![],
            value: peerdav_lib::sync::merge_types::RevisionValue::Live {
                parent_dir: peerdav_lib::sync::merge_types::EntityId("root".into()),
                name: "root".into(),
                node: peerdav_lib::sync::merge_types::Node::Dir,
            },
        }),
        "root revision",
    );
    let file_revision = checked(
        peerdav_lib::sync::merge::revision(peerdav_lib::sync::merge_types::LocalChange {
            entity: peerdav_lib::sync::merge_types::EntityId("file".into()),
            change_id: peerdav_lib::sync::merge_types::ChangeId("file-create".into()),
            parents: vec![],
            value: peerdav_lib::sync::merge_types::RevisionValue::Live {
                parent_dir: peerdav_lib::sync::merge_types::EntityId("root".into()),
                name: "sample.txt".into(),
                node: peerdav_lib::sync::merge_types::Node::File(
                    peerdav_lib::sync::merge_types::FileNode {
                        blocks: vec![peerdav_lib::sync::merge_types::BlockRef {
                            id: binding_block.block_id.clone(),
                            len: binding_block.len,
                        }],
                        length: payload.len() as u64,
                        digest: hex::encode(Sha256::digest(payload)),
                    },
                ),
            },
        }),
        "file revision",
    );
    let tree = Tree {
        version: 2,
        repo_id: repo.repo_id().into(),
        directory: "root".into(),
        entries: vec![root_revision, file_revision],
        children: vec![],
        bindings: vec![binding_block.clone()],
    };
    let (root, tree_wire) = checked(repo.seal_tree(&tree), "seal tree");
    let operation = random_hex::<16>();
    let commit = Commit {
        version: 2,
        repo_id: repo.repo_id().into(),
        operation_id: operation.clone(),
        parents: vec![],
        root: root.clone(),
    };
    let (commit_ref, commit_wire) = checked(repo.seal_commit(&commit), "seal commit");
    let objects = vec![
        PreparedObject::new(block_key.clone(), block),
        PreparedObject::new(
            format!("v2/{}/trees/{}", repo.repo_id(), root.digest),
            tree_wire,
        ),
        PreparedObject::new(
            format!(
                "v2/{}/commits/{}-{}",
                repo.repo_id(),
                operation,
                commit_ref.digest
            ),
            commit_wire,
        ),
    ];
    let directory = checked(tempfile::tempdir(), "temporary index directory");
    let path = directory.path().join("append-index");
    let mut index_key = Zeroizing::new([0; 32]);
    OsRng.fill_bytes(&mut *index_key);
    let mut index = checked(
        AppendIndex::open(&path, index_key.clone(), repo.repo_id().into()),
        "open publisher index",
    );
    checked(
        AppendPublisher::prepare(&mut index, &repo, &operation, &[], root.clone(), &objects),
        "durable prepare",
    );
    // Simulate process restart after durable prepare, before any object is published.
    drop(index);
    let mut index = checked(
        AppendIndex::open(&path, index_key.clone(), repo.repo_id().into()),
        "reopen prepared index",
    );
    assert!(
        checked(
            AppendPublisher::resume(
                &mut index,
                remote.as_ref(),
                &operation,
                &AtomicBool::new(false)
            )
            .await,
            "resume publication"
        ) == Some(OperationState::Published)
    );
    for object in &objects {
        assert!(
            read(&remote, object.key()).await == object.bytes(),
            "published bytes changed"
        );
    }
    let (reopened, _) = checked(
        AppendRepository::open_selected(remote.clone(), binding.clone(), password.as_bytes()).await,
        "reopen repository",
    );
    let fetched_tree = checked(
        reopened.open_tree(&read(&remote, objects[1].key()).await, &root),
        "authenticate remote tree",
    );
    assert!(
        fetched_tree.entries.len() == 2 && fetched_tree.bindings == vec![binding_block.clone()],
        "tree graph changed"
    );
    let fetched_commit = checked(
        reopened.open_commit(
            &read(&remote, objects[2].key()).await,
            &ObjectRef {
                kind: "commit".into(),
                digest: commit_ref.digest.clone(),
            },
        ),
        "authenticate remote commit",
    );
    assert!(fetched_commit == commit, "commit changed");
    assert!(
        checked(
            reopened.open_block_with_ref(&read(&remote, &block_key).await, Some(&binding_block)),
            "authenticate remote block"
        )
        .as_slice()
            == payload
    );
    let closure = checked(
        append_discovery::discover(
            remote.clone(),
            &binding,
            password.as_bytes(),
            &mut index,
            "remote-discovery",
        )
        .await,
        "discover complete closure",
    );
    assert!(
        closure.commits().len() == 1
            && closure.commits().contains_key(&commit_ref.digest)
            && closure.trees().len() == 1
            && closure.trees().contains_key(&root.digest),
        "discovery closure mismatch"
    );
    drop(index);
    let mut index = checked(
        AppendIndex::open(&path, index_key, repo.repo_id().into()),
        "reopen published index",
    );
    let before = checked(budget.lock(), "budget lock").requests;
    checked(
        AppendPublisher::prepare(&mut index, &reopened, &operation, &[], root, &objects),
        "idempotent prepare",
    );
    assert!(
        checked(
            AppendPublisher::resume(
                &mut index,
                remote.as_ref(),
                &operation,
                &AtomicBool::new(false)
            )
            .await,
            "idempotent resume"
        ) == Some(OperationState::Published)
    );
    assert!(
        checked(budget.lock(), "budget lock").requests == before,
        "published resume unexpectedly performed I/O"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit remote Qiniu AppendV2 acceptance only; billable writes remain in random prefix"]
async fn qiniu_random_prefix_append_v2_acceptance() {
    let started = Instant::now();
    let access = Zeroizing::new(checked(
        std::env::var("PEERDAV_QINIU_ACCESS_KEY"),
        "cargo-injected Qiniu access key",
    ));
    let secret = Zeroizing::new(checked(
        std::env::var("PEERDAV_QINIU_SECRET_KEY"),
        "cargo-injected Qiniu secret key",
    ));
    assert!(
        !access.is_empty() && !secret.is_empty(),
        "cargo-injected credentials are required"
    );
    let prefix = format!("peerdav-validation/{}/", random_hex::<24>());
    eprintln!("Qiniu AppendV2 prefix={prefix}");
    let state = Arc::new(Mutex::new(Budget::default()));
    let client = checked(
        reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIME)
            .build(),
        "build direct HTTP client",
    );
    let guard = Guard {
        client,
        prefix: prefix.clone(),
        state: state.clone(),
        started,
    };
    let backend = checked(
        AmazonS3Builder::new()
            .with_endpoint(ENDPOINT)
            .with_bucket_name("peerdav")
            .with_region("cn-east-1")
            .with_access_key_id(access.as_str())
            .with_secret_access_key(secret.as_str())
            .with_virtual_hosted_style_request(false)
            .with_retry(RetryConfig {
                max_retries: 0,
                retry_timeout: REQUEST_TIME,
                ..RetryConfig::default()
            })
            .with_http_connector(guard)
            .build(),
        "build restricted S3 backend",
    );
    let remote = Arc::new(Remote { backend, prefix });
    checked(
        tokio::time::timeout(MAX_TIME, async {
            exercise(remote.clone(), AppendMode::Encrypted, &state).await;
            exercise(remote, AppendMode::AuthenticatedPlain, &state).await;
        })
        .await,
        "180-second acceptance deadline",
    );
    let budget = checked(state.lock(), "budget lock");
    assert!(
        budget.requests > 0
            && budget.requests <= MAX_REQUESTS
            && budget.bytes <= MAX_BYTES
            && started.elapsed() <= MAX_TIME,
        "final budget exceeded"
    );
    eprintln!(
        "Qiniu AppendV2 completed: requests={}, reserved application bytes={}",
        budget.requests, budget.bytes
    );
}
