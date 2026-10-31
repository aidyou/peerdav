use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::cloud::config_types::{
    AddMappingInput, CloudMapping, ConnectionRedacted, RepositoryRedacted,
};
use crate::sync::manager_types::SyncStatus;

#[derive(Clone)]
pub struct SharedCloud(pub Arc<super::runtime::CloudRuntime>);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCapabilities {
    pub safe_apply: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudRuntimeSnapshot {
    pub connections: Vec<ConnectionRedacted>,
    pub repositories: Vec<RepositoryRedacted>,
    pub append_repositories: Vec<crate::cloud::config_types::AppendRepositoryRedacted>,
    pub mappings: Vec<CloudMapping>,
    pub statuses: Vec<SyncStatus>,
    pub capabilities: CloudCapabilities,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudActionResult {
    pub connection: Option<ConnectionRedacted>,
    pub repository: Option<RepositoryRedacted>,
    pub mapping: Option<CloudMapping>,
    pub run_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCounts {
    pub uploads: u64,
    pub downloads: u64,
    pub deletes: u64,
    pub conflicts: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSummary {
    pub uploads: u64,
    pub downloads: u64,
    pub deletes: u64,
    pub conflicts: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudPreview {
    pub preview_id: String,
    pub mapping_id: String,
    pub counts: PreviewCounts,
    pub summary: PreviewSummary,
    pub requires_confirmation: bool,
    pub head_commit: Option<String>,
    pub root_epoch: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncResult {
    pub run_id: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRecord {
    pub id: String,
    pub relative_path: String,
    pub phase: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRepositoryInput {
    pub connection_id: String,
    pub repository_id: Option<String>,
    pub encrypted: bool,
    pub password: String,
    pub password_confirmation: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRepositoryInput {
    pub connection_id: String,
    pub password: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMappingInput {
    pub mapping: AddMappingInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudRestoreInput {
    pub mapping_id: String,
    pub record_id: String,
    pub destination: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAppendRepositoryInput {
    pub connection_id: String,
    pub mode: crate::sync::append_types::AppendMode,
    pub password: String,
    pub password_confirmation: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinAppendRepositoryInput {
    pub connection_id: String,
    pub anchor_key: String,
    pub anchor_fingerprint: String,
    pub repository_id: String,
    pub mode: crate::sync::append_types::AppendMode,
    pub accept_tofu: bool,
    pub trusted_fingerprint: Option<String>,
    pub password: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveAppendMappingInput {
    pub mapping_id: Option<String>,
    pub mapping: AddMappingInput,
    pub selected_root: String,
}

pub type CloudPath = PathBuf;

impl std::fmt::Debug for CreateRepositoryInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateRepositoryInput")
            .field("connection_id", &self.connection_id)
            .field("repository_id", &self.repository_id)
            .field("encrypted", &self.encrypted)
            .field("password", &"[redacted]")
            .field("password_confirmation", &"[redacted]")
            .finish()
    }
}
impl std::fmt::Debug for JoinRepositoryInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinRepositoryInput")
            .field("connection_id", &self.connection_id)
            .field("password", &"[redacted]")
            .finish()
    }
}
