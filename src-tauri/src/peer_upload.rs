use super::*;
use crate::commands::SharedPeers;
use crate::local_upload::{invalid, io, open_file_checked, token};
use crate::local_upload_types::{
    DirectoryBrowser, DirectoryEntryDto, DirectoryFile, DirectoryPageDto, DirectoryWalk,
};
use crate::peer_upload_types::*;
use futures_util::FutureExt;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

fn task_error(e: tokio::task::JoinError) -> AppError {
    AppError::from(DomainError::Internal {
        detail: e.to_string(),
    })
}
fn canceled() -> AppError {
    AppError::from(DomainError::TransferCanceled)
}
fn bound<'a>(
    map: &'a mut HashMap<String, PeerUploadBatch>,
    id: &str,
    peer: &str,
) -> Result<&'a mut PeerUploadBatch, AppError> {
    let b = map.get_mut(id).ok_or_else(invalid)?;
    if b.status.peer_id != peer {
        return Err(invalid());
    }
    Ok(b)
}
impl PeerManager {
    fn upload_scope(&self, peer: &str) -> Result<String, AppError> {
        let (record, _) = self.peer_client(peer).map_err(AppError::from)?;
        let code = Zeroizing::new(
            self.vault
                .load_peer_code(peer)
                .map_err(AppError::from)?
                .code,
        );
        Ok(hex::encode(Sha256::digest(
            format!(
                "{}\n{}\n{}\n{}",
                record.url,
                record.authorization_id.unwrap_or_default(),
                code.as_str(),
                self.upload_epoch.load(std::sync::atomic::Ordering::Acquire)
            )
            .as_bytes(),
        )))
    }
    pub(crate) async fn select_upload_batch(
        &self,
        peer: String,
        share: String,
        remote: String,
        paths: Vec<PathBuf>,
        mode: &str,
        options: crate::types::IgnoreOptions,
        scope: String,
    ) -> Result<PeerUploadSummary, AppError> {
        let _guard = self.upload_lifecycle.lock().await;
        if self.upload_scope(&peer)? != scope {
            return Err(invalid());
        }
        let remote = normalize_display_path(&remote).map_err(AppError::from)?;
        encode_share_path(&share, &remote).map_err(AppError::from)?;
        if paths.is_empty()
            || (mode == "folder" && paths.len() != 1)
            || options.patterns.len() > 1024 * 1024
        {
            return Err(invalid());
        }
        if self.upload_batches.lock().await.len() >= 4 {
            return Err(invalid());
        }
        let folder = mode == "folder";
        let (roots, browser, root_name) = tokio::task::spawn_blocking(move || {
            let mut roots = Vec::with_capacity(paths.len());
            let mut entries = Vec::with_capacity(paths.len());
            let mut browser = None;
            let mut root_name = String::new();
            for path in paths {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(invalid)?
                    .to_owned();
                crate::cloud::object_service::valid_relative_path(&name)
                    .map_err(|e| AppError::from(DomainError::from(e)))?;
                if folder {
                    let root = Arc::new(
                        crate::cloud::PhysicalRoot::open(&path)
                            .map_err(|e| AppError::from(DomainError::from(e)))?,
                    );
                    browser = Some(PeerBrowser::Directory(DirectoryBrowser::new(
                        root.upload_directory_handle()
                            .map_err(|e| AppError::from(DomainError::from(e)))?,
                        name.clone(),
                    )?));
                    root_name = name.clone();
                    roots.push(SelectedRoot::Directory { root, name });
                } else {
                    let parent =
                        crate::cloud::PhysicalRoot::open(path.parent().ok_or_else(invalid)?)
                            .map_err(|e| AppError::from(DomainError::from(e)))?;
                    let dir = parent
                        .upload_directory_handle()
                        .map_err(|e| AppError::from(DomainError::from(e)))?;
                    let meta = dir.symlink_metadata(&name).map_err(io)?;
                    if !meta.is_file() || meta.file_type().is_symlink() {
                        return Err(invalid());
                    }
                    let file = open_file_checked(&dir, &name, &meta)?;
                    let size = file.metadata().map_err(io)?.len();
                    entries.push(DirectoryEntryDto {
                        name: name.clone(),
                        relative_path: name.clone(),
                        kind: "file".into(),
                        size,
                        directory_token: None,
                        phase: "unscanned".into(),
                    });
                    roots.push(SelectedRoot::File(DirectoryFile {
                        relative: name,
                        file,
                        size,
                    }));
                }
            }
            Ok::<_, AppError>((
                roots,
                match browser {
                    Some(b) => b,
                    None => PeerBrowser::Files {
                        entries,
                        token: token()?,
                        cursor: None,
                    },
                },
                root_name,
            ))
        })
        .await
        .map_err(task_error)??;
        let status = PeerUploadSummary {
            batch_id: token()?,
            peer_id: peer,
            share_name: share,
            remote_dir: remote,
            bucket: String::new(),
            prefix: String::new(),
            root_name,
            phase: "selected".into(),
            current_relative_path: None,
            completed_files: 0,
            completed_bytes: 0,
            skipped_files: 0,
            skipped_symlinks: 0,
            skipped_empty_directories: 0,
            conflict: None,
            error: None,
            recovery_required: false,
        };
        let mut batches = self.upload_batches.lock().await;
        if batches.len() >= 4 {
            return Err(invalid());
        }
        let (cancel, _) = tokio::sync::watch::channel(false);
        batches.insert(
            status.batch_id.clone(),
            PeerUploadBatch {
                status: status.clone(),
                roots: Some(roots),
                browser: Arc::new(Mutex::new(browser)),
                options,
                cancel,
                decision: Arc::new(tokio::sync::Notify::new()),
                resolution: None,
                handle: None,
                stopping: Arc::new(Mutex::new(())),
                scope,
            },
        );
        Ok(status)
    }
    pub(crate) async fn start_upload_batch(
        self: &Arc<Self>,
        peer: &str,
        id: &str,
        policy: DirectoryConflictPolicy,
    ) -> Result<PeerUploadSummary, AppError> {
        let _guard = self.upload_lifecycle.lock().await;
        let scope = self.upload_scope(peer)?;
        let mut map = self.upload_batches.lock().await;
        let b = bound(&mut map, id, peer)?;
        if b.scope != scope || b.status.phase != "selected" {
            return Err(invalid());
        }
        let roots = b.roots.take().ok_or_else(invalid)?;
        let cancel = b.cancel.subscribe();
        let status = b.status.clone();
        let options = b.options.clone();
        let manager = Arc::clone(self);
        let id = id.to_owned();
        b.status.phase = "running".into();
        b.handle = Some(tokio::spawn(async move {
            let operation =
                manager.run_upload_batch(&id, roots, status, options, scope, cancel, policy);
            let result = match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                Ok(r) => r,
                Err(_) => Err(AppError::from(DomainError::Internal {
                    detail: "Peer upload worker panicked".into(),
                })),
            };
            manager.finish_upload_batch(&id, result).await;
        }));
        Ok(b.status.clone())
    }
    async fn run_upload_batch(
        &self,
        id: &str,
        roots: Vec<SelectedRoot>,
        status: PeerUploadSummary,
        options: crate::types::IgnoreOptions,
        scope: String,
        mut cancel: tokio::sync::watch::Receiver<bool>,
        mut policy: DirectoryConflictPolicy,
    ) -> Result<(), AppError> {
        for root in roots {
            if *cancel.borrow() {
                return Err(canceled());
            }
            match root {
                SelectedRoot::File(file) => {
                    self.upload_batch_file(id, &status, &scope, file, &mut cancel, &mut policy)
                        .await?
                }
                SelectedRoot::Directory { root, name } => {
                    let mut walk = DirectoryWalk::with_options(
                        root.upload_directory_handle()
                            .map_err(|e| AppError::from(DomainError::from(e)))?,
                        name,
                        Some(options.clone()),
                    )?;
                    loop {
                        if *cancel.borrow() {
                            return Err(canceled());
                        }
                        let (returned, next) = tokio::task::spawn_blocking(move || {
                            let next = walk.next_chunk();
                            (walk, next)
                        })
                        .await
                        .map_err(task_error)?;
                        walk = returned;
                        {
                            let mut map = self.upload_batches.lock().await;
                            let b = map.get_mut(id).ok_or_else(invalid)?;
                            b.status.skipped_symlinks = walk.skipped_symlinks;
                            b.status.skipped_empty_directories = walk.skipped_empty_directories;
                        }
                        if let Some(file) = next? {
                            self.upload_batch_file(
                                id,
                                &status,
                                &scope,
                                file,
                                &mut cancel,
                                &mut policy,
                            )
                            .await?;
                        }
                        if walk.stack.is_empty() {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    async fn upload_batch_file(
        &self,
        id: &str,
        status: &PeerUploadSummary,
        scope: &str,
        file: DirectoryFile,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
        policy: &mut DirectoryConflictPolicy,
    ) -> Result<(), AppError> {
        if *cancel.borrow() {
            return Err(canceled());
        }
        if self.upload_scope(&status.peer_id)? != scope {
            return Err(invalid());
        }
        if file.size > self.limits.max_transfer_bytes {
            return Err(AppError::from(DomainError::TransferTooLarge {
                size: file.size,
                limit: self.limits.max_transfer_bytes,
            }));
        }
        #[cfg(test)]
        if let Some(mock) = &self.upload_test_transport {
            mock.files.lock().await.push(file.relative.clone());
            if mock.block.load(std::sync::atomic::Ordering::Acquire) {
                cancel.changed().await.map_err(|_| invalid())?;
                return Err(canceled());
            }
            let mut map = self.upload_batches.lock().await;
            let b = map.get_mut(id).ok_or_else(invalid)?;
            b.status.completed_files += 1;
            b.status.completed_bytes += file.size;
            return Ok(());
        }
        let relative = Path::new(&file.relative);
        let name = relative
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(invalid)?;
        let nested = relative.parent().and_then(|s| s.to_str()).unwrap_or("");
        let directory = normalize_display_path(&format!(
            "{}/{}",
            status.remote_dir.trim_end_matches('/'),
            nested
        ))
        .map_err(AppError::from)?;
        let (_, client) = self.peer_client(&status.peer_id).map_err(AppError::from)?;
        let encoded = encode_share_path(&status.share_name, &directory).map_err(AppError::from)?;
        {
            let mut map = self.upload_batches.lock().await;
            map.get_mut(id)
                .ok_or_else(invalid)?
                .status
                .current_relative_path = Some(file.relative.clone());
        }
        tokio::select! { biased; _ = cancel.changed() => return Err(canceled()), r = ensure_remote_directories(&client, &status.share_name, &directory) => r.map_err(AppError::from)? }
        let destination = tokio::select! { biased; _ = cancel.changed() => return Err(canceled()), r = resolve_remote_destination(&client, &encoded, &directory, name, ConflictPolicy::Fail) => r };
        let (remote, _) = match destination {
            Ok(d) => d,
            Err(DomainError::TransferConflict { path }) => {
                let mut choice = *policy;
                if choice == DirectoryConflictPolicy::Ask {
                    let decision = {
                        let mut map = self.upload_batches.lock().await;
                        let b = map.get_mut(id).ok_or_else(invalid)?;
                        b.status.phase = "conflict".into();
                        b.status.conflict = Some(PeerUploadConflict {
                            conflict_id: token()?,
                            relative_path: file.relative.clone(),
                            key: path,
                            etag: None,
                            overwrite_allowed: true,
                            strong_consistency: false,
                        });
                        Arc::clone(&b.decision)
                    };
                    loop {
                        let notified = decision.notified();
                        if *cancel.borrow() {
                            return Err(canceled());
                        }
                        let resolved = {
                            let mut map = self.upload_batches.lock().await;
                            map.get_mut(id).ok_or_else(invalid)?.resolution.take()
                        };
                        if let Some((resolved, remaining)) = resolved {
                            choice = resolved;
                            if remaining {
                                *policy = resolved;
                            }
                            break;
                        }
                        tokio::select! { _ = cancel.changed() => return Err(canceled()), _ = notified => {} }
                    }
                }
                if choice == DirectoryConflictPolicy::Cancel {
                    return Err(canceled());
                }
                if choice == DirectoryConflictPolicy::Skip {
                    let mut map = self.upload_batches.lock().await;
                    let b = map.get_mut(id).ok_or_else(invalid)?;
                    b.status.skipped_files += 1;
                    b.status.phase = "running".into();
                    b.status.conflict = None;
                    b.status.current_relative_path = None;
                    return Ok(());
                }
                tokio::select! { biased; _ = cancel.changed() => return Err(canceled()), r = resolve_remote_destination(&client, &encoded, &directory, name, ConflictPolicy::Overwrite) => r.map_err(AppError::from)? }
            }
            Err(e) => return Err(AppError::from(e)),
        };
        if *cancel.borrow() {
            return Err(canceled());
        }
        if self.upload_scope(&status.peer_id)? != scope {
            return Err(invalid());
        }
        {
            let mut map = self.upload_batches.lock().await;
            let b = map.get_mut(id).ok_or_else(invalid)?;
            b.status.phase = "running".into();
            b.status.conflict = None;
        }
        crate::transfer::upload_opened(
            &client,
            &remote,
            file.file,
            relative,
            file.size,
            None,
            Some((id.to_owned(), cancel.clone())),
        )
        .await
        .map_err(AppError::from)?;
        let mut map = self.upload_batches.lock().await;
        let b = map.get_mut(id).ok_or_else(invalid)?;
        b.status.completed_files += 1;
        b.status.completed_bytes += file.size;
        b.status.current_relative_path = None;
        Ok(())
    }
    async fn finish_upload_batch(&self, id: &str, result: Result<(), AppError>) {
        let mut map = self.upload_batches.lock().await;
        if let Some(b) = map.get_mut(id) {
            b.status.conflict = None;
            b.status.current_relative_path = None;
            match result {
                Ok(()) => {
                    b.status.phase = if *b.cancel.borrow() {
                        "cancelled"
                    } else {
                        "completed"
                    }
                    .into()
                }
                Err(e) if e.code == "TRANSFER_CANCELED" => b.status.phase = "cancelled".into(),
                Err(e) => {
                    b.status.phase = "failed".into();
                    if b.status.error.is_none() {
                        self.events.error(&e);
                        b.status.error = Some(e);
                    }
                }
            }
        }
    }
    pub(crate) async fn cancel_upload_batch(&self, peer: &str, id: &str) -> Result<(), AppError> {
        let stopping = {
            let mut map = self.upload_batches.lock().await;
            if !map.contains_key(id) {
                return Ok(());
            }
            Arc::clone(&bound(&mut map, id, peer)?.stopping)
        };
        let _guard = stopping.lock().await;
        let handle = {
            let mut map = self.upload_batches.lock().await;
            if !map.contains_key(id) {
                return Ok(());
            }
            let b = bound(&mut map, id, peer)?;
            b.cancel.send_replace(true);
            b.decision.notify_one();
            if b.status.phase == "selected" {
                b.status.phase = "cancelled".into();
                b.roots = None;
            }
            b.handle.take()
        };
        if let Some(mut h) = handle {
            match tokio::time::timeout(Duration::from_secs(3), &mut h).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let error = task_error(e);
                    self.finish_upload_batch(id, Err(error.clone())).await;
                    return Err(error);
                }
                Err(_) => {
                    h.abort();
                    if let Err(e) = h.await {
                        log::warn!("peer upload worker aborted: {e}");
                    }
                    let error = AppError::from(DomainError::Internal {
                        detail: "Peer upload cancellation timed out; remote outcome unknown".into(),
                    });
                    self.finish_upload_batch(id, Err(error.clone())).await;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    async fn release_upload_batch(&self, peer: &str, id: &str) -> Result<(), AppError> {
        self.cancel_upload_batch(peer, id).await?;
        self.upload_batches.lock().await.remove(id);
        Ok(())
    }
    pub(crate) async fn stop_upload_batches(&self, peer: Option<&str>) -> Result<(), AppError> {
        let _guard = self.upload_lifecycle.lock().await;
        if peer.is_none() {
            self.upload_epoch
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        let ids: Vec<_> = self
            .upload_batches
            .lock()
            .await
            .iter()
            .filter(|(_, b)| peer.is_none_or(|p| b.status.peer_id == p))
            .map(|(id, b)| (id.clone(), b.status.peer_id.clone()))
            .collect();
        let mut first_error = None;
        for (id, peer) in ids {
            if let Err(error) = self.cancel_upload_batch(&peer, &id).await {
                log::error!("Failed to stop peer upload batch {id} for {peer}: {error:?}");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            self.upload_batches.lock().await.remove(&id);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
#[tauri::command]
pub async fn peer_select_upload_batch(
    app: tauri::AppHandle,
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_name: String,
    remote_dir: String,
    mode: String,
    options: Option<crate::types::IgnoreOptions>,
) -> Result<Option<PeerUploadSummary>, AppError> {
    if mode != "folder" && mode != "files" {
        return Err(invalid());
    }
    let scope = peers.upload_scope(&peer_id)?;
    let shares = peers.shares(&peer_id).await.map_err(AppError::from)?;
    let share = shares
        .into_iter()
        .find(|s| s.name == share_name || s.id == share_name)
        .ok_or_else(invalid)?;
    if !share.permissions.can_write {
        return Err(AppError::from(DomainError::PeerAuthorizationRejected));
    }
    let folder = mode == "folder";
    let selected = tokio::task::spawn_blocking(move || {
        if folder {
            app.dialog().file().blocking_pick_folder().map(|p| vec![p])
        } else {
            app.dialog().file().blocking_pick_files()
        }
    })
    .await
    .map_err(task_error)?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let paths = selected
        .into_iter()
        .map(|p| p.into_path().map_err(|_| invalid()))
        .collect::<Result<Vec<_>, _>>()?;
    peers
        .select_upload_batch(
            peer_id,
            share.id,
            remote_dir,
            paths,
            &mode,
            options.unwrap_or_default(),
            scope,
        )
        .await
        .map(Some)
}
#[tauri::command]
pub async fn peer_start_upload_batch(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    batch_id: String,
    conflict_policy: Option<DirectoryConflictPolicy>,
) -> Result<PeerUploadSummary, AppError> {
    peers
        .inner()
        .start_upload_batch(&peer_id, &batch_id, conflict_policy.unwrap_or_default())
        .await
}
#[tauri::command]
pub async fn peer_status_upload_batch(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    batch_id: String,
) -> Result<PeerUploadSummary, AppError> {
    Ok(
        bound(&mut *peers.upload_batches.lock().await, &batch_id, &peer_id)?
            .status
            .clone(),
    )
}
#[tauri::command]
pub async fn peer_page_upload_batch(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    batch_id: String,
    directory_token: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<DirectoryPageDto, AppError> {
    let (browser, active, completed) = {
        let mut map = peers.upload_batches.lock().await;
        let b = bound(&mut map, &batch_id, &peer_id)?;
        (
            Arc::clone(&b.browser),
            b.status.current_relative_path.clone(),
            b.status.phase == "completed",
        )
    };
    tokio::task::spawn_blocking(move || {
        let mut browser = browser.blocking_lock();
        match &mut *browser {
            PeerBrowser::Directory(b) => b.page(directory_token, cursor, limit, active, completed),
            PeerBrowser::Files {
                entries,
                token: root_token,
                cursor: saved,
            } => {
                if directory_token.as_ref().is_some_and(|t| t != root_token) {
                    return Err(invalid());
                }
                let start = match cursor {
                    None => 0,
                    Some(c) => saved
                        .as_ref()
                        .filter(|(s, _)| *s == c)
                        .map(|(_, i)| *i)
                        .ok_or_else(invalid)?,
                };
                let end = (start + limit.unwrap_or(100).clamp(1, 128)).min(entries.len());
                let mut page = entries[start..end].to_vec();
                for e in &mut page {
                    if active.as_deref() == Some(&e.relative_path) && !completed {
                        e.phase = "active".into();
                    }
                }
                *saved = if end < entries.len() {
                    Some((token()?, end))
                } else {
                    None
                };
                Ok(DirectoryPageDto {
                    directory_token: root_token.clone(),
                    parent_token: None,
                    relative_path: String::new(),
                    entries: page,
                    next_cursor: saved.as_ref().map(|(s, _)| s.clone()),
                })
            }
        }
    })
    .await
    .map_err(task_error)?
}
#[tauri::command]
pub async fn peer_resolve_upload_batch(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    batch_id: String,
    conflict_id: String,
    decision: DirectoryConflictPolicy,
    apply_to_remaining: bool,
) -> Result<(), AppError> {
    let mut map = peers.upload_batches.lock().await;
    resolve_batch_conflict(
        bound(&mut map, &batch_id, &peer_id)?,
        &conflict_id,
        decision,
        apply_to_remaining,
    )
}
fn resolve_batch_conflict(
    b: &mut PeerUploadBatch,
    conflict_id: &str,
    decision: DirectoryConflictPolicy,
    apply_to_remaining: bool,
) -> Result<(), AppError> {
    if decision == DirectoryConflictPolicy::Ask
        || b.status.phase != "conflict"
        || b.status
            .conflict
            .as_ref()
            .is_none_or(|c| c.conflict_id != conflict_id)
        || b.resolution.is_some()
    {
        return Err(invalid());
    }
    b.status.phase = "running".into();
    b.status.conflict = None;
    b.resolution = Some((decision, apply_to_remaining));
    b.decision.notify_one();
    Ok(())
}
#[tauri::command]
pub async fn peer_cancel_upload_batch(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    batch_id: String,
) -> Result<(), AppError> {
    peers.cancel_upload_batch(&peer_id, &batch_id).await
}
#[tauri::command]
pub async fn peer_release_upload_batch(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    batch_id: String,
) -> Result<(), AppError> {
    peers.release_upload_batch(&peer_id, &batch_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        Arc<PeerManager>,
        Arc<TestUploadTransport>,
    ) {
        let temp = tempfile::tempdir().expect("temp");
        let vault = Arc::new(Vault::open(&temp.path().join("vault.pd")).expect("vault"));
        let keys = temp.path().join("keys");
        std::fs::create_dir(&keys).expect("keys");
        vault
            .initialize(keys.to_str().expect("path"), "owner", "test password 2026")
            .expect("initialize");
        vault
            .save_peer_code("abcd", "http://127.0.0.1:9", "secret")
            .expect("secret");
        vault
            .save_peer_record(&crate::types::PeerRecord {
                id: "abcd".into(),
                label: "Peer".into(),
                url: "http://127.0.0.1:9".into(),
                authorization_id: None,
                machine_id: None,
                created_at: 0,
                last_connected_at: None,
            })
            .expect("record");
        let mock = Arc::new(TestUploadTransport::default());
        let mut manager = PeerManager::new(vault, temp.path().join("temp")).expect("manager");
        manager.upload_test_transport = Some(Arc::clone(&mock));
        (temp, Arc::new(manager), mock)
    }
    async fn select(manager: &PeerManager, path: PathBuf) -> PeerUploadSummary {
        manager
            .select_upload_batch(
                "abcd".into(),
                "share".into(),
                "/target".into(),
                vec![path],
                "folder",
                crate::types::IgnoreOptions::default(),
                manager.upload_scope("abcd").expect("scope"),
            )
            .await
            .expect("select")
    }
    #[tokio::test]
    async fn peer_streaming_batch_runs_over_128_without_status_driving_and_honors_ignore() {
        let (temp, manager, mock) = fixture();
        let root = temp.path().join("root");
        std::fs::create_dir_all(root.join("nested")).expect("root");
        for i in 0..300 {
            std::fs::write(root.join("nested").join(i.to_string()), b"x").expect("write");
        }
        std::fs::write(root.join(".gitignore"), "*.log\n").expect("ignore");
        std::fs::write(root.join("drop.log"), b"x").expect("ignored");
        std::fs::write(root.join(".DS_Store"), b"x").expect("builtin");
        #[cfg(unix)]
        std::os::unix::fs::symlink("nested/0", root.join("link")).expect("link");
        let selected = select(&manager, root.clone()).await;
        let browser = manager
            .upload_batches
            .lock()
            .await
            .get(&selected.batch_id)
            .expect("batch")
            .browser
            .clone();
        {
            let mut b = browser.lock().await;
            let PeerBrowser::Directory(b) = &mut *b else {
                panic!("directory")
            };
            let p = b.page(None, None, Some(128), None, false).expect("page");
            assert!(p.entries.iter().any(|e| e.kind == "directory"));
        }
        manager
            .start_upload_batch("abcd", &selected.batch_id, DirectoryConflictPolicy::Ask)
            .await
            .expect("start");
        let h = manager
            .upload_batches
            .lock()
            .await
            .get_mut(&selected.batch_id)
            .expect("batch")
            .handle
            .take()
            .expect("handle");
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("timeout")
            .expect("join");
        let map = manager.upload_batches.lock().await;
        let status = &map.get(&selected.batch_id).expect("batch").status;
        assert_eq!(status.phase, "completed");
        assert_eq!(status.completed_files, 301);
        #[cfg(unix)]
        assert_eq!(status.skipped_symlinks, 1);
        let files = mock.files.lock().await;
        assert!(files.iter().any(|f| f == "root/nested/299"));
        assert!(!files
            .iter()
            .any(|f| f.ends_with("drop.log") || f.ends_with(".DS_Store") || f.ends_with("link")));
    }
    #[tokio::test]
    async fn peer_streaming_batch_files_and_conflict_decisions_are_bounded_and_single_use() {
        let (temp, manager, _) = fixture();
        let mut paths = Vec::new();
        for i in 0..150 {
            let path = temp.path().join(i.to_string());
            std::fs::write(&path, b"x").expect("file");
            paths.push(path);
        }
        let selected = manager
            .select_upload_batch(
                "abcd".into(),
                "share".into(),
                "/".into(),
                paths,
                "files",
                crate::types::IgnoreOptions::default(),
                manager.upload_scope("abcd").expect("scope"),
            )
            .await
            .expect("select");
        let mut map = manager.upload_batches.lock().await;
        let b = map.get_mut(&selected.batch_id).expect("batch");
        assert_eq!(b.roots.as_ref().expect("roots").len(), 150);
        {
            let mut browser = b.browser.lock().await;
            let PeerBrowser::Files { entries, .. } = &mut *browser else {
                panic!("files")
            };
            assert_eq!(entries.len(), 150);
        }
        b.status.phase = "conflict".into();
        b.status.conflict = Some(PeerUploadConflict {
            conflict_id: "first".into(),
            relative_path: "a".into(),
            key: "/a".into(),
            etag: None,
            overwrite_allowed: true,
            strong_consistency: false,
        });
        assert!(
            resolve_batch_conflict(b, "wrong", DirectoryConflictPolicy::Overwrite, false).is_err()
        );
        resolve_batch_conflict(b, "first", DirectoryConflictPolicy::Skip, true).expect("resolve");
        assert_eq!(b.status.phase, "running");
        assert!(b.status.conflict.is_none());
        assert!(
            resolve_batch_conflict(b, "first", DirectoryConflictPolicy::Cancel, false).is_err()
        );
        assert_eq!(
            b.resolution.take(),
            Some((DirectoryConflictPolicy::Skip, true))
        );
        b.status.phase = "conflict".into();
        b.status.conflict = Some(PeerUploadConflict {
            conflict_id: "second".into(),
            relative_path: "b".into(),
            key: "/b".into(),
            etag: None,
            overwrite_allowed: true,
            strong_consistency: false,
        });
        assert!(
            resolve_batch_conflict(b, "first", DirectoryConflictPolicy::Overwrite, false).is_err()
        );
        resolve_batch_conflict(b, "second", DirectoryConflictPolicy::Overwrite, false)
            .expect("second");
    }
    #[tokio::test]
    async fn peer_streaming_batch_release_is_idempotent_and_rechecks_after_stopping_lock() {
        let (temp, manager, _) = fixture();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let selected = select(&manager, root.clone()).await;
        assert!(manager
            .cancel_upload_batch("other", &selected.batch_id)
            .await
            .is_err());
        assert!(manager
            .release_upload_batch("other", &selected.batch_id)
            .await
            .is_err());
        assert!(manager
            .upload_batches
            .lock()
            .await
            .contains_key(&selected.batch_id));
        let (first, second) = tokio::join!(
            manager.release_upload_batch("abcd", &selected.batch_id),
            manager.release_upload_batch("abcd", &selected.batch_id),
        );
        first.expect("first release");
        second.expect("concurrent release");
        manager
            .release_upload_batch("abcd", &selected.batch_id)
            .await
            .expect("repeat release");
        manager
            .cancel_upload_batch("abcd", &selected.batch_id)
            .await
            .expect("cancel missing");

        let selected = select(&manager, root).await;
        let stopping = manager
            .upload_batches
            .lock()
            .await
            .get(&selected.batch_id)
            .expect("batch")
            .stopping
            .clone();
        let guard = stopping.lock().await;
        let release = manager.release_upload_batch("abcd", &selected.batch_id);
        tokio::pin!(release);
        assert!(release.as_mut().now_or_never().is_none());
        // Another release can remove the batch while this caller waits for stopping.
        manager
            .upload_batches
            .lock()
            .await
            .remove(&selected.batch_id);
        drop(guard);
        release.await.expect("release removed while waiting");
        assert!(manager.upload_batches.lock().await.is_empty());
    }
    #[tokio::test]
    async fn peer_streaming_batch_stop_joins_all_workers_after_cancellation_timeout() {
        let (temp, manager, _) = fixture();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let mut workers = Vec::new();
        for _ in 0..2 {
            let selected = select(&manager, root.clone()).await;
            let handle = tokio::spawn(std::future::pending::<()>());
            workers.push(handle.abort_handle());
            let mut map = manager.upload_batches.lock().await;
            let b = map.get_mut(&selected.batch_id).expect("batch");
            b.status.phase = "running".into();
            b.handle = Some(handle);
        }
        let error = manager
            .stop_upload_batches(None)
            .await
            .expect_err("timeout error");
        assert!(error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("cancellation timed out")));
        assert!(workers.iter().all(tokio::task::AbortHandle::is_finished));
        assert!(manager.upload_batches.lock().await.is_empty());
        manager
            .stop_upload_batches(None)
            .await
            .expect("repeat stop");
    }
    #[tokio::test]
    async fn peer_streaming_batch_cancel_joins_worker_and_rejects_wrong_peer_and_late_scope() {
        let (temp, manager, mock) = fixture();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(root.join("file"), b"x").expect("file");
        let selected = select(&manager, root.clone()).await;
        assert!(manager
            .start_upload_batch("other", &selected.batch_id, DirectoryConflictPolicy::Ask)
            .await
            .is_err());
        manager
            .vault
            .save_peer_code("abcd", "http://127.0.0.1:9", "changed")
            .expect("change");
        assert!(manager
            .start_upload_batch("abcd", &selected.batch_id, DirectoryConflictPolicy::Ask)
            .await
            .is_err());
        manager
            .cancel_upload_batch("abcd", &selected.batch_id)
            .await
            .expect("cancel selected");
        let selected = select(&manager, root).await;
        mock.block.store(true, std::sync::atomic::Ordering::Release);
        manager
            .start_upload_batch("abcd", &selected.batch_id, DirectoryConflictPolicy::Ask)
            .await
            .expect("start");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !mock.files.lock().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("running");
        manager
            .cancel_upload_batch("abcd", &selected.batch_id)
            .await
            .expect("cancel");
        let map = manager.upload_batches.lock().await;
        let b = map.get(&selected.batch_id).expect("batch");
        assert_eq!(b.status.phase, "cancelled");
        assert!(b.handle.is_none());
        assert!(b.status.error.is_none());
    }
}
