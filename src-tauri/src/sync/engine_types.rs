use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

use super::{index_types::RunPhase, merge_types::MergeSet, scan_types::ScanReport};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRun {
    pub operation_id: String,
    pub config_revision: u64,
    pub published_commit: Option<String>,
    pub generation: Option<u64>,
    pub snapshot: MergeSet,
    pub source_scan: ScanReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineReport {
    pub phase: RunPhase,
    pub scan: ScanReport,
    pub commit: Option<String>,
    pub generation: Option<u64>,
    pub uploaded_blocks: u64,
    pub applied_files: u64,
    pub conflicts: u64,
    pub pending_recovery: bool,
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("directory manifest operation failed")]
    Tree(#[from] super::tree_types::TreeError),
    #[error("synchronization requires recovery confirmation")]
    PendingRecovery,
    #[error("synchronization recovery failed")]
    Recovery(#[source] super::recovery_types::RecoveryError),
    #[error("synchronization root identity is unavailable")]
    Root(#[source] super::root_types::RootError),
    #[error("synchronization file operation failed")]
    Io(#[source] std::io::Error),
    #[error("synchronization sandbox operation failed")]
    Sandbox(#[source] super::sandbox_types::SandboxError),
    #[error("synchronization wire frame is invalid")]
    Wire(#[from] super::repository_types::WireError),
    #[error("synchronization scan failed")]
    Scan(#[source] super::scan_types::ScanError),
    #[error("synchronization repository operation failed")]
    Repository(#[source] super::repository::RepositoryError),
    #[error("synchronization index operation failed")]
    Index(#[source] super::index_types::IndexError),
    #[error("synchronization publish failed")]
    Publish(#[source] super::publisher_types::PubError),
    #[error("synchronization merge failed")]
    Merge(#[source] super::merge_types::MergeError),
    #[error("synchronization apply failed")]
    Apply(#[source] super::apply_types::ApplyError),
    #[error("synchronization metadata serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("synchronization task failed")]
    Join(#[source] tokio::task::JoinError),
    #[error("synchronization was cancelled")]
    Cancelled,
    #[error("synchronization root is missing")]
    RootMissing(PathBuf),
    #[error("synchronization path is unsafe")]
    UnsafePath(PathBuf),
    #[error("synchronization file changed while uploading")]
    Changed(PathBuf),
    #[error("synchronization platform is unsupported")]
    Unsupported,
}
impl From<super::scan_types::ScanError> for EngineError {
    fn from(e: super::scan_types::ScanError) -> Self {
        Self::Scan(e)
    }
}
impl From<super::repository::RepositoryError> for EngineError {
    fn from(e: super::repository::RepositoryError) -> Self {
        Self::Repository(e)
    }
}
impl From<super::index_types::IndexError> for EngineError {
    fn from(e: super::index_types::IndexError) -> Self {
        Self::Index(e)
    }
}
impl From<super::publisher_types::PubError> for EngineError {
    fn from(e: super::publisher_types::PubError) -> Self {
        match e {
            super::publisher_types::PubError::Cancelled => Self::Cancelled,
            other => Self::Publish(other),
        }
    }
}
impl From<super::merge_types::MergeError> for EngineError {
    fn from(e: super::merge_types::MergeError) -> Self {
        Self::Merge(e)
    }
}
impl From<super::apply_types::ApplyError> for EngineError {
    fn from(e: super::apply_types::ApplyError) -> Self {
        Self::Apply(e)
    }
}
