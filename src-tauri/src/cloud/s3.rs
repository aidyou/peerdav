use std::net::IpAddr;
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, future::BoxFuture, stream};
use object_store::path::Path as ObjectPath;
use object_store::{
    GetOptions, GetRange, ObjectStore as Backend, ObjectStoreExt, PutMode, PutOptions, PutPayload,
};
use rand::RngCore;

use super::proxy::ConnectionTransport;
use super::traits::ObjectStore;
use super::types::{
    CloudError, MAX_PUT_BYTES, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, S3Config,
    StorageCapabilities,
};

#[derive(Clone)]
pub struct S3Store {
    config: S3Config,
    backend: Arc<dyn Backend>,
    verified: Arc<AtomicBool>,
}

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
            backend: Arc::new(backend),
            verified: Arc::new(AtomicBool::new(false)),
        })
    }

    fn path(&self, key: &str) -> ObjectPath {
        ObjectPath::from(format!("{}{}", self.config.prefix, key))
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

fn is_private(host: Option<&str>) -> bool {
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

fn map_err(source: object_store::Error) -> CloudError {
    match source {
        object_store::Error::Precondition { .. } | object_store::Error::AlreadyExists { .. } => {
            CloudError::Precondition
        }
        object_store::Error::NotFound { .. } => CloudError::NotFound,
        source => CloudError::Storage { source },
    }
}

impl ObjectStore for S3Store {
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

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            validate_key(key)?;
            self.backend.delete(&self.path(key)).await.map_err(map_err)
        })
    }
}
