//! Traits shared across the backend.

use crate::error::AppError;
use crate::types::{DiscoveredPeer, PeerRevocationInfo, PeerStatus, PendingRequestInfo};

/// Required transport boundary for single-file, multi-file and recursive folder
/// downloads through the same backend-owned queue. No default methods: each
/// transport implements planning, scope admission, cancellation and payload I/O.
/// `plan` must honor every DownloadSelection variant, deduplicate targets and
/// enforce MAX_DOWNLOAD_ITEMS/MAX_DOWNLOAD_DEPTH before dispatching payloads.
/// `validate` binds credentials, revision and scope for the entire lifecycle;
/// `execute` must preserve existing targets on cancellation or revision conflict,
/// use granted PhysicalRoot handles (never reconstructed absolute paths), and
/// report byte progress through the supplied callback. The shared queue owns
/// conflict decisions, worker monitoring, cancellation, status and release; a
/// provider must not create a parallel UI queue or silently skip unsupported work.
pub trait TransferQueueAdapter: Send + Sync {
    fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError>;
    fn report_error(&self, error: &AppError);
    fn validate(&self) -> futures_util::future::BoxFuture<'_, Result<(), AppError>>;
    fn plan(&self, selection: crate::download_types::DownloadSelection) -> futures_util::future::BoxFuture<'_, Result<Vec<crate::download_types::DownloadItem>, AppError>>;
    fn execute<'a>(&'a self, item: &'a crate::download_types::DownloadItem, root: &'a crate::cloud::PhysicalRoot, overwrite: bool, cancel: tokio_util::sync::CancellationToken, progress: Option<crate::peer::TransferProgressCallback>) -> futures_util::future::BoxFuture<'a, Result<(), AppError>>;
}

/// Required boundary for single-file, multi-file and directory upload queues.
/// All methods are mandatory: an adapter may not hide missing lifecycle support
/// behind an empty page or successful no-op. Queue IDs and conflict IDs remain
/// bound to the admitted peer/connection; cancel must leave status queryable and
/// release must stop/await the worker before dropping file handles. Pagination
/// is bounded; errors preserve the original structured cause. The common start
/// boundary rejects cancellation as a queue conflict policy.
pub(crate) trait UploadQueueAdapter: Send + Sync {
    type Status: Send;
    fn status<'a>(&'a self, queue_id: &'a str) -> futures_util::future::BoxFuture<'a, Result<Self::Status, AppError>>;
    fn page<'a>(&'a self, queue_id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> futures_util::future::BoxFuture<'a, Result<crate::local_upload_types::DirectoryPageDto, AppError>>;
    fn resolve<'a>(&'a self, queue_id: &'a str, conflict_id: &'a str, decision: crate::cloud::directory_types::DirectoryConflictPolicy, apply_to_remaining: bool) -> futures_util::future::BoxFuture<'a, Result<(), AppError>>;
    fn cancel<'a>(&'a self, queue_id: &'a str) -> futures_util::future::BoxFuture<'a, Result<(), AppError>>;
    fn release<'a>(&'a self, queue_id: &'a str) -> futures_util::future::BoxFuture<'a, Result<(), AppError>>;
    fn start<'a>(&'a self, queue_id: &'a str, policy: crate::cloud::directory_types::DirectoryConflictPolicy) -> futures_util::future::BoxFuture<'a, Result<Self::Status, AppError>>;
}

/// Background events emitted by the server. Implementations must not panic
/// and must not block; the Tauri implementation only forwards to the UI.
pub trait ServerEvents: Send + Sync {
    fn error(&self, error: &AppError);
    fn pending_request(&self, request: &PendingRequestInfo);
    fn pending_canceled(&self, _request: &PendingRequestInfo) {}
}

/// Background events emitted by the client. Implementations must not panic
/// and must not block.
pub trait PeerEvents: Send + Sync {
    fn error(&self, error: &AppError);
    fn peer_revoked(&self, peer: &PeerRevocationInfo);
    /// Publishes a reachability change for one saved server.
    fn peer_status(&self, _status: &PeerStatus) {}
    /// Publishes the complete passive discovery snapshot after a server joins,
    /// changes or expires. The default keeps headless sinks source compatible.
    fn discovered_peers(&self, _peers: &[DiscoveredPeer]) {}
}
