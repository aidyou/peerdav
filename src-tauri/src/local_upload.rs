use crate::cloud::object_runtime_types::{
    MAX_UPLOAD_DIRECTORY_DEPTH, MAX_UPLOAD_DIRECTORY_PATH_BYTES,
};
use crate::cloud::{ObjectServiceError, PhysicalRoot};
use crate::local_upload_types::*;
use crate::{AppError, DomainError};
use cap_fs_ext::DirExt;

/// One lifecycle dispatcher for every upload transport. Cancellation is joined by
/// the adapter before the final status is returned; unknown actions never succeed.
pub(crate) async fn queue_action<A: crate::traits::UploadQueueAdapter>(adapter: &A, id: &str, action: crate::file_manager::types::UploadAction) -> Result<A::Status, AppError> {
    use crate::file_manager::types::UploadAction;
    match action {
        UploadAction::Start => start_queue(adapter, id, Default::default()).await,
        UploadAction::Status => adapter.status(id).await,
        UploadAction::Cancel => { adapter.cancel(id).await?; adapter.status(id).await },
    }
}

pub(crate) async fn start_queue<A: crate::traits::UploadQueueAdapter>(adapter: &A, id: &str, policy: crate::cloud::directory_types::DirectoryConflictPolicy) -> Result<A::Status, AppError> {
    crate::types::QueueConflictPolicy::from_upload(policy)?;
    adapter.start(id, policy).await
}

impl crate::peer_upload_types::PeerBrowser {
    pub(crate) fn page(&mut self, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>, active: Option<String>, completed: bool) -> Result<DirectoryPageDto, AppError> {
        match self {
            Self::Directory(browser) => browser.page(directory_token, cursor, limit, active, completed),
            Self::Files { entries, token: root_token, cursor: saved } => {
                if directory_token.as_ref().is_some_and(|t| t != root_token) { return Err(invalid()); }
                let start = match cursor { None => 0, Some(c) => saved.as_ref().filter(|(s, _)| *s == c).map(|(_, i)| *i).ok_or_else(invalid)? };
                let end = (start + limit.unwrap_or(100).clamp(1, 128)).min(entries.len());
                let mut page = entries[start..end].to_vec();
                for e in &mut page { if active.as_deref() == Some(&e.relative_path) && !completed { e.phase = "active".into(); } }
                *saved = if end < entries.len() { Some((token()?, end)) } else { None };
                Ok(DirectoryPageDto { directory_token: root_token.clone(), parent_token: None, relative_path: String::new(), entries: page, next_cursor: saved.as_ref().map(|(s, _)| s.clone()) })
            }
        }
    }
}

pub(crate) fn open_selected_files(paths: Vec<std::path::PathBuf>) -> Result<(Vec<DirectoryFile>, crate::peer_upload_types::PeerBrowser), AppError> {
    if paths.is_empty() || paths.len() > 128 { return Err(invalid()); }
    let mut names = std::collections::HashSet::new();
    let mut files = Vec::new();
    let mut entries = Vec::new();
    for path in paths {
        let name = path.file_name().and_then(|s| s.to_str()).ok_or_else(invalid)?.to_owned();
        crate::cloud::object_service::valid_relative_path(&name).map_err(|e| AppError::from(DomainError::from(e)))?;
        if !names.insert(name.clone()) { return Err(invalid()); }
        let parent = PhysicalRoot::open(path.parent().ok_or_else(invalid)?).map_err(|e| AppError::from(DomainError::from(e)))?;
        let dir = parent.upload_directory_handle().map_err(|e| AppError::from(DomainError::from(e)))?;
        let meta = dir.symlink_metadata(&name).map_err(io)?;
        if !meta.is_file() || meta.file_type().is_symlink() { return Err(invalid()); }
        let file = open_file_checked(&dir, &name, &meta)?;
        let size = file.metadata().map_err(io)?.len();
        entries.push(DirectoryEntryDto { name: name.clone(), relative_path: name.clone(), kind: "file".into(), size, directory_token: None, phase: "unscanned".into() });
        files.push(DirectoryFile { relative: name, file, size });
    }
    Ok((files, crate::peer_upload_types::PeerBrowser::Files { entries, token: token()?, cursor: None }))
}

impl crate::types::QueueConflictPolicy {
    pub(crate) fn from_upload(policy: crate::cloud::directory_types::DirectoryConflictPolicy) -> Result<Self, AppError> {
        use crate::cloud::directory_types::DirectoryConflictPolicy as P;
        use crate::download_types::ConflictResolution as R;
        Ok(Self { cached: match policy {
            P::Ask => None,
            P::Skip => Some(R::Skip),
            P::Overwrite => Some(R::Overwrite),
            P::Cancel => return Err(invalid()),
        } })
    }
    pub(crate) fn upload_choice(&self) -> crate::cloud::directory_types::DirectoryConflictPolicy {
        use crate::cloud::directory_types::DirectoryConflictPolicy as P;
        use crate::download_types::ConflictResolution as R;
        match self.cached { None => P::Ask, Some(R::Skip) => P::Skip, Some(R::Overwrite) => P::Overwrite }
    }
    pub(crate) fn remember(&mut self, decision: crate::download_types::ConflictResolution, remaining: bool) {
        if remaining { self.cached = Some(decision); }
    }
    pub(crate) fn resolve<T: Copy>(
        current_id: Option<&str>, conflict_id: &str,
        slot: &mut Option<(T, bool)>, decision: T, remaining: bool,
    ) -> Result<(), AppError> {
        if current_id != Some(conflict_id) || slot.is_some() { return Err(invalid()); }
        *slot = Some((decision, remaining));
        Ok(())
    }
}

/// Shared queue-owned decision wait. Only a decision is cached; callers must
/// probe each destination afresh before asking, and validate again before I/O.
pub(crate) async fn wait_queue_conflict<C, R, F>(
    notify: &tokio::sync::Notify,
    cancelled: C,
    mut resolution: R,
    policy: &mut crate::types::QueueConflictPolicy,
) -> Result<crate::cloud::directory_types::DirectoryConflictPolicy, AppError>
where
    C: std::future::Future<Output = AppError>,
    R: FnMut() -> F,
    F: std::future::Future<
            Output = Result<
                Option<(crate::cloud::directory_types::DirectoryConflictPolicy, bool)>,
                AppError,
            >,
        >,
{
    tokio::pin!(cancelled);
    loop {
        // Register before reading the slot: Notify retains a wake between read
        // and await, so a UI decision cannot be lost.
        let notified = notify.notified();
        tokio::select! {
            biased;
            error = &mut cancelled => return Err(error),
            value = async { resolution().await } => {
                if let Some((choice, remaining)) = value? {
                    let next = crate::types::QueueConflictPolicy::from_upload(choice)?;
                    if let Some(decision) = next.cached { policy.remember(decision, remaining); }
                    return Ok(choice);
                }
            }
        }
        tokio::select! {
            biased;
            error = &mut cancelled => return Err(error),
            _ = notified => {}
        }
    }
}

pub(crate) fn invalid() -> AppError {
    AppError::from(DomainError::from(
        ObjectServiceError::InvalidLocalCapability,
    ))
}
pub(crate) fn io(e: std::io::Error) -> AppError {
    AppError::from(DomainError::from(ObjectServiceError::Io { source: e }))
}
pub(crate) fn token() -> Result<String, AppError> {
    crate::cloud::object_runtime::random_id()
}
pub(crate) fn open_directory_checked(
    parent: &cap_std::fs::Dir,
    name: &str,
    metadata: &cap_std::fs::Metadata,
) -> Result<cap_std::fs::Dir, AppError> {
    let dir = parent.open_dir_nofollow(name).map_err(io)?;
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        let opened = dir.dir_metadata().map_err(io)?;
        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            return Err(invalid());
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(dir)
}
pub(crate) fn open_file_checked(
    parent: &cap_std::fs::Dir,
    name: &str,
    meta: &cap_std::fs::Metadata,
) -> Result<std::fs::File, AppError> {
    #[cfg(unix)]
    let file = std::fs::File::from(
        rustix::fs::openat(
            parent,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| io(e.into()))?,
    );
    #[cfg(not(unix))]
    let file = {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        parent.open_with(name, &options).map_err(io)?.into_std()
    };
    let opened = file.metadata().map_err(io)?;
    if !opened.is_file() {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as CapMetadataExt;
        use std::os::unix::fs::MetadataExt;
        if meta.dev() != opened.dev() || meta.ino() != opened.ino() {
            return Err(invalid());
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    Ok(file)
}
fn frame(
    dir: cap_std::fs::Dir,
    relative: String,
    options: Option<&crate::types::IgnoreOptions>,
) -> Result<DirectoryFrame, AppError> {
    let entries = dir.entries().map_err(io)?;
    let mut layer = None;
    if options.is_some_and(|o| o.respect_gitignore) {
        match dir.symlink_metadata(".gitignore") {
            Ok(meta)
                if meta.is_file()
                    && !meta.file_type().is_symlink()
                    && meta.len() <= 1024 * 1024 =>
            {
                use std::io::Read;
                let mut text = String::new();
                open_file_checked(&dir, ".gitignore", &meta)?
                    .take(1024 * 1024 + 1)
                    .read_to_string(&mut text)
                    .map_err(io)?;
                if text.len() > 1024 * 1024 {
                    return Err(invalid());
                }
                layer = crate::ignore_rules::build_gitignore(&text).map(|matcher| {
                    crate::ignore_rules::IgnoreLayer {
                        base: relative.clone(),
                        matcher,
                    }
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
    }
    Ok(DirectoryFrame {
        dir,
        entries,
        relative,
        had_entry: false,
        layer,
    })
}
impl DirectoryWalk {
    pub(crate) fn new(root: &PhysicalRoot, name: String) -> Result<Self, AppError> {
        Self::with_options(
            root.upload_directory_handle()
                .map_err(|e| AppError::from(DomainError::from(e)))?,
            name,
            None,
        )
    }
    pub(crate) fn with_options(
        dir: cap_std::fs::Dir,
        name: String,
        options: Option<crate::types::IgnoreOptions>,
    ) -> Result<Self, AppError> {
        let custom = options
            .as_ref()
            .and_then(|o| crate::ignore_rules::build_gitignore(&o.patterns));
        Ok(Self {
            stack: vec![frame(dir, name.clone(), options.as_ref())?],
            skipped_symlinks: 0,
            skipped_empty_directories: 0,
            options,
            custom,
            root_name: name,
        })
    }
    pub(crate) fn next_chunk(&mut self) -> Result<Option<DirectoryFile>, AppError> {
        for _ in 0..64 {
            let Some(top) = self.stack.last_mut() else {
                return Ok(None);
            };
            let Some(entry) = top.entries.next() else {
                if !top.had_entry {
                    self.skipped_empty_directories += 1;
                }
                self.stack.pop();
                continue;
            };
            top.had_entry = true;
            let entry = entry.map_err(io)?;
            let name = entry.file_name().into_string().map_err(|_| invalid())?;
            let relative = format!("{}/{name}", top.relative);
            crate::cloud::object_service::valid_relative_path(&relative)
                .map_err(|e| AppError::from(DomainError::from(e)))?;
            if relative.len() > MAX_UPLOAD_DIRECTORY_PATH_BYTES {
                return Err(invalid());
            }
            let meta = top.dir.symlink_metadata(&name).map_err(io)?;
            if meta.file_type().is_symlink() {
                self.skipped_symlinks += 1;
                continue;
            }
            if self.options.is_some() {
                let layers: Vec<_> = self.stack.iter().filter_map(|f| f.layer.clone()).collect();
                let custom_relative = relative
                    .strip_prefix(&self.root_name)
                    .and_then(|s| s.strip_prefix('/'))
                    .ok_or_else(invalid)?;
                if crate::ignore_rules::is_path_ignored(&relative, &name, meta.is_dir(), &layers)
                    || self.custom.as_ref().is_some_and(|m| {
                        m.matched_path_or_any_parents(custom_relative, meta.is_dir())
                            .is_ignore()
                    })
                {
                    continue;
                }
            }
            let top = self.stack.last().ok_or_else(invalid)?;
            if meta.is_dir() {
                if self.stack.len() >= MAX_UPLOAD_DIRECTORY_DEPTH {
                    return Err(invalid());
                }
                let dir = open_directory_checked(&top.dir, &name, &meta)?;
                self.stack
                    .push(frame(dir, relative, self.options.as_ref())?);
            } else if meta.is_file() {
                let file = open_file_checked(&top.dir, &name, &meta)?;
                let size = file.metadata().map_err(io)?.len();
                return Ok(Some(DirectoryFile {
                    relative,
                    file,
                    size,
                }));
            } else {
                return Err(invalid());
            }
        }
        Ok(None)
    }
}
impl DirectoryBrowser {
    pub(crate) fn new(dir: cap_std::fs::Dir, relative: String) -> Result<Self, AppError> {
        Ok(Self {
            chain: vec![BrowseDirectory {
                token: token()?,
                dir,
                relative,
            }],
            entries: None,
            cursor: None,
            children: Vec::new(),
        })
    }
    pub(crate) fn page(
        &mut self,
        directory_token: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
        active: Option<String>,
        completed: bool,
    ) -> Result<DirectoryPageDto, AppError> {
        let cursor_error = || {
            AppError::from(DomainError::Cloud {
                code: "CLOUD_DIRECTORY_CURSOR_RESET",
                user_reason: "目录分页已过期，请重新打开目录 / Directory cursor expired; reopen the directory",
                source: Box::new(ObjectServiceError::InvalidLocalCapability),
            })
        };
        let limit = limit.unwrap_or(100).clamp(1, 128);
        if let Some(requested) = directory_token {
            if self.chain.last().is_none_or(|d| d.token != requested) {
                if cursor.is_some() {
                    return Err(cursor_error());
                }
                if let Some(index) = self.chain.iter().position(|d| d.token == requested) {
                    self.chain.truncate(index + 1);
                } else if let Some(index) = self.children.iter().position(|d| d.token == requested)
                {
                    if self.chain.len() >= MAX_UPLOAD_DIRECTORY_DEPTH {
                        return Err(invalid());
                    }
                    let child = self.children.swap_remove(index);
                    self.chain.push(child);
                } else {
                    return Err(cursor_error());
                }
                self.entries = None;
                self.cursor = None;
                self.children.clear();
            }
        }
        if cursor.is_some() && (cursor != self.cursor || self.entries.is_none()) {
            return Err(cursor_error());
        }
        if cursor.is_none() {
            self.entries = Some(
                self.chain
                    .last()
                    .ok_or_else(invalid)?
                    .dir
                    .entries()
                    .map_err(io)?,
            );
        }
        self.children.clear();
        let current = self.chain.last().ok_or_else(invalid)?;
        let relative = current.relative.clone();
        let current_token = current.token.clone();
        let parent_token = self.chain.iter().rev().nth(1).map(|d| d.token.clone());
        let mut entries = Vec::with_capacity(limit);
        let mut exhausted = false;
        for _ in 0..limit {
            let Some(entry) = self.entries.as_mut().ok_or_else(invalid)?.next() else {
                exhausted = true;
                break;
            };
            let entry = entry.map_err(io)?;
            let name = entry.file_name().into_string().map_err(|_| invalid())?;
            let path = format!("{relative}/{name}");
            if path.len() > MAX_UPLOAD_DIRECTORY_PATH_BYTES {
                return Err(invalid());
            }
            crate::cloud::object_service::valid_relative_path(&path)
                .map_err(|e| AppError::from(DomainError::from(e)))?;
            let dir = &self.chain.last().ok_or_else(invalid)?.dir;
            let meta = dir.symlink_metadata(&name).map_err(io)?;
            let kind = if meta.file_type().is_symlink() {
                "symlink"
            } else if meta.is_dir() {
                "directory"
            } else if meta.is_file() {
                "file"
            } else {
                "unsupported"
            };
            let directory_token = if kind == "directory" {
                let child = BrowseDirectory {
                    token: token()?,
                    dir: open_directory_checked(dir, &name, &meta)?,
                    relative: path.clone(),
                };
                let token = child.token.clone();
                self.children.push(child);
                Some(token)
            } else {
                None
            };
            let phase = if active.as_deref() == Some(path.as_str()) && !completed {
                "active"
            } else {
                "unscanned"
            };
            entries.push(DirectoryEntryDto {
                name,
                relative_path: path,
                kind: kind.into(),
                size: meta.len(),
                directory_token,
                phase: phase.into(),
            });
        }
        self.cursor = if exhausted { None } else { Some(token()?) };
        Ok(DirectoryPageDto {
            directory_token: current_token,
            parent_token,
            relative_path: relative,
            entries,
            next_cursor: self.cursor.clone(),
        })
    }
}

#[cfg(test)]
mod queue_conflict_tests {
    use super::*;
    use crate::cloud::directory_types::DirectoryConflictPolicy as Policy;

    #[tokio::test]
    async fn resolution_matrix_caches_only_current_queue_remaining_decisions() {
        for choice in [Policy::Skip, Policy::Overwrite] {
            for remaining in [false, true] {
                let notify = tokio::sync::Notify::new();
                let mut current = crate::types::QueueConflictPolicy::default();
                let next_queue = Policy::Ask;
                let chosen = wait_queue_conflict(
                    &notify,
                    std::future::pending(),
                    || std::future::ready(Ok(Some((choice, remaining)))),
                    &mut current,
                )
                .await
                .expect("resolved");
                assert_eq!(chosen, choice);
                assert_eq!(current.upload_choice(), if remaining { choice } else { Policy::Ask });
                assert_eq!(next_queue, Policy::Ask);
            }
        }
    }

    #[tokio::test]
    async fn cancellation_does_not_consume_or_cache_a_pending_resolution() {
        let notify = tokio::sync::Notify::new();
        let mut policy = crate::types::QueueConflictPolicy::default();
        let mut read = false;
        let result = wait_queue_conflict(
            &notify,
            std::future::ready(invalid()),
            || {
                read = true;
                std::future::ready(Ok(Some((Policy::Overwrite, true))))
            },
            &mut policy,
        )
        .await;
        assert!(result.is_err());
        assert!(!read);
        assert_eq!(policy.upload_choice(), Policy::Ask);
    }
}
