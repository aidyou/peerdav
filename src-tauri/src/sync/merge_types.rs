use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use thiserror::Error;

pub const MAX_MERGE_WORKING_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_RECOVERY_PAGE: usize = 100_000;
pub const MAX_FRONTIER: usize = 64;
pub const MAX_PARENTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityId(pub String);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionId(pub String);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeId(pub String);

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockRef {
    pub id: String,
    pub len: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileNode {
    pub blocks: Vec<BlockRef>,
    pub length: u64,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Node {
    File(FileNode),
    Dir,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RevisionValue {
    Live {
        parent_dir: EntityId,
        name: String,
        node: Node,
    },
    Tombstone,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub entity: EntityId,
    pub change_id: ChangeId,
    pub parents: Vec<RevisionId>,
    pub value: RevisionValue,
    pub revision_id: RevisionId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Baseline {
    Unknown,
    Known { frontier: Vec<RevisionId> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalChange {
    pub entity: EntityId,
    pub change_id: ChangeId,
    pub parents: Vec<RevisionId>,
    pub value: RevisionValue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeSet {
    pub revisions: BTreeMap<RevisionId, Revision>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionEntry {
    pub entity: EntityId,
    pub version: RevisionId,
    /// The original path components represented by this revision.
    pub logical_path: Vec<String>,
    /// The collision-safe path components used by the projected view.
    pub projection_path: Vec<String>,
    pub value: RevisionValue,
    pub conflict: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub entries: Vec<ProjectionEntry>,
}

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("invalid name component: {0}")]
    InvalidName(String),
    #[error("invalid identifier: {0}")]
    InvalidId(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("revision {0} has inconsistent body")]
    Corrupt(RevisionId),
    #[error("revision {0} references missing parent {1}")]
    MissingParent(RevisionId, RevisionId),
    #[error("revision {0} belongs to a different entity than its parent {1}")]
    ParentEntity(RevisionId, RevisionId),
    #[error("revision graph contains a cycle at {0}")]
    Cycle(RevisionId),
    #[error("unknown parent entity: {0}")]
    UnknownParent(EntityId),
    #[error("entity path exceeds the supported depth")]
    PathDepth,
    #[error("entity path contains a cycle at {0}")]
    PathCycle(EntityId),

    #[error("frontier is not maximal or contains an unknown revision: {0}")]
    InvalidFrontier(RevisionId),
    #[error("revision body serialization failed: {0}")]
    Serialization(#[source] serde_json::Error),
    #[error("unknown baseline cannot be used for deletion")]
    UnknownDelete,
    #[error("incomplete scan cannot be used for deletion")]
    IncompleteDelete,
}

pub type RevisionMap = BTreeMap<RevisionId, Revision>;
