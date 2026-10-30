use super::index::IndexError;
use super::merge_types::MergeError;
use super::repository::RepositoryError;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PubError {
    #[error("repository operation failed")]
    Repository(#[source] RepositoryError),
    #[error("synchronization index operation failed")]
    Index(#[source] IndexError),
    #[error("merge failed")]
    Merge(#[source] MergeError),
    #[error("metadata serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("directory manifest operation failed")]
    Tree(#[source] super::tree_types::TreeError),
    #[error("publishing was cancelled")]
    Cancelled,
    #[error("publish retry limit exceeded")]
    RetryLimit,
    #[error("published dependency is missing or invalid")]
    MissingDependency,
    #[error("published dependency could not be verified")]
    Dependency(#[source] RepositoryError),
    #[error("repository head rollback detected")]
    Rollback,
    #[error("repository head is stale")]
    StaleHead,
    #[error("publish history exceeds the safety budget")]
    ResourceLimit,
}
impl From<super::tree_types::TreeError> for PubError {
    fn from(error: super::tree_types::TreeError) -> Self {
        match error {
            super::tree_types::TreeError::Cancelled => Self::Cancelled,
            other => Self::Tree(other),
        }
    }
}
impl From<RepositoryError> for PubError {
    fn from(e: RepositoryError) -> Self {
        Self::Repository(e)
    }
}
impl From<IndexError> for PubError {
    fn from(e: IndexError) -> Self {
        Self::Index(e)
    }
}
impl From<MergeError> for PubError {
    fn from(e: MergeError) -> Self {
        Self::Merge(e)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitDto {
    pub version: u32,
    pub operation_id: String,
    pub parents: Vec<String>,
    pub tree: String,
}

#[derive(Clone, Debug)]
pub struct PublishControl {
    pub cancel: Arc<AtomicBool>,
    pub epoch: Arc<AtomicU64>,
    pub expected_epoch: u64,
}
impl PublishControl {
    pub fn allowed(&self) -> bool {
        !self.cancel.load(std::sync::atomic::Ordering::Acquire)
            && self.epoch.load(std::sync::atomic::Ordering::Acquire) == self.expected_epoch
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishBlock {
    pub content: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishAnchor {
    pub generation: u64,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishResult {
    pub commit: String,
    pub generation: u64,
    #[serde(default)]
    pub recovered_operation_found: bool,
}
