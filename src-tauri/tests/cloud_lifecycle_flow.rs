use futures_util::future::BoxFuture;
use peerdav_lib::AppError;
use peerdav_lib::sync::manager::{StartResult, SyncManager};
use peerdav_lib::sync::manager_types::{SyncPhase, SyncStatus, WorkerOutcome};
use peerdav_lib::sync::traits::{BoxError, RunContext, SyncEvents, SyncWorker};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{Barrier, Notify};

#[derive(Default)]
struct Events {
    errors: std::sync::Mutex<Vec<AppError>>,
}
impl SyncEvents for Events {
    fn progress(&self, _: SyncStatus) -> Result<(), BoxError> {
        Ok(())
    }
    fn error(&self, error: AppError) -> Result<(), BoxError> {
        self.errors.lock().unwrap().push(error);
        Ok(())
    }
}

struct GatedWorker {
    calls: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl SyncWorker for GatedWorker {
    fn run(&self, context: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            if context.is_cancelled() {
                Ok(WorkerOutcome::Phase {
                    phase: SyncPhase::Cancelled,
                    detail: None,
                })
            } else {
                Ok(WorkerOutcome::Completed)
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_concurrent_starts_have_one_worker() {
    let events = Arc::new(Events::default());
    let manager = SyncManager::new(Arc::clone(&events) as Arc<dyn SyncEvents>);
    manager.open_gate();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let worker: Arc<dyn SyncWorker> = Arc::new(GatedWorker {
        calls: Arc::clone(&calls),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let barrier = Arc::new(Barrier::new(32));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let barrier = Arc::clone(&barrier);
        let manager = Arc::clone(&manager);
        let worker = Arc::clone(&worker);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.start("parallel", worker)
        }));
    }
    let mut started = 0;
    let mut already = 0;
    for task in tasks {
        match task.await.unwrap().unwrap() {
            StartResult::Started(_) => started += 1,
            StartResult::AlreadyRunning(_) => already += 1,
        }
    }
    assert_eq!((started, already), (1, 31));
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), manager.stop_mapping("parallel"))
        .await
        .unwrap()
        .unwrap();
}

struct CancelBlockedWorker {
    cancel_seen: Arc<Notify>,
    release: Arc<Notify>,
    invalid_context: Arc<AtomicBool>,
    secret: Arc<()>,
}
impl SyncWorker for CancelBlockedWorker {
    fn run(&self, context: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>> {
        let cancel_seen = Arc::clone(&self.cancel_seen);
        let release = Arc::clone(&self.release);
        let invalid = Arc::clone(&self.invalid_context);
        let secret = Arc::clone(&self.secret);
        Box::pin(async move {
            while !context.is_cancelled() {
                tokio::task::yield_now().await;
            }
            invalid.store(context.is_cancelled(), Ordering::SeqCst);
            cancel_seen.notify_one();
            release.notified().await;
            drop(secret);
            Ok(WorkerOutcome::Completed)
        })
    }
}

#[tokio::test]
async fn stop_all_waits_for_cancel_ack_and_worker_drop() {
    let manager = SyncManager::new(Arc::new(Events::default()));
    manager.open_gate();
    let cancel_seen = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let invalid_context = Arc::new(AtomicBool::new(false));
    let secret = Arc::new(());
    let weak_secret: Weak<()> = Arc::downgrade(&secret);
    let worker: Arc<dyn SyncWorker> = Arc::new(CancelBlockedWorker {
        cancel_seen: Arc::clone(&cancel_seen),
        release: Arc::clone(&release),
        invalid_context: Arc::clone(&invalid_context),
        secret: Arc::clone(&secret),
    });
    manager.start("blocked", worker).unwrap();
    drop(secret);
    let mut stopping = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.block_new_and_stop_all().await })
    };
    tokio::time::timeout(Duration::from_secs(1), cancel_seen.notified())
        .await
        .unwrap();
    assert!(!manager.is_open());
    assert_eq!(
        manager.start("new", Arc::new(NoopWorker)).unwrap_err().code,
        "SYNC_GATE_LOCKED"
    );
    assert!(invalid_context.load(Ordering::SeqCst));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut stopping)
            .await
            .is_err()
    );
    release.notify_one();
    stopping.await.unwrap().unwrap();
    assert!(weak_secret.upgrade().is_none());
}

struct NoopWorker;
impl SyncWorker for NoopWorker {
    fn run(&self, _: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>> {
        Box::pin(async { Ok(WorkerOutcome::Completed) })
    }
}

struct PendingWorker;
impl SyncWorker for PendingWorker {
    fn run(&self, context: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>> {
        Box::pin(async move {
            while !context.is_cancelled() {
                tokio::task::yield_now().await;
            }
            Ok(WorkerOutcome::Phase {
                phase: SyncPhase::PendingRecovery,
                detail: Some("recovery".into()),
            })
        })
    }
}

#[tokio::test]
async fn cancellation_can_return_pending_recovery() {
    let manager = SyncManager::new(Arc::new(Events::default()));
    manager.open_gate();
    manager.start("recovery", Arc::new(PendingWorker)).unwrap();
    manager.block_new_and_stop_all().await.unwrap();
    assert_eq!(manager.status("recovery").phase, SyncPhase::PendingRecovery);
}

struct PanickingWorker;
impl SyncWorker for PanickingWorker {
    fn run(&self, _: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>> {
        panic!("panic before future construction")
    }
}

#[tokio::test]
async fn synchronous_worker_panic_is_error_event_and_outlet_failures_escape_nowhere() {
    let events = Arc::new(Events::default());
    let manager = SyncManager::new(Arc::clone(&events) as Arc<dyn SyncEvents>);
    manager.open_gate();
    manager.start("panic", Arc::new(PanickingWorker)).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if manager.status("panic").phase == SyncPhase::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        events
            .errors
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.code == "SYNC_WORKER_PANIC")
            .map(|e| e.code.as_str()),
        Some("SYNC_WORKER_PANIC")
    );
    manager.block_new_and_stop_all().await.unwrap();
}
