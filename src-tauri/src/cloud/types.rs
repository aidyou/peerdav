use std::fmt;

use bytes::Bytes;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ObjectStream = BoxStream<'static, Result<Bytes, CloudError>>;
pub type ObjectListing<'a> = BoxStream<'a, Result<ObjectMetadata, CloudError>>;

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("cloud configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("cloud transport failed")]
    Transport {
        #[source]
        source: reqwest::Error,
    },
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

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyMode {
    #[default]
    Direct,
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

/// Repository objects are independently staged chunks or bounded metadata, not
/// complete files. Conditional S3 PUT cannot be replaced by multipart overwrite.
pub const MAX_PUT_BYTES: usize = 65 * 1024 * 1024;
