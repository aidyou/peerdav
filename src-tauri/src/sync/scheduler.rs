use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

use super::manager::SyncManager;
use super::traits::SyncWorker;

pub struct Scheduler {
    manager: Arc<SyncManager>,
    task: Option<JoinHandle<()>>,
}

impl Scheduler {
    pub fn new(manager: Arc<SyncManager>) -> Self {
        Self {
            manager,
            task: None,
        }
    }
    pub fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
    pub fn start(
        &mut self,
        mapping_id: String,
        interval: Duration,
        worker: Arc<dyn SyncWorker>,
    ) -> bool {
        if interval < Duration::from_secs(60) {
            return false;
        }
        self.stop();
        let manager = Arc::clone(&self.manager);
        self.task = Some(tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            timer.tick().await;
            loop {
                timer.tick().await;
                if manager.is_open() {
                    if let Err(error) = manager.start(mapping_id.clone(), Arc::clone(&worker)) {
                        log::error!("scheduled sync start failed: {error:?}");
                    }
                }
            }
        }));
        true
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}
