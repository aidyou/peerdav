//! Traits shared across the backend.

use crate::error::AppError;
use crate::types::{DiscoveredPeer, PeerRevocationInfo, PeerStatus, PendingRequestInfo};

/// Required transport boundary for the backend-owned transfer queue. No default
/// methods: every transport must implement planning, scope admission and payload I/O.
pub trait TransferQueueAdapter: Send + Sync {
    fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError>;
    fn report_error(&self, error: &AppError);
    fn validate(&self) -> futures_util::future::BoxFuture<'_, Result<(), AppError>>;
    fn plan(&self, selection: crate::download_types::DownloadSelection) -> futures_util::future::BoxFuture<'_, Result<Vec<crate::download_types::DownloadItem>, AppError>>;
    fn execute<'a>(&'a self, item: &'a crate::download_types::DownloadItem, root: &'a crate::cloud::PhysicalRoot, overwrite: bool, cancel: tokio_util::sync::CancellationToken) -> futures_util::future::BoxFuture<'a, Result<(), AppError>>;
}

/// Both upload transports must provide queue admission and a monitored worker.
/// The common start boundary rejects cancellation as a queue conflict policy.
pub(crate) trait UploadQueueAdapter: Send + Sync {
    type Status: Send;
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
