use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, Notify};

pub const MAX_DOWNLOAD_ITEMS: usize = 10_000;
pub const MAX_DOWNLOAD_DEPTH: usize = 64;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConflictResolution {
    Overwrite,
    Skip,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "backend", rename_all = "camelCase", deny_unknown_fields)]
pub enum DownloadBackend {
    Peer {
        #[serde(rename = "peerId")]
        peer_id: String,
        #[serde(rename = "shareId")]
        share_id: String,
    },
    S3 {
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum DownloadSelection {
    Single { path: String },
    Files { paths: Vec<String> },
    Folder { path: String },
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadItem {
    pub remote_path: String,
    pub relative_path: String,
    pub phase: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadConflict {
    pub conflict_id: String,
    pub relative_path: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub queue_id: String,
    pub backend: DownloadBackend,
    pub phase: String,
    pub total_files: usize,
    pub completed_files: usize,
    pub skipped_files: usize,
    pub current_relative_path: Option<String>,
    pub conflict: Option<DownloadConflict>,
    pub error: Option<crate::AppError>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPage {
    pub items: Vec<DownloadItem>,
    pub next_offset: Option<usize>,
}
pub(crate) struct DownloadQueue {
    pub status: DownloadStatus,
    pub items: Vec<DownloadItem>,
    pub root: Arc<crate::cloud::PhysicalRoot>,
    pub adapter: Arc<dyn crate::traits::TransferQueueAdapter>,
    pub cancel: tokio_util::sync::CancellationToken,
    pub lifecycle: tokio_util::sync::CancellationToken,
    pub notify: Arc<Notify>,
    pub resolution: Option<(ConflictResolution, bool)>,
    pub handle: Option<tokio::task::JoinHandle<()>>,
    pub stopping: Arc<Mutex<()>>,
}
pub(crate) type SharedDownloads = Arc<DownloadQueues>;
pub(crate) struct PeerDownloadAdapter {
    pub manager: Arc<crate::PeerManager>,
    pub peer: String,
    pub share: String,
    pub scope: String,
}
pub(crate) struct S3DownloadAdapter {
    pub runtime: Arc<crate::cloud::runtime::CloudRuntime>,
    pub connection: String,
    pub session: Arc<crate::cloud::object_runtime_types::ObjectSession>,
}
#[derive(Default)]
pub struct DownloadQueues {
    pub(crate) queues: Mutex<HashMap<String, DownloadQueue>>,
}
