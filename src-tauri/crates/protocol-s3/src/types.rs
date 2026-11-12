use std::fmt;

use bytes::Bytes;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ObjectStream = BoxStream<'static, Result<Bytes, CloudError>>;
pub type ObjectListing<'a> = BoxStream<'a, Result<ObjectMetadata, CloudError>>;

#[derive(Clone, Debug, Default)]
pub struct ObjectPage {
    pub objects: Vec<ObjectMetadata>,
    pub next_token: Option<String>,
}

/// Opaque continuation state bound to one prefix and pagination budget.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectPageToken {
    pub prefix: String,
    pub provider_token: String,
    pub pages: usize,
    pub entries: usize,
    /// Stable non-secret connection scope; this DTO is transport state, not authorization.
    #[serde(default)]
    pub scope_fingerprint: String,
    /// Bounded provider-token history used to reject pagination cycles.
    #[serde(default)]
    pub seen_tokens: Vec<String>,
}

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("cloud configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("cloud transport failed")]
    Transport {
        #[source]
        source: reqwest::Error,
    },
    /// A service-level request (for example bucket discovery) returned a
    /// non-success status. The provider's body is kept, truncated at the caller.
    #[error("cloud service returned HTTP {status}: {detail}")]
    Service { status: u16, detail: String },
    #[error("cloud storage operation failed")]
    Storage {
        #[source]
        source: object_store::Error,
    },
    #[error("conditional operation failed: object precondition was not met")]
    Precondition,
    #[error("object was not found")]
    NotFound,
    #[error("provider does not enforce the required conditional writes")]
    Unsupported,
    #[error("object payload exceeds the bounded upload limit")]
    PayloadTooLarge,
    #[error("object key is outside the configured prefix")]
    PrefixViolation,
    #[error("multipart upload failed during {operation}")]
    Multipart {
        operation: &'static str,
        #[source]
        source: object_store::Error,
    },
    /// Both the operation that needed an abort and the failed abort are kept.
    /// object_store hides provider upload ids, so the remote upload id remains
    /// unknown here; callers must not claim the upload was cleaned up.
    #[error("multipart upload failed during {operation} and aborting it failed: {primary}")]
    MultipartAbort {
        operation: &'static str,
        primary: Box<CloudError>,
        #[source]
        abort: object_store::Error,
    },
    /// Completion failed after the provider may already have committed the
    /// object, so the outcome is unknown, not cancelled.
    #[error("multipart upload completion result is unknown and may require recovery")]
    CompleteUnknown {
        #[source]
        source: object_store::Error,
    },
    #[error("multipart upload cancelled")]
    UploadCancelled,
    #[error("multipart upload budget exceeded")]
    UploadBudgetExceeded,
    #[error("secure random generation failed")]
    Random {
        #[source]
        source: rand::Error,
    },
}

#[derive(Clone, Debug)]
pub struct ObjectMetadata {
    pub key: String,
    pub size: u64,
    /// Opaque provider revision; never interpreted as a content hash.
    pub etag: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PutCondition {
    Any,
    CreateOnly,
    MatchEtag(String),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StorageCapabilities {
    pub create_only: bool,
    pub conditional_replace: bool,
    pub range_read: bool,
    pub conditional_writes_verified: bool,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyMode {
    #[default]
    Direct,
    Client,
    Environment,
    Custom {
        url: String,
        username: Option<String>,
        password: Option<String>,
    },
}

impl fmt::Debug for ProxyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => f.write_str("Direct"),
            Self::Client => f.write_str("Client"),
            Self::Environment => f.write_str("Environment"),
            Self::Custom { .. } => f.write_str("Custom([redacted])"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub path_style: bool,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    /// Buckets the user picked from discovery. `bucket` remains the primary one
    /// for legacy sync paths; an empty list falls back to `[bucket]`.
    #[serde(default)]
    pub buckets: Vec<String>,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub proxy: ProxyMode,
}

impl fmt::Debug for S3Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &"[redacted]")
            .field("bucket", &"[redacted]")
            .field("region", &self.region)
            .field("prefix", &"[redacted]")
            .field("path_style", &self.path_style)
            .field("credentials", &"[redacted]")
            .field("allow_insecure_http", &self.allow_insecure_http)
            .field("proxy", &self.proxy)
            .finish()
    }
}

/// Physical namespace of a store. Credentials and connection aliases are excluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectStoreScope {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
}

/// Repository objects are independently staged chunks or bounded metadata, not
/// complete files. Conditional S3 PUT cannot be replaced by multipart overwrite.
pub const MAX_PUT_BYTES: usize = 65 * 1024 * 1024;
