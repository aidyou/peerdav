//! Explicitly authorized, prefix-isolated Qiniu contract; never run by default.
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream};
use peerdav_file_core::*;
use peerdav_protocol_s3::*;
use rand::RngCore;
use std::{collections::BTreeSet, num::NonZeroUsize, sync::Arc, time::Duration};

type Check<T = ()> = Result<T, String>;

static DIAGNOSTIC_SECRETS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

// Unknown provider messages are untrusted. Never format a whole error with Debug,
// and reject ambiguous URL/header/query text rather than trying to parse it.
fn sanitized(message: &str, secrets: &[String]) -> String {
    if message.len() > 512 || message.chars().any(char::is_control) {
        return "[withheld: unbounded/control text]".into();
    }
    let mut text = message.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        let percent: String = secret.bytes().map(|b| format!("%{b:02X}")).collect();
        for form in [
            secret.clone(),
            encoded.clone(),
            encoded.replace('+', "%20"),
            percent.clone(),
            percent.to_lowercase(),
        ] {
            text = text.replace(&form, "[redacted]");
        }
    }
    let lower = text.to_ascii_lowercase();
    if [
        "://",
        "www.",
        "authorization",
        "credential",
        "signature",
        "sessiontoken",
        "session-token",
        "security-token",
        "x-amz-",
        "?",
        "&",
        "access_key",
        "secret_key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[withheld: URL/auth/query text]".into();
    }
    // Only diagnostic vocabulary with no provider-supplied payload is emitted.
    // Everything else is withheld after in-memory credential scrubbing.
    if [
        "[redacted]",
        "Stat: Provider",
        "cloud storage operation failed",
        "Generic S3 error: Content-Disposition header contained non UTF-8 characters",
        "Content-Disposition header contained non UTF-8 characters",
        "failed to convert header to a str",
    ]
    .contains(&text.as_str())
    {
        text
    } else {
        "[withheld: unknown source message]".into()
    }
}

fn diagnose(error: &(dyn std::error::Error + 'static)) {
    let secrets = DIAGNOSTIC_SECRETS.get().map(Vec::as_slice).unwrap_or(&[]);
    let mut current = Some(error);
    for depth in 0..8 {
        let Some(source) = current else { break };
        let class = if source.is::<CloudError>() {
            "cloud"
        } else if source.is::<object_store::Error>() {
            "object_store"
        } else if source.is::<reqwest::Error>() {
            "reqwest"
        } else if source.is::<std::io::Error>() {
            "io"
        } else {
            "inner"
        };
        eprintln!(
            "qiniu source depth={depth} class={class} message={}",
            sanitized(&source.to_string(), secrets)
        );
        current = source.source();
    }
}

#[test]
fn diagnostic_sanitizer_fake_secrets() {
    let secret = "FAKE key/+token".to_owned();
    let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
    let percent: String = secret.bytes().map(|b| format!("%{b:02X}")).collect();
    for input in [
        secret.clone(),
        encoded.clone(),
        encoded.replace('+', "%20"),
        percent.clone(),
        percent.to_lowercase(),
    ] {
        assert_eq!(
            sanitized(&input, std::slice::from_ref(&secret)),
            "[redacted]"
        );
    }
    for input in [
        "https://example.invalid/key?token=FAKE",
        "Authorization: Bearer FAKE",
        "sessionToken=FAKE",
        "foo?token=FAKE",
        "bad\nFAKE",
    ] {
        assert!(!sanitized(input, &[]).contains("FAKE"));
    }
    assert!(!sanitized(&"x".repeat(513), &[]).contains("xxx"));
    assert_eq!(
        sanitized("unknown innocent-looking payload", &[]),
        "[withheld: unknown source message]"
    );
    assert_eq!(
        sanitized(
            "Content-Disposition header contained non UTF-8 characters",
            &[]
        ),
        "Content-Disposition header contained non UTF-8 characters"
    );
}

fn cloud(error: CloudError) -> String {
    diagnose(&error);
    // Never format provider errors: their source chains can contain signed URLs.
    match error {
        CloudError::Service { status, .. } => format!("CLOUD_SERVICE status={status}"),
        CloudError::Transport { source } => format!(
            "CLOUD_TRANSPORT status={}",
            source.status().map(|s| s.as_u16()).unwrap_or(0)
        ),
        CloudError::Precondition => "CLOUD_PRECONDITION".into(),
        CloudError::NotFound => "CLOUD_NOT_FOUND".into(),
        CloudError::InvalidConfig(_) => "CLOUD_CONFIG_INVALID".into(),
        CloudError::Unsupported => "CLOUD_UNSUPPORTED".into(),
        CloudError::Storage { source } => match source {
            object_store::Error::Generic { .. } => "CLOUD_STORAGE_GENERIC".into(),
            object_store::Error::NotSupported { .. } => "CLOUD_STORAGE_NOT_SUPPORTED".into(),
            object_store::Error::PermissionDenied { .. } => {
                "CLOUD_STORAGE_PERMISSION_DENIED".into()
            }
            object_store::Error::Unauthenticated { .. } => "CLOUD_STORAGE_UNAUTHENTICATED".into(),
            _ => "CLOUD_STORAGE".into(),
        },
        _ => "CLOUD_STORAGE".into(),
    }
}
fn core(error: FileError) -> String {
    diagnose(&error);
    use std::error::Error;
    if let Some(source) = error.source().and_then(|e| e.downcast_ref::<CloudError>()) {
        return match source {
            CloudError::Storage { source } => match source {
                object_store::Error::Generic { .. } => "CLOUD_STORAGE_GENERIC".into(),
                object_store::Error::InvalidPath { .. } => "CLOUD_STORAGE_INVALID_PATH".into(),
                _ => "CLOUD_STORAGE".into(),
            },
            _ => cloud_error_code(&error).into(),
        };
    }
    cloud_error_code(&error).to_owned()
}
fn require(ok: bool, code: &str) -> Check {
    if ok { Ok(()) } else { Err(code.to_owned()) }
}
fn credential(name: &str) -> Check<String> {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing cargo-injected {name}"))
}
fn body(value: &'static [u8]) -> ObjectStream {
    stream::once(async move { Ok(Bytes::from_static(value)) }).boxed()
}
fn context() -> OperationContext {
    OperationContext {
        cancellation: Default::default(),
        deadline: None,
        budget: OperationBudget {
            max_bytes: 4096,
            max_entries: NonZeroUsize::new(32).unwrap(),
            max_chunk_bytes: NonZeroUsize::new(4096).unwrap(),
        },
    }
}
async fn bytes(store: &dyn ObjectStore, key: &str) -> Check<Vec<u8>> {
    let chunks: Vec<Bytes> = store
        .get(key, None)
        .await
        .map_err(cloud)?
        .try_collect()
        .await
        .map_err(cloud)?;
    Ok(chunks.concat())
}
async fn create(
    store: &S3Store,
    key: &str,
    value: &'static [u8],
    owned: &mut BTreeSet<String>,
) -> Check<ObjectMetadata> {
    let metadata = store
        .put(key, body(value), PutCondition::CreateOnly)
        .await
        .map_err(cloud)?;
    owned.insert(key.to_owned());
    Ok(metadata)
}
async fn rejected(result: Result<ObjectMetadata, CloudError>) -> Check {
    match result {
        Err(CloudError::Precondition) => Ok(()),
        Err(error) => Err(cloud(error)),
        Ok(_) => Err("CONDITIONAL_WRITE_NOT_ENFORCED".into()),
    }
}

async fn contract(store: Arc<S3Store>, owned: &mut BTreeSet<String>) -> Check {
    // All keys are relative to the random configured physical prefix. Even list
    // operations use a nonempty child prefix, never the bucket root.
    let keys = [
        "图片/中文 图片.png",
        "图片/literal%2F.png",
        "图片/space image.png",
    ];
    for key in keys {
        create(&store, key, b"\x89PNG\r\n\x1a\nraw-image", owned).await?;
    }
    create(&store, "图片邻居/not-in-list", b"neighbor", owned).await?;
    let backend = CoreS3Backend::new(store.clone(), 1).map_err(core)?;
    let bridge = CoreS3ReadBridge(CoreS3Backend::new(store.clone(), 1).map_err(core)?);
    require(
        !store.capabilities().conditional_writes_verified,
        "UNEXPECTED_VERIFIED_STATE",
    )?;
    for key in keys {
        eprintln!("qiniu stage=core_stat key={key}");
        let stat = backend
            .stat(context(), backend.scope().entry(key))
            .await
            .map_err(core)?;
        require(
            stat.id.opaque() == key && stat.size == Some(17),
            "CORE_STAT_RAW_KEY",
        )?;
        let response = backend
            .read(
                context(),
                ReadRequest {
                    entry: backend.scope().entry(key),
                    range: None,
                },
            )
            .await
            .map_err(core)?;
        let chunks: Vec<Bytes> = response.body.try_collect().await.map_err(core)?;
        require(
            chunks.concat() == b"\x89PNG\r\n\x1a\nraw-image",
            "CORE_READ_ROUNDTRIP",
        )?;
        require(
            bytes(&bridge, key).await? == b"\x89PNG\r\n\x1a\nraw-image",
            "BRIDGE_READ_ROUNDTRIP",
        )?;
        require(
            bridge.stat(key).await.map_err(cloud)?.key == key,
            "BRIDGE_STAT_RAW_KEY",
        )?;
    }
    // Core writes only replace a key that this process successfully created.
    backend
        .write(
            context(),
            WriteRequest {
                entry: backend.scope().entry(keys[0]),
                condition: Condition::Any,
                mode: WriteMode::BoundedAtomic { max_bytes: 4096 },
                body: stream::once(async { Ok(Bytes::from_static(b"core")) }).boxed(),
            },
        )
        .await
        .map_err(core)?;
    require(
        bytes(&bridge, keys[0]).await? == b"core",
        "CORE_WRITE_ROUNDTRIP",
    )?;
    bridge
        .put(keys[0], body(b"bridge"), PutCondition::Any)
        .await
        .map_err(cloud)?;
    require(
        bytes(store.as_ref(), keys[0]).await? == b"bridge",
        "BRIDGE_WRITE_ROUNDTRIP",
    )?;
    let ranged: Vec<Bytes> = bridge
        .get(keys[0], Some(1..4))
        .await
        .map_err(cloud)?
        .try_collect()
        .await
        .map_err(cloud)?;
    require(ranged.concat() == b"rid", "BRIDGE_RANGE")?;

    let expected: BTreeSet<String> = keys.iter().map(|s| s.to_string()).collect();
    let mut request = ListRequest {
        parent: backend.scope().entry("图片/"),
        query: None,
        prefix: None,
        cursor: None,
        limit: NonZeroUsize::new(1).unwrap(),
    };
    let mut listed = BTreeSet::new();
    let mut pages = 0;
    loop {
        pages += 1;
        require(pages <= 8, "CORE_PAGINATION_LOOP")?;
        let page = backend
            .list(context(), request.clone())
            .await
            .map_err(core)?;
        require(page.entries().len() <= 1, "CORE_PAGE_LIMIT")?;
        for entry in page.entries() {
            require(
                listed.insert(entry.id.opaque().to_owned()),
                "CORE_DUPLICATE_KEY",
            )?;
        }
        request.cursor = page.next().cloned();
        if request.cursor.is_none() {
            break;
        }
    }
    require(
        pages >= 3 && listed == expected,
        "CORE_RAW_TRAILING_SLASH_PAGINATION",
    )?;
    let mut listed = BTreeSet::new();
    let mut token = None;
    for page_number in 0..8 {
        let page = bridge
            .list_page("图片/", token.as_deref())
            .await
            .map_err(cloud)?;
        for entry in page.objects {
            require(listed.insert(entry.key), "BRIDGE_DUPLICATE_KEY")?;
        }
        token = page.next_token;
        if token.is_none() {
            break;
        }
        require(page_number < 7, "BRIDGE_PAGINATION_LOOP")?;
    }
    require(listed == expected, "BRIDGE_RAW_TRAILING_SLASH_LIST")?;

    // Real provider conditional probe, with known keys and immediate ownership
    // tracking. Do not invoke verify_capabilities: it generates hidden artifact
    // names. This probe does NOT forge the production verified capability epoch.
    let key = "probe/cas";
    let first = create(&store, key, b"first", owned).await?;
    rejected(
        bridge
            .put(key, body(b"bad"), PutCondition::CreateOnly)
            .await,
    )
    .await?;
    require(
        bytes(store.as_ref(), key).await? == b"first",
        "CREATE_ONLY_CHANGED_DATA",
    )?;
    let etag = first.etag.ok_or("MISSING_ETAG")?;
    let (left, right) = tokio::join!(
        store.put(key, body(b"left"), PutCondition::MatchEtag(etag.clone())),
        bridge.put(key, body(b"right"), PutCondition::MatchEtag(etag.clone()))
    );
    match (left, right) {
        (Ok(_), Err(CloudError::Precondition)) => require(
            bytes(store.as_ref(), key).await? == b"left",
            "CAS_WINNER_DATA",
        )?,
        (Err(CloudError::Precondition), Ok(_)) => require(
            bytes(store.as_ref(), key).await? == b"right",
            "CAS_WINNER_DATA",
        )?,
        (Err(e), _) | (_, Err(e)) => return Err(cloud(e)),
        _ => return Err("CAS_MULTIPLE_WINNERS".into()),
    }
    rejected(
        store
            .put(key, body(b"stale"), PutCondition::MatchEtag(etag))
            .await,
    )
    .await?;
    let create_key = "probe/create-race";
    let (left, right) = tokio::join!(
        store.put(create_key, body(b"left"), PutCondition::CreateOnly),
        bridge.put(create_key, body(b"right"), PutCondition::CreateOnly)
    );
    if left.is_ok() || right.is_ok() {
        owned.insert(create_key.into());
    }
    match (left, right) {
        (Ok(_), Err(CloudError::Precondition)) | (Err(CloudError::Precondition), Ok(_)) => {}
        (Err(e), _) | (_, Err(e)) => return Err(cloud(e)),
        _ => return Err("CREATE_MULTIPLE_WINNERS".into()),
    }
    eprintln!(
        "qiniu contract: raw/core/bridge/list/page/range/CAS probe passed; production verified flag unchanged"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "live writes: requires PEERDAV_QINIU_LIVE=1 and Cargo-injected credentials"]
async fn qiniu_contract() {
    let result = live().await;
    if let Err(code) = result {
        panic!("qiniu contract failed: {code}");
    }
}
async fn live() -> Check {
    require(
        std::env::var("PEERDAV_QINIU_LIVE").ok().as_deref() == Some("1"),
        "set PEERDAV_QINIU_LIVE=1 to authorize live test",
    )?;
    // Credential presence is inspected inside the Cargo-launched process only.
    // Values are used only in memory, never printed, serialized, or passed as CLI arguments.
    let access_key = credential("PEERDAV_QINIU_ACCESS_KEY")?;
    let secret_key = credential("PEERDAV_QINIU_SECRET_KEY")?;
    DIAGNOSTIC_SECRETS
        .set(vec![access_key.clone(), secret_key.clone()])
        .map_err(|_| "DIAGNOSTICS_ALREADY_INITIALIZED")?;
    let mut random = [0u8; 16];
    rand::rngs::OsRng
        .try_fill_bytes(&mut random)
        .map_err(|_| "RANDOM_FAILED")?;
    let prefix = format!("peerdav-contract/{}/", hex::encode(random));
    let store = Arc::new(
        S3Store::new(S3Config {
            endpoint: "https://peerdav-test.s3.cn-east-1.qiniucs.com".into(),
            bucket: "peerdav-test".into(),
            region: "cn-east-1".into(),
            prefix: prefix.clone(),
            path_style: false,
            access_key,
            secret_key,
            session_token: None,
            buckets: vec![],
            allow_insecure_http: false,
            proxy: ProxyMode::Direct,
        })
        .map_err(cloud)?,
    );
    eprintln!("qiniu contract prefix={prefix}");
    let mut owned = BTreeSet::new();
    // No assertion/panic in the contract: every error and timeout reaches cleanup.
    let result = match tokio::time::timeout(
        Duration::from_secs(240),
        contract(store.clone(), &mut owned),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err("CONTRACT_TIMEOUT_OUTCOME_MAY_BE_UNKNOWN".into()),
    };
    if let Err(code) = &result {
        eprintln!("qiniu contract operation failed code={code}");
    }
    let created = owned.len();
    let mut failures = 0;
    for key in &owned {
        match tokio::time::timeout(Duration::from_secs(30), store.delete(key)).await {
            Ok(Ok(())) => {
                match tokio::time::timeout(Duration::from_secs(30), store.stat(key)).await {
                    Ok(Err(CloudError::NotFound)) => {}
                    Ok(Err(error)) => {
                        failures += 1;
                        eprintln!("cleanup verification {}", cloud(error));
                    }
                    _ => {
                        failures += 1;
                        eprintln!("cleanup verification failed code=CLEANUP_NOT_CONFIRMED");
                    }
                }
            }
            Ok(Err(error)) => {
                failures += 1;
                eprintln!("cleanup {}", cloud(error));
            }
            Err(_) => {
                failures += 1;
                eprintln!("cleanup code=CLEANUP_TIMEOUT");
            }
        }
    }
    eprintln!(
        "qiniu cleanup prefix={prefix} successfully_created={created} confirmed_deleted={} failures={failures}",
        created - failures
    );
    if failures != 0 {
        return Err("CLEANUP_FAILED".into());
    }
    result
}
