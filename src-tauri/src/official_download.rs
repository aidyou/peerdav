use crate::{AppError, DomainError, download_types::*, traits::TransferQueueAdapter};
use futures_util::future::BoxFuture;

fn unique_name(original: &str, names: &mut std::collections::HashSet<String>) -> Result<String, AppError> {
    if original.is_empty() || original.contains(['/', '\\', '\0', ':']) || matches!(original, "." | "..") { return Err(crate::file_manager::service::invalid()); }
    let (stem, extension) = original.rsplit_once('.').filter(|(stem, _)| !stem.is_empty())
        .map(|(stem, extension)| (stem, format!(".{extension}"))).unwrap_or((original, String::new()));
    let mut name = original.to_owned();
    let mut suffix = 2usize;
    while !names.insert(name.clone()) { name = format!("{stem} ({suffix}){extension}"); suffix += 1; }
    Ok(name)
}

impl OfficialDownloadAdapter {
    async fn list_page(&self, parent: &str, cursor: Option<&str>) -> Result<crate::cloud::official_api_types::OfficialPage, AppError> {
        #[cfg(test)]
        if let Some(pages) = &self.pages {
            return pages.get(&(parent.into(), cursor.map(str::to_owned))).cloned().ok_or_else(crate::file_manager::service::invalid);
        }
        self.runtime.list_official(&self.connection, parent, cursor).await
    }
}

impl TransferQueueAdapter for OfficialDownloadAdapter {
    fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError> {
        self.runtime.vault.transfer_cancellation().map_err(AppError::from)
    }
    fn report_error(&self, error: &AppError) {
        if let Err(source) = self.runtime.object_events.error(error.clone()) { log::error!("official download event failed: {source}"); }
    }
    fn validate(&self) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async {
            self.runtime.admit()?;
            let snapshot = self.runtime.config.snapshot().map_err(crate::cloud::runtime::config_error)?;
            if snapshot.official_connections.iter().any(|entry| entry.id == self.connection && entry.revision == self.revision) { Ok(()) }
            else { Err(crate::file_manager::service::invalid()) }
        })
    }
    fn plan(&self, selection: DownloadSelection) -> BoxFuture<'_, Result<Vec<DownloadItem>, AppError>> {
        Box::pin(async move {
            self.validate().await?;
            let paths = match selection {
                DownloadSelection::Single { path } => vec![path],
                DownloadSelection::Files { paths } => paths,
                DownloadSelection::Folder { path } => vec![path],
            };
            if paths.is_empty() || paths.len() > MAX_DOWNLOAD_ITEMS { return Err(crate::file_manager::service::invalid()); }
            let mut names = std::collections::HashSet::new();
            let mut items = Vec::new();
            let mut files = self.files.lock().await.clone();
            let mut content_types = self.content_types.lock().await.clone();
            let mut selected = std::collections::HashSet::new();
            let paths: Vec<_> = paths.iter().filter(|path| {
                let mut ancestor = self.parents.get(*path);
                for _ in 0..MAX_DOWNLOAD_DEPTH {
                    let Some(parent) = ancestor else { break; };
                    if parent != *path && self.directories.contains_key(parent) && paths.contains(parent) { return false; }
                    ancestor = self.parents.get(parent);
                }
                true
            }).cloned().collect();
            let cancel = self.cancellation()?;
            let mut pages = 0usize;
            let mut entries = 0usize;
            for path in paths {
                if !selected.insert(path.clone()) { continue; }
                if cancel.is_cancelled() { return Err(DomainError::TransferCanceled.into()); }
                let original = self.names.get(&path).cloned().ok_or_else(crate::file_manager::service::invalid)?;
                let format = crate::cloud::official_api::OfficialApi::export_format(content_types.get(&path).map(String::as_str)).map_err(crate::cloud::official_runtime::api_error)?;
                let original = format.map(|(_, ext)| format!("{original}{ext}")).unwrap_or(original);
                let name = unique_name(&original, &mut names)?;
                if let Some(id) = self.directories.get(&path) {
                    let mut stack = vec![(id.clone(), name, 1usize)];
                    let mut visited = std::collections::HashSet::new();
                    while let Some((parent, relative, depth)) = stack.pop() {
                        if depth >= MAX_DOWNLOAD_DEPTH || !visited.insert(parent.clone()) { return Err(crate::file_manager::service::invalid()); }
                        let mut cursor = None;
                        let first_item = items.len();
                        let mut child_count = 0usize;
                        let mut cursors = std::collections::HashSet::new();
                        let mut siblings = std::collections::HashSet::new();
                        let mut ids = std::collections::HashSet::new();
                        loop {
                            pages += 1;
                            if pages > MAX_DOWNLOAD_ITEMS || cancel.is_cancelled() { return Err(crate::file_manager::service::invalid()); }
                            let page = tokio::select! {
                                _ = cancel.cancelled() => return Err(DomainError::TransferCanceled.into()),
                                page = self.list_page(&parent, cursor.as_deref()) => page?,
                            };
                            self.validate().await?;
                            entries += page.entries.len();
                            child_count += page.entries.len();
                            if entries > MAX_DOWNLOAD_ITEMS { return Err(crate::file_manager::service::invalid()); }
                            for entry in page.entries {
                                if !ids.insert(entry.id.clone()) { return Err(crate::file_manager::service::invalid()); }
                                let format = crate::cloud::official_api::OfficialApi::export_format(entry.content_type.as_deref()).map_err(crate::cloud::official_runtime::api_error)?;
                                let name = format.map(|(_, ext)| format!("{}{ext}", entry.name)).unwrap_or_else(|| entry.name.clone());
                                let local = format!("{relative}/{}", unique_name(&name, &mut siblings)?);
                                // Validate display names as local paths; provider IDs remain opaque.
                                crate::download::item(String::new(), local.clone(), entry.size)?;
                                if entry.is_dir { stack.push((entry.id, local, depth + 1)); }
                                else {
                                    let token = crate::local_upload::token()?;
                                    if let Some(kind) = entry.content_type { content_types.insert(token.clone(), kind); }
                                    files.insert(token.clone(), (entry.id, entry.size));
                                    items.push(crate::download::item(token, local, entry.size)?);
                                }
                            }
                            match page.next {
                                Some(next) if cursors.insert(next.clone()) => cursor = Some(next),
                                Some(_) => return Err(crate::file_manager::service::invalid()),
                                None if page.complete => break,
                                None => return Err(crate::file_manager::service::invalid()),
                            }
                        }
                        if child_count == 0 && items.len() == first_item {
                            let mut item = crate::download::item(String::new(), relative, 0)?;
                            item.is_dir = true;
                            items.push(item);
                        }
                    }
                } else {
                    let (_, size) = files.get(&path).ok_or_else(crate::file_manager::service::invalid)?;
                    items.push(crate::download::item(path, name, *size)?);
                }
                if items.len() > MAX_DOWNLOAD_ITEMS { return Err(crate::file_manager::service::invalid()); }
            }
            self.validate().await?;
            *self.files.lock().await = files;
            *self.content_types.lock().await = content_types;
            Ok(items)
        })
    }
    fn execute<'a>(&'a self, item: &'a DownloadItem, root: &'a crate::cloud::PhysicalRoot, overwrite: bool,
        cancel: tokio_util::sync::CancellationToken, progress: Option<crate::peer::TransferProgressCallback>) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let _network = self.runtime.network.read().await;
            self.validate().await?;
            let (id, expected) = self.files.lock().await.get(&item.remote_path).cloned().ok_or_else(crate::file_manager::service::invalid)?;
            if cancel.is_cancelled() { return Err(DomainError::TransferCanceled.into()); }
            let (_, api) = self.runtime.official_api(&self.connection).await?;
            let kind = self.content_types.lock().await.get(&item.remote_path).cloned();
            let export = crate::cloud::official_api::OfficialApi::export_format(kind.as_deref()).map_err(crate::cloud::official_runtime::api_error)?;
            let request = async { match export { Some((mime, _)) => api.export(&id, mime).await, None => api.media(&id).await } };
            let mut response = tokio::select! {
                _ = cancel.cancelled() => return Err(DomainError::TransferCanceled.into()),
                response = request => response.map_err(crate::cloud::official_runtime::api_error)?,
            };
            let identity = root.relative_target_identity(&item.relative_path).map_err(|source| AppError::from(DomainError::from(source)))?;
            let temporary = tempfile::tempdir().map_err(crate::local_upload::io)?;
            let payload = temporary.path().join("payload");
            let mut output = tokio::fs::File::create(&payload).await.map_err(crate::local_upload::io)?;
            let mut transferred = 0u64;
            loop {
                let chunk = tokio::select! {
                    _ = cancel.cancelled() => return Err(DomainError::TransferCanceled.into()),
                    chunk = response.chunk() => chunk.map_err(|source| crate::cloud::official_runtime::api_error(crate::cloud::official_api_types::OfficialApiError::Transport(source.without_url())))?,
                };
                let Some(chunk) = chunk else { break; };
                transferred = transferred.checked_add(chunk.len() as u64).ok_or_else(crate::file_manager::service::invalid)?;
                if transferred > if export.is_some() { 10 * 1024 * 1024 } else { expected } { return Err(crate::file_manager::service::invalid()); }
                use tokio::io::AsyncWriteExt;
                output.write_all(&chunk).await.map_err(crate::local_upload::io)?;
                if let Some(progress) = &progress { progress(transferred, expected); }
            }
            output.sync_all().await.map_err(crate::local_upload::io)?;
            drop(output);
            self.validate().await?;
            if (export.is_none() && transferred != expected) || (overwrite && root.relative_target_identity(&item.relative_path).map_err(|source| AppError::from(DomainError::from(source)))? != identity) {
                return Err(crate::file_manager::service::invalid());
            }
            let root = root.try_clone().map_err(|source| AppError::from(DomainError::from(source)))?;
            let relative = item.relative_path.clone();
            tokio::task::spawn_blocking(move || {
                let result = root.install_download(&relative, &payload, overwrite, &cancel);
                drop(temporary);
                result
            })
                .await.map_err(|source| AppError::from(DomainError::Internal { detail: source.to_string() }))?
                .map_err(|source| AppError::from(DomainError::from(source)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[tokio::test]
    async fn download_plans_bind_listed_files_and_reject_directories_and_stale_accounts() {
        let data = tempfile::tempdir().expect("data");
        let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("init");
        let config = crate::cloud::config::CloudConfigStore::new(vault.clone());
        let connection = config.save_official_connection(crate::cloud::official_types::OfficialConnection {
            id: String::new(), name: "Google".into(), provider: crate::cloud::oauth_types::OfficialProvider::GoogleDrive,
            config_version: 1, revision: 0, client_id: "client".into(), client_secret: None,
            refresh_token: "invalid-offline".into(), root_id: "root".into(),
        }).expect("save");
        let runtime = crate::cloud::runtime::CloudRuntime::new(vault, Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
        runtime.resume().expect("resume");
        let mut adapter = OfficialDownloadAdapter { parents: std::collections::HashMap::from([("docs/a.txt".into(), "docs/".into()), ("other/a.txt".into(), "other/".into())]), content_types: Default::default(), pages: None, runtime: runtime.clone(), connection: connection.id.clone(), revision: 1,
            names: std::collections::HashMap::from([("docs/a.txt".into(), "a.txt".into()), ("other/a.txt".into(), "a.txt".into())]),
            directories: Default::default(),
            files: tokio::sync::Mutex::new(std::collections::HashMap::from([("docs/a.txt".into(), ("id-a".into(), 12)), ("other/a.txt".into(), ("id-b".into(), 13))])) };
        let plan = adapter.plan(DownloadSelection::Single { path: "docs/a.txt".into() }).await.expect("listed file");
        assert_eq!(plan[0].relative_path, "a.txt");
        adapter.content_types.lock().await.insert("docs/a.txt".into(), "application/vnd.google-apps.document".into());
        let exported = adapter.plan(DownloadSelection::Single { path: "docs/a.txt".into() }).await.expect("Docs export plan");
        assert_eq!(exported[0].relative_path, "a.txt.docx");
        adapter.content_types.lock().await.clear();
        assert!(adapter.plan(DownloadSelection::Single { path: "forged".into() }).await.is_err());
        assert!(adapter.plan(DownloadSelection::Folder { path: "docs/".into() }).await.is_err());
        let duplicates = adapter.plan(DownloadSelection::Files { paths: vec!["docs/a.txt".into(), "other/a.txt".into()] }).await.expect("same-name files");
        assert_eq!(duplicates[0].relative_path, "a.txt");
        assert_eq!(duplicates[1].relative_path, "a (2).txt");
        assert_eq!(duplicates[1].remote_path, "other/a.txt");
        use crate::cloud::official_api_types::{OfficialEntry, OfficialPage};
        let entry = |id: &str, name: &str, is_dir| OfficialEntry { id: id.into(), name: name.into(), is_dir, size: 1, content_type: None, revision: None, online_preview: false };
        adapter.directories.insert("docs/".into(), "folder".into());
        adapter.names.insert("docs/".into(), "Documents".into());
        adapter.pages = Some(std::collections::HashMap::from([
            (("folder".into(), None), OfficialPage { entries: vec![entry("one", "a.txt", false), entry("nested", "nested", true)], next: Some("next".into()), complete: false }),
            (("folder".into(), Some("next".into())), OfficialPage { entries: vec![entry("two", "a.txt", false)], next: None, complete: true }),
            (("nested".into(), None), OfficialPage { entries: vec![entry("three", "b.txt", false)], next: None, complete: true }),
        ]));
        let folder = adapter.plan(DownloadSelection::Folder { path: "docs/".into() }).await.expect("folder recursion");
        assert_eq!(folder.iter().map(|i| i.relative_path.as_str()).collect::<Vec<_>>(), ["Documents/a.txt", "Documents/a (2).txt", "Documents/nested/b.txt"]);
        let mixed = adapter.plan(DownloadSelection::Files { paths: vec!["docs/".into(), "docs/a.txt".into(), "other/a.txt".into()] }).await.expect("mixed and overlapping selections");
        assert_eq!(mixed.len(), 4);
        assert_eq!(mixed[3].relative_path, "a.txt");
        adapter.names.insert("docs/unrelated".into(), "independent.txt".into());
        adapter.files.lock().await.insert("docs/unrelated".into(), ("independent-id".into(), 1));
        adapter.parents.insert("docs/unrelated".into(), "different-parent".into());
        let independent = adapter.plan(DownloadSelection::Files { paths: vec!["docs/".into(), "docs/unrelated".into()] }).await.expect("ID prefix is not ancestry");
        assert_eq!(independent.len(), 4);
        assert!(independent.iter().any(|item| item.remote_path == "docs/unrelated"));
        adapter.pages.as_mut().expect("pages").get_mut(&("nested".into(), None)).expect("nested").entries.clear();
        let empty = adapter.plan(DownloadSelection::Folder { path: "docs/".into() }).await.expect("empty child directory");
        assert!(empty.iter().any(|item| item.is_dir && item.relative_path == "Documents/nested"));
        adapter.pages.as_mut().expect("pages").get_mut(&("nested".into(), None)).expect("nested").entries = vec![entry("escape", "../escape", false)];
        assert!(adapter.plan(DownloadSelection::Folder { path: "docs/".into() }).await.is_err());
        adapter.pages.as_mut().expect("pages").get_mut(&("nested".into(), None)).expect("nested").entries = vec![entry("folder", "cycle", true)];
        assert!(adapter.plan(DownloadSelection::Folder { path: "docs/".into() }).await.is_err());
        adapter.pages.as_mut().expect("pages").get_mut(&("folder".into(), Some("next".into()))).expect("page").next = Some("next".into());
        assert!(adapter.plan(DownloadSelection::Folder { path: "docs/".into() }).await.is_err());
        let cancel = tokio_util::sync::CancellationToken::new(); cancel.cancel();
        let root = crate::cloud::PhysicalRoot::open(data.path()).expect("root");
        assert!(adapter.execute(&plan[0], &root, false, cancel, None).await.is_err());
        config.delete_connection(&connection.id).expect("remove");
        assert!(adapter.validate().await.is_err());
    }
}
