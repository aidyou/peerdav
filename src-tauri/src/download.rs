use crate::local_upload::{invalid, token};
use crate::{AppError, DomainError, download_types::*, traits::TransferQueueAdapter};
use futures_util::FutureExt;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
fn app(e: crate::cloud::ObjectServiceError) -> AppError {
    AppError::from(DomainError::from(e))
}
pub(crate) fn item(
    remote_path: String,
    relative_path: String,
    size: u64,
) -> Result<DownloadItem, AppError> {
    crate::cloud::object_service::valid_relative_path(&relative_path).map_err(app)?;
    if relative_path.len() > 4096 || relative_path.split('/').count() > MAX_DOWNLOAD_DEPTH {
        return Err(invalid());
    }
    Ok(DownloadItem {
        is_dir: false,
        remote_path,
        relative_path,
        size,
        phase: "pending".into(),
        transferred: None,
        progress: Arc::new(AtomicU64::new(0)),
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
            current_transferred: None,
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
                progress: Arc::new(AtomicU64::new(0)),
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
        let (items, root, adapter, cancel, lifecycle, notify, progress) = {
            let map = self.queues.lock().await;
            let q = map.get(id).ok_or_else(invalid)?;
            (
                q.items.clone(),
                Arc::clone(&q.root),
                Arc::clone(&q.adapter),
                q.cancel.clone(),
                q.lifecycle.clone(),
                Arc::clone(&q.notify),
                Arc::clone(&q.progress),
            )
        };
        let operation = async {
            // Fully fresh file-only selections can transfer concurrently without
            // changing interactive conflict ordering. Existing targets and folder
            // plans keep the original decision path below.
            let fresh = items.len() > 1 && items.iter().all(|item| !item.is_dir)
                && items.iter().map(|item| root.relative_target_exists(&item.relative_path).map_err(app)).collect::<Result<Vec<_>, _>>()?.iter().all(|exists| !exists);
            if fresh {
                for start in (0..items.len()).step_by(4) {
                    if cancel.is_cancelled() { return Err(AppError::from(DomainError::TransferCanceled)); }
                    adapter.validate().await?;
                    let end = (start + 4).min(items.len());
                    {
                        let mut map = self.queues.lock().await;
                        let q = map.get_mut(id).ok_or_else(invalid)?;
                        for item in &mut q.items[start..end] { item.phase = "active".into(); }
                    }
                    let results = futures_util::future::join_all(items[start..end].iter().map(|item| {
                        let progress = Arc::clone(&item.progress);
                        let callback: crate::peer::TransferProgressCallback = Arc::new(move |bytes, _| { progress.store(bytes, Ordering::Relaxed); });
                        adapter.execute(item, &root, false, cancel.clone(), Some(callback))
                    })).await;
                    let mut failure = None;
                    let mut map = self.queues.lock().await;
                    let q = map.get_mut(id).ok_or_else(invalid)?;
                    for (offset, result) in results.into_iter().enumerate() {
                        match result {
                            Ok(()) => { q.items[start + offset].phase = "completed".into(); q.status.completed_files += 1; }
                            Err(error) => { q.items[start + offset].phase = "failed".into(); if failure.is_none() { failure = Some(error); } }
                        }
                    }
                    if let Some(error) = failure { return Err(error); }
                }
                if cancel.is_cancelled() { return Err(AppError::from(DomainError::TransferCanceled)); }
                return Ok(());
            }
            let mut policy = crate::types::QueueConflictPolicy::default();
            for (index, item) in items.iter().enumerate() {
                if cancel.is_cancelled() { return Err(AppError::from(DomainError::TransferCanceled)); }
                adapter.validate().await?;
                {
                    let mut map = self.queues.lock().await;
                    let q = map.get_mut(id).ok_or_else(invalid)?;
                    q.status.current_relative_path = Some(item.relative_path.clone());
                    progress.store(0, Ordering::Relaxed);
                }
                if item.is_dir {
                    root.ensure_download_directory(&item.relative_path).map_err(app)?;
                    let mut map = self.queues.lock().await;
                    let q = map.get_mut(id).ok_or_else(invalid)?;
                    q.status.completed_files += 1;
                    q.items[index].phase = "completed".into();
                    continue;
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
                    let progress_cb: crate::peer::TransferProgressCallback = {
                        let progress = Arc::clone(&progress);
                        Arc::new(move |transferred, _total| {
                            progress.store(transferred, Ordering::Relaxed);
                        })
                    };
                    adapter
                        .execute(item, &root, choice == Some(ConflictResolution::Overwrite), cancel.clone(), Some(progress_cb))
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
        let map = self.queues.lock().await;
        let q = map.get(id).ok_or_else(invalid)?;
        let mut status = q.status.clone();
        // 同步进度回调只能原子写入；在读取状态时落到 DTO，供前端渲染当前行进度。
        if status.current_relative_path.is_some() {
            status.current_transferred = Some(q.progress.load(Ordering::Relaxed));
        }
        Ok(status)
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
            items: q.items[offset..end].iter().cloned().map(|mut item| {
                if item.phase == "active" { item.transferred = Some(item.progress.load(Ordering::Relaxed)); }
                item
            }).collect(),
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
    #[test]
    fn download_selection_accepts_only_opaque_id_contract() {
        for mode in ["single", "folder"] {
            let selection: DownloadSelection = serde_json::from_value(serde_json::json!({ "mode": mode, "id": "opaque:not/a/display/name" })).expect("opaque ID");
            match selection {
                DownloadSelection::Single { path } | DownloadSelection::Folder { path } => assert_eq!(path, "opaque:not/a/display/name"),
                DownloadSelection::Files { .. } => panic!("single target"),
            }
            assert!(serde_json::from_value::<DownloadSelection>(serde_json::json!({ "mode": mode, "path": "legacy" })).is_err());
        }
        let selection: DownloadSelection = serde_json::from_value(serde_json::json!({ "mode": "files", "ids": ["id-a", "id-b"] })).expect("batch IDs");
        assert!(matches!(selection, DownloadSelection::Files { paths } if paths == ["id-a", "id-b"]));
        assert!(serde_json::from_value::<DownloadSelection>(serde_json::json!({ "mode": "files", "paths": ["legacy"] })).is_err());
        let dto = serde_json::to_value(item("opaque-id".into(), "local.txt".into(), 1).expect("item")).expect("queue identity DTO");
        assert_eq!(dto["id"], "opaque-id");
        assert_eq!(dto["relativePath"], "local.txt");
        assert!(dto.get("remotePath").is_none());
    }

    struct Adapter {
        calls: Arc<AtomicUsize>,
    }
    static ACTIVE_DOWNLOADS: AtomicUsize = AtomicUsize::new(0);
    static PEAK_DOWNLOADS: AtomicUsize = AtomicUsize::new(0);
    impl TransferQueueAdapter for Adapter {
        fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError> {
            Ok(tokio_util::sync::CancellationToken::new())
        }
        fn report_error(&self, _: &AppError) {}
        fn validate(&self) -> BoxFuture<'_, Result<(), AppError>> {
            Box::pin(async { Ok(()) })
        }
        fn plan(&self, selection: DownloadSelection) -> BoxFuture<'_, Result<Vec<DownloadItem>, AppError>> {
            Box::pin(async move {
                if matches!(selection, DownloadSelection::Folder { .. }) {
                    let mut directory = item(String::new(), "empty/nested".into(), 0)?;
                    directory.is_dir = true;
                    return Ok(vec![directory]);
                }
                Ok(vec![
                    item("a".into(), "a".into(), 0)?,
                    item("b".into(), "b".into(), 0)?,
                ])
            })
        }
        fn execute<'a>(
            &'a self,
            item: &'a DownloadItem,
            root: &'a crate::cloud::PhysicalRoot,
            overwrite: bool,
            cancel: tokio_util::sync::CancellationToken,
            _progress: Option<crate::peer::TransferProgressCallback>,
        ) -> BoxFuture<'a, Result<(), AppError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let active = ACTIVE_DOWNLOADS.fetch_add(1, Ordering::SeqCst) + 1;
                PEAK_DOWNLOADS.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                ACTIVE_DOWNLOADS.fetch_sub(1, Ordering::SeqCst);
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
    async fn fresh_files_download_concurrently_and_join_before_release() {
        PEAK_DOWNLOADS.store(0, Ordering::SeqCst);
        let dir = tempfile::tempdir().expect("dir");
        let queues = Arc::new(DownloadQueues::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let q = queues.select(DownloadBackend::S3 { connection_id: "test".into() }, DownloadSelection::Files { paths: vec![] }, crate::cloud::PhysicalRoot::open(dir.path()).expect("root"), Arc::new(Adapter { calls: calls.clone() })).await.expect("queue");
        queues.start(&q.queue_id).await.expect("start");
        let end = wait(&queues, &q.queue_id, "completed").await;
        assert_eq!(end.completed_files, 2);
        assert_eq!(PEAK_DOWNLOADS.load(Ordering::SeqCst), 2);
        assert_eq!(ACTIVE_DOWNLOADS.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(dir.path().join("a")).expect("a"), b"new");
        assert_eq!(std::fs::read(dir.path().join("b")).expect("b"), b"new");
        queues.release(&q.queue_id).await.expect("release");
    }

    #[tokio::test]
    async fn empty_directories_are_installed_without_file_payload_requests() {
        let dir = tempfile::tempdir().expect("dir");
        let queues = Arc::new(DownloadQueues::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let q = queues.select(DownloadBackend::Official { connection_id: "test".into() }, DownloadSelection::Folder { path: "folder/".into() }, crate::cloud::PhysicalRoot::open(dir.path()).expect("root"), Arc::new(Adapter { calls: calls.clone() })).await.expect("queue");
        queues.start(&q.queue_id).await.expect("start");
        wait(&queues, &q.queue_id, "completed").await;
        assert!(dir.path().join("empty/nested").is_dir());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
