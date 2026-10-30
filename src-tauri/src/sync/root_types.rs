use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::index::{BaselineRecord, IndexError};

#[derive(Debug, Error)]
pub enum RootError {
    #[error("root path is invalid")]
    InvalidPath,
    #[error("root is missing")]
    Missing,
    #[error("root is unavailable")]
    Unavailable,
    #[error("root is not a directory")]
    NotDirectory,
    #[error("root identity is unsupported on this platform")]
    Unsupported,
    #[error("root binding epoch exhausted")]
    EpochExhausted,
    #[error("root preview is stale or does not match the selected root")]
    StalePreview,
    #[error("manual confirmation is required")]
    ConfirmationRequired,
    #[error("synchronization index operation failed")]
    Index(#[from] IndexError),
    #[error("root filesystem operation failed")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootBaseline {
    pub root_epoch: u64,
    pub baseline: BaselineRecord,
}

/// Stable identity of a directory, used to distinguish a replacement at the
/// same path from the directory that was originally selected.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RootPauseReason {
    Missing,
    Replaced,
    Unavailable,
    ReselectedAwaitingManual,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RootBindingStatus {
    Ready,
    Paused { reason: RootPauseReason },
}

/// Persisted root selection.  The path is encrypted by SyncIndex when this
/// value is stored; it is not intended for user-facing error messages.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootBinding {
    pub root_path: PathBuf,
    pub identity: RootIdentity,
    pub epoch: u64,
    pub status: RootBindingStatus,
    pub requires_preview: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum RootStatus {
    Ready,
    Paused(RootPauseReason),
}

impl RootBinding {
    pub fn status(&self) -> RootStatus {
        match &self.status {
            RootBindingStatus::Ready => RootStatus::Ready,
            RootBindingStatus::Paused { reason } => RootStatus::Paused(reason.clone()),
        }
    }
}
