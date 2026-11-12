use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::AtomicU64,
        Arc,
    },
};
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
    Official {
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
    S3 {
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum DownloadSelection {
    Single { #[serde(rename = "id")] path: String },
    Files { #[serde(rename = "ids")] paths: Vec<String> },
    Folder { #[serde(rename = "id")] path: String },
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadItem {
    pub is_dir: bool,
    /// Remote identity; only the protocol adapter may interpret its contents.
    #[serde(rename = "id")]
    pub remote_path: String,
    pub relative_path: String,
    /// 文件字节大小，供下载队列展示“大小”列与行内进度分母。
    pub size: u64,
    pub phase: String,
    pub transferred: Option<u64>,
    #[serde(skip)]
    pub(crate) progress: Arc<AtomicU64>,
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
    /// 当前文件已传输字节；仅在有正在传输的文件时由队列状态动态填充。
    pub current_transferred: Option<u64>,
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
    /// 当前文件已传输字节。同步进度回调只能原子写入，`status()` 读取时再落到 DTO。
    pub progress: Arc<AtomicU64>,
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
pub(crate) struct OfficialDownloadAdapter {
    pub parents: HashMap<String, String>,
    pub content_types: Mutex<HashMap<String, String>>,
    #[cfg(test)]
    pub pages: Option<HashMap<(String, Option<String>), crate::cloud::official_api_types::OfficialPage>>,
    pub runtime: Arc<crate::cloud::runtime::CloudRuntime>,
    pub connection: String,
    pub revision: u64,
    pub names: HashMap<String, String>,
    pub directories: HashMap<String, String>,
    pub files: Mutex<HashMap<String, (String, u64)>>,
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
