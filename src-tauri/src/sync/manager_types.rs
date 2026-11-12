use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

#[derive(Clone)]
pub struct RunContext {
    pub cancel: Arc<AtomicBool>,
    pub epoch: Arc<AtomicU64>,
    pub expected_epoch: u64,
    pub run_id: u64,
    pub mapping_id: String,
}

impl RunContext {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Acquire)
            || self.epoch.load(std::sync::atomic::Ordering::Acquire) != self.expected_epoch
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    Idle,
    /// Local mutations are blocked while remote state is being validated or repaired.
    Frozen,
    Scanning,
    Uploading,
    Publishing,
    Applying,
    Completed,
    Cancelled,
    PausedRootMissing,
    PausedRootReplaced,
    PausedNeedsManual,
    PendingRecovery,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub mapping_id: String,
    pub run_id: Option<u64>,
    pub phase: SyncPhase,
    pub detail: Option<String>,
}
impl SyncStatus {
    pub fn idle(mapping_id: impl Into<String>) -> Self {
        Self {
            mapping_id: mapping_id.into(),
            run_id: None,
            phase: SyncPhase::Idle,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerOutcome {
    Completed,
    Phase {
        phase: SyncPhase,
        detail: Option<String>,
    },
}
