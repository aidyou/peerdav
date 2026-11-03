pub use crate::cloud::directory_types::DirectoryConflictPolicy;
use crate::local_upload_types::{DirectoryBrowser, DirectoryFile};
use serde::Serialize;
use std::sync::Arc;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerUploadConflict {
    pub conflict_id: String,
    pub relative_path: String,
    pub key: String,
    pub etag: Option<String>,
    pub overwrite_allowed: bool,
    pub strong_consistency: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerUploadSummary {
    pub batch_id: String,
    pub queue_id: String,
    pub peer_id: String,
    pub share_name: String,
    pub remote_dir: String,
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
    pub conflict: Option<PeerUploadConflict>,
    pub error: Option<crate::AppError>,
    pub recovery_required: bool,
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestUploadTransport {
    pub files: tokio::sync::Mutex<Vec<String>>,
    pub block: std::sync::atomic::AtomicBool,
}
pub(crate) struct PeerUploadAdapter<'a> {
    pub manager: &'a Arc<crate::PeerManager>,
    pub peer: &'a str,
}

pub(crate) enum SelectedRoot {
    Directory {
        root: Arc<crate::cloud::PhysicalRoot>,
        name: String,
    },
    File(DirectoryFile),
}
pub(crate) enum PeerBrowser {
    Directory(DirectoryBrowser),
    Files {
        entries: Vec<crate::local_upload_types::DirectoryEntryDto>,
        token: String,
        cursor: Option<(String, usize)>,
    },
}
pub(crate) struct PeerUploadBatch {
    pub status: PeerUploadSummary,
    pub roots: Option<Vec<SelectedRoot>>,
    pub browser: Arc<tokio::sync::Mutex<PeerBrowser>>,
    pub options: crate::types::IgnoreOptions,
    pub cancel: tokio::sync::watch::Sender<bool>,
    pub decision: Arc<tokio::sync::Notify>,
    pub resolution: Option<(DirectoryConflictPolicy, bool)>,
    pub handle: Option<tokio::task::JoinHandle<()>>,
    pub stopping: Arc<tokio::sync::Mutex<()>>,
    pub scope: String,
    pub lifecycle: tokio_util::sync::CancellationToken,
}
