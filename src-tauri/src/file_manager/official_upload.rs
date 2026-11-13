use super::types::*;
use crate::{AppError, cloud::runtime::CloudRuntime};
use std::{collections::HashMap, sync::Arc};

impl crate::traits::UploadQueueAdapter for OfficialUploadAdapter<'_> {
    type Status = crate::cloud::directory_types::ObjectUploadDirectoryDto;
    fn status<'a>(&'a self, id: &'a str) -> futures_util::future::BoxFuture<'a, Result<Self::Status, AppError>> { Box::pin(action(self.backend, self.cloud, id, "status")) }
    fn page<'a>(&'a self, id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> futures_util::future::BoxFuture<'a, Result<crate::local_upload_types::DirectoryPageDto, AppError>> {
        Box::pin(async move {
            validate(self.cloud, &self.backend.connection_id, self.backend.revision)?;
            let mut jobs = self.backend.uploads.lock().await;
            let job = jobs.get_mut(id).ok_or_else(super::service::invalid)?;
            if let Some(browser) = job.browser.as_mut() {
                let active = job.status.current_relative_path.clone();
                let completed = job.status.phase == "completed";
                let mut page = browser.page(directory_token, cursor, limit, active.clone(), completed)?;
                for entry in &mut page.entries {
                    if completed && entry.kind == "directory" { entry.phase = "completed".into(); }
                    if let Some((phase, bytes, _)) = job.entry_states.get(&entry.relative_path) {
                        entry.phase = phase.clone(); entry.transferred = Some(*bytes);
                    }
                    if active.as_deref() == Some(entry.relative_path.as_str()) {
                        entry.transferred = Some(job.progress.load(std::sync::atomic::Ordering::Relaxed).min(entry.size));
                    }
                }
                return Ok(page);
            }
            if directory_token.as_deref().is_some_and(|token| token != id) { return Err(super::service::invalid()); }
            let start = cursor.as_deref().map(str::parse::<usize>).transpose().map_err(|_| super::service::invalid())?.unwrap_or(0);
            if start > job.page.entries.len() { return Err(super::service::invalid()); }
            let end = start.saturating_add(limit.unwrap_or(100).clamp(1, 128)).min(job.page.entries.len());
            let mut page = job.page.clone();
            page.entries = job.page.entries[start..end].to_vec();
            for entry in &mut page.entries {
                if let Some((phase, bytes, _)) = job.entry_states.get(&entry.relative_path) { entry.phase = phase.clone(); entry.transferred = Some(*bytes); }
                if entry.kind == "directory" {
                    let prefix = format!("{}/", entry.relative_path);
                    let children: Vec<_> = job.entry_states.iter().filter(|(path, _)| path.starts_with(&prefix)).collect();
                    entry.size = children.iter().map(|(_, (_, _, size))| *size).sum();
                    entry.transferred = Some(children.iter().map(|(_, (_, bytes, _))| *bytes).sum());
                    let completed = children.iter().all(|(_, (phase, _, _))| matches!(phase.as_str(), "completed" | "skipped"));
                    entry.phase = if completed && children.iter().all(|(_, (phase, _, _))| phase == "skipped") { "skipped" } else if completed { "completed" } else if children.iter().any(|(_, (phase, _, _))| phase == "failed") { "failed" } else if children.iter().any(|(_, (phase, _, _))| phase == "cancelled") { "cancelled" } else if job.status.current_relative_path.as_ref().is_some_and(|path| path.starts_with(&prefix)) { "active" } else { "selected" }.into();
                    if let Some(path) = job.status.current_relative_path.as_ref().filter(|path| path.starts_with(&prefix)) {
                        if let Some((phase, _, size)) = job.entry_states.get(path) { if phase == "active" { entry.transferred = Some(entry.transferred.unwrap_or(0).saturating_add(job.progress.load(std::sync::atomic::Ordering::Relaxed).min(*size))); } }
                    }
                }
                if job.status.current_relative_path.as_deref() == Some(entry.relative_path.as_str()) && entry.phase == "active" {
                    entry.transferred = Some(job.progress.load(std::sync::atomic::Ordering::Relaxed).min(entry.size));
                }
            }
            page.next_cursor = (end < job.page.entries.len()).then(|| end.to_string());
            Ok(page)
        })
    }
    fn resolve<'a>(&'a self, id: &'a str, conflict_id: &'a str, decision: crate::cloud::directory_types::DirectoryConflictPolicy, remaining: bool) -> futures_util::future::BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            use crate::cloud::directory_types::DirectoryConflictPolicy;
            validate(self.cloud, &self.backend.connection_id, self.backend.revision)?;
            let mut jobs = self.backend.uploads.lock().await;
            let job = jobs.get_mut(id).ok_or_else(super::service::invalid)?;
            if job.cancel.is_cancelled() || job.status.phase != "conflict" || !matches!(decision, DirectoryConflictPolicy::Skip | DirectoryConflictPolicy::Overwrite) || (decision == DirectoryConflictPolicy::Overwrite && job.status.conflict.as_ref().and_then(|c| c.etag.as_ref()).is_none()) { return Err(super::service::invalid()); }
            crate::types::QueueConflictPolicy::resolve(job.status.conflict.as_ref().map(|c| c.conflict_id.as_str()), conflict_id, &mut job.resolution, decision, remaining)?;
            job.status.phase = "running".into(); job.status.conflict = None;
            job.decision.notify_one();
            Ok(())
        })
    }
    fn cancel<'a>(&'a self, id: &'a str) -> futures_util::future::BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            action(self.backend, self.cloud, id, "cancel").await?;
            let handle = self.backend.uploads.lock().await.get_mut(id).ok_or_else(super::service::invalid)?.handle.take();
            if let Some(handle) = handle { handle.await.map_err(|error| AppError::from(crate::DomainError::Internal { detail: error.to_string() }))?; }
            Ok(())
        })
    }
    fn release<'a>(&'a self, id: &'a str) -> futures_util::future::BoxFuture<'a, Result<(), AppError>> { Box::pin(release(self.backend, id)) }
    fn start<'a>(&'a self, id: &'a str, policy: crate::cloud::directory_types::DirectoryConflictPolicy) -> futures_util::future::BoxFuture<'a, Result<Self::Status, AppError>> {
        Box::pin(async move { if policy != crate::cloud::directory_types::DirectoryConflictPolicy::Ask { return Err(super::service::invalid()); } action(self.backend, self.cloud, id, "start").await })
    }
}

async fn upload_folder(api: &crate::cloud::official_api_types::OfficialApi, parent: &str, name: &str) -> Result<String, AppError> {
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    let mut found = None;
    let mut observed = 0usize;
    loop {
        let page = api.list(parent, cursor.as_deref()).await.map_err(crate::cloud::official_runtime::api_error)?;
        observed = observed.saturating_add(page.entries.len());
        if observed > 100_000 { return Err(super::service::invalid()); }
        for entry in page.entries.into_iter().filter(|entry| entry.name == name) {
            if !entry.is_dir || found.is_some() {
                return Err(AppError { code: "FILE_TARGET_AMBIGUOUS".into(), message: "目标名称对应多个条目或不是目录，请打开具体目标目录后上传 / Target name is ambiguous or is not a folder; open the intended folder before uploading".into(), detail: None, context: Some(serde_json::json!({ "operation": "upload", "name": name })) });
            }
            found = Some(entry.id);
        }
        match page.next {
            Some(next) if seen.len() < 1000 && seen.insert(next.clone()) => cursor = Some(next),
            Some(_) => return Err(super::service::invalid()),
            None if page.complete => break,
            None => return Err(super::service::invalid()),
        }
    }
    if let Some(id) = found { return Ok(id); }
    api.create_folder_id(parent, name).await.map_err(crate::cloud::official_runtime::api_error)
}

pub(crate) async fn action(backend: &OfficialBrowserBackend, cloud: &Arc<CloudRuntime>, id: &str, action: &str) -> Result<crate::cloud::directory_types::ObjectUploadDirectoryDto, AppError> {
    let mut jobs = backend.uploads.lock().await;
    let job = jobs.get_mut(id).ok_or_else(super::service::invalid)?;
    if action == "cancel" {
        job.cancel.cancel();
        if job.status.phase == "selected" { job.files.clear(); job.status.phase = "cancelled".into(); }
    }
    if action == "start" {
        if job.status.phase != "selected" || job.cancel.is_cancelled() { return Err(super::service::invalid()); }
        let files = std::mem::take(&mut job.files);
        let mut walk = job.walk.take();
        let streaming = walk.is_some();
        let parent = job.parent.clone();
        let root_name = job.status.root_name.clone();
        let decision = job.decision.clone();
        let cancel = job.cancel.clone();
        let jobs = backend.uploads.clone();
        let cloud = cloud.clone();
        let connection = backend.connection_id.clone();
        let revision = backend.revision;
        let id = id.to_owned();
        job.status.phase = "running".into();
        job.handle = Some(tokio::spawn(async move {
            use futures_util::FutureExt;
            let work = async {
                let _network = cloud.network.read().await;
                let mut folders = HashMap::from([(String::new(), parent)]);
                if !root_name.is_empty() {
                    validate(&cloud, &connection, revision)?;
                    let (_, api) = cloud.official_api(&connection).await?;
                    let root = upload_folder(&api, folders.get("").ok_or_else(super::service::invalid)?, &root_name).await?;
                    folders.insert(root_name, root);
                }
                let mut policy = crate::types::QueueConflictPolicy::default();
                let mut files = files.into_iter();
                loop {
                    if cancel.is_cancelled() { return Err(crate::DomainError::TransferCanceled.into()); }
                    let file = if let Some(file) = files.next() { Some(file) }
                    else if let Some(current) = walk.take() {
                        let (current, next) = tokio::task::spawn_blocking(move || {
                            let mut current = current;
                            let next = current.next_chunk();
                            (current, next)
                        }).await.map_err(|error| AppError::from(crate::DomainError::Internal { detail: error.to_string() }))?;
                        let mut jobs = jobs.lock().await;
                        let job = jobs.get_mut(&id).ok_or_else(super::service::invalid)?;
                        job.status.skipped_symlinks = current.skipped_symlinks;
                        job.status.skipped_empty_directories = current.skipped_empty_directories;
                        if !current.stack.is_empty() { walk = Some(current); }
                        match next? { Some(file) => Some(file), None if walk.is_some() => continue, None => None }
                    } else { None };
                    let Some(file) = file else { break; };
                    if streaming {
                        jobs.lock().await.get_mut(&id).ok_or_else(super::service::invalid)?.status.total_files += 1;
                    }
                    if file.size > crate::types::MAX_TRANSFER_BYTES { return Err(crate::DomainError::TransferTooLarge { size: file.size, limit: crate::types::MAX_TRANSFER_BYTES }.into()); }
                    validate(&cloud, &connection, revision)?;
                    let (_, api) = cloud.official_api(&connection).await?;
                    let mut parts: Vec<_> = file.relative.split('/').collect();
                    let name = parts.pop().ok_or_else(super::service::invalid)?;
                    let mut current = String::new();
                    for part in parts {
                        let next = if current.is_empty() { part.into() } else { format!("{current}/{part}") };
                        if !folders.contains_key(&next) {
                            validate(&cloud, &connection, revision)?;
                            let child = upload_folder(&api, folders.get(&current).ok_or_else(super::service::invalid)?, part).await?;
                            folders.insert(next.clone(), child);
                        }
                        current = next;
                    }
                    let progress = {
                        let mut jobs = jobs.lock().await;
                        let job = jobs.get_mut(&id).ok_or_else(super::service::invalid)?;
                        if job.entry_states.len() >= crate::local_upload_types::MAX_UPLOAD_ENTRY_STATES {
                            job.entry_states.retain(|_, (phase, _, _)| phase == "active");
                        }
                        job.status.current_relative_path = Some(file.relative.clone());
                        job.progress.store(0, std::sync::atomic::Ordering::Relaxed);
                        job.entry_states.insert(file.relative.clone(), ("active".into(), 0, file.size));
                        if let Some(entry) = job.page.entries.iter_mut().find(|entry| entry.relative_path == file.relative) { entry.phase = "active".into(); }
                        job.progress.clone()
                    };
                    let destination = folders.get(&current).ok_or_else(super::service::invalid)?;
                    let mut cursor = None;
                    let mut seen = std::collections::HashSet::new();
                    let mut matches = Vec::new();
                    loop {
                        let page = api.list(destination, cursor.as_deref()).await.map_err(crate::cloud::official_runtime::api_error)?;
                        matches.extend(page.entries.into_iter().filter(|entry| entry.name == name));
                        if matches.len() > 100_000 { return Err(super::service::invalid()); }
                        match page.next {
                            Some(next) if seen.len() < 1000 && seen.insert(next.clone()) => cursor = Some(next),
                            Some(_) => return Err(super::service::invalid()),
                            None if page.complete => break,
                            None => return Err(super::service::invalid()),
                        }
                    }
                    if !matches.is_empty() {
                        use crate::cloud::directory_types::{DirectoryConflictDto, DirectoryConflictPolicy};
                        let target = (matches.len() == 1 && !matches[0].is_dir).then(|| &matches[0]);
                        let expected = target.and_then(|entry| entry.revision.clone());
                        let choice = if policy.upload_choice() == DirectoryConflictPolicy::Ask || (policy.upload_choice() == DirectoryConflictPolicy::Overwrite && expected.is_none()) {
                            let conflict_id = crate::local_upload::token()?;
                            {
                                let mut jobs = jobs.lock().await;
                                let job = jobs.get_mut(&id).ok_or_else(super::service::invalid)?;
                                job.status.phase = "conflict".into();
                                job.status.conflict = Some(DirectoryConflictDto { overwrite_allowed: expected.is_some(), conflict_id, relative_path: file.relative.clone(), key: file.relative.clone(), etag: expected.clone() });
                            }
                            crate::local_upload::wait_queue_conflict(&decision, async { cancel.cancelled().await; AppError::from(crate::DomainError::TransferCanceled) }, || async {
                                Ok(jobs.lock().await.get_mut(&id).ok_or_else(super::service::invalid)?.resolution.take())
                            }, &mut policy).await?
                        } else { policy.upload_choice() };
                        if choice == DirectoryConflictPolicy::Overwrite {
                            let target = target.ok_or_else(super::service::invalid)?;
                            let expected = expected.as_deref().ok_or_else(super::service::invalid)?;
                            validate(&cloud, &connection, revision)?;
                            // Version compatibility check is not an atomic conditional write.
                            api.replace_file_stream_progress(&target.id, file.file, file.size, Some(expected), Some(progress)).await.map_err(crate::cloud::official_runtime::api_error)?;
                            validate(&cloud, &connection, revision)?;
                            let mut jobs = jobs.lock().await;
                            let job = jobs.get_mut(&id).ok_or_else(super::service::invalid)?;
                            job.status.completed_files += 1;
                            if let Some(entry) = job.page.entries.iter_mut().find(|entry| entry.relative_path == file.relative) { entry.phase = "completed".into(); entry.transferred = Some(file.size); }
                            job.entry_states.insert(file.relative.clone(), ("completed".into(), file.size, file.size));
                            job.status.completed_bytes = job.status.completed_bytes.saturating_add(file.size);
                        } else {
                            let mut jobs = jobs.lock().await;
                            let job = jobs.get_mut(&id).ok_or_else(super::service::invalid)?;
                            job.status.skipped_files += 1;
                            job.entry_states.insert(file.relative.clone(), ("skipped".into(), 0, file.size));
                            if let Some(entry) = job.page.entries.iter_mut().find(|entry| entry.relative_path == file.relative) { entry.phase = "skipped".into(); }
                        }
                        continue;
                    }
                    api.upload_new_progress(destination, name, file.file, file.size, progress).await.map_err(crate::cloud::official_runtime::api_error)?;
                    validate(&cloud, &connection, revision)?;
                    let mut jobs = jobs.lock().await;
                    let job = jobs.get_mut(&id).ok_or_else(super::service::invalid)?;
                    job.status.completed_files += 1;
                    if let Some(entry) = job.page.entries.iter_mut().find(|entry| entry.relative_path == file.relative) { entry.phase = "completed".into(); entry.transferred = Some(file.size); }
                    job.entry_states.insert(file.relative.clone(), ("completed".into(), file.size, file.size));
                    job.status.completed_bytes = job.status.completed_bytes.saturating_add(file.size);
                }
                Ok::<(), AppError>(())
            };
            let invalidation = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if let Err(error) = validate(&cloud, &connection, revision) { return error; }
                }
            };
            let result = std::panic::AssertUnwindSafe(async {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(AppError::from(crate::DomainError::TransferCanceled)),
                    error = invalidation => Err(error),
                    result = work => result,
                }
            }).catch_unwind().await.unwrap_or_else(|_| Err(crate::DomainError::Internal { detail: "official upload worker panicked".into() }.into()));
            if let Err(error) = &result { cloud.object_report(error.clone()); }
            if let Some(job) = jobs.lock().await.get_mut(&id) {
                job.status.phase = if cancel.is_cancelled() { "cancelled" } else if result.is_ok() { "completed" } else { "failed" }.into();
                for entry in &mut job.page.entries {
                    if entry.phase == "active" {
                        entry.phase = job.status.phase.clone();
                        entry.transferred = Some(job.progress.load(std::sync::atomic::Ordering::Relaxed).min(entry.size));
                    }
                }
                if let Some(path) = &job.status.current_relative_path {
                    if let Some((phase, bytes, size)) = job.entry_states.get_mut(path) {
                        if phase == "active" { *phase = job.status.phase.clone(); *bytes = job.progress.load(std::sync::atomic::Ordering::Relaxed).min(*size); }
                    }
                }
                job.status.error = result.err();
                job.status.conflict = None;
                job.resolution = None;
                job.status.current_relative_path = None;
            }
        }));
    } else if !matches!(action, "status" | "cancel") { return Err(super::service::invalid()); }
    Ok(job.status.clone())
}

fn validate(cloud: &CloudRuntime, connection: &str, revision: u64) -> Result<(), AppError> {
    cloud.admit()?;
    if !cloud.config.snapshot().map_err(crate::cloud::runtime::config_error)?.official_connections.iter().any(|entry| entry.id == connection && entry.revision == revision) { return Err(super::service::invalid()); }
    Ok(())
}

pub(crate) async fn release(backend: &OfficialBrowserBackend, id: &str) -> Result<(), AppError> {
    let handle = {
        let mut jobs = backend.uploads.lock().await;
        if let Some(job) = jobs.get_mut(id) { job.cancel.cancel(); job.handle.take() } else { None }
    };
    if let Some(handle) = handle { handle.await.map_err(|e| AppError::from(crate::DomainError::Internal { detail: e.to_string() }))?; }
    backend.uploads.lock().await.remove(id);
    Ok(())
}

impl Drop for OfficialBrowserBackend {
    fn drop(&mut self) {
        if let Ok(jobs) = self.uploads.try_lock() { for job in jobs.values() { job.cancel.cancel(); } }
    }
}
