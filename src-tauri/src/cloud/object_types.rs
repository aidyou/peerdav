use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::types::ObjectMetadata;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ProtectedIdentity {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
    pub alias: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtectedMapping {
    pub identity: ProtectedIdentity,
    pub repository_id: String,
}
/// Explicit, local-user confirmation; discovery and aliases never create trust.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StorageIdentityAssociation {
    New,
    Existing(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct StorageIdentityBinding {
    pub endpoint: String,
    pub bucket: String,
    pub physical_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct StorageIdentityRegistry {
    pub version: u32,
    pub epoch: u64,
    pub bindings: Vec<StorageIdentityBinding>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct MoveObject {
    pub source: String,
    pub destination: String,
    pub revision: String,
    pub size: u64,
    pub digest: Option<String>,
    pub attempted: bool,
    pub completed: bool,
    pub requests: u32,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct MoveTask {
    pub entries: Vec<MoveObject>,
    pub complete: bool,
}

#[derive(Debug, Error)]
pub enum ObjectGuardError {
    #[error("storage identity has not been confirmed locally")]
    IdentityUnconfirmed,
    #[error("storage identity binding is invalid or would rebind an endpoint")]
    InvalidBinding,
    #[error("storage identity cannot be registered while operations are active")]
    BindingBusy,
    #[error("protected object root is already registered")]
    Conflict,
    #[error("object root is protected")]
    Protected,
    #[error("object root identity is invalid")]
    InvalidIdentity,
    #[error("object guard storage or lock failed: {source}")]
    Storage {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("an operation for this object root is still in progress")]
    OperationInProgress,
    #[error("protected mapping was not found")]
    NotFound,
    #[error("object guard key is invalid or records are corrupted")]
    KeyOrCorruption,
    #[error("object guard is busy")]
    Busy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub key: String,
    pub etag: Option<String>,
}

/// A server-issued deletion capability.  The caller can only submit this
/// opaque id back; keys are deliberately not part of the confirmation API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletePlan {
    pub id: String,
    pub expires_at_unix: u64,
    pub revision: String,
    pub registry_epoch: u64,
    pub entries: Vec<ObjectRef>,
}

#[derive(Clone, Debug)]
pub struct BrowsePage {
    pub prefix: String,
    pub objects: Vec<ObjectMetadata>,
    pub next: Option<String>,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProtectionState {
    Creating,
    Protected,
}

pub const OBJECT_GUARD_SCHEMA: i32 = 1;
