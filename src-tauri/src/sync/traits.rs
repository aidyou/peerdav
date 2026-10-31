use super::manager_types::{SyncStatus, WorkerOutcome};
use crate::AppError;
use futures_util::future::BoxFuture;
use std::sync::Arc;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub use super::manager_types::RunContext;

/// Read and authenticate plaintext blocks without exposing publication or repository state.
pub trait BlockReader: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn verify_local(&self, id: &str, bytes: &[u8]) -> Result<bool, Self::Error>;
    fn read_block<'a>(
        &'a self,
        id: &'a str,
        expected_len: usize,
    ) -> BoxFuture<'a, Result<zeroize::Zeroizing<Vec<u8>>, Self::Error>>;
}

pub trait SyncWorker: Send + Sync + 'static {
    fn run(&self, context: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>>;
}
pub trait SyncWorkerFactory: Send + Sync + 'static {
    fn create(&self, mapping_id: &str) -> Arc<dyn SyncWorker>;
}
pub trait SyncEvents: Send + Sync + 'static {
    fn progress(&self, status: SyncStatus) -> Result<(), BoxError>;
    fn error(&self, error: AppError) -> Result<(), BoxError>;
}
#[derive(Default)]
pub struct NoopSyncEvents;
impl SyncEvents for NoopSyncEvents {
    fn progress(&self, _: SyncStatus) -> Result<(), BoxError> {
        Ok(())
    }
    fn error(&self, _: AppError) -> Result<(), BoxError> {
        Ok(())
    }
}
