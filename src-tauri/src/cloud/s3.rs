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

#[derive(Clone)]
pub struct S3Store {
    config: S3Config,
    backend: Arc<dyn Backend>,
    paginated: Option<Arc<object_store::aws::AmazonS3>>,
    verified: Arc<AtomicBool>,
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
        let transport = ConnectionTransport::new(&config.proxy)?;
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_region(&config.region)
            .with_bucket_name(&config.bucket)
            .with_access_key_id(&config.access_key)
            .with_secret_access_key(&config.secret_key)
            .with_endpoint(&config.endpoint)
            .with_virtual_hosted_style_request(!config.path_style)
            .with_allow_http(url.scheme() == "http")
            .with_http_connector(transport);
        if let Some(token) = &config.session_token {
            builder = builder.with_token(token);
        }
        let backend = builder.build().map_err(map_err)?;
        Ok(Self {
            config,
            backend: Arc::new(backend.clone()),
            paginated: Some(Arc::new(backend)),
            verified: Arc::new(AtomicBool::new(false)),
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
            verified: Arc::new(AtomicBool::new(false)),
            multipart_factory: Some(factory),
            multipart_budget: None,
            multipart_peak_buffer: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn path(&self, key: &str) -> ObjectPath {
        ObjectPath::from(format!("{}{}", self.config.prefix, key))
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

pub(super) fn is_private(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host.trim_matches(['[', ']']);
        host == "localhost"
            || host.parse::<IpAddr>().is_ok_and(|ip| match ip {
                IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
                IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
            })
    })
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

impl ObjectStore for S3Store {
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
        let path = self.path(prefix);
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
                    Some(&self.path(prefix).to_string()),
                    PaginatedListOptions {
                        max_keys: Some(PAGE_LIMIT),
                        page_token: (!state.provider_token.is_empty())
                            .then_some(state.provider_token.clone()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(map_err)?;
            let root = self.config.prefix.clone();
            let mut objects = Vec::with_capacity(result.result.objects.len());
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
            Ok(ObjectPage {
                objects,
                next_token,
            })
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            self.backend
                .head(&self.path(key))
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
            let opts = GetOptions {
                range: range.map(GetRange::Bounded),
                ..Default::default()
            };
            let result = self
                .backend
                .get_opts(&self.path(key), opts)
                .await
                .map_err(map_err)?;
            Ok(result.into_stream().map_err(map_err).boxed())
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
                    &self.path(key),
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
                .begin_multipart(self.path(key))
                .await
                .map_err(|source| CloudError::Multipart {
                    operation: "create",
                    source,
                })?;
            let mut pending = Vec::with_capacity(PART_SIZE);
            let mut size = 0usize;
            let mut parts = 0usize;
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
                            if let Err(source) = upload.put_part(PutPayload::from(payload)).await {
                                let primary = CloudError::Multipart {
                                    operation: "upload part",
                                    source,
                                };
                                return Err(abort_multipart(
                                    &mut upload,
                                    "abort after part failure",
                                    primary,
                                )
                                .await);
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
                if let Err(source) = upload
                    .put_part(PutPayload::from(Bytes::from(pending)))
                    .await
                {
                    let primary = CloudError::Multipart {
                        operation: "upload final part",
                        source,
                    };
                    return Err(abort_multipart(
                        &mut upload,
                        "abort after final part failure",
                        primary,
                    )
                    .await);
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

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            self.backend.delete(&self.path(key)).await.map_err(map_err)
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
        assert_eq!(calls.peak_inflight.load(Ordering::SeqCst), 1);
        let peak = store.multipart_peak_buffer.load(Ordering::SeqCst);
        assert!(peak <= 2 * PART);
        eprintln!(
            "multipart resource: total={total} part_sizes={sizes:?} peak_buffer_and_payload={peak} peak_inflight=1 (excludes producer-owned Bytes and backend/runtime RSS)"
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
        assert!(store.multipart_peak_buffer.load(Ordering::SeqCst) <= PART);
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
            assert_eq!(calls.put_parts.load(Ordering::SeqCst), 1);
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
