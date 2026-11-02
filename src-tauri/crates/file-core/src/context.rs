use crate::*;
use std::{future::Future, time::Instant};

impl OperationContext {
    /// Validate the caller's page bound before issuing a provider request.
    pub fn check_list_budget(&self, request: &ListRequest) -> Result<(), FileError> {
        self.check(Operation::List, MutationOutcome::NotApplied)?;
        if request.limit > self.budget.max_entries {
            return Err(FileError::new(
                FileErrorKind::BudgetExceeded,
                Operation::List,
                MutationOutcome::NotApplied,
            ));
        }
        Ok(())
    }
    pub fn check(&self, operation: Operation, outcome: MutationOutcome) -> Result<(), FileError> {
        let kind = if self.cancellation.is_cancelled() {
            Some(FileErrorKind::Cancelled)
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Some(FileErrorKind::Deadline)
        } else {
            None
        };
        match kind {
            Some(kind) => Err(FileError::new(kind, operation, outcome)),
            None => Ok(()),
        }
    }

    /// For a single in-flight wait. Adapters must pass Unknown after dispatching
    /// a mutation, then perform explicit cleanup/reconciliation outside this wait.
    /// Dropping a network future does not prove rollback. Never wrap the entire
    /// multipart lifecycle and thereby skip its cleanup handler.
    pub async fn wait<T, F>(
        &self,
        operation: Operation,
        outcome: MutationOutcome,
        future: F,
    ) -> Result<T, FileError>
    where
        F: Future<Output = Result<T, FileError>> + Send,
        T: Send,
    {
        self.check(operation, outcome)?;
        let deadline = async {
            match self.deadline {
                Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(FileError::new(FileErrorKind::Cancelled, operation, outcome)),
            _ = deadline => Err(FileError::new(FileErrorKind::Deadline, operation, outcome)),
            result = future => result,
        }
    }
}
