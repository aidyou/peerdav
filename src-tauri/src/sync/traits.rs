use super::manager_types::{SyncStatus, WorkerOutcome};
use crate::AppError;
use futures_util::future::BoxFuture;
use std::sync::Arc;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub use super::manager_types::RunContext;

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
