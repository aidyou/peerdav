use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub const APPEND_VERSION: u32 = 2;
/// 加密同步盘所有对象统一存放在该云端前缀下，与镜像同步的 `_peerdav_mirror` 区分，
/// 用户用网盘工具浏览时能直接看清两类数据的归属。
pub const APPEND_ROOT: &str = "_peerdav_encrypt";
pub const APPEND_ANCHORS: &str = "_peerdav_encrypt/anchors/";
pub const NONCE_LEN: usize = 24;
pub const MASTER_LEN: usize = 32;
pub const REPO_ID_LEN: usize = 16;
pub const BLOCK_ID_LEN: usize = 32;
pub const MAX_BLOCK_SIZE: usize = 8 * 1024 * 1024;
pub const MAX_METADATA_SIZE: usize = 32 * 1024 * 1024;
pub const MAX_TREE_SIZE: usize = 512 * 1024 * 1024;
pub const MAX_TREE_DEPTH: usize = 4096;
pub const MAX_DISCOVERY_NODES: usize = 100_000;
pub const MAX_DISCOVERY_REQUESTS: usize = 20_000;
pub const MAX_DISCOVERY_PAGES: usize = 1000;
pub const MAX_DISCOVERY_READ_BYTES: usize = 2 * 1024 * 1024 * 1024;
pub const MAX_DISCOVERY_ALLOCATION: usize = 256 * 1024 * 1024;

/// Cumulative work counters, not process RSS or a live-heap measurement.
/// `bytes_read` is a nonrefundable pre-GET stat-declared length reservation,
/// including failed and interrupted requests, not actual received bytes.
/// `requests` includes stat/HEAD, GET and LIST attempts.
/// `pages` counts actual attempted LIST page requests only.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscoveryBudget {
    pub requests: usize,
    pub pages: usize,
    pub bytes_read: usize,
    pub raw_entries: usize,
    pub tree_plaintext: usize,
    pub allocation_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct DiscoveryLimits {
    pub append: AppendBudget,
    pub max_allocation: usize,
}
impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            append: AppendBudget::default(),
            max_allocation: MAX_DISCOVERY_ALLOCATION,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DiscoveryVisit {
    Enter,
    Exit,
}

pub(crate) struct CountingWriter(pub usize);

#[derive(Deserialize)]
pub(crate) struct DiscoveryEnvelopeHeader {
    pub plaintext_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryState {
    Observed,
    Pending,
    Validated,
    Invalid,
}

/// Local authentication-cache identity; never part of CAS objects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationContext {
    pub repo_id: String,
    pub anchor_fingerprint: String,
    pub object_digest: String,
    pub validator_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidatedClosure {
    commits: BTreeMap<String, Commit>,
    trees: BTreeMap<String, Tree>,
}
impl ValidatedClosure {
    pub(crate) fn new(commits: BTreeMap<String, Commit>, trees: BTreeMap<String, Tree>) -> Self {
        Self { commits, trees }
    }
    pub fn commits(&self) -> &BTreeMap<String, Commit> {
        &self.commits
    }
    pub fn trees(&self) -> &BTreeMap<String, Tree> {
        &self.trees
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRef {
    pub directory: String,
    pub object: ObjectRef,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRef {
    pub operation_id: String,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tree {
    pub version: u32,
    pub repo_id: String,
    pub directory: String,
    pub entries: Vec<crate::sync::merge_types::Revision>,
    pub children: Vec<DirectoryRef>,
    pub bindings: Vec<BlockBinding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub version: u32,
    pub repo_id: String,
    pub operation_id: String,
    pub parents: Vec<CommitRef>,
    pub root: ObjectRef,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustSource {
    Unconfirmed,
    TrustedFingerprint,
    AcceptedTofu,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBinding {
    pub key: String,
    pub repo_id: String,
    pub fingerprint: String,
    pub mode: AppendMode,
    pub source: TrustSource,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub key: String,
    pub fingerprint: String,
    pub repo_id: String,
    pub descriptor: Descriptor,
    pub trust: TrustSource,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendBudget {
    pub max_metadata: usize,
    pub max_tree_total: usize,
    pub max_depth: usize,
}
impl Default for AppendBudget {
    fn default() -> Self {
        Self {
            max_metadata: MAX_METADATA_SIZE,
            max_tree_total: MAX_TREE_SIZE,
            max_depth: MAX_TREE_DEPTH,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppendMode {
    Encrypted,
    AuthenticatedPlain,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunker {
    pub name: String,
    pub version: String,
    pub normalization: String,
    pub level: u8,
    pub min: u64,
    pub average: u64,
    pub max: u64,
    pub seed: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub version: u32,
    pub repo_id: String,
    pub mode: AppendMode,
    pub chunker: Chunker,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KdfParams {
    pub algorithm: String,
    pub version: u32,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub output_len: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrappedMaster {
    pub kdf: KdfParams,
    pub salt: String,
    pub nonce: String,
    pub ciphertext: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub descriptor: Descriptor,
    pub wrapped_master: WrappedMaster,
    pub auth: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub repo_id: String,
    pub kind: String,
    pub mode: AppendMode,
    pub logical_id: String,
    pub plaintext_len: u64,
    pub nonce: Option<String>,
    pub payload: String,
    pub auth: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub kind: String,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockBinding {
    pub block_id: String,
    pub len: u64,
    pub object: ObjectRef,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedObject {
    key: String,
    bytes: Vec<u8>,
}
impl PreparedObject {
    pub fn new(key: String, bytes: Vec<u8>) -> Self {
        Self { key, bytes }
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Prepared,
    Publishing,
    PublishedUnknown,
    Published,
    ApplyPending,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedOperation {
    pub operation_id: String,
    pub parents_frontier: Vec<String>,
    pub root: ObjectRef,
    pub objects: Vec<PreparedObject>,
    pub bytes_digest: String,
    pub state: OperationState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationObject {
    pub key: String,
    pub bytes: Vec<u8>,
}

/// Test/deployment limits may only tighten the protocol hard ceilings.
#[derive(Clone, Debug)]
pub struct PublishLimits {
    pub max_objects: usize,
    pub max_staging: u64,
}
impl Default for PublishLimits {
    fn default() -> Self {
        Self {
            max_objects: MAX_PUBLISH_OBJECTS,
            max_staging: MAX_PUBLISH_STAGING,
        }
    }
}

pub const MAX_PUBLISH_OBJECTS: usize = 100_000;
pub const MAX_PUBLISH_STAGING: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_PUBLISH_WRITE: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_PUBLISH_REQUESTS: u64 = 20_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenObject {
    pub key: String,
    pub digest: String,
    pub len: usize,
    pub chunks: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenOperation {
    /// Legacy records without complete local-closure validation cannot publish.
    #[serde(default)]
    pub closure_validation_version: u32,
    pub operation_id: String,
    pub parents_frontier: Vec<String>,
    pub root: ObjectRef,
    pub objects: Vec<FrozenObject>,
    pub state: OperationState,
    pub next_object: usize,
    pub requests: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("invalid frozen publication: {0}")]
    Invalid(&'static str),
    #[error("publication budget exhausted; frozen operation retained")]
    Budget,
    #[error("publication cancelled; frozen operation retained")]
    Cancelled,
    #[error("publication index failed")]
    Index(#[from] crate::sync::index_types::IndexError),
    #[error("publication object validation failed")]
    Append(#[from] AppendError),
    #[error("publication request failed")]
    Cloud(#[from] crate::cloud::types::CloudError),
}

#[derive(Debug, Error)]
pub enum AppendError {
    #[error("invalid append input: {0}")]
    Input(&'static str),
    #[error("append authentication failed")]
    Authentication,
    #[error("append dependency is pending: {0}")]
    PendingDependency(#[source] crate::cloud::types::CloudError),
    #[error("append object is invalid: {0}")]
    Invalid(&'static str),
    #[error("append trust policy rejected")]
    TrustRejected,
    #[error("append discovery graph is invalid: {0}")]
    InvalidGraph(&'static str),
    #[error("append discovery listing is incomplete: {0}")]
    IncompleteListing(#[source] crate::cloud::types::CloudError),
    #[error("append discovery resource limit: {0}")]
    ResourceLimit(&'static str),
    #[error("append fingerprint mismatch")]
    FingerprintMismatch,
    #[error("append storage failed: {0}")]
    Storage(#[from] crate::cloud::types::CloudError),
    #[error("append index failed: {0}")]
    Index(#[from] crate::sync::index_types::IndexError),
    #[error("append encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("append random generation failed: {0}")]
    Random(#[source] rand::Error),
    #[error("append KDF failed: {0}")]
    Kdf(#[source] argon2::Error),
}
