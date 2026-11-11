use crate::DomainError;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc, time::Instant};

/// Connection selectors are accepted only when opening a session. Operations use
/// an opaque session ID and never accept a replacement protocol or connection.
#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "protocol",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FileLocation {
    Peerdav {
        peer_id: String,
        share_id: String,
    },
    Cloud {
        connection_id: String,
        #[serde(default)]
        root_id: Option<String>,
    },
    S3 {
        connection_id: String,
        bucket: String,
    },
}

/// Shared upload selection modes. Adapters must handle every variant; wire
/// parsing stays at the command boundary rather than inside provider branches.
#[derive(Clone, Copy)]
pub(crate) enum UploadMode {
    Single,
    Files,
    Folder,
}

#[derive(Clone, Copy)]
pub(crate) enum UploadAction {
    Start,
    Status,
    Cancel,
}

pub(crate) struct OfficialServiceFactory;
pub(crate) struct S3ServiceFactory;
pub(crate) struct PeerServiceFactory;

pub(crate) struct PeerBrowserBackend {
    pub peer_id: String,
    pub share_id: String,
    pub authorization: String,
}
pub(crate) struct CloudBrowserBackend {
    pub connection_id: String,
    pub revision: u64,
    pub identity: crate::cloud::ProtectedIdentity,
}

pub(crate) struct OfficialUploadAdapter<'a> {
    pub backend: &'a OfficialBrowserBackend,
    pub cloud: &'a Arc<crate::cloud::runtime::CloudRuntime>,
}

pub(crate) struct OfficialUploadJob {
    pub decision: Arc<tokio::sync::Notify>,
    pub resolution: Option<(crate::cloud::directory_types::DirectoryConflictPolicy, bool)>,
    pub page: crate::local_upload_types::DirectoryPageDto,
    pub parent: String,
    pub files: Vec<crate::local_upload_types::DirectoryFile>,
    pub status: crate::cloud::directory_types::ObjectUploadDirectoryDto,
    pub cancel: tokio_util::sync::CancellationToken,
    pub handle: Option<tokio::task::JoinHandle<()>>,
}

pub(crate) struct OfficialBrowserBackend {
    pub uploads: Arc<tokio::sync::Mutex<HashMap<String, OfficialUploadJob>>>,
    pub content_types: tokio::sync::Mutex<HashMap<String, String>>,
    pub connection_id: String,
    pub revision: u64,
    pub root_id: String,
    pub parents: tokio::sync::Mutex<HashMap<String, String>>,
    pub names: tokio::sync::Mutex<HashMap<String, String>>,
    pub directories: tokio::sync::Mutex<HashMap<String, String>>,
    pub cursors: tokio::sync::Mutex<HashMap<String, (String, String)>>,
    pub files: tokio::sync::Mutex<HashMap<String, (String, u64)>>,
    pub provider: crate::cloud::oauth_types::OfficialProvider,
}

pub(crate) struct FileSession {
    pub root: String,
    pub location: FileLocation,
    pub backend: Arc<dyn super::traits::BrowserBackend>,
    pub created: Instant,
}

pub(crate) struct FileDeletePlan {
    pub session: Arc<FileSession>,
    pub target: String,
    pub provider_plan: Option<String>,
    pub created: Instant,
}

pub(crate) static EDIT_SNAPSHOTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
pub(crate) const MAX_EDIT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) struct EditContent {
    pub name: String,
    pub bytes: Vec<u8>,
    pub revision: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEditDto {
    pub edit_id: String,
    pub id: String,
    pub name: String,
    pub local_path: String,
}

pub(crate) struct FileEdit {
    pub session: Arc<FileSession>,
    pub path: String,
    pub local: std::path::PathBuf,
    pub revision: String,
    pub hash: Vec<u8>,
}

#[derive(Default)]
pub struct FileSessions {
    pub(crate) edits: tokio::sync::Mutex<HashMap<String, FileEdit>>,
    pub(crate) sessions: tokio::sync::Mutex<HashMap<String, Arc<FileSession>>>,
    pub(crate) delete_plans: tokio::sync::Mutex<HashMap<String, FileDeletePlan>>,
}
pub type SharedFileSessions = Arc<FileSessions>;
pub(crate) const MAX_FILE_SESSIONS: usize = 64;
pub(crate) const FILE_SESSION_TTL_SECS: u64 = 1800;

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum FileUploadDto {
    Peer(crate::peer_upload_types::PeerUploadSummary),
    Cloud(crate::cloud::directory_types::ObjectUploadDirectoryDto),
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeleteConfirmation {
    Entry,
    Snapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDeleteEntryDto {
    pub id: String,
    pub size: u64,
    pub revision: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDeletePlanDto {
    pub plan_id: String,
    pub target_id: String,
    pub confirmation: DeleteConfirmation,
    pub expires_at_unix: u64,
    pub object_count: usize,
    pub entries: Vec<FileDeleteEntryDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDeleteFailureDto {
    pub id: String,
    pub error: crate::AppError,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDeleteResultDto {
    pub deleted: Vec<String>,
    pub failed: Vec<FileDeleteFailureDto>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCapabilities {
    pub delete_confirmation: DeleteConfirmation,
    pub list: bool,
    pub preview: bool,
    pub upload: bool,
    pub download: bool,
    pub download_directory: bool,
    pub delete: bool,
    pub create_dir: bool,
    pub rename: bool,
    pub external_edit: bool,
    pub online_preview: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSessionDto {
    pub session_id: String,
    pub root: String,
    pub capabilities: FileCapabilities,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryDto {
    /// Opaque identity within the admitted connection/session. Providers without
    /// native IDs use their unique scoped key; names never identify entries.
    pub id: String,
    pub parent_id: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub content_type: Option<String>,
    pub online_preview: bool,
    pub protected: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBreadcrumbDto {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePageDto {
    pub id: String,
    pub breadcrumbs: Vec<FileBreadcrumbDto>,
    pub entries: Vec<FileEntryDto>,
    pub next: Option<String>,
    pub complete: bool,
    pub protected: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PeerBrowserError {
    #[error(transparent)]
    Peer(#[from] DomainError),
}
