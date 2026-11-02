use crate::local_upload::{invalid, token};
use crate::{AppError, DomainError, download_types::*, traits::TransferQueueAdapter};
use futures_util::FutureExt;
use std::sync::Arc;
fn app(e: crate::cloud::ObjectServiceError) -> AppError {
    AppError::from(DomainError::from(e))
}
pub(crate) fn item(remote_path: String, relative_path: String) -> Result<DownloadItem, AppError> {
    crate::cloud::object_service::valid_relative_path(&relative_path).map_err(app)?;
    if relative_path.len() > 4096 || relative_path.split('/').count() > MAX_DOWNLOAD_DEPTH {
        return Err(invalid());
    }
    Ok(DownloadItem {
        remote_path,
        relative_path,
        phase: "pending".into(),
    })
}
impl DownloadQueues {
    pub(crate) async fn select(
        &self,
        backend: DownloadBackend,
        selection: DownloadSelection,
        root: crate::cloud::PhysicalRoot,
        adapter: Arc<dyn TransferQueueAdapter>,
    ) -> Result<DownloadStatus, AppError> {
        if self.queues.lock().await.len() >= 4 {
            return Err(invalid());
        }
        let lifecycle = adapter.cancellation()?;
        let items = adapter.plan(selection).await?;
        let mut names = std::collections::HashSet::new();
        if items.len() > MAX_DOWNLOAD_ITEMS
            || items.iter().any(|i| !names.insert(i.relative_path.clone()))
        {
            return Err(invalid());
        }
        adapter.validate().await?;
        let status = DownloadStatus {
            queue_id: token()?,
            backend,
            phase: "selected".into(),
            total_files: items.len(),
            completed_files: 0,
            skipped_files: 0,
            current_relative_path: None,
            conflict: None,
            error: None,
        };
        let mut map = self.queues.lock().await;
        if map.len() >= 4 {
            return Err(invalid());
        }
        map.insert(
            status.queue_id.clone(),
            DownloadQueue {
                status: status.clone(),
                items,
                root: Arc::new(root),
                adapter,
                lifecycle,
                cancel: tokio_util::sync::CancellationToken::new(),
                notify: Arc::new(tokio::sync::Notify::new()),
                resolution: None,
                handle: None,
                stopping: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        Ok(status)
    }
    pub(crate) async fn start(self: &Arc<Self>, id: &str) -> Result<DownloadStatus, AppError> {
        let mut map = self.queues.lock().await;
        let q = map.get_mut(id).ok_or_else(invalid)?;
        if q.status.phase != "selected" {
            return Err(invalid());
        }
        if q.lifecycle.is_cancelled() { return Err(invalid()); }
        q.adapter.validate().await?;
        q.status.phase = "running".into();
        let this = Arc::clone(self);
        let queue_id = id.to_owned();
        q.handle = Some(tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(this.run(&queue_id))
                .catch_unwind()
                .await;
            let outcome = match result {
                Ok(r) => r,
                Err(_) => Err(AppError::from(DomainError::Internal {
                    detail: "download queue worker panicked".into(),
                })),
            };
            let mut map = this.queues.lock().await;
            if let Some(q) = map.get_mut(&queue_id) {
                q.status.conflict = None;
                match outcome {
                    Ok(()) => q.status.phase = "completed".into(),
                    Err(e) => {
                        q.status.phase = if q.cancel.is_cancelled() {
                            "canceled"
                        } else {
                            "failed"
                        }
                        .into();
                        log::error!("download queue [{}] {} {:?}", e.code, e.message, e.detail);
                        q.adapter.report_error(&e);
                        q.status.error = Some(e);
                    }
                }
            }
        }));
        Ok(q.status.clone())
    }
    async fn run(&self, id: &str) -> Result<(), AppError> {
        let (items, root, adapter, cancel, lifecycle, notify) = {
            let map = self.queues.lock().await;
            let q = map.get(id).ok_or_else(invalid)?;
            (
                q.items.clone(),
                Arc::clone(&q.root),
                Arc::clone(&q.adapter),
                q.cancel.clone(),
                q.lifecycle.clone(),
                Arc::clone(&q.notify),
            )
        };
        let operation = async {
            let mut policy = crate::types::QueueConflictPolicy::default();
            for (index, item) in items.iter().enumerate() {
                if cancel.is_cancelled() { return Err(AppError::from(DomainError::TransferCanceled)); }
                adapter.validate().await?;
                {
                    let mut map = self.queues.lock().await;
                    let q = map.get_mut(id).ok_or_else(invalid)?;
                    q.status.current_relative_path = Some(item.relative_path.clone());
                }
                let exists = root
                    .relative_target_exists(&item.relative_path)
                    .map_err(app)?;
                let identity = root
                    .relative_target_identity(&item.relative_path)
                    .map_err(app)?;
                let mut choice = None;
                if exists {
                    choice = policy.cached;
                    if choice.is_none() {
                        let conflict = DownloadConflict {
                            conflict_id: token()?,
                            relative_path: item.relative_path.clone(),
                        };
                        {
                            let mut map = self.queues.lock().await;
                            let q = map.get_mut(id).ok_or_else(invalid)?;
                            q.status.phase = "conflict".into();
                            q.status.conflict = Some(conflict);
                        }
                        loop {
                            let notified = notify.notified();
                            let resolution = self
                                .queues
                                .lock()
                                .await
                                .get_mut(id)
                                .ok_or_else(invalid)?
                                .resolution
                                .take();
                            if let Some((decision, remaining)) = resolution {
                                choice = Some(decision);
                                policy.remember(decision, remaining);
                                break;
                            }
                            // Recheck scope even while waiting, including lock/reconfigure epochs.
                            tokio::select! { _ = cancel.cancelled() => return Err(AppError::from(DomainError::TransferCanceled)), _ = notified => {}, _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => adapter.validate().await? }
                        }
                    }
                }
                adapter.validate().await?;
                let skip = choice == Some(ConflictResolution::Skip);
                // Probe afresh after a decision. Unsafe entries fail even for skip;
                // cached choices are never cached destination authorization.
                let fresh_exists = root
                    .relative_target_exists(&item.relative_path)
                    .map_err(app)?;
                if choice == Some(ConflictResolution::Overwrite)
                    && root
                        .relative_target_identity(&item.relative_path)
                        .map_err(app)?
                        != identity
                {
                    return Err(AppError::from(DomainError::TransferConflict {
                        path: item.relative_path.clone(),
                    }));
                }
                if !(skip && fresh_exists) {
                    adapter
                        .execute(item, &root, choice == Some(ConflictResolution::Overwrite), cancel.clone())
                        .await?;
                }
                let mut map = self.queues.lock().await;
                let q = map.get_mut(id).ok_or_else(invalid)?;
                q.status.phase = "running".into();
                q.status.conflict = None;
                if skip && fresh_exists {
                    q.status.skipped_files += 1;
                    q.items[index].phase = "skipped".into();
                } else {
                    q.status.completed_files += 1;
                    q.items[index].phase = "completed".into();
                }
            }
            if cancel.is_cancelled() { return Err(AppError::from(DomainError::TransferCanceled)); }
            Ok(())
        };
        let invalidation = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if let Err(error) = adapter.validate().await {
                    return error;
                }
            }
        };
        tokio::pin!(operation);
        tokio::select! {
            biased;
            _ = lifecycle.cancelled() => { cancel.cancel(); operation.await },
            _ = cancel.cancelled() => operation.await,
            error = invalidation => { cancel.cancel(); let result = operation.await; if let Err(e) = result { log::debug!("invalidated download stopped: {e:?}"); } Err(error) },
            r = &mut operation => r,
        }
    }
    pub(crate) async fn status(&self, id: &str) -> Result<DownloadStatus, AppError> {
        Ok(self
            .queues
            .lock()
            .await
            .get(id)
            .ok_or_else(invalid)?
            .status
            .clone())
    }
    pub(crate) async fn page(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<DownloadPage, AppError> {
        let map = self.queues.lock().await;
        let q = map.get(id).ok_or_else(invalid)?;
        if offset > q.items.len() || limit == 0 || limit > 200 {
            return Err(invalid());
        }
        let end = (offset + limit).min(q.items.len());
        Ok(DownloadPage {
            items: q.items[offset..end].to_vec(),
            next_offset: (end < q.items.len()).then_some(end),
        })
    }
    pub(crate) async fn resolve(
        &self,
        id: &str,
        conflict_id: &str,
        decision: ConflictResolution,
        remaining: bool,
    ) -> Result<(), AppError> {
        let mut map = self.queues.lock().await;
        let q = map.get_mut(id).ok_or_else(invalid)?;
        if q.lifecycle.is_cancelled() || q.cancel.is_cancelled() || q.status.phase != "conflict"
            || q.resolution.is_some()
            || q.status.conflict.as_ref().map(|c| c.conflict_id.as_str()) != Some(conflict_id)
        {
            return Err(invalid());
        }
        crate::types::QueueConflictPolicy::resolve(
            q.status.conflict.as_ref().map(|c| c.conflict_id.as_str()),
            conflict_id, &mut q.resolution, decision, remaining,
        )?;
        q.notify.notify_one();
        Ok(())
    }
    pub(crate) async fn cancel(&self, id: &str) -> Result<(), AppError> {
        let stopping = Arc::clone(
            &self
                .queues
                .lock()
                .await
                .get(id)
                .ok_or_else(invalid)?
                .stopping,
        );
        let _guard = stopping.lock().await;
        let handle = {
            let mut map = self.queues.lock().await;
            let q = map.get_mut(id).ok_or_else(invalid)?;
            q.cancel.cancel();
            if q.status.phase == "selected" {
                q.status.phase = "canceled".into();
            }
            q.handle.take()
        };
        if let Some(handle) = handle {
            handle.await.map_err(|e| {
                AppError::from(DomainError::Internal {
                    detail: e.to_string(),
                })
            })?;
        }
        Ok(())
    }
    pub(crate) async fn release(&self, id: &str) -> Result<(), AppError> {
        self.cancel(id).await?;
        self.queues.lock().await.remove(id).ok_or_else(invalid)?;
        Ok(())
    }
    pub(crate) async fn reveal_root(&self, id: &str) -> Result<String, AppError> {
        let map = self.queues.lock().await;
        let q = map.get(id).ok_or_else(invalid)?;
        if q.lifecycle.is_cancelled() { return Err(invalid()); }
        q.adapter.validate().await?;
        Ok(q.root
            .reveal_path()
            .map_err(app)?
            .to_str()
            .ok_or_else(invalid)?
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Adapter {
        calls: Arc<AtomicUsize>,
    }
    impl TransferQueueAdapter for Adapter {
        fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError> {
            Ok(tokio_util::sync::CancellationToken::new())
        }
        fn report_error(&self, _: &AppError) {}
        fn validate(&self) -> BoxFuture<'_, Result<(), AppError>> {
            Box::pin(async { Ok(()) })
        }
        fn plan(&self, _: DownloadSelection) -> BoxFuture<'_, Result<Vec<DownloadItem>, AppError>> {
            Box::pin(async {
                Ok(vec![
                    item("a".into(), "a".into())?,
                    item("b".into(), "b".into())?,
                ])
            })
        }
        fn execute<'a>(
            &'a self,
            item: &'a DownloadItem,
            root: &'a crate::cloud::PhysicalRoot,
            overwrite: bool,
            cancel: tokio_util::sync::CancellationToken,
        ) -> BoxFuture<'a, Result<(), AppError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let mut source = tempfile::NamedTempFile::new().unwrap();
                std::io::Write::write_all(&mut source, b"new").unwrap();
                root.install_download(&item.relative_path, source.path(), overwrite, &cancel)
                    .map_err(app)
            })
        }
    }
    async fn wait(queues: &DownloadQueues, id: &str, phase: &str) -> DownloadStatus {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let status = queues.status(id).await.unwrap();
                if status.phase == phase {
                    return status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn queue_decisions_reset_skip_without_payload_overwrite_and_fixed_reveal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "old").unwrap();
        std::fs::write(dir.path().join("b"), "old").unwrap();
        let queues = Arc::new(DownloadQueues::default());
        let calls = Arc::new(AtomicUsize::new(0));
        for choice in [ConflictResolution::Skip, ConflictResolution::Overwrite] {
            let q = queues
                .select(
                    DownloadBackend::S3 {
                        connection_id: "test".into(),
                    },
                    DownloadSelection::Files { paths: vec![] },
                    crate::cloud::PhysicalRoot::open(dir.path()).unwrap(),
                    Arc::new(Adapter {
                        calls: calls.clone(),
                    }),
                )
                .await
                .unwrap();
            queues.start(&q.queue_id).await.unwrap();
            let conflict = wait(&queues, &q.queue_id, "conflict").await;
            queues
                .resolve(
                    &q.queue_id,
                    &conflict.conflict.unwrap().conflict_id,
                    choice,
                    true,
                )
                .await
                .unwrap();
            let end = wait(&queues, &q.queue_id, "completed").await;
            assert_eq!(
                end.skipped_files,
                if choice == ConflictResolution::Skip {
                    2
                } else {
                    0
                }
            );
            assert_eq!(
                queues.reveal_root(&q.queue_id).await.unwrap(),
                dir.path().to_str().unwrap()
            );
            assert_eq!(
                queues.page(&q.queue_id, 0, 1).await.unwrap().next_offset,
                Some(1)
            );
            queues.release(&q.queue_id).await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), b"new");
    }
    #[tokio::test]
    async fn cancellation_joins_conflict_wait_and_rejects_stale_resolution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "old").unwrap();
        let queues = Arc::new(DownloadQueues::default());
        let q = queues
            .select(
                DownloadBackend::S3 {
                    connection_id: "test".into(),
                },
                DownloadSelection::Files { paths: vec![] },
                crate::cloud::PhysicalRoot::open(dir.path()).unwrap(),
                Arc::new(Adapter {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .await
            .unwrap();
        queues.start(&q.queue_id).await.unwrap();
        wait(&queues, &q.queue_id, "conflict").await;
        assert!(
            queues
                .resolve(&q.queue_id, "stale", ConflictResolution::Overwrite, false)
                .await
                .is_err()
        );
        queues.cancel(&q.queue_id).await.unwrap();
        assert_eq!(queues.status(&q.queue_id).await.unwrap().phase, "canceled");
        queues.release(&q.queue_id).await.unwrap();
    }
    #[tokio::test]
    async fn canceled_install_preserves_destination_and_removes_staging() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"old").unwrap();
        let source = tempfile::NamedTempFile::new().unwrap();
        source.as_file().set_len(8 * 1024 * 1024).unwrap();
        let root = crate::cloud::PhysicalRoot::open(dir.path()).unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let result = tokio::task::spawn_blocking(move || root.install_download("a", source.path(), true, &cancel)).await.unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn fixed_root_rejects_replaced_reveal_and_unsafe_targets() {
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let root = crate::cloud::PhysicalRoot::open(&selected).unwrap();
        std::fs::rename(&selected, dir.path().join("held")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert!(root.reveal_path().is_err());
        std::os::unix::fs::symlink("/tmp", dir.path().join("held/link")).unwrap();
        assert!(root.relative_target_exists("link/a").is_err());
        let file = tempfile::NamedTempFile::new().unwrap();
        root.install_download("nested/a", file.path(), false, &tokio_util::sync::CancellationToken::new())
            .unwrap();
        assert!(dir.path().join("held/nested/a").is_file());
        assert!(!selected.join("nested").exists());
        assert!(
            root.install_download("nested/a", file.path(), false, &tokio_util::sync::CancellationToken::new())
                .is_err()
        );
    }
}
