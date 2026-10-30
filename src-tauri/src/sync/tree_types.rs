use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use super::merge_types::{EntityId, MergeError, MergeSet, Revision};
use super::repository::RepositoryError;
use super::repository_types::WireError;

pub const TREE_SCHEMA_VERSION: u32 = 1;
pub const TREE_OBJECT_LIMIT: usize = super::repository_types::MAX_METADATA_SIZE;
pub const TREE_TOTAL_LIMIT: usize = 512 * 1024 * 1024;
pub const TREE_NODE_LIMIT: usize = super::merge_types::MAX_ENTITIES + 2;

#[derive(Debug, Error)]
pub enum TreeError {
    #[error("directory manifest repository operation failed")]
    Repository(#[from] RepositoryError),
    #[error("directory manifest encoding is invalid")]
    Wire(#[from] WireError),
    #[error("directory manifest causal graph is invalid")]
    Merge(#[from] MergeError),
    #[error("directory manifest serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid directory manifest: {0}")]
    Invalid(&'static str),
    #[error("unsupported directory manifest format")]
    Unsupported,
    #[error("directory manifest operation was cancelled")]
    Cancelled,
    #[error("directory manifest resource limit exceeded: {0}")]
    ResourceLimit(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityHistoryDto {
    pub entity: EntityId,
    pub revisions: Vec<Revision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRefDto {
    pub dir: EntityId,
    pub object: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TreeKind {
    Directory {
        directory: EntityId,
        entries: Vec<EntityHistoryDto>,
        children: Vec<DirectoryRefDto>,
        recovery: Option<String>,
    },
    Recovery {
        entries: Vec<EntityHistoryDto>,
        children: Vec<DirectoryRefDto>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeDto {
    pub version: u32,
    pub tree: TreeKind,
}

/// Only populated by authenticated reads or successful immutable writes.
#[derive(Clone, Default)]
pub struct TreeReuse(pub(crate) BTreeMap<String, (String, Zeroizing<Vec<u8>>)>);

impl TreeReuse {
    /// Preserve objects from a lost CAS attempt as well as the newly read tree.
    /// Eviction affects upload efficiency only, never authenticated history.
    pub(crate) fn absorb(&mut self, other: TreeReuse) {
        let current: usize = self.0.values().map(|(_, bytes)| bytes.len()).sum();
        let incoming: usize = other.0.values().map(|(_, bytes)| bytes.len()).sum();
        if current.saturating_add(incoming) > TREE_TOTAL_LIMIT {
            self.0.clear();
        }
        self.0.extend(other.0);
    }
}

impl std::fmt::Debug for TreeReuse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeReuse")
            .field("objects", &self.0.len())
            .finish()
    }
}

#[derive(Debug)]
pub struct LoadedTree {
    pub snapshot: MergeSet,
    pub reuse: TreeReuse,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TreeOwner {
    Directory(EntityId),
    Recovery,
}

#[derive(Serialize)]
pub(super) struct BorrowedTreeDto<'a> {
    pub version: u32,
    pub tree: BorrowedTreeKind<'a>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum BorrowedTreeKind<'a> {
    Directory {
        directory: &'a EntityId,
        entries: &'a [EntityHistoryDto],
        children: Vec<DirectoryRefDto>,
        recovery: Option<String>,
    },
    Recovery {
        entries: &'a [EntityHistoryDto],
        children: Vec<DirectoryRefDto>,
    },
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct PlannedDirectory {
    pub entries: Vec<EntityHistoryDto>,
    pub children: Vec<EntityId>,
    pub recovery: bool,
}
