use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};

use super::config_types::MIN_SCHEDULE_INTERVAL;
use super::runtime::CloudRuntime;
use crate::AppError;

/// A cancellable schedule task. The join handle is retained so shutdown can
/// synchronously observe task termination rather than detaching it.
pub struct ScheduleHandle {
    cancel: Arc<AtomicBool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl ScheduleHandle {
    fn new(cancel: Arc<AtomicBool>, task: JoinHandle<()>) -> Self {
        Self {
            cancel,
            task: Mutex::new(Some(task)),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub async fn stop_and_join(&self) -> Result<(), AppError> {
        self.cancel();
        let task = self
            .task
            .lock()
            .map_err(|_| {
                schedule_error(
                    "CLOUD_SCHEDULE_LOCK",
                    "定时任务状态不可用 / Schedule state unavailable",
                )
            })?
            .take();
        if let Some(task) = task {
            task.await.map_err(|e| {
                schedule_error(
                    "CLOUD_SCHEDULE_JOIN",
                    &format!("定时任务等待失败 / Schedule join failed: {e}"),
                )
            })?;
        }
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.task
            .lock()
            .map(|task| task.as_ref().is_none_or(JoinHandle::is_finished))
            .unwrap_or(true)
    }

    /// Deterministic constructor for integration tests; production callers use
    /// `CloudRuntime::start_schedule`.
    pub fn start_for_test(
        mapping_id: impl Into<String>,
        interval: Duration,
        source: TickSource,
    ) -> Self {
        spawn_schedule(mapping_id.into(), interval, source, Arc::new(|_| {}))
    }
}

impl Drop for ScheduleHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Ok(mut task) = self.task.lock() {
            if let Some(task) = task.take() {
                // Normal shutdown uses stop_and_join; abort is only a final
                // safety net for handles dropped without an async shutdown.
                task.abort();
            }
        }
    }
}

/// Source used by tests and by the schedule manager. Production workers use
/// the runtime-bound source below; keeping the source explicit prevents tests
/// from needing a global clock or network service.
pub type TickSource = Arc<dyn Fn(String) -> BoxFuture<'static, Result<(), AppError>> + Send + Sync>;

pub type ErrorReporter = Arc<dyn Fn(AppError) + Send + Sync>;

fn spawn_schedule(
    mapping_id: String,
    interval: Duration,
    source: TickSource,
    report_error: ErrorReporter,
) -> ScheduleHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let task_cancel = Arc::clone(&cancel);
    let task = tokio::spawn(async move {
        let mut ticker = time::interval(interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // interval's first tick is immediate; schedules deliberately wait for
        // the first real period after refresh.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if task_cancel.load(Ordering::Acquire) { break; }
                    let result = std::panic::AssertUnwindSafe(async { source(mapping_id.clone()).await })
                        .catch_unwind()
                        .await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            let paused = error.code == "CLOUD_SCHEDULE_PAUSED";
                            (report_error)(error);
                            if paused { break; }
                        }
                        Err(_) => (report_error)(schedule_error(
                            "CLOUD_SCHEDULE_PANIC",
                            "定时任务异常退出 / Scheduled task panicked",
                        )),
                    }
                }
                _ = wait_cancel(Arc::clone(&task_cancel)) => break,
            }
        }
    });
    ScheduleHandle::new(cancel, task)
}

async fn wait_cancel(cancel: Arc<AtomicBool>) {
    while !cancel.load(Ordering::Acquire) {
        time::sleep(Duration::from_millis(25)).await;
    }
}

impl CloudRuntime {
    /// Start one schedule without retaining a strong runtime reference in the
    /// worker. This is also the seam used by deterministic schedule tests.
    /// `mirror:{id}` keys schedule a mirror sync; bare ids schedule an append sync.
    pub fn start_schedule(
        self: &Arc<Self>,
        mapping_id: impl Into<String>,
    ) -> Result<ScheduleHandle, AppError> {
        let mapping_id = mapping_id.into();
        let (interval, enabled) = match mapping_id.strip_prefix("mirror:") {
            Some(id) => {
                let mapping = self.mirror_mapping(id)?;
                (mapping.schedule.interval, mapping.schedule.enabled)
            }
            None => {
                let mapping = self.mapping(&mapping_id)?;
                (mapping.schedule.interval, mapping.schedule.enabled)
            }
        };
        if interval < MIN_SCHEDULE_INTERVAL {
            return Err(schedule_error(
                "CLOUD_SCHEDULE_INTERVAL",
                "定时同步间隔不能少于 60 秒 / Schedule interval must be at least 60 seconds",
            ));
        }
        if !enabled {
            return Err(schedule_error(
                "CLOUD_SCHEDULE_DISABLED",
                "定时同步未启用 / Schedule is disabled",
            ));
        }
        let seconds = interval.min(u64::MAX / 1000);
        let weak = Arc::downgrade(self);
        let source: TickSource = Arc::new(move |id| {
            let weak: Weak<CloudRuntime> = weak.clone();
            Box::pin(async move {
                let Some(runtime) = weak.upgrade() else {
                    return Ok(());
                };
                if !runtime.is_open() {
                    return Ok(());
                }
                match id.strip_prefix("mirror:") {
                    // 手动同步仍在跑时本轮跳过：MIRROR_BUSY 不是错误，不报给用户。
                    Some(mirror_id) => match runtime.mirror_sync(mirror_id).await {
                        Ok(_) => Ok(()),
                        Err(error) if error.code == "MIRROR_BUSY" => Ok(()),
                        Err(error) => Err(error),
                    },
                    None => runtime.scheduled_sync(&id).await,
                }
            })
        });
        let reporter_runtime = Arc::downgrade(self);
        let reporter: ErrorReporter = Arc::new(move |error| {
            if let Some(runtime) = reporter_runtime.upgrade() {
                runtime.manager.report_background(error);
            }
        });
        Ok(spawn_schedule(
            mapping_id,
            Duration::from_secs(seconds),
            source,
            reporter,
        ))
    }

    /// Timer entry point. It intentionally does not manufacture a preview or
    /// confirmation. A mapping is eligible only after a materialized baseline
    /// has been persisted for the current root identity.
    pub(crate) async fn scheduled_sync(&self, mapping_id: &str) -> Result<(), AppError> {
        self.admit()?;
        let mapping = self.mapping(mapping_id)?;
        self.validate_scheduled_root(&mapping)?;
        let root_epoch = match Self::root_epoch(&mapping.local_root) {
            Ok(epoch) => epoch,
            Err(error) => {
                self.manager.report_status(crate::sync::manager_types::SyncStatus { mapping_id: mapping_id.into(), run_id: None, phase: crate::sync::manager_types::SyncPhase::PausedRootMissing, detail: Some("同步目录不可用，请重新选择目录后预览并手动同步 / Sync root unavailable; reselect, preview and sync manually".into()) });
                return Err(error);
            }
        };
        if mapping.protocol == super::config_types::StorageProtocol::AppendV2 {
            if self
                .open_append_index(&mapping, &root_epoch)?
                .active_epoch()
                .map_err(|error| schedule_error("CLOUD_INDEX", &error.to_string()))?
                .is_none()
            {
                return Err(schedule_error(
                    "CLOUD_SCHEDULE_PAUSED",
                    "尚未完成首次人工同步 / First manual sync is required",
                ));
            }
            let preview = self.preview(mapping_id).await?;
            self.manual_sync(mapping_id, &preview.preview_id).await?;
            return Ok(());
        }
        let key = self
            .config
            .get_or_create_index_key()
            .map_err(|e| schedule_error("CLOUD_INDEX", &e.to_string()))?;
        let index_path = self.data_dir.join("cloud-sync.sqlite");
        let index = crate::sync::index::SyncIndex::open(
            &index_path,
            key,
            mapping.repository_id.clone(),
            Self::index_scope(&mapping, &root_epoch),
        )
        .map_err(|e| schedule_error("CLOUD_INDEX", &e.to_string()))?;
        let materialized: Option<crate::sync::merge_types::MergeSet> = index
            .load("engine", "materialized")
            .map_err(|e| schedule_error("CLOUD_INDEX", &e.to_string()))?;
        if materialized.is_none() {
            return Err(schedule_error(
                "CLOUD_SCHEDULE_PAUSED",
                "尚未完成首次人工同步 / First manual sync is required",
            ));
        }
        drop(index);
        // Only an already authorized, materialized binding can reach this path.
        let preview = self.preview(mapping_id).await?;
        self.manual_sync(mapping_id, &preview.preview_id).await?;
        Ok(())
    }
}

/// Owns all schedule handles and reconciles them with enabled mapping config.
pub struct CloudSchedules {
    handles: Mutex<HashMap<String, ScheduleHandle>>,
    reconcile: AsyncMutex<()>,
}
impl CloudSchedules {
    pub fn new() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
            reconcile: AsyncMutex::new(()),
        }
    }

    pub async fn refresh(&self, runtime: &Arc<CloudRuntime>) -> Result<(), AppError> {
        let _reconcile = self.reconcile.lock().await;
        let snapshot = runtime
            .config
            .snapshot()
            .map_err(|e| schedule_error("CLOUD_CONFIG", &e.to_string()))?;
        let mappings = snapshot.mappings;
        let mirror_mappings = snapshot.mirror_mappings;
        let mut wanted: HashMap<String, u64> = mappings
            .iter()
            .filter(|m| m.schedule.enabled)
            .map(|m| (m.id.clone(), m.schedule.interval))
            .collect();
        // 镜像映射与 append 映射的 id 都是十六进制随机串，用前缀区分调度键。
        for mapping in mirror_mappings.iter().filter(|m| m.schedule.enabled) {
            wanted.insert(format!("mirror:{}", mapping.id), mapping.schedule.interval);
        }
        self.stop_all().await?;
        if !runtime.is_open() {
            return Ok(());
        }
        for (id, _interval) in wanted {
            if !runtime.is_open() {
                break;
            }
            let handle = runtime.start_schedule(id.clone())?;
            if !runtime.is_open() {
                handle.stop_and_join().await?;
                break;
            }
            self.handles
                .lock()
                .map_err(|_| {
                    schedule_error(
                        "CLOUD_SCHEDULE_LOCK",
                        "定时任务状态不可用 / Schedule state unavailable",
                    )
                })?
                .insert(id, handle);
        }
        Ok(())
    }

    pub async fn stop_all(&self) -> Result<(), AppError> {
        let handles = std::mem::take(&mut *self.handles.lock().map_err(|_| {
            schedule_error(
                "CLOUD_SCHEDULE_LOCK",
                "定时任务状态不可用 / Schedule state unavailable",
            )
        })?);
        let mut first_error = None;
        for handle in handles.into_values() {
            if let Err(error) = handle.stop_and_join().await {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }
}

fn schedule_error(code: &str, message: &str) -> AppError {
    AppError {
        code: code.into(),
        message: message.into(),
        detail: None,
        context: None,
    }
}
