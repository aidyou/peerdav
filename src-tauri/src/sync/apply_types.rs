use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApplyAction {
    Create,
    Replace,
    Delete,
    DeleteDirectory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApplyPhase {
    Prepared,
    OriginalPreserved,
    Installed,
    Deleted,
    Paused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyIntent {
    pub op_id: String,
    pub action: ApplyAction,
    pub relative_target: PathBuf,
    pub expected_old_identity: Option<FileIdentity>,
    pub expected_old_digest: Option<String>,
    pub expected_old_size: Option<u64>,
    pub staging_identity: Option<FileIdentity>,
    pub staging_digest: Option<String>,
    pub staging_size: Option<u64>,
    pub root_identity: FileIdentity,
    pub recovery_path: PathBuf,
    pub staging_path: PathBuf,
    pub phase: ApplyPhase,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported platform")]
    Unsupported,
    #[error("invalid relative path")]
    InvalidPath,
    #[error("target identity changed")]
    IdentityChanged,
    #[error("digest or size verification failed")]
    VerificationFailed,
    #[error("target already exists")]
    AlreadyExists,
    #[error("ambiguous recovery state")]
    Ambiguous,
    #[error("journal callback failed")]
    Journal,
    #[error("input exceeds maximum size")]
    TooLarge,
    #[error("filesystem sandbox failed")]
    Sandbox(#[from] crate::sync::sandbox_types::SandboxError),
}

pub type DurableJournal<'a> = dyn FnMut(&ApplyIntent) -> Result<(), ApplyError> + 'a;

#[cfg(unix)]
impl From<rustix::io::Errno> for ApplyError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(std::io::Error::from_raw_os_error(error.raw_os_error()))
    }
}
