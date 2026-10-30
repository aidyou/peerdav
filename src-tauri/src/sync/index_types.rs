use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("synchronization index database operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("synchronization index encryption failed")]
    Authentication,
    #[error("synchronization index serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("synchronization index random generation failed")]
    Random(#[source] rand::Error),
    #[error("synchronization index file operation failed")]
    Io(#[from] std::io::Error),
    #[error("synchronization index record is invalid")]
    InvalidRecord,
    #[error("synchronization index schema is unsupported")]
    UnsupportedSchema,
}

/// Unknown is not an empty directory: no deletion may be inferred from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum BaselineRecord {
    Unknown,
    Materialized {
        generation: u64,
        commit: String,
        snapshot: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Scanning,
    Uploading,
    Publishing,
    PublishedPendingApply,
    Applying,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunJournal {
    pub operation_id: String,
    pub config_revision: u64,
    pub phase: RunPhase,
    pub published_commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPhase {
    Prepared,
    OriginalPreserved,
    Installed,
    Confirmed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyJournal {
    pub operation_id: String,
    pub relative_path: String,
    pub staging_path: Option<PathBuf>,
    pub recovery_path: Option<PathBuf>,
    pub expected_digest: Option<String>,
    pub phase: ApplyPhase,
}

pub struct SyncIndex {
    pub(super) connection: rusqlite::Connection,
    pub(super) key: Zeroizing<[u8; 32]>,
    pub(super) repo: String,
    pub(super) mapping: String,
}

pub const INDEX_SCHEMA: i64 = 1;
pub const INDEX_MAX_RECORD: usize = 32 * 1024 * 1024;
