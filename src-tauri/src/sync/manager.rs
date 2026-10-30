use super::manager_types::{RunContext, SyncPhase, SyncStatus, WorkerOutcome};
use super::traits::{SyncEvents, SyncWorker};
use crate::AppError;
use futures_util::FutureExt;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub type WorkerRef = Arc<dyn SyncWorker>;
struct Running {
    run_id: u64,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<WorkerResult>,
}
type WorkerResult = Result<WorkerOutcome, AppError>;
struct ManagerState {
    gate: bool,
    runs: HashMap<String, Running>,
    stopping: HashSet<String>,
}
pub struct SyncManager {
    epoch: Arc<AtomicU64>,
    next_id: AtomicU64,
    state: Mutex<ManagerState>,
    statuses: Mutex<HashMap<String, SyncStatus>>,
    stop_guard: tokio::sync::Mutex<()>,
    events: Arc<dyn SyncEvents>,
}
impl SyncManager {
    pub fn new(events: Arc<dyn SyncEvents>) -> Arc<Self> {
        Arc::new(Self {
            epoch: Arc::new(AtomicU64::new(0)),
            next_id: AtomicU64::new(1),
            state: Mutex::new(ManagerState {
                gate: false,
                runs: HashMap::new(),
                stopping: HashSet::new(),
            }),
            statuses: Mutex::new(HashMap::new()),
            stop_guard: tokio::sync::Mutex::new(()),
            events,
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, ManagerState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                log::error!("sync manager state mutex poisoned; recovering closed gate");
                let mut guard = poisoned.into_inner();
                guard.gate = false;
                guard.stopping.clear();
                for running in guard.runs.values() {
                    running.cancel.store(true, Ordering::Release);
                }
                guard
            }
        }
    }
    fn statuses(&self) -> std::sync::MutexGuard<'_, HashMap<String, SyncStatus>> {
        self.statuses.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn open_gate(&self) {
        self.state().gate = true;
    }
    pub fn is_open(&self) -> bool {
        self.state().gate
    }
    pub fn status(&self, id: &str) -> SyncStatus {
        self.statuses()
            .get(id)
            .cloned()
            .unwrap_or_else(|| SyncStatus::idle(id))
    }
    pub fn snapshots(&self) -> Vec<SyncStatus> {
        self.statuses().values().cloned().collect()
    }
    fn reap(state: &mut ManagerState, id: &str) {
        let finished = state.runs.get(id).is_some_and(|r| r.handle.is_finished());
        if let Some(running) = finished.then(|| state.runs.remove(id)).flatten() {
            let run_id = running.run_id;
            let result = running.handle.now_or_never();
            log::debug!("reaped sync run {run_id}: {result:?}");
        }
    }
    pub fn start(
        self: &Arc<Self>,
        mapping_id: impl Into<String>,
        worker: WorkerRef,
    ) -> Result<StartResult, AppError> {
        let mapping_id = mapping_id.into();
        let mut state = self.state();
        if !state.gate {
            return Err(manager_error(
                "SYNC_GATE_LOCKED",
                "同步不可用：保险库已锁定 / Sync is unavailable while the vault is locked",
            ));
        }
        Self::reap(&mut state, &mapping_id);
        if state.stopping.contains(&mapping_id) {
            return Err(manager_error(
                "SYNC_STOPPING",
                "同步正在停止：请稍后重试 / Sync is stopping; retry shortly",
            ));
        }
        if let Some(r) = state.runs.get(&mapping_id) {
            return Ok(StartResult::AlreadyRunning(r.run_id));
        }
        let run_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        let context = RunContext {
            cancel: Arc::clone(&cancel),
            epoch: Arc::clone(&self.epoch),
            expected_epoch: self.epoch.load(Ordering::Acquire),
            run_id,
            mapping_id: mapping_id.clone(),
        };
        let task_mapping = mapping_id.clone();
        let task_manager = Arc::clone(self);
        let task_cancel = Arc::clone(&cancel);
        let (ready_tx, ready_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            if ready_rx.await.is_err() {
                return Err(manager_error(
                    "SYNC_START_ABORTED",
                    "同步启动已取消 / Sync startup cancelled",
                ));
            }
            let result = std::panic::AssertUnwindSafe(async move { worker.run(context).await })
                .catch_unwind()
                .await;
            let outcome = match result {
                Ok(r) => r,
                Err(_) => Err(manager_error(
                    "SYNC_WORKER_PANIC",
                    "同步工作任务异常退出 / Sync worker panicked",
                )),
            };
            let outcome = match outcome {
                Ok(WorkerOutcome::Completed) if task_cancel.load(Ordering::Acquire) => {
                    Ok(WorkerOutcome::Phase {
                        phase: SyncPhase::Cancelled,
                        detail: None,
                    })
                }
                other => other,
            };
            match &outcome {
                Ok(WorkerOutcome::Completed) => task_manager.publish(SyncStatus {
                    mapping_id: task_mapping.clone(),
                    run_id: Some(run_id),
                    phase: SyncPhase::Completed,
                    detail: None,
                }),
                Ok(WorkerOutcome::Phase { phase, detail }) => task_manager.publish(SyncStatus {
                    mapping_id: task_mapping.clone(),
                    run_id: Some(run_id),
                    phase: phase.clone(),
                    detail: detail.clone(),
                }),
                Err(error) => task_manager.fail(task_mapping, run_id, error.clone()),
            }
            outcome
        });
        state.runs.insert(
            mapping_id.clone(),
            Running {
                run_id,
                cancel,
                handle,
            },
        );
        drop(state);
        self.publish(SyncStatus {
            mapping_id,
            run_id: Some(run_id),
            phase: SyncPhase::Scanning,
            detail: None,
        });
        if ready_tx.send(()).is_err() {
            log::error!("sync ready signal failed for run {run_id}");
        }
        Ok(StartResult::Started(run_id))
    }
    pub async fn stop_mapping(&self, mapping_id: &str) -> Result<(), AppError> {
        let _stop_guard = self.stop_guard.lock().await;
        let running = {
            let mut state = self.state();
            state.stopping.insert(mapping_id.to_owned());
            state.runs.remove(mapping_id)
        };
        let result = if let Some(r) = running {
            r.cancel.store(true, Ordering::Release);
            let joined = r.handle.await.map_err(|e| {
                manager_error(
                    "SYNC_JOIN_FAILED",
                    &format!("同步任务等待失败 / Sync join failed: {e}"),
                )
            });
            Some((r.run_id, joined))
        } else {
            None
        };
        {
            let mut state = self.state();
            state.stopping.remove(mapping_id);
        }
        if let Some((run_id, result)) = result {
            match result {
                Ok(Ok(WorkerOutcome::Phase { phase, detail })) => self.publish(SyncStatus {
                    mapping_id: mapping_id.to_owned(),
                    run_id: Some(run_id),
                    phase,
                    detail,
                }),
                Ok(Ok(WorkerOutcome::Completed)) => self.publish(SyncStatus {
                    mapping_id: mapping_id.to_owned(),
                    run_id: Some(run_id),
                    phase: SyncPhase::Completed,
                    detail: None,
                }),
                Ok(Err(error)) | Err(error) => self.fail(mapping_id.to_owned(), run_id, error),
            };
        }
        Ok(())
    }
    pub async fn block_new_and_stop_all(&self) -> Result<(), AppError> {
        let _stop_guard = self.stop_guard.lock().await;
        let running = {
            let mut state = self.state();
            state.gate = false;
            self.epoch.fetch_add(1, Ordering::AcqRel);
            state.runs.drain().map(|(_, r)| r).collect::<Vec<_>>()
        };
        for r in &running {
            r.cancel.store(true, Ordering::Release);
        }
        for r in running {
            let result = r.handle.await.map_err(|e| {
                manager_error(
                    "SYNC_JOIN_FAILED",
                    &format!("同步任务等待失败 / Sync join failed: {e}"),
                )
            })?;
            if let Ok(WorkerOutcome::Phase { phase, detail }) = result {
                log::debug!("stopped run {} with phase {phase:?}: {detail:?}", r.run_id);
            }
        }
        Ok(())
    }
    pub fn report_background(&self, error: AppError) {
        log::error!(
            "cloud background failure: code={} detail={:?} context={:?}",
            error.code,
            error.detail,
            error.context
        );
        if let Err(emit_error) = self.events.error(error) {
            log::error!("cloud background error event failed: {emit_error}");
        }
    }
    pub fn report_status(&self, status: SyncStatus) {
        self.publish(status);
    }
    fn publish(&self, status: SyncStatus) {
        self.statuses()
            .insert(status.mapping_id.clone(), status.clone());
        if let Err(e) = self.events.progress(status) {
            log::error!("sync progress event failed: {e}");
        }
    }
    fn fail(&self, mapping_id: String, run_id: u64, error: AppError) {
        self.publish(SyncStatus {
            mapping_id,
            run_id: Some(run_id),
            phase: SyncPhase::Failed,
            detail: Some(error.message.clone()),
        });
        if let Err(e) = self.events.error(error) {
            log::error!("sync error event failed: {e}");
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartResult {
    Started(u64),
    AlreadyRunning(u64),
}
fn manager_error(code: &str, message: &str) -> AppError {
    AppError {
        code: code.to_owned(),
        message: message.to_owned(),
        detail: None,
        context: None,
    }
}
