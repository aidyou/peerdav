use super::{
    ObjectServiceError, PhysicalRoot, directory_types::*, object_runtime_types::*,
    runtime::CloudRuntime, types::CloudError,
};
#[cfg(test)]
use crate::local_upload::open_directory_checked;
use crate::{AppError, DomainError};
use futures_util::{FutureExt, StreamExt};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::io::AsyncReadExt;

fn app(e: impl Into<DomainError>) -> AppError {
    AppError::from(e.into())
}
fn invalid() -> AppError {
    app(ObjectServiceError::InvalidLocalCapability)
}
fn cancelled() -> AppError {
    app(ObjectServiceError::Cancelled)
}
fn token() -> Result<String, AppError> {
    super::object_runtime::random_id()
}

impl crate::traits::UploadQueueAdapter for S3UploadAdapter<'_> {
    type Status = ObjectUploadDirectoryDto;
    fn status<'a>(&'a self, id: &'a str) -> futures_util::future::BoxFuture<'a, Result<Self::Status, AppError>> { Box::pin(self.runtime.object_status_upload_directory(self.connection, id)) }
    fn page<'a>(&'a self, id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> futures_util::future::BoxFuture<'a, Result<DirectoryPageDto, AppError>> { Box::pin(self.runtime.object_page_upload_directory(self.connection, id, directory_token, cursor, limit)) }
    fn resolve<'a>(&'a self, id: &'a str, conflict_id: &'a str, decision: DirectoryConflictPolicy, apply_to_remaining: bool) -> futures_util::future::BoxFuture<'a, Result<(), AppError>> { Box::pin(self.runtime.object_resolve_upload_directory_conflict(self.connection, id, conflict_id, decision, apply_to_remaining)) }
    fn cancel<'a>(&'a self, id: &'a str) -> futures_util::future::BoxFuture<'a, Result<(), AppError>> { Box::pin(self.runtime.object_cancel_upload_directory(self.connection, id)) }
    fn release<'a>(&'a self, id: &'a str) -> futures_util::future::BoxFuture<'a, Result<(), AppError>> { Box::pin(self.runtime.object_release_upload_directory(self.connection, id)) }
    fn start<'a>(&'a self, id: &'a str, policy: DirectoryConflictPolicy) -> futures_util::future::BoxFuture<'a, Result<Self::Status, AppError>> {
        Box::pin(self.runtime.object_start_upload_directory_inner(self.connection, id, policy))
    }
}

impl CloudRuntime {
    pub(crate) async fn object_select_upload_files(&self, connection_id: &str, revision: u64, prefix: String, paths: Vec<std::path::PathBuf>) -> Result<ObjectUploadDirectoryDto, AppError> {
        self.object_select_upload_files_bound(connection_id, revision, prefix, paths, None).await
    }
    pub(crate) async fn object_select_upload_files_bound(&self, connection_id: &str, revision: u64, prefix: String, paths: Vec<std::path::PathBuf>, identity: Option<&super::ProtectedIdentity>) -> Result<ObjectUploadDirectoryDto, AppError> {
        self.object_admit()?;
        super::object_service::valid_prefix(&prefix).map_err(app)?;
        if prefix.len() > MAX_UPLOAD_DIRECTORY_PATH_BYTES { return Err(invalid()); }
        let _lifecycle = self.object_lifecycle.lock().await;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        if session.revision != revision || identity.is_some_and(|identity| *identity != session.identity) { return Err(invalid()); }
        let lifecycle = self.vault.transfer_cancellation().map_err(AppError::from)?;
        let (files, browser) = tokio::task::spawn_blocking(move || crate::local_upload::open_selected_files(paths)).await.map_err(|e| app(DomainError::Internal { detail: e.to_string() }))??;
        let queue_id = token()?;
        let status = ObjectUploadDirectoryDto { batch_id: queue_id.clone(), queue_id, connection_id: connection_id.into(), connection_revision: revision, bucket: session.bucket.clone(), prefix, root_name: String::new(), phase: "selected".into(), current_relative_path: None, completed_files: 0, completed_bytes: 0, total_files: files.len() as u64, skipped_files: 0, skipped_symlinks: 0, skipped_empty_directories: 0, conflict: None, error: None, recovery_required: false };
        let mut state = self.objects.lock().await;
        state.prune();
        if state.upload_directories.len() >= 4 || state.local_handle_count().saturating_add(files.len()) > MAX_LOCAL_HANDLES { return Err(invalid()); }
        state.upload_directories.insert(status.batch_id.clone(), DirectoryBatch { status: status.clone(), session, root: None, files: Some(files), browser: Arc::new(tokio::sync::Mutex::new(browser)), cancel: Arc::new(AtomicBool::new(false)), cancel_signal: Arc::new(tokio::sync::Notify::new()), decision: Arc::new(tokio::sync::Notify::new()), resolution: None, handle: None, stopping: Arc::new(tokio::sync::Mutex::new(())), created: std::time::Instant::now(), lifecycle });
        Ok(status)
    }
    pub(crate) async fn object_select_upload_directory(
        &self,
        connection_id: &str,
        revision: u64,
        prefix: String,
        path: &Path,
    ) -> Result<ObjectUploadDirectoryDto, AppError> {
        self.object_select_upload_directory_bound(connection_id, revision, prefix, path, None).await
    }
    pub(crate) async fn object_select_upload_directory_bound(
        &self, connection_id: &str, revision: u64, prefix: String, path: &Path, identity: Option<&super::ProtectedIdentity>,
    ) -> Result<ObjectUploadDirectoryDto, AppError> {
        self.object_admit()?;
        super::object_service::valid_prefix(&prefix).map_err(app)?;
        if prefix.len() > MAX_UPLOAD_DIRECTORY_PATH_BYTES {
            return Err(invalid());
        }
        let _lifecycle = self.object_lifecycle.lock().await;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        if session.revision != revision || identity.is_some_and(|identity| *identity != session.identity) {
            return Err(invalid());
        }
        let root_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(invalid)?
            .to_owned();
        super::object_service::valid_relative_path(&root_name).map_err(app)?;
        let path = path.to_owned();
        let root = Arc::new(
            tokio::task::spawn_blocking(move || PhysicalRoot::open(path))
                .await
                .map_err(|e| {
                    app(DomainError::Internal {
                        detail: e.to_string(),
                    })
                })?
                .map_err(app)?,
        );
        let browser = DirectoryBrowser {
            chain: vec![BrowseDirectory {
                token: token()?,
                dir: root.upload_directory_handle().map_err(app)?,
                relative: root_name.clone(),
            }],
            entries: None,
            cursor: None,
            children: Vec::new(),
        };
        let queue_id = token()?;
        let status = ObjectUploadDirectoryDto {
            batch_id: queue_id.clone(),
            queue_id,
            connection_id: connection_id.into(),
            connection_revision: revision,
            bucket: session.bucket.clone(),
            prefix,
            root_name,
            phase: "selected".into(),
            current_relative_path: None,
            completed_files: 0,
            completed_bytes: 0,
            total_files: 0,
            skipped_files: 0,
            skipped_symlinks: 0,
            skipped_empty_directories: 0,
            conflict: None,
            error: None,
            recovery_required: false,
        };
        let mut state = self.objects.lock().await;
        state.prune();
        if state.upload_directories.len() >= 4 || state.local_handle_count() >= MAX_LOCAL_HANDLES {
            return Err(invalid());
        }
        state.upload_directories.insert(
            status.batch_id.clone(),
            DirectoryBatch {
                status: status.clone(),
                session,
                root: Some(root),
                files: None,
                browser: Arc::new(tokio::sync::Mutex::new(crate::peer_upload_types::PeerBrowser::Directory(browser))),
                cancel: Arc::new(AtomicBool::new(false)),
                cancel_signal: Arc::new(tokio::sync::Notify::new()),
                decision: Arc::new(tokio::sync::Notify::new()),
                resolution: None,
                handle: None,
                stopping: Arc::new(tokio::sync::Mutex::new(())),
                created: std::time::Instant::now(),
                lifecycle: self.vault.transfer_cancellation().map_err(AppError::from)?,
            },
        );
        Ok(status)
    }
    pub async fn object_status_upload_directory(
        &self,
        connection: &str,
        id: &str,
    ) -> Result<ObjectUploadDirectoryDto, AppError> {
        self.object_admit()?;
        let state = self.objects.lock().await;
        let batch = state.upload_directories.get(id).ok_or_else(invalid)?;
        if batch.status.connection_id != connection {
            return Err(invalid());
        }
        Ok(batch.status.clone())
    }
    pub async fn object_start_upload_directory(
        self: &Arc<Self>, connection: &str, id: &str, policy: DirectoryConflictPolicy,
    ) -> Result<ObjectUploadDirectoryDto, AppError> {
        crate::local_upload::start_queue(&S3UploadAdapter { runtime: self, connection }, id, policy).await
    }
    async fn object_start_upload_directory_inner(
        self: &Arc<Self>,
        connection: &str,
        id: &str,
        policy: DirectoryConflictPolicy,
    ) -> Result<ObjectUploadDirectoryDto, AppError> {
        self.object_admit()?;
        let _lifecycle = self.object_lifecycle.lock().await;
        if self
            .object_recovery_read()?
            .iter()
            .any(|s| s.connection_id == connection && s.recovery_required)
        {
            return Err(app(DomainError::Cloud {
                code: "CLOUD_UPLOAD_PENDING_RECOVERY",
                user_reason: "请先确认上次上传的未知结果 / Acknowledge the previous unknown upload outcome first",
                source: Box::new(ObjectServiceError::InvalidLocalCapability),
            }));
        }
        let session = self.object_session(connection).await?;
        let mut state = self.objects.lock().await;
        let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
        if batch.lifecycle.is_cancelled() || batch.status.connection_id != connection
            || !Arc::ptr_eq(&batch.session, &session)
            || batch.status.phase != "selected"
        {
            return Err(invalid());
        }
        batch.status.phase = "running".into();
        let root = batch.root.clone();
        let files = batch.files.take();
        let cancel = Arc::clone(&batch.cancel);
        let cancel_signal = Arc::clone(&batch.cancel_signal);
        let lifecycle = batch.lifecycle.clone();
        let name = batch.status.root_name.clone();
        let prefix = batch.status.prefix.clone();
        let id = id.to_owned();
        let runtime = Arc::clone(self);
        batch.handle = Some(tokio::spawn(async move {
            // Only pre-upload waits are interruptible. Once upload starts, keep
            // polling it so the service can cancel and clean up multipart state;
            // object_cancel_upload_directory bounds that cleanup to three seconds.
            let run = runtime.directory_run(
                &id,
                root,
                files,
                name,
                prefix,
                session,
                Arc::clone(&cancel),
                Arc::clone(&cancel_signal),
                policy,
            );
            let operation = async {
                tokio::pin!(run);
                tokio::select! {
                    r = &mut run => r,
                    _ = lifecycle.cancelled() => {
                        cancel.store(true, Ordering::Release);
                        cancel_signal.notify_one();
                        run.await
                    }
                }
            };
            let result = match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                Ok(result) => result,
                Err(_) => Err(app(DomainError::Internal {
                    detail: "Directory upload task panicked".into(),
                })),
            };
            runtime.directory_finish(&id, result).await;
        }));
        Ok(batch.status.clone())
    }
    async fn directory_run(
        &self,
        id: &str,
        root: Option<Arc<PhysicalRoot>>,
        files: Option<Vec<crate::local_upload_types::DirectoryFile>>,
        name: String,
        prefix: String,
        session: Arc<ObjectSession>,
        cancel: Arc<AtomicBool>,
        cancel_signal: Arc<tokio::sync::Notify>,
        policy: DirectoryConflictPolicy,
    ) -> Result<(), AppError> {
        let mut policy = crate::types::QueueConflictPolicy::from_upload(policy)?;
        let mut walk = root.as_ref().map(|root| DirectoryWalk::new(root, name)).transpose()?;
        let mut files = files.unwrap_or_default().into_iter();
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let mut walked = false;
            let file = if let Some(mut current) = walk.take() {
                walked = true;
                let (returned, next) = tokio::task::spawn_blocking(move || {
                    let next = current.next_chunk(); (current, next)
                }).await.map_err(|e| app(DomainError::Internal { detail: e.to_string() }))?;
                let mut state = self.objects.lock().await;
                let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
                batch.status.skipped_symlinks = returned.skipped_symlinks;
                batch.status.skipped_empty_directories = returned.skipped_empty_directories;
                let done = returned.stack.is_empty();
                walk = Some(returned);
                match next? { Some(file) => file, None if done => break, None => continue }
            } else {
                match files.next() { Some(file) => file, None => break }
            };
            let _permit = tokio::select! {
                _ = cancel_signal.notified() => return Err(cancelled()),
                permit = self.network.read() => permit,
            };
            self.object_admit()?;
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let key = if prefix.is_empty() {
                file.relative.clone()
            } else {
                format!("{}/{}", prefix.trim_end_matches('/'), file.relative)
            };
            if key.len() > MAX_UPLOAD_DIRECTORY_PATH_BYTES {
                return Err(invalid());
            }
            session.service.preflight_upload(&key).map_err(app)?;
            {
                let mut state = self.objects.lock().await;
                let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
                batch.status.current_relative_path = Some(file.relative.clone());
                // 目录批次流式扫描，只有发现一个文件才能确定总量；选中文件批次已在选择时计数。
                if walked {
                    batch.status.total_files += 1;
                }
            }
            let stat = tokio::select! {
                _ = cancel_signal.notified() => return Err(cancelled()),
                result = session.store.stat(&key) => result,
            };
            let existing = match stat {
                Ok(meta) if meta.key == key => Some(meta),
                Ok(_) => return Err(invalid()),
                Err(CloudError::NotFound) => None,
                Err(e) => return Err(app(e)),
            };
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let mut choice = policy.upload_choice();
            if let Some(meta) = &existing {
                if choice == DirectoryConflictPolicy::Ask {
                    let notify = {
                        let mut state = self.objects.lock().await;
                        let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
                        batch.status.phase = "conflict".into();
                        batch.status.conflict = Some(DirectoryConflictDto {
                            overwrite_allowed: meta.etag.as_ref().is_some_and(|etag| !etag.is_empty()),
                            conflict_id: token()?,
                            relative_path: file.relative.clone(),
                            key: key.clone(),
                            etag: meta.etag.clone(),
                        });
                        Arc::clone(&batch.decision)
                    };
                    choice = crate::local_upload::wait_queue_conflict(
                        &notify,
                        async {
                            if !cancel.load(Ordering::Acquire) {
                                cancel_signal.notified().await;
                            }
                            cancelled()
                        },
                        || async {
                            Ok(self
                                .objects
                                .lock()
                                .await
                                .upload_directories
                                .get_mut(id)
                                .ok_or_else(invalid)?
                                .resolution
                                .take())
                        },
                        &mut policy,
                    )
                    .await?;
                }
                if choice == DirectoryConflictPolicy::Cancel {
                    return Err(cancelled());
                }
                if choice == DirectoryConflictPolicy::Skip {
                    let mut state = self.objects.lock().await;
                    let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
                    batch.status.skipped_files += 1;
                    batch.status.current_relative_path = None;
                    batch.status.conflict = None;
                    batch.status.phase = "running".into();
                    continue;
                }
                if meta.etag.as_ref().is_none_or(|s| s.is_empty()) {
                    return Err(app(ObjectServiceError::DestinationExists));
                }
            }
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let recovery = ObjectTransferDto {
                transfer_id: id.into(),
                connection_id: session_identity_connection(self, id).await?,
                connection_revision: session.revision,
                direction: "upload".into(),
                key: Some(key.clone()),
                size_hint: Some(file.size),
                phase: "running".into(),
                error: None,
                recovery_required: true,
            };
            {
                let mut state = self.objects.lock().await;
                let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
                if cancel.load(Ordering::Acquire) {
                    return Err(cancelled());
                }
                // No await between durable intent and its in-memory mirror: a
                // cancellation must not hide an upload requiring reconciliation.
                self.object_recovery_write(&recovery)?;
                batch.status.recovery_required = true;
                batch.status.conflict = None;
                batch.status.phase = "running".into();
            }
            let stream = futures_util::stream::try_unfold(
                tokio::fs::File::from_std(file.file),
                |mut file| async move {
                    let mut buffer = vec![0u8; 1024 * 1024];
                    let count =
                        file.read(&mut buffer)
                            .await
                            .map_err(|source| CloudError::Storage {
                                source: object_store::Error::Generic {
                                    store: "local file",
                                    source: Box::new(source),
                                },
                            })?;
                    if count == 0 {
                        return Ok(None);
                    }
                    buffer.truncate(count);
                    Ok(Some((bytes::Bytes::from(buffer), file)))
                },
            )
            .boxed();
            let result = session
                .service
                .upload_stream_with_control(
                    &key,
                    stream,
                    Arc::clone(&cancel),
                    existing.and_then(|m| m.etag),
                )
                .await
                .map_err(app);
            match &result {
                Err(e)
                    if matches!(
                        e.code.as_str(),
                        "CLOUD_UPLOAD_PENDING_RECOVERY" | "INTERNAL_ERROR"
                    ) =>
                {
                    return result.map(|_| ());
                }
                _ => {
                    self.object_recovery_remove(id)?;
                    self.objects
                        .lock()
                        .await
                        .upload_directories
                        .get_mut(id)
                        .ok_or_else(invalid)?
                        .status
                        .recovery_required = false;
                }
            }
            result?;
            let mut state = self.objects.lock().await;
            let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
            batch.status.completed_files += 1;
            batch.status.completed_bytes += file.size;
            batch.status.current_relative_path = None;
            batch.status.conflict = None;
            batch.status.phase = "running".into();
        }
        Ok(())
    }
    async fn directory_finish(&self, id: &str, result: Result<(), AppError>) {
        let mut state = self.objects.lock().await;
        if let Some(batch) = state.upload_directories.get_mut(id) {
            batch.status.phase = match &result {
                Ok(()) => "completed",
                Err(e) if e.code == "CLOUD_CANCELLED" => "cancelled",
                Err(_) => "failed",
            }
            .into();
            batch.status.error = result.err();
            batch.status.conflict = None;
            batch.status.current_relative_path = None;
            batch.created = std::time::Instant::now();
            if let Some(error) = batch
                .status
                .error
                .clone()
                .filter(|e| e.code != "CLOUD_CANCELLED")
            {
                self.object_report(error);
            }
        }
    }
    pub async fn object_resolve_upload_directory_conflict(
        &self,
        connection: &str,
        id: &str,
        conflict_id: &str,
        policy: DirectoryConflictPolicy,
        remaining: bool,
    ) -> Result<(), AppError> {
        self.object_admit()?;
        if matches!(policy, DirectoryConflictPolicy::Ask | DirectoryConflictPolicy::Cancel) {
            return Err(invalid());
        }
        let mut state = self.objects.lock().await;
        let batch = state.upload_directories.get_mut(id).ok_or_else(invalid)?;
        if batch.lifecycle.is_cancelled() || batch.status.connection_id != connection
            || batch.status.phase != "conflict"
            || batch
                .status
                .conflict
                .as_ref()
                .is_none_or(|conflict| conflict.conflict_id != conflict_id)
            || batch.resolution.is_some()
            || (policy == DirectoryConflictPolicy::Overwrite && batch.status.conflict.as_ref().is_none_or(|conflict| !conflict.overwrite_allowed))
            || batch.cancel.load(Ordering::Acquire)
        {
            return Err(invalid());
        }
        crate::types::QueueConflictPolicy::resolve(
            batch.status.conflict.as_ref().map(|c| c.conflict_id.as_str()),
            conflict_id, &mut batch.resolution, policy, remaining,
        )?;
        // Close this decision before waking the worker so duplicates cannot replace it.
        batch.status.phase = "running".into();
        batch.status.conflict = None;
        batch.decision.notify_one();
        Ok(())
    }
    pub async fn object_cancel_upload_directory(
        &self,
        connection: &str,
        id: &str,
    ) -> Result<(), AppError> {
        let stopping = {
            let state = self.objects.lock().await;
            let Some(batch) = state.upload_directories.get(id) else {
                return Ok(());
            };
            if batch.status.connection_id != connection {
                return Err(invalid());
            }
            Arc::clone(&batch.stopping)
        };
        // Never infer join completion from phase: the worker can publish a terminal
        // phase before its JoinHandle completes. This per-batch lock is not the
        // global objects/lifecycle lock and is never needed by the worker.
        let _stopping = stopping.lock().await;
        let handle = {
            let mut state = self.objects.lock().await;
            let Some(batch) = state.upload_directories.get_mut(id) else {
                return Ok(());
            };
            if batch.status.connection_id != connection {
                return Err(invalid());
            }
            batch.cancel.store(true, Ordering::Release);
            batch.cancel_signal.notify_one();
            batch.decision.notify_one();
            if batch.status.phase == "selected" {
                batch.status.phase = "cancelled".into();
            }
            batch.handle.take()
        };
        if let Some(mut handle) = handle {
            match tokio::time::timeout(std::time::Duration::from_secs(3), &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(source)) => {
                    self.directory_finish(
                        id,
                        Err(app(DomainError::Internal {
                            detail: source.to_string(),
                        })),
                    )
                    .await;
                }
                Err(_) => {
                    handle.abort();
                    if let Err(source) = handle.await {
                        log::warn!("directory worker aborted: {source}");
                    }
                    let error = app(DomainError::Internal {
                        detail: "Directory upload stop timed out; current outcome unknown".into(),
                    });
                    self.directory_finish(id, Err(error)).await;
                }
            }
        }
        Ok(())
    }
    pub async fn object_page_upload_directory(
        &self,
        connection: &str,
        id: &str,
        directory_token: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<DirectoryPageDto, AppError> {
        self.object_admit()?;
        let (browser, active, completed) = {
            let state = self.objects.lock().await;
            let batch = state.upload_directories.get(id).ok_or_else(invalid)?;
            if batch.status.connection_id != connection {
                return Err(invalid());
            }
            (
                Arc::clone(&batch.browser),
                batch.status.current_relative_path.clone(),
                batch.status.phase == "completed",
            )
        };
        tokio::task::spawn_blocking(move || {
            browser
                .blocking_lock()
                .page(directory_token, cursor, limit, active, completed)
        })
        .await
        .map_err(|e| {
            app(DomainError::Internal {
                detail: e.to_string(),
            })
        })?
    }
    pub async fn object_release_upload_directory(
        &self,
        connection: &str,
        id: &str,
    ) -> Result<(), AppError> {
        self.object_cancel_upload_directory(connection, id).await?;
        self.objects.lock().await.upload_directories.remove(id);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn directory_open_rejects_replaced_child_and_symlink() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("child"))?;
        let parent = cap_std::fs::Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority())?;
        let metadata = parent.symlink_metadata("child")?;
        assert!(open_directory_checked(&parent, "child", &metadata).is_ok());
        // Keep the original inode alive so replacement cannot reuse its identity.
        std::fs::rename(temp.path().join("child"), temp.path().join("original"))?;
        std::fs::create_dir(temp.path().join("child"))?;
        assert!(open_directory_checked(&parent, "child", &metadata).is_err());
        std::fs::remove_dir(temp.path().join("child"))?;
        std::os::unix::fs::symlink("original", temp.path().join("child"))?;
        assert!(open_directory_checked(&parent, "child", &metadata).is_err());
        Ok(())
    }
    #[test]
    fn streaming_directory_walk_state_is_depth_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("nested"))?;
        for n in 0..4096 {
            std::fs::write(temp.path().join("nested").join(n.to_string()), b"")?;
        }
        let root = PhysicalRoot::open(temp.path())?;
        let mut walk = DirectoryWalk::new(&root, "root".into())
            .map_err(|e| std::io::Error::other(e.message))?;
        let first = walk
            .next_chunk()
            .map_err(|e| std::io::Error::other(e.message))?;
        assert!(first.is_some());
        assert!(walk.stack.len() <= 2);
        let mut count = 1;
        while !walk.stack.is_empty() {
            if walk
                .next_chunk()
                .map_err(|e| std::io::Error::other(e.message))?
                .is_some()
            {
                count += 1;
            }
            assert!(walk.stack.len() <= 2);
        }
        assert_eq!(count, 4096);
        Ok(())
    }
}
async fn session_identity_connection(runtime: &CloudRuntime, id: &str) -> Result<String, AppError> {
    Ok(runtime
        .objects
        .lock()
        .await
        .upload_directories
        .get(id)
        .ok_or_else(invalid)?
        .status
        .connection_id
        .clone())
}
