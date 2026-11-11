use super::{
    append_types::{AppendError, CommitRef},
    apply_types::FileIdentity,
    index_types::IndexError,
    materialize_types::MaterializeError,
    merge_types::MergeError,
    scan_types::{ScanError, ScanReport},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Append frontiers are authenticated commit references, never CAS generations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaggedFrontier {
    pub repo_id: String,
    pub anchor_fingerprint: String,
    pub tips: Vec<CommitRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalEpoch {
    pub id: String,
    pub sequence: u64,
    pub frontier: TaggedFrontier,
    pub root: PathBuf,
    pub private: PathBuf,
    pub mapping: String,
    pub root_identity: FileIdentity,
    pub source_scan: ScanReport,
    pub revision_count: usize,
    pub binding_count: usize,
    #[serde(default)]
    pub selection: Vec<String>,
    #[serde(default)]
    pub publication: Option<String>,
}

pub struct AppendPullRequest<'a> {
    pub root: &'a Path,
    pub private: &'a Path,
    pub mapping: &'a str,
    /// Unique local run identity, also used by discovery's cumulative budget.
    pub run_id: &'a str,
}

pub(crate) struct PullBlocks<'a, S: ?Sized> {
    pub repository: &'a super::append_repository::AppendRepository,
    pub store: &'a S,
    pub bindings: std::collections::BTreeMap<(String, u64), super::append_types::BlockBinding>,
    pub index: &'a std::sync::Mutex<&'a mut super::append_index::AppendIndex>,
    pub operation: &'a str,
}

pub struct AppendEngineReport {
    pub frontier: TaggedFrontier,
    pub local_epoch: u64,
    pub applied_files: u64,
    pub resumed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AppendEngineError {
    #[error("append local changes require publication support; local data retained")]
    LocalChangesUnsupported,
    #[error("append local epoch binding changed")]
    BindingChanged,
    #[error("append synchronization cancelled")]
    Cancelled,
    #[error("append pending recovery requires confirmation")]
    RecoveryPaused,
    #[error("append publication failed")]
    Publish(#[from] super::append_types::PublishError),
    #[error("append dependency authentication failed")]
    Append(#[from] AppendError),
    #[error("append index failed")]
    Index(#[from] IndexError),
    #[error("append scan failed")]
    Scan(#[from] ScanError),
    #[error("append merge failed")]
    Merge(#[from] MergeError),
    #[error("append filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("append local materialization failed")]
    Local(#[from] super::engine_types::EngineError),
    #[error("append materialization failed")]
    Materialize(#[from] MaterializeError<AppendError>),
    #[error("append recovery failed")]
    Recovery(#[from] super::recovery_types::RecoveryError),
}
