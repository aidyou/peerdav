use crate::*;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use std::{
    error::Error,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

fn scope(generation: u64) -> BackendScope {
    BackendScope::new(
        "s3".into(),
        "https://example.test".into(),
        "bucket/中文%".into(),
        generation,
    )
    .unwrap()
}
fn context() -> OperationContext {
    OperationContext {
        cancellation: CancellationToken::new(),
        deadline: None,
        budget: OperationBudget {
            max_bytes: 4,
            max_chunk_bytes: NonZeroUsize::new(3).unwrap(),
            max_entries: NonZeroUsize::new(2).unwrap(),
        },
    }
}
fn listing() -> ListRequest {
    ListRequest {
        parent: scope(1).entry("中文%/"),
        query: None,
        prefix: None,
        cursor: None,
        limit: NonZeroUsize::new(2).unwrap(),
    }
}
fn body() -> ByteStream {
    Box::pin(stream::empty())
}
fn entry(scope: &BackendScope) -> Entry {
    Entry {
        id: scope.entry("中文%文件"),
        name: "中文%文件".into(),
        kind: EntryKind::File,
        size: None,
        modified: None,
        content_type: None,
        revision: None,
    }
}
fn unsupported<T>(result: Result<T, FileError>, operation: Operation) {
    let error = result.err().unwrap();
    assert_eq!(error.kind, FileErrorKind::Unsupported);
    assert_eq!(error.operation, operation);
    assert_eq!(error.outcome, MutationOutcome::NotApplied);
    assert_eq!(error.retry, RetryClass::Never);
}

#[test]
fn identity_scope_and_range_contract() {
    let identity = scope(1).entry("中文%2F/a//../b");
    assert_eq!(identity.opaque(), "中文%2F/a//../b");
    assert!(scope(1).validate_entry(&identity, Operation::Read).is_ok());
    for other in [
        scope(2),
        BackendScope::new(
            "peer".into(),
            "https://example.test".into(),
            "bucket/中文%".into(),
            1,
        )
        .unwrap(),
        BackendScope::new(
            "s3".into(),
            "https://other.test".into(),
            "bucket/中文%".into(),
            1,
        )
        .unwrap(),
        BackendScope::new(
            "s3".into(),
            "https://example.test".into(),
            "other".into(),
            1,
        )
        .unwrap(),
    ] {
        assert_eq!(
            other
                .validate_entry(&identity, Operation::Read)
                .unwrap_err()
                .kind,
            FileErrorKind::ScopeMismatch
        );
    }
    assert!(
        BackendScope::new(
            "s3".into(),
            "https://user:secret@host".into(),
            "bucket".into(),
            1
        )
        .is_err()
    );
    assert!(ReadRange::new(1, 1).is_err());
    assert!(ReadRange::new(2, 1).is_err());
    let range = ReadRange::new(u64::MAX - 1, u64::MAX).unwrap();
    assert_eq!(
        (range.start(), range.end(), range.len()),
        (u64::MAX - 1, u64::MAX, 1)
    );
    assert!(!range.is_empty());
    assert_eq!(Revision::new("\"opaque%中文\"").opaque(), "\"opaque%中文\"");
}

#[test]
fn completeness_cursor_and_page_bounds() {
    let request = listing();
    assert!(context().check_list_budget(&request).is_ok());
    let mut over_budget = request.clone();
    over_budget.limit = NonZeroUsize::new(3).unwrap();
    assert_eq!(
        context().check_list_budget(&over_budget).unwrap_err().kind,
        FileErrorKind::BudgetExceeded
    );
    for completeness in [Completeness::Complete, Completeness::Truncated] {
        let page = Page::new(&request, vec![], completeness).unwrap();
        assert!(page.next().is_none());
    }
    let cursor = ListCursor::new(&request, "opaque%中文".into()).unwrap();
    let page = Page::new(
        &request,
        vec![entry(&scope(1))],
        Completeness::More(cursor.clone()),
    )
    .unwrap();
    assert_eq!(page.next().unwrap().opaque(), "opaque%中文");
    assert_eq!(page.entries().len(), 1);
    let mut resumed = request.clone();
    resumed.cursor = Some(cursor.clone());
    assert!(resumed.validate_cursor().is_ok());
    assert!(ListCursor::new(&resumed, cursor.opaque().into()).is_err());
    assert!(Page::new(&resumed, vec![], Completeness::More(cursor)).is_err());
    for changed in 0..5 {
        let mut altered = resumed.clone();
        match changed {
            0 => altered.parent = scope(2).entry("中文%/"),
            1 => altered.parent = scope(1).entry("other"),
            2 => altered.query = Some("find".into()),
            3 => altered.prefix = Some("prefix".into()),
            _ => altered.limit = NonZeroUsize::new(1).unwrap(),
        }
        assert!(altered.validate_cursor().is_err());
    }
    assert!(Page::new(&request, vec![entry(&scope(2))], Completeness::Complete).is_err());
    assert!(Page::new(&request, vec![entry(&scope(1)); 3], Completeness::Complete).is_err());
}

#[test]
fn capability_matrix_never_promotes_guarantees() {
    let scope = scope(1);
    let mut caps = Capabilities::default();
    unsupported(caps.check_list(&scope, &listing()), Operation::List);
    unsupported(caps.check_stat(&scope, &scope.entry("a")), Operation::Stat);
    let read = ReadRequest {
        entry: scope.entry("a"),
        range: Some(ReadRange::new(0, 1).unwrap()),
    };
    caps.read = true;
    unsupported(caps.check_read(&scope, &read), Operation::Read);
    caps.range_read = true;
    assert!(caps.check_read(&scope, &read).is_ok());
    caps.list = true;
    let mut request = listing();
    request.query = Some("q".into());
    unsupported(caps.check_list(&scope, &request), Operation::List);
    caps.writes.push(WriteSupport::BoundedAtomic {
        condition: ConditionKind::CreateOnly,
        max_bytes: 4,
    });
    let mut write = WriteRequest {
        entry: scope.entry("a"),
        condition: Condition::CreateOnly,
        mode: WriteMode::BoundedAtomic { max_bytes: 4 },
        body: body(),
    };
    assert!(caps.check_write(&scope, &write).is_ok());
    write.mode = WriteMode::BoundedAtomic { max_bytes: 5 };
    unsupported(caps.check_write(&scope, &write), Operation::Write);
    write.mode = WriteMode::Streaming;
    unsupported(caps.check_write(&scope, &write), Operation::Write);
    caps.writes.push(WriteSupport::Streaming {
        condition: ConditionKind::Any,
    });
    unsupported(caps.check_write(&scope, &write), Operation::Write);
    write.condition = Condition::Any;
    assert!(caps.check_write(&scope, &write).is_ok());
    let mut delete = DeleteRequest {
        entry: scope.entry("a"),
        condition: Condition::Any,
        mode: DeleteMode::File,
    };
    caps.deletes.push(DeleteSupport {
        mode: DeleteMode::File,
        condition: ConditionKind::Any,
    });
    assert!(caps.check_delete(&scope, &delete).is_ok());
    delete.mode = DeleteMode::Recursive;
    unsupported(caps.check_delete(&scope, &delete), Operation::Delete);
    unsupported(
        caps.check_create_dir(
            &scope,
            &CreateDirRequest {
                entry: scope.entry("dir"),
                condition: Condition::CreateOnly,
            },
        ),
        Operation::CreateDir,
    );
    let mut mv = MoveRequest {
        source: scope.entry("a"),
        destination: scope.entry("b"),
        source_condition: Condition::Any,
        destination_condition: Condition::CreateOnly,
        atomicity: MoveAtomicity::Atomic,
    };
    caps.moves.push(MoveSupport {
        atomicity: MoveAtomicity::NativeNonAtomic,
        source_condition: ConditionKind::Any,
        destination_condition: ConditionKind::CreateOnly,
    });
    unsupported(caps.check_move(&scope, &mv), Operation::Move);
    mv.atomicity = MoveAtomicity::NativeNonAtomic;
    assert!(caps.check_move(&scope, &mv).is_ok());
    mv.destination = crate::tests::scope(2).entry("b");
    assert_eq!(
        caps.check_move(&scope, &mv).unwrap_err().kind,
        FileErrorKind::ScopeMismatch
    );
}

struct DenyBackend {
    scope: BackendScope,
    capabilities: Capabilities,
}
impl FileBackend for DenyBackend {
    fn scope(&self) -> &BackendScope {
        &self.scope
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}
#[tokio::test(flavor = "current_thread")]
async fn dyn_backend_defaults_no_body_poll_or_side_effect() {
    let backend: Box<dyn FileBackend> = Box::new(DenyBackend {
        scope: scope(1),
        capabilities: Capabilities::default(),
    });
    let id = backend.scope().entry("a");
    assert!(!backend.capabilities().read);
    unsupported(backend.list(context(), listing()).await, Operation::List);
    unsupported(backend.stat(context(), id.clone()).await, Operation::Stat);
    unsupported(
        backend
            .read(
                context(),
                ReadRequest {
                    entry: id.clone(),
                    range: None,
                },
            )
            .await,
        Operation::Read,
    );
    let polls = Arc::new(AtomicUsize::new(0));
    let observed = polls.clone();
    let body = Box::pin(stream::poll_fn(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(Some(Ok(Bytes::from_static(b"x"))))
    }));
    unsupported(
        backend
            .write(
                context(),
                WriteRequest {
                    entry: id.clone(),
                    condition: Condition::Any,
                    mode: WriteMode::Streaming,
                    body,
                },
            )
            .await,
        Operation::Write,
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    unsupported(
        backend
            .create_dir(
                context(),
                CreateDirRequest {
                    entry: id.clone(),
                    condition: Condition::Any,
                },
            )
            .await,
        Operation::CreateDir,
    );
    for mode in [
        DeleteMode::File,
        DeleteMode::EmptyDirectory,
        DeleteMode::Recursive,
    ] {
        unsupported(
            backend
                .delete(
                    context(),
                    DeleteRequest {
                        entry: id.clone(),
                        mode,
                        condition: Condition::Any,
                    },
                )
                .await,
            Operation::Delete,
        );
    }
    unsupported(
        backend
            .move_entry(
                context(),
                MoveRequest {
                    source: id,
                    destination: backend.scope().entry("b"),
                    source_condition: Condition::Any,
                    destination_condition: Condition::Any,
                    atomicity: MoveAtomicity::Atomic,
                },
            )
            .await,
        Operation::Move,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_wakes_pending_wait_and_preserves_outcome() {
    let context = context();
    let token = context.cancellation.clone();
    let wait = context.wait::<(), _>(
        Operation::Write,
        MutationOutcome::Unknown,
        std::future::pending(),
    );
    let cancel = async {
        tokio::task::yield_now().await;
        token.cancel();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(wait, cancel) })
            .await
            .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.kind, FileErrorKind::Cancelled);
    assert_eq!(error.outcome, MutationOutcome::Unknown);
    assert_eq!(
        context
            .check(Operation::Write, MutationOutcome::Applied)
            .unwrap_err()
            .outcome,
        MutationOutcome::Applied
    );
    let mut deadline = crate::tests::context();
    deadline.deadline = Some(Instant::now() + Duration::from_millis(1));
    assert_eq!(
        deadline
            .wait::<(), _>(
                Operation::Read,
                MutationOutcome::NotApplied,
                std::future::pending()
            )
            .await
            .unwrap_err()
            .kind,
        FileErrorKind::Deadline
    );
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_stream_pull_total_chunk_and_cancel() {
    let polls = Arc::new(AtomicUsize::new(0));
    let observed = polls.clone();
    let input = Box::pin(stream::poll_fn(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(Some(Ok(Bytes::from_static(b"abc"))))
    }));
    let mut output = bounded_stream(
        input,
        context(),
        Operation::Read,
        MutationOutcome::NotApplied,
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        output.next().await.unwrap().unwrap(),
        Bytes::from_static(b"abc")
    );
    assert_eq!(
        output.next().await.unwrap().unwrap_err().kind,
        FileErrorKind::BudgetExceeded
    );
    assert!(output.next().await.is_none());
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    let mut oversized = bounded_stream(
        Box::pin(stream::iter([Ok(Bytes::from_static(b"abcd"))])),
        context(),
        Operation::Read,
        MutationOutcome::NotApplied,
    );
    assert_eq!(
        oversized.next().await.unwrap().unwrap_err().kind,
        FileErrorKind::BudgetExceeded
    );
    let context = context();
    let token = context.cancellation.clone();
    let mut pending = bounded_stream(
        Box::pin(stream::pending()),
        context,
        Operation::Read,
        MutationOutcome::NotApplied,
    );
    let read = pending.next();
    let cancel = async {
        tokio::task::yield_now().await;
        token.cancel();
    };
    let (item, ()) =
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(read, cancel) })
            .await
            .unwrap();
    assert_eq!(item.unwrap().unwrap_err().kind, FileErrorKind::Cancelled);
}

#[test]
fn error_keeps_primary_cleanup_status_and_outcomes() {
    for outcome in [
        MutationOutcome::NotApplied,
        MutationOutcome::Applied,
        MutationOutcome::Unknown,
    ] {
        let mut error = FileError::new(FileErrorKind::Provider, Operation::Write, outcome)
            .with_source(Box::new(std::io::Error::other("primary failure")));
        error.status = Some(503);
        error.provider_code = Some("ProviderCode".into());
        error.retry = RetryClass::IfReplaySafe;
        error.cleanup.push(CleanupFailure {
            operation: Operation::Delete,
            status: Some(500),
            provider_code: Some("AbortFailed".into()),
            recovery_id: Some("upload-id".into()),
            source: Box::new(std::io::Error::other("abort failed")),
        });
        assert_eq!(error.source().unwrap().to_string(), "primary failure");
        assert!(error.source().unwrap().is::<std::io::Error>());
        assert_eq!(error.cleanup[0].source.to_string(), "abort failed");
        assert_eq!(error.outcome, outcome);
        assert_eq!(error.status, Some(503));
        assert!(!error.to_string().contains("primary failure"));
    }
}
