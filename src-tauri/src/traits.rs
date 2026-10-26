//! Traits shared across the backend.

use crate::error::AppError;
use crate::types::{DiscoveredPeer, PeerRevocationInfo, PeerStatus, PendingRequestInfo};

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
