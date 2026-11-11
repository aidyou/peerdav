use futures_util::future::BoxFuture;
use peerdav_lib::AppError;
use peerdav_lib::sync::manager::{StartResult, SyncManager};
use peerdav_lib::sync::manager_types::{SyncPhase, WorkerOutcome};
use peerdav_lib::sync::traits::{NoopSyncEvents, RunContext, SyncEvents, SyncWorker};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct Worker {
    calls: Arc<AtomicUsize>,
    panic: bool,
    wait: bool,
}
impl SyncWorker for Worker {
    fn run(&self, context: RunContext) -> BoxFuture<'static, Result<WorkerOutcome, AppError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let panic = self.panic;
        let wait = self.wait;
        Box::pin(async move {
            if panic {
                panic!("worker panic");
            }
            if wait {
                while !context.is_cancelled() {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }
            Ok(WorkerOutcome::Completed)
        })
    }
}
fn manager() -> Arc<SyncManager> {
    SyncManager::new(Arc::new(NoopSyncEvents))
}
fn worker() -> Arc<dyn SyncWorker> {
    Arc::new(Worker {
        calls: Arc::new(AtomicUsize::new(0)),
        panic: false,
        wait: false,
    })
}

#[tokio::test]
async fn gate_zero_rejects_start() {
    let m = manager();
    assert_eq!(m.start("a", worker()).unwrap_err().code, "SYNC_GATE_LOCKED");
}
#[tokio::test]
async fn manual_start_runs_once() {
    let m = manager();
    m.open_gate();
    assert!(matches!(
        m.start("a", worker()),
        Ok(StartResult::Started(_))
    ));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(m.status("a").phase, SyncPhase::Completed);
}
#[tokio::test]
async fn duplicate_start_is_rejected_while_worker_waits() {
    let m = manager();
    m.open_gate();
    let w = Arc::new(Worker {
        calls: Arc::new(AtomicUsize::new(0)),
        panic: false,
        wait: true,
    });
    assert!(matches!(
        m.start("a", w.clone()),
        Ok(StartResult::Started(_))
    ));
    assert!(matches!(
        m.start("a", w),
        Ok(StartResult::AlreadyRunning(_))
    ));
    assert!(m.is_running("a"));
    assert!(!m.is_running("b"));
    m.stop_mapping("a").await.unwrap();
    assert!(!m.is_running("a"));
}
#[tokio::test]
async fn stop_one_mapping_keeps_other_active() {
    let m = manager();
    m.open_gate();
    let w = Arc::new(Worker {
        calls: Arc::new(AtomicUsize::new(0)),
        panic: false,
        wait: true,
    });
    m.start("a", w.clone()).unwrap();
    m.start("b", w).unwrap();
    m.stop_mapping("a").await.unwrap();
    assert_eq!(m.status("a").phase, SyncPhase::Cancelled);
    assert_eq!(m.status("b").phase, SyncPhase::Scanning);
    m.stop_mapping("b").await.unwrap();
}
#[tokio::test]
async fn stop_all_closes_gate() {
    let m = manager();
    m.open_gate();
    m.start("a", worker()).unwrap();
    m.block_new_and_stop_all().await.unwrap();
    assert!(!m.is_open());
    assert_eq!(m.start("b", worker()).unwrap_err().code, "SYNC_GATE_LOCKED");
}
#[tokio::test]
async fn panic_worker_becomes_failed() {
    let m = manager();
    m.open_gate();
    m.start(
        "a",
        Arc::new(Worker {
            calls: Arc::new(AtomicUsize::new(0)),
            panic: true,
            wait: false,
        }),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(m.status("a").phase, SyncPhase::Failed);
}
#[tokio::test]
async fn scheduler_interval_boundary_is_explicit() {
    let m = manager();
    assert!(!peerdav_lib::sync::scheduler::Scheduler::new(m).start(
        "a".into(),
        Duration::from_secs(59),
        worker()
    ));
}
#[tokio::test]
async fn worker_reference_can_be_dropped_after_stop() {
    let m = manager();
    m.open_gate();
    let w = worker();
    m.start("a", Arc::clone(&w)).unwrap();
    m.stop_mapping("a").await.unwrap();
    drop(w);
    assert_eq!(m.status("a").phase, SyncPhase::Cancelled);
}

#[tokio::test]
async fn failing_event_outlet_cannot_hide_worker_terminal_state() {
    let manager = SyncManager::new(Arc::new(FailingEvents));
    manager.open_gate();
    manager.start("event-test", worker()).unwrap();
    wait_for_phase(&manager, "event-test", SyncPhase::Completed).await;
    manager.block_new_and_stop_all().await.unwrap();
}

struct FailingEvents;
impl SyncEvents for FailingEvents {
    fn progress(
        &self,
        _: peerdav_lib::sync::manager_types::SyncStatus,
    ) -> Result<(), peerdav_lib::sync::traits::BoxError> {
        Err("progress".into())
    }
    fn error(&self, _: AppError) -> Result<(), peerdav_lib::sync::traits::BoxError> {
        Err("error".into())
    }
}
async fn wait_for_phase(m: &SyncManager, id: &str, phase: SyncPhase) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if m.status(id).phase == phase {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("phase timeout");
}

#[tokio::test]
async fn completed_run_can_restart() {
    let m = manager();
    m.open_gate();
    m.start("a", worker()).unwrap();
    wait_for_phase(&m, "a", SyncPhase::Completed).await;
    assert!(matches!(
        m.start("a", worker()),
        Ok(StartResult::Started(_))
    ));
}

#[tokio::test]
async fn stopping_mapping_rejects_restart_until_joined() {
    let m = manager();
    m.open_gate();
    m.start(
        "a",
        Arc::new(Worker {
            calls: Arc::new(AtomicUsize::new(0)),
            panic: false,
            wait: true,
        }),
    )
    .unwrap();
    let stopping = {
        let m = Arc::clone(&m);
        tokio::spawn(async move { m.stop_mapping("a").await })
    };
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(m.start("a", worker()).unwrap_err().code, "SYNC_STOPPING");
    stopping.await.unwrap().unwrap();
}

#[tokio::test]
async fn stop_all_closes_gate_before_joining() {
    let m = manager();
    m.open_gate();
    m.start(
        "a",
        Arc::new(Worker {
            calls: Arc::new(AtomicUsize::new(0)),
            panic: false,
            wait: true,
        }),
    )
    .unwrap();
    m.block_new_and_stop_all().await.unwrap();
    assert!(!m.is_open());
}

#[tokio::test]
async fn start_gate_is_required_after_stop_all() {
    let m = manager();
    m.open_gate();
    m.start("a", worker()).unwrap();
    m.block_new_and_stop_all().await.unwrap();
    assert_eq!(m.start("a", worker()).unwrap_err().code, "SYNC_GATE_LOCKED");
}

#[tokio::test]
async fn pending_recovery_status_is_preserved() {
    let m = manager();
    m.open_gate();
    m.start(
        "a",
        Arc::new(Worker {
            calls: Arc::new(AtomicUsize::new(0)),
            panic: false,
            wait: false,
        }),
    )
    .unwrap();
    wait_for_phase(&m, "a", SyncPhase::Completed).await;
    assert_eq!(m.status("a").phase, SyncPhase::Completed);
}
