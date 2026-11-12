use super::{service::{invalid, unavailable}, types::*};
use crate::{AppError, PeerManager, cloud::runtime::CloudRuntime};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};
use tokio::io::AsyncReadExt;

fn io(source: std::io::Error) -> AppError {
    crate::DomainError::Io { operation: "external edit", path: None, source }.into()
}

pub(crate) fn editable_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if matches!(name.as_str(), "dockerfile" | "makefile" | "gemfile" | "rakefile" | "procfile" | "jenkinsfile" | "vagrantfile" | "license" | "licence" | "readme" | "changelog" | "authors" | "notice" | "copying" | "todo" | ".env") || name.starts_with(".env.") { return true; }
    let extension = name.rsplit('.').next().unwrap_or_default();
    matches!(extension, "txt" | "md" | "markdown" | "json" | "jsonl" | "yaml" | "yml" | "toml" | "xml" | "csv" | "tsv" | "log" | "rs" | "js" | "jsx" | "ts" | "tsx" | "vue" | "py" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "css" | "scss" | "html" | "sql" | "sh" | "bash" | "zsh" | "ini" | "conf" | "cfg" | "rtf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "pages" | "numbers" | "key")
}

#[cfg(test)]
fn content_event(kind: &notify::EventKind) -> bool {
    crate::file_watch::content_event(kind) && !matches!(kind, notify::EventKind::Remove(_))
}

pub(crate) async fn read_local(path: &Path) -> Result<Vec<u8>, AppError> {
    let meta = tokio::fs::symlink_metadata(path).await.map_err(io)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_EDIT_BYTES as u64 { return Err(unavailable()); }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    let file = options.open(path).await.map_err(io)?;
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_BYTES as u64 + 1).read_to_end(&mut bytes).await.map_err(io)?;
    if bytes.len() > MAX_EDIT_BYTES { return Err(unavailable()); }
    Ok(bytes)
}

impl FileSessions {
    pub async fn prepare_edit(&self, peers: &PeerManager, cloud: &CloudRuntime, session_id: &str, path: &str) -> Result<FileEditDto, AppError> {
        let session = self.admitted(peers, cloud, session_id).await?;
        if !session.backend.capabilities().external_edit { return Err(unavailable()); }
        // Serialize preparation and writes: resource bounds apply to in-flight reads too.
        let mut edits = self.edits.lock().await;
        if edits.len() >= 16 { return Err(unavailable()); }
        let content = session.backend.edit_read(peers, cloud, path).await?;
        if edits.values().any(|edit| Arc::ptr_eq(&edit.session, &session) && edit.path == path) { return Err(unavailable()); }
        if !editable_name(&content.name) { return Err(unavailable()); }
        self.admitted(peers, cloud, session_id).await?;
        if content.name.is_empty() || matches!(content.name.as_str(), "." | "..") || content.name.contains(['/', '\\', '\0', ':']) || content.bytes.len() > MAX_EDIT_BYTES { return Err(unavailable()); }
        let id = crate::local_upload::token()?;
        let editor_root = cloud.data_dir.join("editor");
        tokio::fs::create_dir_all(&editor_root).await.map_err(io)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&editor_root, std::fs::Permissions::from_mode(0o700)).await.map_err(io)?;
        }
        let directory = editor_root.join(&id);
        tokio::fs::create_dir_all(&directory).await.map_err(io)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).await.map_err(io)?;
        }
        let local = directory.join(&content.name);
        let record = serde_json::json!({ "version": 1, "location": session.location, "remotePath": path, "fileName": content.name, "revision": content.revision, "baselineHash": hex::encode(Sha256::digest(&content.bytes)) });
        // Keep recovery metadata outside the watched directory. Never auto-replay
        // these records after restart without renewed admission and review.
        let record_path = directory.with_extension("json");
        let record_bytes = serde_json::to_vec(&record).map_err(|error| AppError::from(crate::DomainError::Internal { detail: error.to_string() }))?;
        tokio::fs::write(record_path, record_bytes).await.map_err(io)?;
        tokio::fs::write(&local, &content.bytes).await.map_err(io)?;
        let dto = FileEditDto { edit_id: id.clone(), id: path.into(), name: content.name.clone(), local_path: local.to_string_lossy().into_owned() };
        edits.insert(id, FileEdit { session, path: path.into(), local, revision: content.revision, hash: Sha256::digest(&content.bytes).to_vec() });
        Ok(dto)
    }

    pub async fn commit_edit(&self, peers: &PeerManager, cloud: &CloudRuntime, id: &str) -> Result<bool, AppError> {
        self.commit_edit_snapshot(peers, cloud, id, None).await
    }

    async fn commit_edit_snapshot(&self, peers: &PeerManager, cloud: &CloudRuntime, id: &str, expected: Option<&[u8]>) -> Result<bool, AppError> {
        let mut edits = self.edits.lock().await;
        let edit = edits.get_mut(id).ok_or_else(invalid)?;
        edit.session.backend.admit(peers, cloud).await?;
        let bytes = read_local(&edit.local).await?;
        if expected.is_some_and(|expected| expected != bytes) { return Ok(false); }
        let hash = Sha256::digest(&bytes).to_vec();
        if hash == edit.hash { return Ok(false); }
        if edit.revision.is_empty() { return Err(invalid()); }
        let revision = edit.session.backend.edit_write(peers, cloud, &edit.path, &edit.revision, bytes).await?;
        edit.revision = revision;
        edit.hash = hash;
        if let Some(parent) = edit.local.parent() {
            let record = serde_json::json!({ "version": 1, "location": edit.session.location, "remotePath": edit.path, "fileName": edit.local.file_name().map(|s| s.to_string_lossy()), "revision": edit.revision, "baselineHash": hex::encode(&edit.hash) });
            let bytes = serde_json::to_vec(&record).map_err(|error| AppError::from(crate::DomainError::Internal { detail: error.to_string() }))?;
            tokio::fs::write(parent.with_extension("json"), bytes).await.map_err(io)?;
        }
        Ok(true)
    }

    pub async fn discard_edit(&self, id: &str) -> Result<(), AppError> {
        if let Some(edit) = self.edits.lock().await.remove(id) {
            if let Some(parent) = edit.local.parent() {
                tokio::fs::remove_dir_all(parent).await.map_err(io)?;
                match tokio::fs::remove_file(parent.with_extension("json")).await {
                    Ok(()) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(io(error)),
                }
            }
        }
        Ok(())
    }
}

// The task owns the watcher. No path from the frontend can enter this queue.
// Ignore deletes/metadata; a create/rename targeting the registered path also
// handles editors which save through an atomic replacement.
pub(crate) fn start_watch(app: tauri::AppHandle, sessions: SharedFileSessions, peers: Arc<PeerManager>, cloud: Arc<CloudRuntime>, info: FileEditDto) -> Result<(), AppError> {
    let local = std::path::PathBuf::from(&info.local_path);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let target = local.clone();
    let watcher_cloud = cloud.clone();
    let watcher = crate::file_watch::watch(local.parent().ok_or_else(invalid)?, false, move |event| {
        let relevant = match event {
            Ok(crate::file_watch_types::FileChange::Paths(paths)) => paths.iter().any(|path| path == &target),
            Ok(crate::file_watch_types::FileChange::Rescan) => true,
            Err(error) => { watcher_cloud.object_report(crate::DomainError::Internal { detail: format!("editor watch error: {error}") }.into()); true }
        };
        if relevant {
            match sender.try_send(()) { Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}, Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {} }
        }
    }).map_err(|e| AppError::from(crate::DomainError::Internal { detail: e.to_string() }))?;
    let worker_cloud = cloud.clone();
    let worker_id = info.edit_id.clone();
    let worker = tauri::async_runtime::spawn(async move {
        let _watcher = watcher;
        loop {
            tokio::select! {
                event = receiver.recv() => { if event.is_none() { break; } },
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                    if !sessions.edits.lock().await.contains_key(&worker_id) { break; }
                    continue;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            while receiver.try_recv().is_ok() {}
            // Require two equal snapshots before uploading. No periodic network
            // retry: a failed save remains local for an explicit retry.
            // Bound snapshot memory across all edit watchers.
            let _snapshot = EDIT_SNAPSHOTS.lock().await;
            let first = read_local(&local).await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let second = read_local(&local).await;
            let (first, second) = match (first, second) {
                (Ok(first), Ok(second)) => (first, second),
                (Err(error), _) | (_, Err(error)) => {
                    // Removing a local edit never deletes or uploads remotely.
                    if local.exists() { worker_cloud.object_report(error); }
                    continue;
                }
            };
            if first != second { continue; }
            if let Err(error) = sessions.commit_edit_snapshot(&peers, &worker_cloud, &worker_id, Some(&second)).await { worker_cloud.object_report(error); }
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(error) = worker.await { cloud.object_report(crate::DomainError::Internal { detail: format!("editor watcher exited: {error}") }.into()); }
        drop(app);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_content_and_atomic_save_events_trigger_upload() {
        use notify::{EventKind, event::*};
        assert!(content_event(&EventKind::Modify(ModifyKind::Data(DataChange::Content))));
        assert!(content_event(&EventKind::Modify(ModifyKind::Name(RenameMode::To))));
        assert!(content_event(&EventKind::Create(CreateKind::File)));
        assert!(!content_event(&EventKind::Remove(RemoveKind::File)));
        assert!(!content_event(&EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions))));
        assert!(!content_event(&EventKind::Access(AccessKind::Read)));
    }
    #[test]
    fn external_open_rejects_executable_and_shortcut_types() {
        assert!(editable_name("notes.txt"));
        assert!(editable_name("report.DOCX"));
        assert!(!editable_name("run.exe"));
        assert!(!editable_name("run.desktop"));
        assert!(!editable_name("website.url"));
    }

    #[tokio::test]
    async fn native_watcher_observes_atomic_replacement() {
        use notify::Watcher;
        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("notes.txt");
        std::fs::write(&file, b"old").expect("write");
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
            if let Ok(event) = event { sender.send(event).ok(); }
        }).expect("watcher");
        watcher.watch(root.path(), notify::RecursiveMode::NonRecursive).expect("watch");
        let temporary = root.path().join("replacement");
        std::fs::write(&temporary, b"saved").expect("replacement");
        std::fs::rename(&temporary, &file).expect("rename");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = receiver.recv().await {
                if content_event(&event.kind) && event.paths.contains(&file) { return; }
            }
            panic!("watcher closed before replacement event");
        }).await.expect("replacement observed");
        assert_eq!(read_local(&file).await.expect("read"), b"saved");
    }

    #[tokio::test]
    async fn local_edit_rejects_symlinks_and_oversize() {
        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("text.txt");
        tokio::fs::write(&file, b"hello").await.expect("write");
        assert_eq!(read_local(&file).await.expect("read"), b"hello");
        #[cfg(unix)] {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&file, &link).expect("link");
            assert!(read_local(&link).await.is_err());
        }
        std::fs::OpenOptions::new().write(true).open(&file).expect("open").set_len(MAX_EDIT_BYTES as u64 + 1).expect("size");
        assert!(read_local(&file).await.is_err());
    }
}
