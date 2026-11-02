use crate::*;
use futures_util::{StreamExt, stream};

/// Adds pull-based cancellation/deadline/chunk/total bounds to a provider body.
/// Error terminates the stream; no detached producer or prefetch is introduced.
/// The outcome is supplied by the adapter: reads/pre-publication input are
/// NotApplied; uncertain in-flight mutation input must use Unknown.
pub fn bounded_stream(
    body: ByteStream,
    context: OperationContext,
    operation: Operation,
    outcome: MutationOutcome,
) -> ByteStream {
    Box::pin(stream::unfold(
        Some((body, context, 0u64)),
        move |state| async move {
            let (mut body, context, used) = state?;
            let item = context
                .wait(operation, outcome, async { Ok(body.next().await) })
                .await;
            match item {
                Ok(None) => None,
                Err(error) | Ok(Some(Err(error))) => Some((Err(error), None)),
                Ok(Some(Ok(chunk))) => {
                    match used.checked_add(chunk.len() as u64).filter(|total| {
                        *total <= context.budget.max_bytes
                            && chunk.len() <= context.budget.max_chunk_bytes.get()
                    }) {
                        Some(total) => Some((Ok(chunk), Some((body, context, total)))),
                        None => Some((
                            Err(FileError::new(
                                FileErrorKind::BudgetExceeded,
                                operation,
                                outcome,
                            )),
                            None,
                        )),
                    }
                }
            }
        },
    ))
}
