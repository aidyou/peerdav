use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use base64::Engine;
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, future::BoxFuture, stream};
use object_store::list::{PaginatedListOptions, PaginatedListStore};
use object_store::path::Path as ObjectPath;
use object_store::{
    GetOptions, GetRange, MultipartUpload, ObjectStore as Backend, ObjectStoreExt, PutMode,
    PutOptions, PutPayload,
};
use rand::RngCore;

use super::proxy::ConnectionTransport;
use super::traits::ObjectStore;
use super::types::{
    CloudError, MAX_PUT_BYTES, ObjectListing, ObjectMetadata, ObjectPage, ObjectPageToken,
    ObjectStoreScope, ObjectStream, PutCondition, S3Config, StorageCapabilities,
};

/// Shared core response invariant, also used by the legacy production GET path.
fn validate_read_metadata(
    requested: Option<&Range<u64>>,
    actual: &Range<u64>,
    size: u64,
) -> Result<(), CloudError> {
    let valid = match requested {
        Some(range) => {
            actual.start == range.start
                && actual.end == range.end.min(size)
                && actual.start < actual.end
        }
        None => *actual == (0..size),
    };
    if valid {
        Ok(())
    } else {
        Err(crate::core_backend::input_error(
            peerdav_file_core::FileError::new(
                peerdav_file_core::FileErrorKind::Provider,
                peerdav_file_core::Operation::Read,
                peerdav_file_core::MutationOutcome::NotApplied,
            ),
        ))
    }
}

#[derive(Clone)]
pub struct S3Store {
    config: S3Config,
    backend: Arc<dyn Backend>,
    paginated: Option<Arc<object_store::aws::AmazonS3>>,
    verified: Arc<AtomicBool>,
    delete_verified: Arc<tokio::sync::Mutex<Option<bool>>>,
    transport: ConnectionTransport,
    #[cfg(test)]
    multipart_factory: Option<MultipartFactory>,
    #[cfg(test)]
    multipart_budget: Option<usize>,
    #[cfg(test)]
    multipart_peak_buffer: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
type MultipartFactory = Arc<
    dyn Fn(ObjectPath) -> BoxFuture<'static, Result<Box<dyn MultipartUpload>, object_store::Error>>
        + Send
        + Sync,
>;

impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Store")
            .field("config", &self.config)
            .finish()
    }
}

impl S3Store {
    pub fn new(mut config: S3Config) -> Result<Self, CloudError> {
        let url = url::Url::parse(&config.endpoint)
            .map_err(|_| CloudError::InvalidConfig("invalid endpoint URL".into()))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
            || url.host_str().is_none()
        {
            return Err(CloudError::InvalidConfig(
                "endpoint must contain only scheme, host and port".into(),
            ));
        }
        if url.scheme() != "https"
            && !(url.scheme() == "http" && config.allow_insecure_http && is_private(url.host_str()))
        {
            return Err(CloudError::InvalidConfig(
                "HTTPS is required; private HTTP requires explicit consent".into(),
            ));
        }
        if config.bucket.is_empty()
            || config.region.is_empty()
            || config.access_key.is_empty()
            || config.secret_key.is_empty()
        {
            return Err(CloudError::InvalidConfig(
                "bucket, region and explicit credentials are required".into(),
            ));
        }
        if !config.prefix.is_empty() {
            validate_key(config.prefix.trim_end_matches('/'))?;
            config.prefix = format!("{}/", config.prefix.trim_end_matches('/'));
        }
        // object_store 0.14.2 omits the bucket from the request path when a
        // custom endpoint with a port (or an IP/loopback host) is configured
        // with virtual-host addressing. MinIO then receives `HEAD /key` and
        // answers 400. Path-style is the only valid mode for those endpoints,
        // including configurations persisted before this normalization.
        if !config.path_style && requires_path_style(&url) {
            config.path_style = true;
        }
        let transport = ConnectionTransport::new(&config.proxy)?;
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_region(&config.region)
            .with_bucket_name(&config.bucket)
            .with_access_key_id(&config.access_key)
            .with_secret_access_key(&config.secret_key)
            .with_endpoint(&config.endpoint)
            .with_virtual_hosted_style_request(!config.path_style)
            .with_allow_http(url.scheme() == "http")
            .with_http_connector(transport.clone().qiniu_compatibility());
        if let Some(token) = &config.session_token {
            builder = builder.with_token(token);
        }
        let backend = builder.build().map_err(map_err)?;
        Ok(Self {
            config,
            backend: Arc::new(backend.clone()),
            paginated: Some(Arc::new(backend)),
            transport,
            verified: Arc::new(AtomicBool::new(false)),
            delete_verified: Arc::new(tokio::sync::Mutex::new(None)),
            #[cfg(test)]
            multipart_factory: None,
            #[cfg(test)]
            multipart_budget: None,
            #[cfg(test)]
            multipart_peak_buffer: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    #[cfg(test)]
    fn test_store(backend: Arc<dyn Backend>, factory: MultipartFactory) -> Self {
        Self {
            config: S3Config {
                endpoint: "https://localhost".into(),
                bucket: "test".into(),
                region: "test".into(),
                prefix: String::new(),
                path_style: true,
                access_key: "test".into(),
                secret_key: "test".into(),
                session_token: None,
                buckets: Vec::new(),
                allow_insecure_http: false,
                proxy: super::types::ProxyMode::Direct,
            },
            backend,
            paginated: None,
            transport: ConnectionTransport::new(&super::types::ProxyMode::Direct).expect("test transport"),
            verified: Arc::new(AtomicBool::new(false)),
            delete_verified: Arc::new(tokio::sync::Mutex::new(None)),
            multipart_factory: Some(factory),
            multipart_budget: None,
            multipart_peak_buffer: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub(crate) async fn core_stat(&self, key: &str) -> Result<(ObjectMetadata, i64), CloudError> {
        validate_key(key)?;
        let meta = self.backend.head(&self.path(key)?).await.map_err(map_err)?;
        let modified = meta.last_modified.timestamp();
        Ok((Self::meta(key.to_owned(), meta), modified))
    }

    pub(crate) async fn core_read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
    ) -> Result<(ObjectMetadata, i64, Range<u64>, ObjectStream), CloudError> {
        validate_key(key)?;
        let requested = range.clone();
        let result = self
            .backend
            .get_opts(
                &self.path(key)?,
                GetOptions {
                    range: range.map(GetRange::Bounded),
                    ..Default::default()
                },
            )
            .await
            .map_err(map_err)?;
        let metadata = Self::meta(key.to_owned(), result.meta.clone());
        let actual = result.range.clone();
        validate_read_metadata(requested.as_ref(), &actual, metadata.size)?;
        Ok((
            metadata,
            result.meta.last_modified.timestamp(),
            actual,
            result.into_stream().map_err(map_err).boxed(),
        ))
    }

    fn path(&self, key: &str) -> Result<ObjectPath, CloudError> {
        // Keys are raw UTF-8, not URL paths. From encodes path parts (including
        // literal '%'); parse preserves their identity. HTTP escaping belongs
        // to the SDK, and listed keys must never be percent-decoded here.
        ObjectPath::parse(format!("{}{}", self.config.prefix, key))
            .map_err(object_store::Error::from)
            .map_err(map_err)
    }

    async fn delete_revision_request(&self, key: &str, etag: &str) -> Result<(), CloudError> {
        // object_store 0.14 has no conditional DELETE primitive. Reuse its
        // S3 signing and the configured transport; never emulate this with
        // HEAD followed by an unconditional delete.
        use object_store::signer::Signer;
        let backend = self.paginated.as_ref().ok_or(CloudError::Unsupported)?;
        let url = backend
            .signed_url(
                reqwest::Method::DELETE,
                &self.path(key)?,
                std::time::Duration::from_secs(60),
            )
            .await
            .map_err(map_err)?;
        let response = self.transport.client()
            .delete(url)
            .header(reqwest::header::IF_MATCH, etag)
            .send()
            .await
            .map_err(|source| CloudError::Transport { source: source.without_url() })?;
        match response.status().as_u16() {
            200 | 204 => Ok(()),
            412 => Err(CloudError::Precondition),
            404 => Err(CloudError::NotFound),
            status => {
                let mut body = response.bytes_stream();
                let mut detail = Vec::new();
                while let Some(chunk) = body.next().await {
                    let chunk = chunk.map_err(|source| CloudError::Transport { source: source.without_url() })?;
                    let count = chunk.len().min(4096 - detail.len());
                    detail.extend_from_slice(&chunk[..count]);
                    if detail.len() == 4096 { break; }
                }
                Err(CloudError::Service { status, detail: String::from_utf8_lossy(&detail).into_owned() })
            }
        }
    }

    async fn verify_delete_precondition(&self) -> Result<(), CloudError> {
        let mut verified = self.delete_verified.lock().await;
        if let Some(supported) = *verified {
            return if supported { Ok(()) } else { Err(CloudError::Unsupported) };
        }
        let mut random = [0; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut random).map_err(|source| CloudError::Random { source })?;
        let key = format!("probes/{}/conditional-delete", hex::encode(random));
        let original = self.put(&key, body(b"delete-probe"), PutCondition::CreateOnly).await?;
        let revision = original.etag.as_deref().filter(|etag| !etag.is_empty()).ok_or(CloudError::Unsupported)?;
        // A provider that ignores If-Match may delete this isolated random probe,
        // but must never be allowed to delete any user object.
        match self.delete_revision_request(&key, "\"peerdav-impossible-revision\"").await {
            Err(CloudError::Precondition) => {}
            Err(error) => return Err(error),
            Ok(()) => {
                // Do not create another probe for every item in this folder.
                *verified = Some(false);
                return Err(CloudError::Unsupported);
            }
        }
        let retained = self.stat(&key).await?;
        if retained.etag.as_deref() != Some(revision) || retained.size != original.size {
            *verified = Some(false);
            return Err(CloudError::Unsupported);
        }
        self.delete_revision_request(&key, revision).await?;
        match self.stat(&key).await {
            Err(CloudError::NotFound) => {}
            Err(error) => return Err(error),
            Ok(_) => {
                *verified = Some(false);
                return Err(CloudError::Unsupported);
            }
        }
        *verified = Some(true);
        Ok(())
    }

    fn list_prefix(&self, prefix: &str) -> Result<String, CloudError> {
        self.path(prefix)?;
        // Path normalizes trailing '/', but paginated S3 listing accepts a raw
        // string prefix: retain the directory delimiter and literal characters.
        Ok(format!("{}{}", self.config.prefix, prefix))
    }

    #[cfg(test)]
    async fn begin_multipart(
        &self,
        path: ObjectPath,
    ) -> Result<Box<dyn MultipartUpload>, object_store::Error> {
        if let Some(factory) = &self.multipart_factory {
            factory(path).await
        } else {
            self.backend.put_multipart(&path).await
        }
    }

    #[cfg(not(test))]
    async fn begin_multipart(
        &self,
        path: ObjectPath,
    ) -> Result<Box<dyn MultipartUpload>, object_store::Error> {
        self.backend.put_multipart(&path).await
    }

    fn meta(key: String, m: object_store::ObjectMeta) -> ObjectMetadata {
        ObjectMetadata {
            key,
            size: m.size,
            etag: m.e_tag,
        }
    }

    /// Probes only random isolated objects. Never deletes repository objects.
    /// A caller must refuse repository writes unless this succeeds. Probe
    /// artifacts are retained for diagnostics, consistent with no automatic GC.
    pub async fn verify_capabilities(&self) -> Result<StorageCapabilities, CloudError> {
        self.verified.store(false, Ordering::Release);
        let mut random = [0_u8; 16];
        rand::rngs::OsRng
            .try_fill_bytes(&mut random)
            .map_err(|source| CloudError::Random { source })?;
        let key = format!("probes/{}/cas", hex::encode(random));
        let first = self
            .put(&key, body(b"first"), PutCondition::CreateOnly)
            .await?;
        match self.put(&key, body(b"bad"), PutCondition::CreateOnly).await {
            Err(CloudError::Precondition) => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(CloudError::Unsupported),
        }
        let revision = first.etag.ok_or(CloudError::Unsupported)?;
        let (left, right) = tokio::join!(
            self.put(
                &key,
                body(b"left"),
                PutCondition::MatchEtag(revision.clone())
            ),
            self.put(
                &key,
                body(b"right"),
                PutCondition::MatchEtag(revision.clone())
            ),
        );
        verify_single_winner(left, right)?;
        match self
            .put(&key, body(b"stale"), PutCondition::MatchEtag(revision))
            .await
        {
            Err(CloudError::Precondition) => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(CloudError::Unsupported),
        }
        let create_key = format!("{key}-create");
        let (left, right) = tokio::join!(
            self.put(&create_key, body(b"left"), PutCondition::CreateOnly),
            self.put(&create_key, body(b"right"), PutCondition::CreateOnly),
        );
        verify_single_winner(left, right)?;
        self.verified.store(true, Ordering::Release);
        Ok(self.capabilities())
    }
}

fn verify_single_winner(
    left: Result<ObjectMetadata, CloudError>,
    right: Result<ObjectMetadata, CloudError>,
) -> Result<(), CloudError> {
    match (left, right) {
        (Ok(_), Err(CloudError::Precondition)) | (Err(CloudError::Precondition), Ok(_)) => Ok(()),
        (Err(CloudError::Precondition), Err(CloudError::Precondition)) | (Ok(_), Ok(_)) => {
            Err(CloudError::Unsupported)
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn body(bytes: &'static [u8]) -> ObjectStream {
    stream::once(async move { Ok(Bytes::from_static(bytes)) }).boxed()
}

pub fn is_private(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host.trim_matches(['[', ']']);
        host == "localhost"
            || host.parse::<IpAddr>().is_ok_and(|ip| match ip {
                IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
                IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
            })
    })
}

/// Virtual-host addressing needs a DNS name without a custom port. IP literals,
/// loopback names and explicit ports are incompatible with bucket subdomains.
fn requires_path_style(url: &url::Url) -> bool {
    url.port().is_some() || is_private(url.host_str())
}

fn validate_key(key: &str) -> Result<(), CloudError> {
    if key.is_empty()
        || key.contains(['\\', '\0', ':'])
        || key
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(CloudError::InvalidConfig("invalid object key".into()));
    }
    Ok(())
}

fn prefix_contains(prefix: &str, key: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    prefix.is_empty() || key == prefix || key.starts_with(&format!("{prefix}/"))
}

fn pagination_scope_fingerprint(config: &S3Config) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.endpoint.hash(&mut hasher);
    config.bucket.hash(&mut hasher);
    config.prefix.hash(&mut hasher);
    config.region.hash(&mut hasher);
    config.path_style.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn map_err(source: object_store::Error) -> CloudError {
    match source {
        object_store::Error::Precondition { .. } | object_store::Error::AlreadyExists { .. } => {
            CloudError::Precondition
        }
        object_store::Error::NotFound { .. } => CloudError::NotFound,
        source => CloudError::Storage { source },
    }
}

/// Aborts a multipart upload while preserving the error that required the abort.
/// When abort fails, both the original error and the abort error are retained;
/// the remote upload id is unknown because object_store does not expose it.
async fn abort_multipart(
    upload: &mut Box<dyn MultipartUpload>,
    operation: &'static str,
    primary: CloudError,
) -> CloudError {
    match upload.abort().await {
        Ok(()) => primary,
        Err(abort) => CloudError::MultipartAbort {
            operation,
            primary: Box::new(primary),
            abort,
        },
    }
}

impl S3Store {
    pub(crate) fn core_list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
        limit: usize,
    ) -> BoxFuture<'a, Result<(ObjectPage, std::collections::HashMap<String, i64>), CloudError>> {
        self.list_page_with_times(prefix, token, limit)
    }
}

impl S3Store {
    fn scope(&self) -> Option<ObjectStoreScope> {
        Some(ObjectStoreScope {
            endpoint: self.config.endpoint.clone(),
            bucket: self.config.bucket.clone(),
            prefix: self.config.prefix.clone(),
        })
    }
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities {
            create_only: true,
            conditional_replace: true,
            range_read: true,
            conditional_writes_verified: self.verified.load(Ordering::Acquire),
        }
    }

    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        if !prefix.is_empty() && validate_key(prefix.trim_end_matches('/')).is_err() {
            return stream::once(async { Err(CloudError::PrefixViolation) }).boxed();
        }
        let root = self.config.prefix.clone();
        let path = match self.path(prefix) {
            Ok(path) => path,
            Err(error) => return stream::once(async move { Err(error) }).boxed(),
        };
        self.backend
            .list(Some(&path))
            .map(move |result| {
                let meta = result.map_err(map_err)?;
                let key = meta
                    .location
                    .as_ref()
                    .strip_prefix(&root)
                    .ok_or(CloudError::PrefixViolation)?
                    .to_owned();
                validate_key(&key)?;
                Ok(Self::meta(key, meta))
            })
            .boxed()
    }

    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        self.list_page_bounded(prefix, token, 1_000)
    }
}

impl S3Store {
    fn list_page_bounded<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
        limit: usize,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move { self.list_page_with_times(prefix, token, limit).await.map(|(page, _)| page) })
    }

    fn list_page_with_times<'a>(
        &'a self, prefix: &'a str, token: Option<&'a str>, limit: usize,
    ) -> BoxFuture<'a, Result<(ObjectPage, std::collections::HashMap<String, i64>), CloudError>> {
        Box::pin(async move {
            const PAGE_LIMIT: usize = 1_000;
            const ENTRY_LIMIT: usize = 100_000;
            if !prefix.is_empty() && validate_key(prefix.trim_end_matches('/')).is_err() {
                return Err(CloudError::PrefixViolation);
            }
            // Continuation DTOs carry traversal state only: their counters and
            // fingerprint are not authenticated and must never authorize access.
            // Always constrain response keys independently to the requested root.
            let scope_fingerprint = pagination_scope_fingerprint(&self.config);
            let state = token
                .map(|raw| {
                    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(raw)
                        .map_err(|_| {
                            CloudError::InvalidConfig("invalid continuation token".into())
                        })?;
                    serde_json::from_slice::<ObjectPageToken>(&bytes)
                        .map_err(|_| CloudError::InvalidConfig("invalid continuation token".into()))
                })
                .transpose()?;
            if state.as_ref().is_some_and(|state| state.prefix != prefix) {
                return Err(CloudError::PrefixViolation);
            }
            if state
                .as_ref()
                .is_some_and(|state| state.scope_fingerprint != scope_fingerprint)
            {
                return Err(CloudError::PrefixViolation);
            }
            let state = state.unwrap_or(ObjectPageToken {
                prefix: prefix.to_owned(),
                provider_token: String::new(),
                pages: 0,
                entries: 0,
                scope_fingerprint: scope_fingerprint.clone(),
                seen_tokens: Vec::new(),
            });
            if state.pages >= PAGE_LIMIT || state.entries >= ENTRY_LIMIT {
                return Err(CloudError::UploadBudgetExceeded);
            }
            let store = self.paginated.as_ref().ok_or(CloudError::Unsupported)?;
            let result = store
                .list_paginated(
                    Some(&self.list_prefix(prefix)?),
                    PaginatedListOptions {
                        max_keys: Some(limit.min(PAGE_LIMIT).min(ENTRY_LIMIT - state.entries)),
                        page_token: (!state.provider_token.is_empty())
                            .then_some(state.provider_token.clone()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(map_err)?;
            if result.result.objects.len() > limit.min(PAGE_LIMIT).min(ENTRY_LIMIT - state.entries)
            {
                return Err(CloudError::UploadBudgetExceeded);
            }
            let root = self.config.prefix.clone();
            let mut objects = Vec::with_capacity(result.result.objects.len());
            let mut modified = std::collections::HashMap::new();
            for meta in result.result.objects {
                let key = meta
                    .location
                    .as_ref()
                    .strip_prefix(&root)
                    .ok_or(CloudError::PrefixViolation)?
                    .to_owned();
                if !prefix_contains(prefix, &key) {
                    return Err(CloudError::PrefixViolation);
                }
                validate_key(&key)?;
                modified.insert(key.clone(), meta.last_modified.timestamp());
                objects.push(Self::meta(key, meta));
            }
            let next_token = if let Some(provider_token) = result.page_token {
                if provider_token == state.provider_token
                    || state.seen_tokens.iter().any(|seen| seen == &provider_token)
                    || state.seen_tokens.len() >= PAGE_LIMIT
                {
                    return Err(CloudError::InvalidConfig(
                        "provider returned a repeated continuation token".into(),
                    ));
                }
                let mut seen_tokens = state.seen_tokens;
                seen_tokens.push(provider_token.clone());
                let encoded = serde_json::to_vec(&ObjectPageToken {
                    prefix: prefix.to_owned(),
                    provider_token,
                    pages: state.pages + 1,
                    entries: state.entries + objects.len(),
                    scope_fingerprint,
                    seen_tokens,
                })
                .map_err(|_| {
                    CloudError::InvalidConfig("pagination token serialization failed".into())
                })?;
                Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(encoded))
            } else {
                None
            };
            Ok((ObjectPage {
                objects,
                next_token,
            }, modified))
        })
    }
}

impl ObjectStore for S3Store {
    fn scope(&self) -> Option<ObjectStoreScope> {
        S3Store::scope(self)
    }
    fn capabilities(&self) -> StorageCapabilities {
        S3Store::capabilities(self)
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        S3Store::list(self, prefix)
    }
    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        S3Store::list_page(self, prefix, token)
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            self.backend
                .head(&self.path(key)?)
                .await
                .map(|m| Self::meta(key.into(), m))
                .map_err(map_err)
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            if range.as_ref().is_some_and(|range| range.start >= range.end) {
                return Err(CloudError::InvalidConfig("invalid object range".into()));
            }
            // Shared implementation validates SDK metadata before exposing bytes.
            self.core_read(key, range).await.map(|(_, _, _, body)| body)
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            let mut data = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                data.len()
                    .checked_add(chunk.len())
                    .filter(|size| *size <= MAX_PUT_BYTES)
                    .ok_or(CloudError::PayloadTooLarge)?;
                data.extend_from_slice(&chunk);
            }
            let size = data.len();
            let mode = match condition {
                PutCondition::Any => PutMode::Overwrite,
                PutCondition::CreateOnly => PutMode::Create,
                PutCondition::MatchEtag(etag) => PutMode::Update(object_store::UpdateVersion {
                    e_tag: Some(etag),
                    version: None,
                }),
            };
            let result = self
                .backend
                .put_opts(
                    &self.path(key)?,
                    PutPayload::from(Bytes::from(data)),
                    PutOptions {
                        mode,
                        ..Default::default()
                    },
                )
                .await
                .map_err(map_err)?;
            Ok(ObjectMetadata {
                key: key.into(),
                size: size as u64,
                etag: result.e_tag,
            })
        })
    }

    fn upload_stream<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.upload_stream_with_control(key, body, Arc::new(AtomicBool::new(false)))
    }

    fn upload_stream_with_control<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        cancel: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            const PART_SIZE: usize = 8 * 1024 * 1024;
            const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024 * 1024;
            #[cfg(test)]
            let max_stream_bytes = self
                .multipart_budget
                .unwrap_or(MAX_STREAM_BYTES)
                .min(MAX_STREAM_BYTES);
            #[cfg(not(test))]
            let max_stream_bytes = MAX_STREAM_BYTES;
            validate_key(key)?;
            let mut upload = self
                .begin_multipart(self.path(key)?)
                .await
                .map_err(|source| CloudError::Multipart {
                    operation: "create",
                    source,
                })?;
            let mut pending = Vec::with_capacity(PART_SIZE);
            let mut size = 0usize;
            let mut parts = 0usize;
            let mut inflight = Vec::with_capacity(4);
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(abort_multipart(
                        &mut upload,
                        "abort after cancellation",
                        CloudError::UploadCancelled,
                    )
                    .await);
                }
                match body.next().await {
                    Some(Ok(chunk)) => {
                        let next_size = size
                            .checked_add(chunk.len())
                            .filter(|size| *size <= max_stream_bytes);
                        let Some(next_size) = next_size else {
                            return Err(abort_multipart(
                                &mut upload,
                                "abort after budget exceeded",
                                CloudError::UploadBudgetExceeded,
                            )
                            .await);
                        };
                        size = next_size;
                        // Never copy an entire input chunk into pending: a producer
                        // may yield a very large Bytes. Full parts share its allocation.
                        let mut offset = 0;
                        while offset < chunk.len() {
                            if cancel.load(Ordering::Acquire) {
                                return Err(abort_multipart(
                                    &mut upload,
                                    "abort after cancellation",
                                    CloudError::UploadCancelled,
                                )
                                .await);
                            }
                            let payload = if pending.is_empty() && chunk.len() - offset >= PART_SIZE
                            {
                                let part = chunk.slice(offset..offset + PART_SIZE);
                                offset += PART_SIZE;
                                part
                            } else {
                                let take = (PART_SIZE - pending.len()).min(chunk.len() - offset);
                                pending.extend_from_slice(&chunk[offset..offset + take]);
                                offset += take;
                                #[cfg(test)]
                                self.multipart_peak_buffer
                                    .fetch_max(pending.len(), Ordering::SeqCst);
                                if pending.len() < PART_SIZE {
                                    continue;
                                }
                                Bytes::from(std::mem::replace(
                                    &mut pending,
                                    Vec::with_capacity(PART_SIZE),
                                ))
                            };
                            #[cfg(test)]
                            self.multipart_peak_buffer
                                .fetch_max(payload.len() + pending.len(), Ordering::SeqCst);
                            inflight.push(upload.put_part(PutPayload::from(payload)));
                            #[cfg(test)]
                            self.multipart_peak_buffer.fetch_max(inflight.len() * PART_SIZE + pending.len(), Ordering::SeqCst);
                            if inflight.len() == 4 {
                                let results = futures_util::future::join_all(std::mem::take(&mut inflight)).await;
                                if let Some(source) = results.into_iter().find_map(Result::err) {
                                    let primary = CloudError::Multipart { operation: "upload part", source };
                                    return Err(abort_multipart(&mut upload, "abort after part failure", primary).await);
                                }
                            }
                            parts += 1;
                        }
                    }
                    Some(Err(error)) => {
                        return Err(
                            abort_multipart(&mut upload, "abort after input error", error).await,
                        );
                    }
                    None => break,
                }
            }
            if cancel.load(Ordering::Acquire) {
                return Err(abort_multipart(
                    &mut upload,
                    "abort after cancellation",
                    CloudError::UploadCancelled,
                )
                .await);
            }
            if !pending.is_empty() || parts == 0 {
                #[cfg(test)]
                self.multipart_peak_buffer
                    .fetch_max(pending.len(), Ordering::SeqCst);
                inflight.push(upload.put_part(PutPayload::from(Bytes::from(pending))));
            }
            let results = futures_util::future::join_all(inflight).await;
            if let Some(source) = results.into_iter().find_map(Result::err) {
                let primary = CloudError::Multipart { operation: "upload part", source };
                return Err(abort_multipart(&mut upload, "abort after part failure", primary).await);
            }
            if cancel.load(Ordering::Acquire) {
                return Err(abort_multipart(
                    &mut upload,
                    "abort after cancellation",
                    CloudError::UploadCancelled,
                )
                .await);
            }
            // Completion has an indeterminate result: the provider may have
            // committed the object before the error. Never abort or delete here;
            // report the unknown state so callers record a recovery obligation.
            let result = upload
                .complete()
                .await
                .map_err(|source| CloudError::CompleteUnknown { source })?;
            Ok(ObjectMetadata {
                key: key.into(),
                size: size as u64,
                etag: result.e_tag,
            })
        })
    }

    fn delete_if_match<'a>(&'a self, key: &'a str, etag: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            if etag.trim().is_empty() || etag.trim() == "*"
                || reqwest::header::HeaderValue::from_str(etag).is_err()
            {
                return Err(CloudError::InvalidConfig("valid delete revision is required".into()));
            }
            self.path(key)?;
            self.verify_delete_precondition().await?;
            self.delete_revision_request(key, etag).await
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            self.backend.delete(&self.path(key)?).await.map_err(map_err)
        })
    }
}

#[cfg(test)]
mod cfgtest {
    use super::*;
    use futures_util::stream;
    use object_store::memory::InMemory;
    use object_store::{PutResult, UploadPart};
    use std::fmt;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    type AsyncResult<'a, T> = Pin<Box<dyn Future<Output = object_store::Result<T>> + Send + 'a>>;

    fn store() -> S3Store {
        let backend: Arc<dyn Backend> = Arc::new(InMemory::new());
        let factory: MultipartFactory = Arc::new({
            let backend = Arc::clone(&backend);
            move |path| {
                let backend = Arc::clone(&backend);
                Box::pin(async move { backend.put_multipart(&path).await })
            }
        });
        S3Store::test_store(backend, factory)
    }

    #[test]
    fn custom_endpoint_addressing_never_drops_the_bucket() {
        let parse = |value: &str| url::Url::parse(value).expect("url");
        for endpoint in [
            "http://127.0.0.1:9000",
            "http://localhost:9000",
            "https://minio.example.com:9000",
            "http://[::1]:9000",
        ] {
            assert!(requires_path_style(&parse(endpoint)), "{endpoint}");
        }
        for endpoint in ["https://s3.us-east-1.amazonaws.com", "https://minio.example.com"] {
            assert!(!requires_path_style(&parse(endpoint)), "{endpoint}");
        }
    }

    #[tokio::test]
    async fn read_bridge_put_any_supports_directory_marker_without_cas() {
        let bridge = crate::CoreS3ReadBridge(
            crate::CoreS3Backend::new(Arc::new(store()), 1).expect("backend"),
        );
        let result = ObjectStore::put(
            &bridge,
            "dir/.peerdav-directory",
            stream::iter([Ok(Bytes::new())]).boxed(),
            PutCondition::Any,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn core_adapter_raw_keys_cas_range_and_delete() {
        use crate::CoreS3Backend;
        use peerdav_file_core::*;
        let mut low = store();
        low.config.prefix = "资料/原样%2F/".into();
        low.verified.store(true, Ordering::Release);
        let adapter = CoreS3Backend::new(Arc::new(low), 7).expect("adapter");
        for key in ["中文/100%.txt", "literal/a%2Fb"] {
            let id = adapter.scope().entry(key);
            let write = |condition| WriteRequest {
                entry: id.clone(),
                condition,
                mode: WriteMode::BoundedAtomic { max_bytes: 6 },
                body: stream::iter([Ok(Bytes::from_static(b"abcdef"))]).boxed(),
            };
            let created = adapter
                .write(core_context(), write(Condition::CreateOnly))
                .await
                .expect("create")
                .entry
                .expect("entry");
            assert_eq!(created.id.opaque(), key);
            assert_eq!(
                adapter
                    .write(core_context(), write(Condition::CreateOnly))
                    .await
                    .expect_err("existing")
                    .kind,
                FileErrorKind::Precondition
            );
            assert_eq!(
                adapter
                    .write(
                        core_context(),
                        write(Condition::Match(Revision::new("stale")))
                    )
                    .await
                    .expect_err("stale")
                    .kind,
                FileErrorKind::Precondition
            );
            adapter
                .write(
                    core_context(),
                    write(Condition::Match(created.revision.expect("etag"))),
                )
                .await
                .expect("replace");
            let mut response = adapter
                .read(
                    core_context(),
                    ReadRequest {
                        entry: id.clone(),
                        range: Some(ReadRange::new(2, 99).expect("range")),
                    },
                )
                .await
                .expect("read");
            assert_eq!(response.range, Some(ReadRange::new(2, 6).expect("actual")));
            assert_eq!(
                response.body.next().await.expect("chunk").expect("bytes"),
                Bytes::from_static(b"cdef")
            );
            assert!(response.body.next().await.is_none());
            assert_eq!(
                adapter
                    .stat(core_context(), id.clone())
                    .await
                    .expect("stat")
                    .size,
                Some(6)
            );
            adapter
                .delete(
                    core_context(),
                    DeleteRequest {
                        entry: id.clone(),
                        mode: DeleteMode::File,
                        condition: Condition::Any,
                    },
                )
                .await
                .expect("delete");
            assert_eq!(
                adapter
                    .stat(core_context(), id)
                    .await
                    .expect_err("gone")
                    .kind,
                FileErrorKind::NotFound
            );
        }
    }

    fn core_context() -> peerdav_file_core::OperationContext {
        use std::num::NonZeroUsize;
        peerdav_file_core::OperationContext {
            cancellation: Default::default(),
            deadline: None,
            budget: peerdav_file_core::OperationBudget {
                max_bytes: 100 * 1024 * 1024,
                max_chunk_bytes: NonZeroUsize::new(8 * 1024 * 1024).expect("chunk"),
                max_entries: NonZeroUsize::new(1000).expect("entries"),
            },
        }
    }

    #[tokio::test]
    async fn core_bridge_verified_snapshot_cas_conflicts_and_rebuild() {
        use peerdav_file_core::{FileError, FileErrorKind};
        let low = Arc::new(store());
        let stale = crate::CoreS3ReadBridge(
            crate::CoreS3Backend::new(low.clone(), 7).expect("backend"),
        );
        low.verify_capabilities().await.expect("fixture probe");
        assert!(low.capabilities().conditional_writes_verified);
        assert!(!stale.capabilities().create_only);
        assert!(!stale.capabilities().conditional_replace);
        assert!(!stale.capabilities().conditional_writes_verified);
        for condition in [PutCondition::CreateOnly, PutCondition::MatchEtag("etag".into())] {
            let body = stream::poll_fn(|_| -> std::task::Poll<Option<Result<Bytes, CloudError>>> {
                panic!("old snapshot must not poll body")
            }).boxed();
            let error = stale.put("key", body, condition).await.expect_err("old snapshot");
            let CloudError::Storage { source: object_store::Error::Generic { source, .. } } = error else {
                panic!("expected core preflight error");
            };
            assert_eq!(source.downcast_ref::<FileError>().expect("core error").kind, FileErrorKind::Unsupported);
        }
        // The probe does not manufacture an epoch. Rebuild explicitly, retaining
        // the caller's connection generation when the connection is unchanged.
        let bridge = crate::CoreS3ReadBridge(
            crate::CoreS3Backend::new(low, 7).expect("verified backend"),
        );
        assert!(bridge.capabilities().create_only);
        assert!(bridge.capabilities().conditional_replace);
        assert!(bridge.capabilities().conditional_writes_verified);
        let key = "中文/literal%2F";
        let first = bridge.put(key, body(b"first"), PutCondition::CreateOnly).await.expect("create");
        assert_eq!(first.key, key);
        assert_eq!(first.size, 5);
        assert!(matches!(bridge.put(key, body(b"bad"), PutCondition::CreateOnly).await, Err(CloudError::Precondition)));
        assert!(matches!(bridge.put(key, body(b"bad"), PutCondition::MatchEtag("stale".into())).await, Err(CloudError::Precondition)));
        let replaced = bridge.put(key, body(b"replacement"), PutCondition::MatchEtag(first.etag.expect("etag"))).await.expect("match");
        assert_eq!(replaced.key, key);
        assert_eq!(replaced.size, 11);
        assert!(replaced.etag.is_some());
        assert!(matches!(bridge.replace_edit(key, body(b"stale edit"), "stale").await, Err(CloudError::Precondition)));
        let edited = bridge.replace_edit(key, body(b"edited"), replaced.etag.as_deref().expect("revision")).await.expect("edit CAS");
        assert_eq!(edited.size, 6);
    }

    #[tokio::test]
    async fn core_bridge_bounded_conditions_preserve_limit_and_original_errors() {
        let low = Arc::new(store());
        low.verify_capabilities().await.expect("fixture probe");
        let bridge = crate::CoreS3ReadBridge(
            crate::CoreS3Backend::new(low.clone(), 1).expect("backend"),
        );
        for (index, condition) in [PutCondition::Any, PutCondition::CreateOnly, PutCondition::MatchEtag("etag".into())].into_iter().enumerate() {
            let key = format!("oversized-{index}");
            let body = stream::iter([Ok(Bytes::from(vec![0; MAX_PUT_BYTES + 1]))]).boxed();
            assert!(matches!(bridge.put(&key, body, condition).await, Err(CloudError::PayloadTooLarge)));
            assert!(matches!(low.stat(&key).await, Err(CloudError::NotFound)));
        }
        for condition in [PutCondition::Any, PutCondition::CreateOnly, PutCondition::MatchEtag("etag".into())] {
            let error = bridge.put("source", stream::iter([Err(CloudError::Transport {
                source: reqwest::Client::new().get("not a URL").build().expect_err("invalid URL"),
            })]).boxed(), condition).await.expect_err("body source");
            assert!(matches!(error, CloudError::Transport { source } if source.is_builder()));
        }
    }

    #[tokio::test]
    async fn core_application_mutations_preserve_metadata_cancel_and_abort_variant() {
        let (low, calls, _) = scripted_store(Script {
            abort_failure: Some("abort unavailable"),
            ..Script::default()
        });
        let bridge = crate::CoreS3ReadBridge(crate::CoreS3Backend::new(Arc::new(low), 1).expect("backend"));
        let ordinary = crate::CoreS3ReadBridge(crate::CoreS3Backend::new(Arc::new(store()), 1).expect("backend"));
        let metadata = ordinary.put("raw%/key", stream::iter([Ok(Bytes::from_static(b"abc"))]).boxed(), PutCondition::Any).await.expect("put");
        assert_eq!(metadata.key, "raw%/key");
        assert_eq!(metadata.size, 3);
        ordinary.delete("raw%/key").await.expect("delete");
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let body = stream::once(async move {
            signal.store(true, Ordering::Release);
            Ok(Bytes::from_static(b"cancel"))
        }).boxed();
        let error = bridge.upload_stream_with_control("upload", body, cancel).await.expect_err("cancel");
        assert!(matches!(error, CloudError::MultipartAbort { primary, .. } if matches!(*primary, CloudError::UploadCancelled)));
        assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn core_adapter_unsupported_scope_and_preflight_never_poll_body() {
        use peerdav_file_core::*;
        let low = Arc::new(store());
        let adapter = crate::CoreS3Backend::new(low.clone(), 1).expect("adapter");
        let other = crate::CoreS3Backend::new(low, 2).expect("adapter");
        let id = adapter.scope().entry("key");
        let forbidden = || {
            stream::poll_fn(|_| -> std::task::Poll<Option<Result<Bytes, FileError>>> {
                panic!("unsupported body polled")
            })
            .boxed()
        };
        for (condition, mode) in [
            (Condition::Match(Revision::new("x")), WriteMode::Streaming),
            (
                Condition::CreateOnly,
                WriteMode::BoundedAtomic { max_bytes: 6 },
            ),
            (
                Condition::Any,
                WriteMode::BoundedAtomic {
                    max_bytes: MAX_PUT_BYTES as u64 + 1,
                },
            ),
        ] {
            assert_eq!(
                adapter
                    .write(
                        core_context(),
                        WriteRequest {
                            entry: id.clone(),
                            condition,
                            mode,
                            body: forbidden()
                        }
                    )
                    .await
                    .expect_err("unsupported")
                    .kind,
                FileErrorKind::Unsupported
            );
        }
        assert_eq!(
            other
                .write(
                    core_context(),
                    WriteRequest {
                        entry: id.clone(),
                        condition: Condition::Any,
                        mode: WriteMode::Streaming,
                        body: forbidden()
                    }
                )
                .await
                .expect_err("generation")
                .kind,
            FileErrorKind::ScopeMismatch
        );
        assert_eq!(
            adapter
                .create_dir(
                    core_context(),
                    CreateDirRequest {
                        entry: id.clone(),
                        condition: Condition::Any
                    }
                )
                .await
                .expect_err("mkdir")
                .kind,
            FileErrorKind::Unsupported
        );
        assert_eq!(
            adapter
                .move_entry(
                    core_context(),
                    MoveRequest {
                        source: id.clone(),
                        destination: id.clone(),
                        source_condition: Condition::Any,
                        destination_condition: Condition::Any,
                        atomicity: MoveAtomicity::Atomic
                    }
                )
                .await
                .expect_err("move")
                .kind,
            FileErrorKind::Unsupported
        );
        for (mode, condition) in [
            (DeleteMode::File, Condition::Match(Revision::new("x"))),
            (DeleteMode::Recursive, Condition::Any),
            (DeleteMode::EmptyDirectory, Condition::Any),
        ] {
            assert_eq!(
                adapter
                    .delete(
                        core_context(),
                        DeleteRequest {
                            entry: id.clone(),
                            mode,
                            condition
                        }
                    )
                    .await
                    .expect_err("delete unsupported")
                    .kind,
                FileErrorKind::Unsupported
            );
        }
        let context = core_context();
        context.cancellation.cancel();
        assert_eq!(
            adapter
                .write(
                    context,
                    WriteRequest {
                        entry: id.clone(),
                        condition: Condition::Any,
                        mode: WriteMode::Streaming,
                        body: forbidden()
                    }
                )
                .await
                .expect_err("cancel")
                .kind,
            FileErrorKind::Cancelled
        );
        let mut context = core_context();
        context.deadline = Some(std::time::Instant::now());
        assert_eq!(
            adapter
                .write(
                    context,
                    WriteRequest {
                        entry: id,
                        condition: Condition::Any,
                        mode: WriteMode::Streaming,
                        body: forbidden()
                    }
                )
                .await
                .expect_err("deadline")
                .kind,
            FileErrorKind::Deadline
        );
    }

    #[tokio::test]
    async fn core_adapter_bounded_budget_and_source_preserved() {
        use peerdav_file_core::*;
        use std::error::Error;
        let adapter = crate::CoreS3Backend::new(Arc::new(store()), 1).expect("adapter");
        let id = adapter.scope().entry("key");
        let error = adapter
            .write(
                core_context(),
                WriteRequest {
                    entry: id.clone(),
                    condition: Condition::Any,
                    mode: WriteMode::BoundedAtomic { max_bytes: 3 },
                    body: stream::iter([Ok(Bytes::from_static(b"four"))]).boxed(),
                },
            )
            .await
            .expect_err("limit");
        assert_eq!(error.kind, FileErrorKind::BudgetExceeded);
        assert_eq!(error.outcome, MutationOutcome::NotApplied);
        assert!(error.source().is_some());
        assert_eq!(
            adapter
                .stat(core_context(), id.clone())
                .await
                .expect_err("never published")
                .kind,
            FileErrorKind::NotFound
        );
        let error = FileError::new(
            FileErrorKind::Transport,
            Operation::Read,
            MutationOutcome::NotApplied,
        )
        .with_source(Box::new(TestCause("original input failure")));
        let error = adapter
            .write(
                core_context(),
                WriteRequest {
                    entry: id,
                    condition: Condition::Any,
                    mode: WriteMode::BoundedAtomic { max_bytes: 3 },
                    body: stream::iter([Err(error)]).boxed(),
                },
            )
            .await
            .expect_err("source");
        let mut source: &(dyn Error + 'static) = &error;
        while let Some(next) = source.source() {
            source = next;
        }
        assert_eq!(source.to_string(), "original input failure");
    }

    #[tokio::test]
    async fn core_adapter_stream_cancel_deadline_abort_and_unknown() {
        use peerdav_file_core::*;
        for deadline in [false, true] {
            let (low, calls, deletes) = scripted_store(Script {
                abort_failure: Some("abort unavailable"),
                ..Script::default()
            });
            let adapter = crate::CoreS3Backend::new(Arc::new(low), 1).expect("adapter");
            let mut context = core_context();
            let cancel = context.cancellation.clone();
            if deadline {
                context.deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(10));
            }
            let body = stream::once(async move {
                if !deadline {
                    cancel.cancel();
                }
                std::future::pending::<Result<Bytes, FileError>>().await
            })
            .boxed();
            let error = adapter
                .write(
                    context,
                    WriteRequest {
                        entry: adapter.scope().entry("key"),
                        condition: Condition::Any,
                        mode: WriteMode::Streaming,
                        body,
                    },
                )
                .await
                .expect_err("interrupted");
            assert_eq!(
                error.kind,
                if deadline {
                    FileErrorKind::Deadline
                } else {
                    FileErrorKind::Cancelled
                }
            );
            assert_eq!(error.cleanup.len(), 1);
            assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
            assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
            assert_eq!(deletes.load(Ordering::SeqCst), 0);
        }
        let (low, calls, _) = scripted_store(Script {
            complete_failure: Some("lost completion response"),
            ..Script::default()
        });
        let adapter = crate::CoreS3Backend::new(Arc::new(low), 1).expect("adapter");
        let error = adapter
            .write(
                core_context(),
                WriteRequest {
                    entry: adapter.scope().entry("key"),
                    condition: Condition::Any,
                    mode: WriteMode::Streaming,
                    body: stream::iter([Ok(Bytes::from_static(b"data"))]).boxed(),
                },
            )
            .await
            .expect_err("unknown");
        assert_eq!(error.outcome, MutationOutcome::Unknown);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 1);
        assert_eq!(calls.abort.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn core_adapter_list_request_binding_and_budget_before_io() {
        use peerdav_file_core::*;
        use std::num::NonZeroUsize;
        let adapter = crate::CoreS3Backend::new(Arc::new(store()), 8).expect("adapter");
        let request = ListRequest {
            parent: adapter.scope().entry("资料/100%/"),
            query: None,
            prefix: Some("a%2F".into()),
            cursor: None,
            limit: NonZeroUsize::new(3).expect("limit"),
        };
        let mut query = request.clone();
        query.query = Some("unsupported search".into());
        assert_eq!(
            adapter
                .list(core_context(), query)
                .await
                .expect_err("query")
                .kind,
            FileErrorKind::Unsupported
        );
        let mut context = core_context();
        context.budget.max_entries = NonZeroUsize::new(2).expect("budget");
        assert_eq!(
            adapter
                .list(context, request.clone())
                .await
                .expect_err("budget")
                .kind,
            FileErrorKind::BudgetExceeded
        );
        let mut changed = request.clone();
        changed.cursor = Some(ListCursor::new(&request, "token".into()).expect("cursor"));
        changed.limit = NonZeroUsize::new(4).expect("changed limit");
        assert_eq!(
            adapter
                .list(core_context(), changed)
                .await
                .expect_err("bound cursor")
                .kind,
            FileErrorKind::InvalidInput
        );
        let context = core_context();
        context.cancellation.cancel();
        assert_eq!(
            adapter
                .list(context, request)
                .await
                .expect_err("cancel before unsupported in-memory pagination")
                .kind,
            FileErrorKind::Cancelled
        );
    }

    #[tokio::test]
    async fn core_adapter_read_budget_and_streaming_success() {
        use peerdav_file_core::*;
        let adapter = crate::CoreS3Backend::new(Arc::new(store()), 1).expect("adapter");
        let id = adapter.scope().entry("stream%2Ffile");
        let result = adapter
            .write(
                core_context(),
                WriteRequest {
                    entry: id.clone(),
                    condition: Condition::Any,
                    mode: WriteMode::Streaming,
                    body: stream::iter([Ok(Bytes::from_static(b"payload"))]).boxed(),
                },
            )
            .await
            .expect("stream");
        assert_eq!(result.entry.expect("entry").size, Some(7));
        let mut context = core_context();
        context.budget.max_bytes = 3;
        let mut response = adapter
            .read(
                context,
                ReadRequest {
                    entry: id,
                    range: None,
                },
            )
            .await
            .expect("response");
        assert_eq!(
            response
                .body
                .next()
                .await
                .expect("error")
                .expect_err("budget")
                .kind,
            FileErrorKind::BudgetExceeded
        );
        assert!(response.body.next().await.is_none());
    }

    #[test]
    fn raw_key_path_api_contract() {
        // object_store 0.14.2: From encodes path parts; parse accepts raw keys.
        assert_eq!(
            ObjectPath::from("图片/文件 100%.txt").as_ref(),
            "%E5%9B%BE%E7%89%87/%E6%96%87%E4%BB%B6 100%25.txt"
        );
        for key in ["图片/文件 100%.txt", "%E5%9B%BE%E7%89%87/a%2Fb"] {
            assert_eq!(ObjectPath::parse(key).expect("raw path").as_ref(), key);
        }
    }

    #[test]
    fn raw_key_listing_prefix_and_validation() {
        let mut store = store();
        store.config.prefix = "资料/嵌套%2F/".into();
        for prefix in [
            "",
            "图片/",
            "space dir/",
            "%E5%9B%BE%E7%89%87/",
            "literal%/a%2F",
        ] {
            assert_eq!(
                store.list_prefix(prefix).expect("list prefix"),
                format!("{}{prefix}", store.config.prefix)
            );
        }
        for key in [
            "",
            "/escape",
            "../escape",
            "a/../b",
            "a/./b",
            "a//b",
            "a\\b",
            "a\0b",
            "a:b",
        ] {
            assert!(validate_key(key).is_err(), "accepted unsafe key {key:?}");
        }
        assert!(store.path("invalid\nb").is_err());
    }

    #[tokio::test]
    async fn raw_key_backend_roundtrip() {
        for root in ["", "资料/嵌套 空格%2F/"] {
            let mut store = store();
            store.config.prefix = root.into();
            for key in [
                "图片/中文 文件.txt",
                "space dir/100%.txt",
                "literal%/a%2Fb",
                "%E5%9B%BE%E7%89%87/old%20file",
            ] {
                store
                    .put(key, body(b"raw"), PutCondition::Any)
                    .await
                    .expect("put");
                let raw = ObjectPath::parse(format!("{root}{key}")).expect("raw key");
                assert_eq!(
                    store
                        .backend
                        .head(&raw)
                        .await
                        .expect("actual stored raw key")
                        .location,
                    raw
                );
                assert_eq!(store.stat(key).await.expect("stat").key, key);
                let bytes = store
                    .get(key, None)
                    .await
                    .expect("get")
                    .map_ok(|chunk| chunk.to_vec())
                    .try_concat()
                    .await
                    .expect("read");
                assert_eq!(bytes.as_slice(), b"raw");
                let prefix = format!("{}/", key.split('/').next().expect("directory"));
                let listed = store
                    .list(&prefix)
                    .try_collect::<Vec<_>>()
                    .await
                    .expect("list directory");
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].key, key);
                store.delete(key).await.expect("delete");
                assert!(matches!(
                    store.backend.head(&raw).await,
                    Err(object_store::Error::NotFound { .. })
                ));
            }
        }
    }

    #[tokio::test]
    async fn raw_key_existing_encoded_objects_remain_literal() {
        let store = store();
        for key in [
            "图片/文件.txt",
            "%E5%9B%BE%E7%89%87/%E6%96%87%E4%BB%B6.txt",
            "图片/a%2Fb",
        ] {
            store
                .backend
                .put(
                    &ObjectPath::parse(key).expect("raw key"),
                    PutPayload::from_static(key.as_bytes()),
                )
                .await
                .expect("seed literal provider key");
        }
        let listed = store
            .list("")
            .try_collect::<Vec<_>>()
            .await
            .expect("list root");
        assert_eq!(listed.len(), 3);
        for meta in listed {
            let data = store
                .get(&meta.key, None)
                .await
                .expect("get listed literal key")
                .map_ok(|chunk| chunk.to_vec())
                .try_concat()
                .await
                .expect("read");
            assert_eq!(data.as_slice(), meta.key.as_bytes());
        }
    }

    #[tokio::test]
    async fn raw_key_multipart_uses_same_boundary() {
        let mut store = store();
        store.config.prefix = "资料/嵌套%/".into();
        let key = "图片/文件 空格%2F.txt";
        store
            .upload_stream(key, body(b"multipart"))
            .await
            .expect("upload");
        let raw = ObjectPath::parse(format!("{}{key}", store.config.prefix)).expect("raw path");
        assert_eq!(
            store
                .backend
                .head(&raw)
                .await
                .expect("multipart raw location")
                .location,
            raw
        );
        assert_eq!(store.stat(key).await.expect("stat").size, 9);
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct Script {
        part_failure: Option<&'static str>,
        complete_failure: Option<&'static str>,
        abort_failure: Option<&'static str>,
    }

    #[derive(Debug, Default)]
    struct UploadCalls {
        put_parts: AtomicUsize,
        complete: AtomicUsize,
        abort: AtomicUsize,
        part_sizes: std::sync::Mutex<Vec<usize>>,
        inflight: AtomicUsize,
        peak_inflight: AtomicUsize,
        verified_bytes: AtomicUsize,
        verify_pattern: AtomicBool,
        cancel_after_part: std::sync::Mutex<Option<Arc<AtomicBool>>>,
    }

    #[derive(Debug)]
    struct ScriptedUpload {
        script: Script,
        calls: Arc<UploadCalls>,
    }

    impl MultipartUpload for ScriptedUpload {
        fn put_part(&mut self, data: PutPayload) -> UploadPart {
            self.calls.put_parts.fetch_add(1, Ordering::SeqCst);
            self.calls
                .part_sizes
                .lock()
                .expect("sizes")
                .push(data.content_length());
            let calls = Arc::clone(&self.calls);
            let failure = self.script.part_failure;
            Box::pin(async move {
                let active = calls.inflight.fetch_add(1, Ordering::SeqCst) + 1;
                calls.peak_inflight.fetch_max(active, Ordering::SeqCst);
                let mut offset = calls.verified_bytes.load(Ordering::SeqCst);
                for chunk in data.iter() {
                    if calls.verify_pattern.load(Ordering::SeqCst) {
                        for (i, byte) in chunk.iter().enumerate() {
                            assert_eq!(*byte, ((offset + i) % 251) as u8);
                        }
                    }
                    offset += chunk.len();
                }
                calls.verified_bytes.store(offset, Ordering::SeqCst);
                tokio::task::yield_now().await;
                if let Some(cancel) = calls.cancel_after_part.lock().expect("cancel").as_ref() {
                    cancel.store(true, Ordering::Release);
                }
                calls.inflight.fetch_sub(1, Ordering::SeqCst);
                match failure {
                    Some(message) => Err(test_error(message)),
                    None => Ok(()),
                }
            })
        }

        fn complete<'life0, 'async_trait>(&'life0 mut self) -> AsyncResult<'async_trait, PutResult>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
        {
            let calls = Arc::clone(&self.calls);
            calls.complete.fetch_add(1, Ordering::SeqCst);
            let failure = self.script.complete_failure;
            Box::pin(async move {
                match failure {
                    Some(message) => Err(test_error(message)),
                    None => Ok(PutResult {
                        e_tag: Some("scripted-etag".into()),
                        version: None,
                        extensions: Default::default(),
                    }),
                }
            })
        }

        fn abort<'life0, 'async_trait>(&'life0 mut self) -> AsyncResult<'async_trait, ()>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
        {
            let calls = Arc::clone(&self.calls);
            calls.abort.fetch_add(1, Ordering::SeqCst);
            let failure = self.script.abort_failure;
            Box::pin(async move {
                match failure {
                    Some(message) => Err(test_error(message)),
                    None => Ok(()),
                }
            })
        }
    }

    #[derive(Debug)]
    struct TestCause(&'static str);

    impl fmt::Display for TestCause {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for TestCause {}

    fn test_error(message: &'static str) -> object_store::Error {
        object_store::Error::Generic {
            store: "scripted",
            source: Box::new(TestCause(message)),
        }
    }

    /// Counts backend deletes so failure paths can prove they never remove
    /// existing objects. The scripted factory bypasses every other backend call.
    #[derive(Debug)]
    struct DeleteSpy {
        deletes: Arc<AtomicUsize>,
    }

    impl fmt::Display for DeleteSpy {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("DeleteSpy")
        }
    }

    impl Backend for DeleteSpy {
        fn put_opts<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _location: &'life1 ObjectPath,
            _payload: PutPayload,
            _opts: PutOptions,
        ) -> AsyncResult<'async_trait, PutResult>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
            'life1: 'async_trait,
        {
            Box::pin(async { Err(test_error("unexpected put")) })
        }

        fn put_multipart_opts<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _location: &'life1 ObjectPath,
            _opts: object_store::PutMultipartOptions,
        ) -> AsyncResult<'async_trait, Box<dyn MultipartUpload>>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
            'life1: 'async_trait,
        {
            Box::pin(async { Err(test_error("unexpected multipart")) })
        }

        fn get_opts<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _location: &'life1 ObjectPath,
            _options: GetOptions,
        ) -> AsyncResult<'async_trait, object_store::GetResult>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
            'life1: 'async_trait,
        {
            Box::pin(async { Err(test_error("unexpected get")) })
        }

        fn delete_stream(
            &self,
            locations: futures_util::stream::BoxStream<'static, object_store::Result<ObjectPath>>,
        ) -> futures_util::stream::BoxStream<'static, object_store::Result<ObjectPath>> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            locations
        }

        fn list(
            &self,
            _prefix: Option<&ObjectPath>,
        ) -> futures_util::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            stream::empty().boxed()
        }

        fn list_with_delimiter<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _prefix: Option<&'life1 ObjectPath>,
        ) -> AsyncResult<'async_trait, object_store::ListResult>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
            'life1: 'async_trait,
        {
            Box::pin(async { Err(test_error("unexpected list_with_delimiter")) })
        }

        fn copy_opts<'life0, 'life1, 'life2, 'async_trait>(
            &'life0 self,
            _from: &'life1 ObjectPath,
            _to: &'life2 ObjectPath,
            _options: object_store::CopyOptions,
        ) -> AsyncResult<'async_trait, ()>
        where
            Self: 'async_trait,
            'life0: 'async_trait,
            'life1: 'async_trait,
            'life2: 'async_trait,
        {
            Box::pin(async { Err(test_error("unexpected copy")) })
        }
    }

    fn scripted_store(script: Script) -> (S3Store, Arc<UploadCalls>, Arc<AtomicUsize>) {
        let calls = Arc::new(UploadCalls::default());
        let deletes = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn Backend> = Arc::new(DeleteSpy {
            deletes: Arc::clone(&deletes),
        });
        let factory: MultipartFactory = Arc::new({
            let calls = Arc::clone(&calls);
            move |_path| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    Ok(Box::new(ScriptedUpload { script, calls }) as Box<dyn MultipartUpload>)
                })
            }
        });
        (S3Store::test_store(backend, factory), calls, deletes)
    }

    fn chunks(bytes: Vec<u8>, size: usize) -> ObjectStream {
        let chunks = bytes
            .chunks(size)
            .map(|chunk| Bytes::copy_from_slice(chunk))
            .collect::<Vec<_>>();
        stream::iter(chunks.into_iter().map(Ok)).boxed()
    }

    // Generated on demand; neither the producer nor verifier holds the whole object.
    fn generated(total: usize, chunk_size: usize) -> ObjectStream {
        stream::unfold(0usize, move |offset| async move {
            if offset == total {
                return None;
            }
            let end = (offset + chunk_size).min(total);
            let chunk: Vec<u8> = (offset..end).map(|i| (i % 251) as u8).collect();
            Some((Ok(Bytes::from(chunk)), end))
        })
        .boxed()
    }

    #[tokio::test]
    async fn multipart_generated_large_stream_has_bounded_parts_and_buffers() {
        const PART: usize = 8 * 1024 * 1024;
        let total = 65 * 1024 * 1024 + 1;
        let (store, calls, _) = scripted_store(Script::default());
        calls.verify_pattern.store(true, Ordering::SeqCst);
        let metadata = store
            .upload_stream("generated", generated(total, 1024 * 1024 + 17))
            .await
            .expect("upload");
        assert_eq!(metadata.size, total as u64);
        assert_eq!(calls.verified_bytes.load(Ordering::SeqCst), total);
        let sizes = calls.part_sizes.lock().expect("sizes");
        assert_eq!(&sizes[..8], &[PART; 8]);
        assert_eq!(sizes[8], 1024 * 1024 + 1);
        assert_eq!(calls.peak_inflight.load(Ordering::SeqCst), 4);
        let peak = store.multipart_peak_buffer.load(Ordering::SeqCst);
        assert!(peak <= 5 * PART);
        eprintln!(
            "multipart resource: total={total} part_sizes={sizes:?} peak_buffer_and_payload={peak} peak_inflight=4 (excludes producer-owned Bytes and backend/runtime RSS)"
        );
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status
                .lines()
                .filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:"))
            {
                eprintln!("multipart process resource: {line}");
            }
        }
    }

    #[tokio::test]
    async fn multipart_large_input_bytes_are_sliced_without_whole_chunk_buffering() {
        const PART: usize = 8 * 1024 * 1024;
        let total = 3 * PART + 37;
        let (store, calls, _) = scripted_store(Script::default());
        calls.verify_pattern.store(true, Ordering::SeqCst);
        store
            .upload_stream("single-chunk", generated(total, total))
            .await
            .expect("upload");
        assert_eq!(
            *calls.part_sizes.lock().expect("sizes"),
            vec![PART, PART, PART, 37]
        );
        assert_eq!(calls.verified_bytes.load(Ordering::SeqCst), total);
        assert!(store.multipart_peak_buffer.load(Ordering::SeqCst) <= 4 * PART);
    }

    #[tokio::test]
    async fn multipart_two_gib_budget_boundary_using_small_test_limit() {
        // Same production comparison, injected downwards only; no 2 GiB allocation.
        for (total, succeeds) in [(1024, true), (1025, false)] {
            let (mut store, calls, deletes) = scripted_store(Script::default());
            store.multipart_budget = Some(1024);
            let result = store.upload_stream("budget", generated(total, 257)).await;
            if succeeds {
                assert_eq!(result.expect("exact budget").size, 1024);
                assert_eq!(calls.complete.load(Ordering::SeqCst), 1);
                assert_eq!(calls.abort.load(Ordering::SeqCst), 0);
            } else {
                assert!(matches!(result, Err(CloudError::UploadBudgetExceeded)));
                assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
                assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
            }
            assert_eq!(deletes.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn multipart_cancellation_after_full_or_final_part_blocks_complete() {
        for total in [8 * 1024 * 1024 + 1, 17, 0] {
            let (store, calls, deletes) = scripted_store(Script::default());
            let cancel = Arc::new(AtomicBool::new(false));
            *calls.cancel_after_part.lock().expect("cancel") = Some(Arc::clone(&cancel));
            let result = store
                .upload_stream_with_control("cancel-part", generated(total, total.max(1)), cancel)
                .await;
            assert!(matches!(result, Err(CloudError::UploadCancelled)));
            assert!((1..=4).contains(&calls.put_parts.load(Ordering::SeqCst)));
            assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
            assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
            assert_eq!(deletes.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn multipart_empty_stream_cancelled_on_eof_aborts_without_part() {
        let (store, calls, deletes) = scripted_store(Script::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let input_cancel = Arc::clone(&cancel);
        let input = stream::poll_fn(move |_| {
            input_cancel.store(true, Ordering::Release);
            std::task::Poll::Ready(None)
        })
        .boxed();
        let result = store
            .upload_stream_with_control("empty-cancel", input, cancel)
            .await;
        assert!(matches!(result, Err(CloudError::UploadCancelled)));
        assert_eq!(calls.put_parts.load(Ordering::SeqCst), 0);
        assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn multipart_upload_stream_preserves_65mib_plus_one_bytes_and_size() {
        let store = store();
        let data = vec![0x5a; 65 * 1024 * 1024 + 1];
        let metadata = store
            .upload_stream("large", chunks(data.clone(), 1024 * 1024))
            .await
            .expect("multipart upload");
        assert_eq!(metadata.size, data.len() as u64);
        let mut received = Vec::new();
        let mut body = store.get("large", None).await.expect("get");
        while let Some(chunk) = body.next().await {
            received.extend_from_slice(&chunk.expect("body"));
        }
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn multipart_put_rejects_65mib_plus_one_bytes() {
        let store = store();
        let data = vec![0x11; MAX_PUT_BYTES + 1];
        let error = store
            .put("too-large", chunks(data, 1024 * 1024), PutCondition::Any)
            .await
            .expect_err("bounded put must reject");
        assert!(matches!(error, CloudError::PayloadTooLarge));
    }

    #[tokio::test]
    async fn multipart_cancel_before_first_part_aborts() {
        let store = store();
        let cancel = Arc::new(AtomicBool::new(true));
        let error = store
            .upload_stream_with_control("cancel-first", chunks(vec![1; 1024], 1024), cancel)
            .await
            .expect_err("cancel");
        assert!(matches!(error, CloudError::UploadCancelled));
    }

    #[tokio::test]
    async fn multipart_input_failure_aborts() {
        let store = store();
        let input = stream::iter(vec![
            Ok(Bytes::from(vec![2; 1024])),
            Err(CloudError::NotFound),
        ])
        .boxed();
        let error = store
            .upload_stream("input-failure", input)
            .await
            .expect_err("input failure");
        assert!(matches!(error, CloudError::NotFound));
    }

    #[tokio::test]
    async fn multipart_cancel_after_input_aborts() {
        let store = store();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_input = Arc::clone(&cancel);
        let input = stream::iter(vec![Ok(Bytes::from(vec![3; 1024]))])
            .chain(stream::once(async move {
                cancel_input.store(true, Ordering::Release);
                Ok(Bytes::new())
            }))
            .boxed();
        let error = store
            .upload_stream_with_control("cancel-after-input", input, cancel)
            .await
            .expect_err("cancel");
        assert!(matches!(error, CloudError::UploadCancelled));
    }

    #[tokio::test]
    async fn multipart_part_failure_aborts_without_touching_existing_object() {
        let (store, calls, deletes) = scripted_store(Script {
            part_failure: Some("part upload failed"),
            ..Default::default()
        });
        let error = store
            .upload_stream(
                "part-failure",
                chunks(vec![0x31; 8 * 1024 * 1024], 1024 * 1024),
            )
            .await
            .expect_err("part failure");
        match error {
            CloudError::Multipart { operation, source } => {
                assert_eq!(operation, "upload part");
                assert!(source.to_string().contains("part upload failed"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(calls.put_parts.load(Ordering::SeqCst), 1);
        assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn multipart_part_and_abort_failure_preserve_both_causes() {
        let (store, calls, deletes) = scripted_store(Script {
            part_failure: Some("part upload failed"),
            abort_failure: Some("abort failed"),
            ..Default::default()
        });
        let error = store
            .upload_stream(
                "part-abort-failure",
                chunks(vec![0x32; 8 * 1024 * 1024], 1024 * 1024),
            )
            .await
            .expect_err("part and abort failure");
        let abort_source = std::error::Error::source(&error)
            .expect("abort error is retained as source")
            .to_string();
        assert!(abort_source.contains("abort failed"));
        match error {
            CloudError::MultipartAbort {
                operation,
                primary,
                abort,
            } => {
                assert_eq!(operation, "abort after part failure");
                match *primary {
                    CloudError::Multipart { operation, source } => {
                        assert_eq!(operation, "upload part");
                        assert!(source.to_string().contains("part upload failed"));
                    }
                    other => panic!("unexpected primary error: {other:?}"),
                }
                assert!(abort.to_string().contains("abort failed"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn multipart_cancel_and_abort_failure_keep_cancellation_semantics() {
        let (store, calls, deletes) = scripted_store(Script {
            abort_failure: Some("abort failed"),
            ..Default::default()
        });
        let cancel = Arc::new(AtomicBool::new(true));
        let error = store
            .upload_stream_with_control(
                "cancel-abort-failure",
                chunks(vec![0x33; 1024], 1024),
                cancel,
            )
            .await
            .expect_err("cancel and abort failure");
        match error {
            CloudError::MultipartAbort {
                operation,
                primary,
                abort,
            } => {
                assert_eq!(operation, "abort after cancellation");
                assert!(matches!(primary.as_ref(), CloudError::UploadCancelled));
                assert!(abort.to_string().contains("abort failed"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(calls.put_parts.load(Ordering::SeqCst), 0);
        assert_eq!(calls.abort.load(Ordering::SeqCst), 1);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 0);
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn multipart_complete_failure_is_unknown_and_never_aborts_or_deletes() {
        let (store, calls, deletes) = scripted_store(Script {
            complete_failure: Some("complete failed"),
            ..Default::default()
        });
        let error = store
            .upload_stream("complete-failure", chunks(vec![0x34; 1024], 1024))
            .await
            .expect_err("complete failure");
        match error {
            CloudError::CompleteUnknown { source } => {
                assert!(source.to_string().contains("complete failed"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(calls.put_parts.load(Ordering::SeqCst), 1);
        assert_eq!(calls.complete.load(Ordering::SeqCst), 1);
        assert_eq!(calls.abort.load(Ordering::SeqCst), 0);
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
    }
}
