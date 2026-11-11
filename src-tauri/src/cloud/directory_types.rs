use super::object_runtime_types::ObjectSession;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum DirectoryConflictPolicy {
    #[default]
    Ask,
    Skip,
    Overwrite,
    Cancel,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectUploadDirectoryDto {
    pub batch_id: String,
    pub queue_id: String,
    pub connection_id: String,
    pub connection_revision: u64,
    pub bucket: String,
    pub prefix: String,
    pub root_name: String,
    pub phase: String,
    pub current_relative_path: Option<String>,
    pub completed_files: u64,
    pub completed_bytes: u64,
    pub total_files: u64,
    pub skipped_files: u64,
    pub skipped_symlinks: u64,
    pub skipped_empty_directories: u64,
    pub conflict: Option<DirectoryConflictDto>,
    pub error: Option<crate::AppError>,
    pub recovery_required: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryConflictDto {
    pub overwrite_allowed: bool,
    pub conflict_id: String,
    pub relative_path: String,
    pub key: String,
    pub etag: Option<String>,
}
pub(crate) use crate::local_upload_types::{BrowseDirectory, DirectoryBrowser, DirectoryWalk};
pub use crate::local_upload_types::{DirectoryEntryDto, DirectoryPageDto};
pub(crate) struct S3UploadAdapter<'a> {
    pub runtime: &'a Arc<super::runtime::CloudRuntime>,
    pub connection: &'a str,
}

pub(crate) struct DirectoryBatch {
    pub status: ObjectUploadDirectoryDto,
    pub session: Arc<ObjectSession>,
    pub root: Option<Arc<super::PhysicalRoot>>,
    pub files: Option<Vec<crate::local_upload_types::DirectoryFile>>,
    pub browser: Arc<tokio::sync::Mutex<crate::peer_upload_types::PeerBrowser>>,
    pub cancel: Arc<AtomicBool>,
    pub cancel_signal: Arc<tokio::sync::Notify>,
    pub decision: Arc<tokio::sync::Notify>,
    pub resolution: Option<(DirectoryConflictPolicy, bool)>,
    pub handle: Option<tokio::task::JoinHandle<()>>,
    // Serialize cancel/join ownership independently of worker-visible phase changes.
    pub stopping: Arc<tokio::sync::Mutex<()>>,
    pub created: std::time::Instant,
    pub lifecycle: tokio_util::sync::CancellationToken,
}
