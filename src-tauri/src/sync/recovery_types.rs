use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::apply_types::ApplyError;
use super::index_types::IndexError;

/// Result of startup materialisation recovery.  A recovery never changes a
/// baseline; the engine must reconcile a remote snapshot before confirming it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoverySummary {
    pub requires_remote_reconcile: bool,
    pub recovered_operations: Vec<String>,
    pub needs_user_confirmation: bool,
    pub operation: Option<String>,
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("recovery index operation failed")]
    Index(#[from] IndexError),
    #[error("recovery filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("recovery apply operation failed")]
    Apply(#[from] ApplyError),
    #[error("recovery unsupported on this platform")]
    Unsupported,
    #[error("recovery root or private directory is not trusted")]
    UntrustedPath,
    #[error("recovery found a concurrent local change")]
    Concurrent,
    #[error("recovery record is invalid")]
    InvalidRecord,
}
