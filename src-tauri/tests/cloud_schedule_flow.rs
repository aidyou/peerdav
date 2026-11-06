use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use futures_util::FutureExt;
use peerdav_lib::cloud::runtime_schedule::{ScheduleHandle, TickSource};

fn source(calls: Arc<AtomicUsize>) -> TickSource {
    Arc::new(move |_mapping| {
        let calls = Arc::clone(&calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        .boxed()
    })
}

#[tokio::test]
async fn first_tick_is_skipped_and_second_tick_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (first_tick, first_tick_rx) = tokio::sync::oneshot::channel();
    let notified = Arc::new(std::sync::Mutex::new(Some(first_tick)));
    let tick_source: TickSource = {
        let calls = Arc::clone(&calls);
        let notified = Arc::clone(&notified);
        Arc::new(move |_mapping| {
            let calls = Arc::clone(&calls);
            let notified = Arc::clone(&notified);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if let Some(sender) = notified.lock().expect("lock").take() {
                    let _ = sender.send(());
                }
                Ok(())
            }
            .boxed()
        })
    };
    let handle = ScheduleHandle::start_for_test("mapping", Duration::from_millis(20), tick_source);
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    tokio::time::timeout(Duration::from_secs(1), first_tick_rx)
        .await
        .expect("first real tick")
        .expect("notification");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    handle.stop_and_join().await.expect("schedule joins");
}

#[tokio::test]
async fn stopped_schedule_does_not_run_pending_tick() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = ScheduleHandle::start_for_test(
        "mapping",
        Duration::from_millis(20),
        source(Arc::clone(&calls)),
    );
    handle.stop_and_join().await.expect("schedule joins");
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dropping_handle_aborts_worker() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = ScheduleHandle::start_for_test(
        "mapping",
        Duration::from_millis(20),
        source(Arc::clone(&calls)),
    );
    drop(handle);
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn paused_guard_error_does_not_terminate_enabled_schedule() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (resumed, resumed_rx) = tokio::sync::oneshot::channel();
    let resumed = Arc::new(std::sync::Mutex::new(Some(resumed)));
    let tick_source: TickSource = {
        let calls = Arc::clone(&calls);
        Arc::new(move |_mapping| {
            let calls = Arc::clone(&calls);
            let resumed = Arc::clone(&resumed);
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(peerdav_lib::AppError {
                        code: "CLOUD_ROOT_NEEDS_BASELINE".into(),
                        message: "First sync requires confirmation".into(),
                        detail: None,
                        context: None,
                    });
                }
                if let Some(sender) = resumed.lock().expect("notification lock").take() {
                    assert!(sender.send(()).is_ok());
                }
                Ok(())
            }
            .boxed()
        })
    };
    let handle = ScheduleHandle::start_for_test("mapping", Duration::from_millis(10), tick_source);
    tokio::time::timeout(Duration::from_secs(1), resumed_rx)
        .await.expect("later tick resumes").expect("notification");
    assert!(calls.load(Ordering::SeqCst) >= 2);
    assert!(!handle.is_finished());
    handle.stop_and_join().await.expect("schedule joins");
}

#[tokio::test]
async fn source_receives_mapping_id() {
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let seen_source = {
        let seen = Arc::clone(&seen);
        Arc::new(move |mapping: String| {
            let seen = Arc::clone(&seen);
            async move {
                *seen.lock().expect("lock") = mapping;
                Ok(())
            }
            .boxed()
        }) as TickSource
    };
    let handle = ScheduleHandle::start_for_test("map-x", Duration::from_millis(10), seen_source);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(&*seen.lock().expect("lock"), "map-x");
    handle.stop_and_join().await.expect("schedule joins");
}
