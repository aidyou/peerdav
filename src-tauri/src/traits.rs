//! Traits shared across the backend.

use crate::error::AppError;
use crate::types::{PeerRevocationInfo, PendingRequestInfo};

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
}
