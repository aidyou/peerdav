//! Traits shared across the backend.

use crate::error::AppError;
use crate::types::PendingRequestInfo;

/// Background events emitted by the server. Implementations must not panic
/// and must not block; the Tauri implementation only forwards to the UI.
pub trait ServerEvents: Send + Sync {
    fn error(&self, error: &AppError);
    fn pending_request(&self, request: &PendingRequestInfo);
}
