use super::{traits::BrowserBackend, types::*};
use crate::{AppError, PeerManager, PreviewPayload, cloud::runtime::CloudRuntime};
use futures_util::future::BoxFuture;
use std::sync::Arc;

impl super::traits::BrowserServiceFactory for OfficialServiceFactory {
    fn create<'a>(&'a self, _: &'a PeerManager, cloud: &'a CloudRuntime, location: &'a FileLocation)
        -> BoxFuture<'a, Result<Option<(Arc<dyn BrowserBackend>, String)>, AppError>> {
        Box::pin(async move {
            let FileLocation::Cloud { connection_id, root_id } = location else { return Ok(None); };
            cloud.admit()?;
            let snapshot = cloud.config.snapshot().map_err(crate::cloud::runtime::config_error)?;
            let Some(entry) = snapshot.official_connections.into_iter().find(|entry| &entry.id == connection_id) else { return Ok(None); };
            if root_id.as_ref().is_some_and(|root| root != &entry.root_id) { return Err(super::service::invalid()); }
            let backend: Arc<dyn BrowserBackend> = Arc::new(OfficialBrowserBackend {
                connection_id: entry.id, revision: entry.revision, root_id: entry.root_id, provider: entry.provider,
                parents: Default::default(), names: Default::default(), uploads: Default::default(), content_types: Default::default(), directories: Default::default(), cursors: Default::default(), files: Default::default(),
            });
            Ok(Some((backend, String::new())))
        })
    }
}

impl OfficialBrowserBackend {
    async fn protected_path(&self, path: &str) -> bool {
        let names = self.names.lock().await;
        let parents = self.parents.lock().await;
        let mut current = path;
        for _ in 0..=crate::download_types::MAX_DOWNLOAD_DEPTH {
            if names.get(current).is_some_and(|name| matches!(name.as_str(), "_peerdav_encrypt" | "_peerdav_mirror")) { return true; }
            let Some(parent) = parents.get(current) else { return false; };
            current = parent;
        }
        // Fail closed for malformed/cyclic ancestry rather than guessing from IDs.
        true
    }

    async fn admit_write(&self, peers: &PeerManager, cloud: &CloudRuntime, path: &str) -> Result<(), AppError> {
        self.admit(peers, cloud).await?;
        if self.protected_path(path).await { return Err(super::service::unavailable()); }
        // Sync data lives in reserved namespaces. Guard the target's provider
        // ancestry, not every write on the connection when any mapping is busy.
        // This also applies when a previously selected upload starts later.
        Ok(())
    }

    async fn map_page(&self, path: &str, parent: &str, page: crate::cloud::official_api_types::OfficialPage) -> Result<FilePageDto, AppError> {
        let protected = self.protected_path(path).await;
        let mut directories = self.directories.lock().await;
        let mut files = self.files.lock().await;
        let mut names = self.names.lock().await;
        let mut cursors = self.cursors.lock().await;
        if page.next.is_some() && cursors.len() >= 1024 { return Err(listing_limit()); }
        let mut mapped = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            // Names are display data, not identity: Drive permits same-name siblings.
            let logical = entry.id.clone();
            mapped.push((logical, entry));
        }
        let additions = mapped.iter().filter(|(logical, _)| !directories.contains_key(logical) && !files.contains_key(logical)).count();
        if directories.len().saturating_add(files.len()).saturating_add(additions) > 100_000 {
            return Err(listing_limit());
        }
        let next = if let Some(cursor) = page.next {
            let token = crate::local_upload::token()?;
            cursors.insert(token.clone(), (parent.into(), cursor));
            Some(token)
        } else { None };
        let mut entries = Vec::with_capacity(mapped.len());
        for (logical, entry) in mapped {
            self.parents.lock().await.insert(logical.clone(), path.into());
            if let Some(kind) = &entry.content_type { self.content_types.lock().await.insert(logical.clone(), kind.clone()); }
            names.insert(logical.clone(), entry.name.clone());
            if entry.is_dir { directories.insert(logical.clone(), entry.id); }
            else { files.insert(logical.clone(), (entry.id, entry.size)); }
            let entry_protected = protected || matches!(entry.name.as_str(), "_peerdav_encrypt" | "_peerdav_mirror");
            entries.push(FileEntryDto { id: logical, parent_id: path.into(), name: entry.name, is_dir: entry.is_dir, size: entry.size, modified: None, content_type: entry.content_type, online_preview: entry.online_preview, protected: entry_protected });
        }
        let parents = self.parents.lock().await;
        let mut current = path.to_owned();
        let mut breadcrumbs = Vec::new();
        let mut visited = std::collections::HashSet::new();
        while !current.is_empty() {
            if !visited.insert(current.clone()) || breadcrumbs.len() >= crate::download_types::MAX_DOWNLOAD_DEPTH { return Err(super::service::invalid()); }
            let name = names.get(&current).cloned().ok_or_else(super::service::invalid)?;
            breadcrumbs.push(FileBreadcrumbDto { id: current.clone(), name });
            current = parents.get(&current).cloned().ok_or_else(super::service::invalid)?;
        }
        breadcrumbs.reverse();
        Ok(FilePageDto { id: path.into(), breadcrumbs, entries, complete: page.complete, next, protected })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::official_api_types::{OfficialEntry, OfficialPage};

    fn backend() -> OfficialBrowserBackend {
        OfficialBrowserBackend { connection_id: "test".into(), revision: 1, root_id: "root".into(),
            provider: crate::cloud::oauth_types::OfficialProvider::GoogleDrive,
            parents: Default::default(), names: Default::default(), uploads: Default::default(), content_types: Default::default(), directories: Default::default(), files: Default::default(), cursors: Default::default() }
    }
    fn page(ids: &[&str], is_dir: bool) -> OfficialPage {
        OfficialPage { entries: ids.iter().map(|id| OfficialEntry { id: (*id).into(), name: "same.txt".into(),
            is_dir, size: 7, content_type: None, revision: None, online_preview: false }).collect(), next: None, complete: true }
    }

    #[test]
    fn revision_extraction_covers_dropbox_and_microsoft_etags() {
        let provider = crate::cloud::oauth_types::OfficialProvider::Dropbox;
        let meta = serde_json::json!({ "rev": "abc123", "path_display": "/a.txt" });
        assert_eq!(delete_revision(provider, &meta).expect("revision"), "dropbox-rev:abc123");
        assert!(delete_revision(provider, &serde_json::json!({ "rev": "" })).is_err());
        assert_eq!(delete_revision(provider, &serde_json::json!({ "id": "id:folder", "path_display": "/docs" })).expect("folder id"), "dropbox-id:id:folder");
        let microsoft = serde_json::json!({ "eTag": "\"etag\"" });
        assert_eq!(delete_revision(crate::cloud::oauth_types::OfficialProvider::OneDrive, &microsoft).expect("etag"), "\"etag\"");
    }

    #[test]
    fn official_providers_with_move_implementations_advertise_move() {
        for provider in [
            crate::cloud::oauth_types::OfficialProvider::GoogleDrive,
            crate::cloud::oauth_types::OfficialProvider::OneDrive,
            crate::cloud::oauth_types::OfficialProvider::BaiduNetdisk,
            crate::cloud::oauth_types::OfficialProvider::Dropbox,
        ] {
            let mut backend = backend();
            backend.provider = provider;
            assert!(backend.supports_move(), "{provider:?}");
        }
    }

    #[tokio::test]
    async fn upload_selection_follows_advertised_provider_capabilities() {
        use crate::cloud::oauth_types::OfficialProvider;
        for (provider, allowed) in [
            (OfficialProvider::Dropbox, true),
            (OfficialProvider::BaiduNetdisk, true),
            (OfficialProvider::OneDrive, true),
        ] {
            let data = tempfile::tempdir().expect("data");
            let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
            vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("init");
            let config = crate::cloud::config::CloudConfigStore::new(vault.clone());
            let connection = config.save_official_connection(crate::cloud::official_types::OfficialConnection {
                id: String::new(), name: "Drive".into(), provider,
                config_version: 1, revision: 0, client_id: "client".into(), client_secret: None,
                refresh_token: "invalid-offline".into(), root_id: "/".into(),
            }).expect("connection");
            let runtime = CloudRuntime::new(vault.clone(), Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
            runtime.resume().expect("resume");
            let peers = Arc::new(PeerManager::new(vault, data.path().join("peer-temp")).expect("peers"));
            let mut backend = backend();
            backend.connection_id = connection.id;
            backend.revision = connection.revision;
            backend.provider = provider;
            backend.root_id = "/".into();
            let file = data.path().join("a.txt");
            std::fs::write(&file, "abc").expect("file");
            let result = backend.select_upload(&peers, &runtime, String::new(), vec![file], UploadMode::Files, Default::default()).await;
            if allowed {
                let FileUploadDto::Cloud(selected) = result.expect("advertised upload selection") else { panic!("cloud queue") };
                assert_eq!(selected.total_files, 1);
                backend.release_upload(&peers, &runtime, &selected.batch_id).await.expect("release");
            } else {
                assert_eq!(result.expect_err("unadvertised upload").code, "FILE_CAPABILITY_UNAVAILABLE");
            }
        }
    }

    #[tokio::test]
    async fn google_upload_selection_has_no_network_side_effects_and_cancellation_releases_handles() {
        let data = tempfile::tempdir().expect("data");
        let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("init");
        let config = crate::cloud::config::CloudConfigStore::new(vault.clone());
        let connection = config.save_official_connection(crate::cloud::official_types::OfficialConnection {
            id: String::new(), name: "Google".into(), provider: crate::cloud::oauth_types::OfficialProvider::GoogleDrive,
            config_version: 1, revision: 0, client_id: "client".into(), client_secret: None,
            refresh_token: "invalid-offline".into(), root_id: "root".into(),
        }).expect("connection");
        let runtime = CloudRuntime::new(vault.clone(), Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
        runtime.resume().expect("resume");
        let peers = Arc::new(PeerManager::new(vault, data.path().join("peer-temp")).expect("peers"));
        let mut backend = backend(); backend.connection_id = connection.id; backend.revision = connection.revision;
        let reserved = backend.create_dir(&peers, &runtime, "", "_peerdav_encrypt").await.expect_err("ordinary creation must not shadow a sync namespace");
        assert_eq!(reserved.code, "FILE_CAPABILITY_UNAVAILABLE");
        let reserved_folder = data.path().join("_peerdav_mirror");
        std::fs::create_dir(&reserved_folder).expect("reserved folder");
        let reserved = backend.select_upload(&peers, &runtime, String::new(), vec![reserved_folder], UploadMode::Folder, Default::default()).await.expect_err("ordinary upload cannot create sync namespace");
        assert_eq!(reserved.code, "FILE_CAPABILITY_UNAVAILABLE");
        assert!(backend.uploads.lock().await.is_empty());
        let file = data.path().join("a.txt"); std::fs::write(&file, "abc").expect("file");
        let FileUploadDto::Cloud(selected) = backend.select_upload(&peers, &runtime, String::new(), vec![file], UploadMode::Files, Default::default()).await.expect("select offline") else { panic!("cloud queue") };
        assert_eq!(selected.phase, "selected");
        assert_eq!(selected.total_files, 1);
        let page = backend.upload_page(&peers, &runtime, &selected.batch_id, None, None, None).await.expect("existing dialog page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].name, "a.txt");
        assert_eq!(page.entries[0].size, 3);
        assert!(backend.upload_page(&peers, &runtime, &selected.batch_id, Some("wrong-queue".into()), None, None).await.is_err());
        assert!(backend.upload_page(&peers, &runtime, &selected.batch_id, None, Some("2".into()), None).await.is_err());
        let empty = backend.upload_page(&peers, &runtime, &selected.batch_id, None, Some("1".into()), Some(128)).await.expect("end cursor");
        assert!(empty.entries.is_empty() && empty.next_cursor.is_none());
        assert_eq!(backend.uploads.lock().await[&selected.batch_id].files.len(), 1);
        let adapter = OfficialUploadAdapter { backend: &backend, cloud: &runtime };
        {
            let mut jobs = backend.uploads.lock().await;
            let job = jobs.get_mut(&selected.batch_id).expect("selected queue");
            job.status.phase = "conflict".into();
            job.status.conflict = Some(crate::cloud::directory_types::DirectoryConflictDto { overwrite_allowed: false, conflict_id: "conflict".into(), relative_path: "a.txt".into(), key: "a.txt".into(), etag: None });
        }
        use crate::traits::UploadQueueAdapter;
        assert!(adapter.resolve(&selected.batch_id, "wrong", crate::cloud::directory_types::DirectoryConflictPolicy::Skip, false).await.is_err());
        adapter.resolve(&selected.batch_id, "conflict", crate::cloud::directory_types::DirectoryConflictPolicy::Skip, true).await.expect("resolve real conflict");
        assert!(adapter.resolve(&selected.batch_id, "conflict", crate::cloud::directory_types::DirectoryConflictPolicy::Skip, true).await.is_err());
        backend.upload_action(&peers, &runtime, &selected.batch_id, UploadAction::Cancel).await.expect("cancel");
        backend.release_upload(&peers, &runtime, &selected.batch_id).await.expect("release");
        assert!(backend.uploads.lock().await.is_empty());
        let tree = data.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).expect("nested directory");
        std::fs::create_dir(tree.join("empty")).expect("empty directory");
        std::fs::write(tree.join("nested/b.txt"), "payload").expect("nested file");
        let FileUploadDto::Cloud(folder) = backend.select_upload(&peers, &runtime, String::new(), vec![tree], UploadMode::Folder, Default::default()).await.expect("folder selection") else { panic!("cloud queue") };
        assert_eq!(folder.skipped_empty_directories, 1);
        let page = backend.upload_page(&peers, &runtime, &folder.batch_id, None, None, None).await.expect("folder page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].relative_path, "tree/nested");
        assert_eq!(page.entries[0].kind, "directory");
        backend.release_upload(&peers, &runtime, &folder.batch_id).await.expect("release folder");
        let file = data.path().join("a.txt");
        let FileUploadDto::Cloud(pending) = backend.select_upload(&peers, &runtime, String::new(), vec![file], UploadMode::Files, Default::default()).await.expect("select before sync") else { panic!("cloud queue") };
        let mirror_root = tempfile::tempdir().expect("mirror root");
        let mirror = runtime.save_mirror_mapping(crate::mirror::SaveMirrorMappingInput { id: None, connection_id: backend.connection_id.clone(), bucket: Some("root".into()), local_root: mirror_root.path().to_owned(), cloud_prefix: "sample".into(), full_check: false, schedule: Default::default() }).expect("mirror mapping");
        let mut sync_page = self::page(&["sync-namespace"], true);
        sync_page.entries[0].name = "_peerdav_mirror".into();
        backend.map_page("", "root", sync_page).await.expect("sync namespace");
        backend.map_page("sync-namespace", "sync-namespace", self::page(&["sync-child"], true)).await.expect("sync child");
        backend.map_page("", "root", self::page(&["ordinary"], true)).await.expect("ordinary sibling");
        backend.map_page("ordinary", "ordinary", self::page(&["ordinary-child"], true)).await.expect("ordinary nested folder");
        for phase in [crate::sync::manager_types::SyncPhase::Frozen, crate::sync::manager_types::SyncPhase::Scanning, crate::sync::manager_types::SyncPhase::Uploading, crate::sync::manager_types::SyncPhase::Publishing, crate::sync::manager_types::SyncPhase::Applying] {
            runtime.manager.report_status(crate::sync::manager_types::SyncStatus { mapping_id: mirror.id.clone(), run_id: None, phase, detail: None });
            for path in ["", "ordinary", "ordinary-child"] {
                backend.admit_write(&peers, &runtime, path).await.expect("sync must not freeze unrelated folders");
            }
            for path in ["sync-namespace", "sync-child"] {
                assert_eq!(backend.admit_write(&peers, &runtime, path).await.expect_err("sync namespace remains protected").code, "FILE_CAPABILITY_UNAVAILABLE");
            }
        }
        let FileUploadDto::Cloud(during_sync) = backend.select_upload(&peers, &runtime, "ordinary-child".into(), vec![data.path().join("a.txt")], UploadMode::Files, Default::default()).await.expect("ordinary upload selection while sync runs") else { panic!("cloud queue") };
        backend.release_upload(&peers, &runtime, &during_sync.batch_id).await.expect("release independent queue");
        for path in ["sync-namespace", "sync-child"] {
            assert_eq!(backend.create_dir(&peers, &runtime, path, "new").await.expect_err("protected creation").code, "FILE_CAPABILITY_UNAVAILABLE");
            assert_eq!(backend.delete_plan(&peers, &runtime, path).await.expect_err("protected deletion").code, "FILE_CAPABILITY_UNAVAILABLE");
            assert_eq!(backend.move_entry(&peers, &runtime, path, "ordinary").await.expect_err("protected source").code, "FILE_CAPABILITY_UNAVAILABLE");
            assert_eq!(backend.move_entry(&peers, &runtime, "ordinary", path).await.expect_err("protected destination").code, "FILE_CAPABILITY_UNAVAILABLE");
        }
        // A queued upload must still recheck its target, not only its selection-time admission.
        backend.uploads.lock().await.get_mut(&pending.batch_id).expect("pending queue").status.prefix = "sync-child".into();
        assert_eq!(backend.upload_action(&peers, &runtime, &pending.batch_id, UploadAction::Start).await.expect_err("recheck queued target at start").code, "FILE_CAPABILITY_UNAVAILABLE");
        backend.upload_action(&peers, &runtime, &pending.batch_id, UploadAction::Cancel).await.expect("cancel remains available while frozen");
        backend.release_upload(&peers, &runtime, &pending.batch_id).await.expect("release frozen queue");
        assert!(backend.select_upload(&peers, &runtime, "forged/".into(), Vec::new(), UploadMode::Files, Default::default()).await.is_err());
    }

    #[tokio::test]
    async fn sync_namespace_entries_are_protected_by_provider_identity() {
        let backend = backend();
        let mut page = page(&["namespace"], true);
        page.entries[0].name = "_peerdav_encrypt".into();
        let mapped = backend.map_page("", "root", page).await.expect("namespace");
        assert!(mapped.entries[0].protected);
        let path = &mapped.entries[0].id;
        assert!(backend.protected_path(path).await);
        let child = backend.map_page(path, "namespace", super::tests::page(&["child"], false)).await.expect("protected child");
        assert!(child.protected && child.entries[0].protected);
        backend.names.lock().await.insert("independent".into(), "ordinary.txt".into());
        backend.parents.lock().await.insert("independent".into(), path.clone());
        assert!(backend.protected_path("independent").await, "parent identity protects an unrelated-looking ID");
        let lookalike = format!("{path}not-a-child");
        backend.names.lock().await.insert(lookalike.clone(), "ordinary.txt".into());
        backend.parents.lock().await.insert(lookalike.clone(), String::new());
        assert!(!backend.protected_path(&lookalike).await, "ID prefix alone does not establish ancestry");
    }

    #[test]
    fn google_delete_accepts_official_version_without_etag() {
        use crate::cloud::oauth_types::OfficialProvider;
        assert_eq!(delete_revision(OfficialProvider::GoogleDrive, &serde_json::json!({"version":"42"})).expect("Drive v3 version"), "google-version:42");
        assert_eq!(delete_revision(OfficialProvider::OneDrive, &serde_json::json!({"eTag":"etag"})).expect("Graph revision"), "etag");
        assert!(delete_revision(OfficialProvider::OneDrive, &serde_json::json!({"version":"42"})).is_err());
    }

    #[tokio::test]
    async fn official_http_pages_flow_into_distinct_browser_entries() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use crate::cloud::{oauth_types::OfficialProvider, official_api::OfficialApi};
        for (provider, body) in [
            (OfficialProvider::GoogleDrive, r#"{"files":[{"id":"a","name":"same.txt","mimeType":"text/plain","size":"7"},{"id":"b","name":"same.txt","mimeType":"text/plain","size":"7"}],"nextPageToken":"next"}"#),
            (OfficialProvider::OneDrive, r#"{"value":[{"id":"a","name":"a.txt","size":7},{"id":"b","name":"folder","size":0,"folder":{}}]}"#),
            (OfficialProvider::BaiduNetdisk, r#"{"errno":0,"list":[{"path":"/a.txt","server_filename":"a.txt","isdir":0,"size":7},{"path":"/folder","server_filename":"folder","isdir":1,"size":0}]}"#),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("address");
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut request = [0; 4096]; socket.read(&mut request).await.expect("read");
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("write");
            });
            let api = OfficialApi::new(provider, zeroize::Zeroizing::new("fixture".into())).expect("api");
            let page = api.list_fixture(&format!("http://{address}/list"), None).await.expect("http list");
            let backend = backend();
            let mapped = backend.map_page("", "root", page).await.expect("browser entries");
            assert_eq!(mapped.entries.len(), 2);
            assert_ne!(mapped.entries[0].id, mapped.entries[1].id);
            assert_eq!(backend.names.lock().await.get(&mapped.entries[0].id), Some(&mapped.entries[0].name));
            if let Some(next) = mapped.next {
                assert_eq!(backend.cursors.lock().await.get(&next), Some(&("root".into(), "next".into())));
            }
            server.await.expect("server");
        }
    }

    #[tokio::test]
    async fn listing_limit_is_not_reported_as_an_unimplemented_capability() {
        let backend = backend();
        let mut directories = backend.directories.lock().await;
        for index in 0..99_999 { directories.insert(format!("{index}/"), index.to_string()); }
        drop(directories);
        backend.map_page("", "root", page(&["last"], false)).await.expect("exact limit");
        backend.map_page("", "root", page(&["last"], false)).await.expect("refresh does not consume capacity");
        let error = backend.map_page("", "root", page(&["new"], false)).await.err().expect("limit");
        assert_eq!(error.code, "FILE_LIST_LIMIT");
        assert_eq!(backend.files.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn same_name_siblings_and_refresh_keep_distinct_provider_identities() {
        for is_dir in [false, true] {
            let backend = backend();
            let first = backend.map_page("", "root", page(&["id-a", "id-b"], is_dir)).await.expect("legal duplicate names");
            assert_ne!(first.entries[0].id, first.entries[1].id);
            assert_eq!(first.entries[0].id, "id-a");
            let dto = serde_json::to_value(&first).expect("client identity DTO");
            assert_eq!(dto["entries"][0]["id"], "id-a");
            assert_eq!(dto["entries"][0]["parentId"], "");
            assert!(dto["entries"][0].get("path").is_none());
            assert!(dto.get("path").is_none());
            let refreshed = backend.map_page("", "root", page(&["id-b", "id-a"], is_dir)).await.expect("refresh");
            assert_eq!(first.entries[0].id, refreshed.entries[1].id);
            let next = backend.map_page("", "root", page(&["id-c"], is_dir)).await.expect("next page");
            assert_ne!(first.entries[0].id, next.entries[0].id);
            if is_dir {
                let child = backend.map_page(&first.entries[0].id, "id-a", page(&["child"], false)).await.expect("child");
                assert_eq!(child.entries[0].parent_id, first.entries[0].id);
                assert_eq!(child.breadcrumbs[0].name, "same.txt");
                assert_eq!(child.breadcrumbs[0].id, first.entries[0].id);
            }
        }
    }
}

fn listing_limit() -> AppError {
    AppError { code: "FILE_LIST_LIMIT".into(), message: "目录会话达到资源上限，请重新打开网盘 / Directory session limit reached; reopen the drive".into(), detail: None, context: None }
}

fn delete_revision(provider: crate::cloud::oauth_types::OfficialProvider, meta: &serde_json::Value) -> Result<String, AppError> {
    meta.get("eTag").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
        .or_else(|| (provider == crate::cloud::oauth_types::OfficialProvider::GoogleDrive).then(|| meta.get("version").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(|v| format!("google-version:{v}"))).flatten())
        .or_else(|| (provider == crate::cloud::oauth_types::OfficialProvider::Dropbox).then(|| meta.get("rev").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(|v| format!("dropbox-rev:{v}"))).flatten())
        // Dropbox folders have no rev; delete_v2 is not conditional, so the
        // stable entry id is enough to satisfy the confirmation boundary.
        .or_else(|| (provider == crate::cloud::oauth_types::OfficialProvider::Dropbox).then(|| meta.get("id").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(|v| format!("dropbox-id:{v}"))).flatten())
        .or_else(|| (provider == crate::cloud::oauth_types::OfficialProvider::BaiduNetdisk).then(|| meta.get("md5").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(|v| format!("baidu-md5:{v}")).or_else(|| meta.get("fs_id").and_then(serde_json::Value::as_u64).map(|v| format!("baidu-fs:{v}")))).flatten())
        .ok_or_else(|| crate::cloud::official_runtime::api_error(crate::cloud::official_api_types::OfficialApiError::InvalidResponse))
}

fn unsupported() -> AppError {
    AppError { code: "FILE_CAPABILITY_UNAVAILABLE".into(), message: "此网盘操作暂未实现 / This drive operation is not available yet".into(), detail: None, context: None }
}

impl BrowserBackend for OfficialBrowserBackend {
    fn prepare<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime) -> BoxFuture<'a, Result<(), AppError>> {
        self.admit(peers, cloud)
    }
    fn upload_resolve<'a>(&'a self, peers: &'a Arc<PeerManager>, cloud: &'a Arc<CloudRuntime>, id: &'a str, conflict_id: &'a str, decision: crate::cloud::directory_types::DirectoryConflictPolicy, apply_to_remaining: bool) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move { self.admit(peers, cloud).await?; crate::traits::UploadQueueAdapter::resolve(&OfficialUploadAdapter { backend: self, cloud }, id, conflict_id, decision, apply_to_remaining).await })
    }
    fn supports_move(&self) -> bool { true }
    fn move_entry<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str, destination: &'a str) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            self.admit_write(peers, cloud, id).await?;
            self.admit_write(peers, cloud, destination).await?;
            if id.is_empty() || id == destination { return Err(super::service::invalid()); }
            let from = self.parents.lock().await.get(id).cloned().ok_or_else(super::service::invalid)?;
            let target = if destination.is_empty() { self.root_id.clone() } else { self.directories.lock().await.get(destination).cloned().ok_or_else(super::service::invalid)? };
            if from == destination { return Err(super::service::invalid()); }
            {
                let parents = self.parents.lock().await;
                let mut current = destination;
                let mut visited = std::collections::HashSet::new();
                while !current.is_empty() {
                    if current == id || !visited.insert(current) || visited.len() > crate::download_types::MAX_DOWNLOAD_DEPTH { return Err(super::service::invalid()); }
                    current = parents.get(current).ok_or_else(super::service::invalid)?;
                }
            }
            let from_id = if from.is_empty() { self.root_id.clone() } else { self.directories.lock().await.get(&from).cloned().ok_or_else(super::service::invalid)? };
            let _network = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            api.move_item(id, &from_id, &target).await.map_err(crate::cloud::official_runtime::api_error)?;
            self.parents.lock().await.insert(id.into(), destination.into());
            Ok(())
        })
    }
    fn create_dir<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, name: &'a str) -> BoxFuture<'a, Result<FileEntryDto, AppError>> {
        Box::pin(async move {
            self.admit_write(peers, cloud, path).await?;
            if matches!(name, "_peerdav_encrypt" | "_peerdav_mirror") { return Err(super::service::unavailable()); }
            if !self.capabilities().create_dir { return Err(unsupported()); }
            let parent = if path.is_empty() { self.root_id.clone() } else { self.directories.lock().await.get(path).cloned().ok_or_else(super::service::invalid)? };
            let _network = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            let id = api.create_folder(&parent, name).await.map_err(crate::cloud::official_runtime::api_error)?;
            self.admit(peers, cloud).await?;
            self.directories.lock().await.insert(id.clone(), id.clone());
            self.parents.lock().await.insert(id.clone(), path.into());
            self.names.lock().await.insert(id.clone(), name.into());
            Ok(super::service::directory_entry(id, path.into(), name.into()))
        })
    }
    fn edit_read<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str) -> BoxFuture<'a, Result<EditContent, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let (id, size) = self.files.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?;
            if size > MAX_EDIT_BYTES as u64 { return Err(super::service::unavailable()); }
            let _permit = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            let before = api.metadata(&id).await.map_err(crate::cloud::official_runtime::api_error)?;
            let revision = delete_revision(self.provider, &before)?;
            let mut response = api.media(&id).await.map_err(crate::cloud::official_runtime::api_error)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| crate::cloud::official_runtime::api_error(crate::cloud::official_api_types::OfficialApiError::Transport(e.without_url())))? {
                if bytes.len().saturating_add(chunk.len()) > MAX_EDIT_BYTES { return Err(super::service::unavailable()); }
                bytes.extend_from_slice(&chunk);
            }
            let after = api.metadata(&id).await.map_err(crate::cloud::official_runtime::api_error)?;
            if delete_revision(self.provider, &after)? != revision { return Err(super::service::invalid()); }
            self.admit(peers, cloud).await?;
            let name = self.names.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?;
            Ok(EditContent { name, bytes, revision })
        })
    }
    fn edit_write<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, revision: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<String, AppError>> {
        Box::pin(async move {
            self.admit_write(peers, cloud, path).await?;
            if bytes.len() > MAX_EDIT_BYTES { return Err(super::service::unavailable()); }
            let (id, _) = self.files.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?;
            let _permit = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            // Only use the revision returned by this write, never a later GET.
            api.replace_content(&id, revision, bytes).await.map_err(crate::cloud::official_runtime::api_error)
        })
    }
    fn online_preview<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str) -> BoxFuture<'a, Result<String, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let (id, _) = self.files.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?;
            let _permit = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            let meta = api.metadata(&id).await.map_err(crate::cloud::official_runtime::api_error)?;
            let link = meta.get("webViewLink").or_else(|| meta.get("webUrl")).and_then(serde_json::Value::as_str).ok_or_else(super::service::unavailable)?;
            let url = url::Url::parse(link).map_err(|_| super::service::invalid())?;
            let host = url.host_str().unwrap_or_default();
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || !(host == "drive.google.com" || host == "docs.google.com" || host == "onedrive.live.com" || host.ends_with(".sharepoint.com")) { return Err(super::service::invalid()); }
            self.admit(peers, cloud).await?;
            Ok(url.into())
        })
    }
    fn capabilities(&self) -> FileCapabilities {
        FileCapabilities { delete_confirmation: DeleteConfirmation::Entry, list: true, preview: true,
            upload: matches!(self.provider, crate::cloud::oauth_types::OfficialProvider::GoogleDrive | crate::cloud::oauth_types::OfficialProvider::Dropbox | crate::cloud::oauth_types::OfficialProvider::BaiduNetdisk | crate::cloud::oauth_types::OfficialProvider::OneDrive),
            download: true, download_directory: true,
            delete: true, create_dir: true,
            rename: false, external_edit: true,
            online_preview: matches!(self.provider, crate::cloud::oauth_types::OfficialProvider::GoogleDrive | crate::cloud::oauth_types::OfficialProvider::OneDrive) }
    }
    fn admit<'a>(&'a self, _: &'a PeerManager, cloud: &'a CloudRuntime) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            cloud.admit()?;
            let snapshot = cloud.config.snapshot().map_err(crate::cloud::runtime::config_error)?;
            if snapshot.official_connections.iter().any(|entry| entry.id == self.connection_id && entry.revision == self.revision && entry.root_id == self.root_id) { Ok(()) }
            else { Err(super::service::invalid()) }
        })
    }
    fn list<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, next: Option<&'a str>) -> BoxFuture<'a, Result<FilePageDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let parent = if path.is_empty() { self.root_id.clone() } else {
                self.directories.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?
            };
            let cursor = if let Some(next) = next {
                let mut cursors = self.cursors.lock().await;
                let (bound, value) = cursors.remove(next).ok_or_else(super::service::invalid)?;
                if bound != parent { return Err(super::service::invalid()); }
                Some(value)
            } else { None };
            let page = cloud.list_official(&self.connection_id, &parent, cursor.as_deref()).await?;
            self.admit(peers, cloud).await?;
            self.map_page(path, &parent, page).await
        })
    }
    fn preview<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str) -> BoxFuture<'a, Result<PreviewPayload, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            if !self.capabilities().preview { return Err(unsupported()); }
            let (id, size) = self.files.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?;
            let limit = crate::types::MAX_PREVIEW_BYTES;
            if size > limit { return Err(crate::DomainError::PreviewTooLarge { size, limit }.into()); }
            let _network = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            let mut response = api.media(&id).await.map_err(crate::cloud::official_runtime::api_error)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|source| crate::cloud::official_runtime::api_error(crate::cloud::official_api_types::OfficialApiError::Transport(source.without_url())))? {
                let size = (bytes.len() as u64).saturating_add(chunk.len() as u64);
                if size > limit { return Err(crate::DomainError::PreviewTooLarge { size, limit }.into()); }
                bytes.extend_from_slice(&chunk);
            }
            self.admit(peers, cloud).await?;
            let name = self.names.lock().await.get(path).cloned().ok_or_else(super::service::invalid)?;
            crate::peer::preview_payload(name, bytes).map_err(AppError::from)
        })
    }
    fn delete_plan<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeletePlanDto, AppError>> {
        Box::pin(async move {
            self.admit_write(peers, cloud, path).await?;
            if !self.capabilities().delete || path.is_empty() { return Err(unsupported()); }
            let directory = self.directories.lock().await.get(path).cloned();
            let (id, size) = if let Some(id) = directory { (id, 0) }
                else { self.files.lock().await.get(path).cloned().ok_or_else(super::service::invalid)? };
            if id == self.root_id { return Err(super::service::invalid()); }
            let _permit = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            let meta = api.metadata(&id).await.map_err(crate::cloud::official_runtime::api_error)?;
            let revision = delete_revision(self.provider, &meta)?;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| super::service::invalid())?.as_secs();
            Ok(crate::cloud::object_runtime_types::ObjectDeletePlanDto { plan_id: revision.clone(), expires_at_unix: now + 300, object_count: 1, connection_revision: self.revision,
                entries: vec![crate::cloud::object_runtime_types::ObjectEntryDto { key: path.into(), size, etag: Some(revision), protected: false }] })
        })
    }
    fn delete_execute<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, plan: Option<&'a str>) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeleteResultDto, AppError>> {
        Box::pin(async move {
            self.admit_write(peers, cloud, path).await?;
            if !self.capabilities().delete || path.is_empty() { return Err(unsupported()); }
            let directory = self.directories.lock().await.get(path).cloned();
            let id = if let Some(id) = directory { id }
                else { self.files.lock().await.get(path).map(|(id, _)| id.clone()).ok_or_else(super::service::invalid)? };
            if id == self.root_id { return Err(super::service::invalid()); }
            let _permit = cloud.network.read().await;
            let (_, api) = cloud.official_api(&self.connection_id).await?;
            api.delete_item(&id, plan.ok_or_else(super::service::invalid)?).await.map_err(crate::cloud::official_runtime::api_error)?;
            let mut removed = std::collections::HashSet::new();
            let mut parents = self.parents.lock().await;
            let mut children = std::collections::HashMap::<&str, Vec<&str>>::new();
            for (child, parent) in parents.iter() { children.entry(parent).or_default().push(child); }
            let mut pending = vec![path];
            while let Some(id) = pending.pop() {
                if !removed.insert(id.to_owned()) { continue; }
                if let Some(entries) = children.get(id) { pending.extend(entries.iter().copied()); }
            }
            drop(children);
            parents.retain(|key, _| !removed.contains(key));
            drop(parents);
            self.files.lock().await.retain(|key, _| !removed.contains(key));
            self.directories.lock().await.retain(|key, _| !removed.contains(key));
            self.names.lock().await.retain(|key, _| !removed.contains(key));
            self.content_types.lock().await.retain(|key, _| !removed.contains(key));
            Ok(crate::cloud::object_runtime_types::ObjectDeleteResultDto { deleted: vec![path.into()], failed: vec![] })
        })
    }
    fn select_upload<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: String, paths: Vec<std::path::PathBuf>, mode: UploadMode, options: crate::types::IgnoreOptions) -> BoxFuture<'a, Result<FileUploadDto, AppError>> {
        Box::pin(async move {
            self.admit_write(peers, cloud, &path).await?;
            if paths.is_empty() || paths.len() > 256 || (!matches!(mode, UploadMode::Files) && paths.len() != 1) { return Err(super::service::invalid()); }
            if !self.capabilities().upload { return Err(unsupported()); }
            if self.uploads.lock().await.len() >= 4 { return Err(listing_limit()); }
            let parent = if path.is_empty() { self.root_id.clone() } else { self.directories.lock().await.get(&path).cloned().ok_or_else(super::service::invalid)? };
            let folder = matches!(mode, UploadMode::Folder);
            let (files, root_name, skipped_symlinks, skipped_empty_directories) = tokio::task::spawn_blocking(move || {
                if !folder { return crate::local_upload::open_selected_files(paths).map(|(files, _)| (files, String::new(), 0, 0)); }
                if paths.len() != 1 { return Err(super::service::invalid()); }
                let name = paths[0].file_name().and_then(|n| n.to_str()).ok_or_else(super::service::invalid)?.to_owned();
                let root = crate::cloud::PhysicalRoot::open(&paths[0]).map_err(|e| AppError::from(crate::DomainError::from(e)))?;
                let mut walk = crate::local_upload_types::DirectoryWalk::with_options(root.upload_directory_handle().map_err(|e| AppError::from(crate::DomainError::from(e)))?, name.clone(), Some(options))?;
                let mut files = Vec::new();
                let mut scans = 0usize;
                while !walk.stack.is_empty() {
                    scans += 1;
                    if scans > 10_000 { return Err(listing_limit()); }
                    if let Some(file) = walk.next_chunk()? { files.push(file); }
                    if files.len() > 256 { return Err(super::service::invalid()); }
                }
                Ok((files, name, walk.skipped_symlinks, walk.skipped_empty_directories))
            }).await.map_err(|e| AppError::from(crate::DomainError::Internal { detail: e.to_string() }))??;
            self.admit(peers, cloud).await?;
            if matches!(root_name.as_str(), "_peerdav_encrypt" | "_peerdav_mirror") || files.iter().any(|file| matches!(file.relative.split('/').next(), Some("_peerdav_encrypt" | "_peerdav_mirror"))) { return Err(super::service::unavailable()); }
            if files.len() > 256 { return Err(super::service::invalid()); }
            for file in &files {
                if file.size > crate::types::MAX_TRANSFER_BYTES { return Err(crate::DomainError::TransferTooLarge { size: file.size, limit: crate::types::MAX_TRANSFER_BYTES }.into()); }
            }
            let id = crate::local_upload::token()?;
            let status = crate::cloud::directory_types::ObjectUploadDirectoryDto { batch_id: id.clone(), queue_id: id.clone(), connection_id: self.connection_id.clone(), connection_revision: self.revision, bucket: self.root_id.clone(), prefix: path, root_name, phase: "selected".into(), current_relative_path: None, completed_files: 0, completed_bytes: 0, total_files: files.len() as u64, skipped_files: 0, skipped_symlinks, skipped_empty_directories, conflict: None, error: None, recovery_required: false };
            let mut jobs = self.uploads.lock().await;
            if jobs.len() >= 4 { return Err(super::service::invalid()); }
            let mut listed = std::collections::BTreeMap::new();
            for file in &files {
                let visible = file.relative.strip_prefix(&format!("{}/", status.root_name)).filter(|_| !status.root_name.is_empty()).unwrap_or(&file.relative);
                let name = visible.split('/').next().ok_or_else(super::service::invalid)?.to_owned();
                let directory = visible.contains('/');
                let relative_path = if status.root_name.is_empty() { name.clone() } else { format!("{}/{name}", status.root_name) };
                listed.entry(name.clone()).or_insert(crate::local_upload_types::DirectoryEntryDto { name, relative_path, kind: if directory { "directory" } else { "file" }.into(), size: if directory { 0 } else { file.size }, directory_token: None, phase: "selected".into(), transferred: Some(0) });
            }
            let page = crate::local_upload_types::DirectoryPageDto { directory_token: id.clone(), parent_token: None, relative_path: status.root_name.clone(), entries: listed.into_values().collect(), next_cursor: None };
            let entry_states = files.iter().map(|file| (file.relative.clone(), ("selected".into(), 0, file.size))).collect();
            jobs.insert(id, OfficialUploadJob { entry_states, progress: Default::default(), decision: Default::default(), resolution: None, page, parent, files, status: status.clone(), cancel: cloud.vault.transfer_cancellation().map_err(AppError::from)?.child_token(), handle: None });
            Ok(FileUploadDto::Cloud(status))
        })
    }
    fn release_upload<'a>(&'a self, _: &'a PeerManager, _: &'a CloudRuntime, id: &'a str) -> BoxFuture<'a, Result<(), AppError>> { Box::pin(super::official_upload::release(self, id)) }
    fn upload_page<'a>(&'a self, peers: &'a Arc<PeerManager>, cloud: &'a Arc<CloudRuntime>, id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> BoxFuture<'a, Result<crate::local_upload_types::DirectoryPageDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            crate::traits::UploadQueueAdapter::page(&OfficialUploadAdapter { backend: self, cloud }, id, directory_token, cursor, limit).await
        })
    }
    fn upload_action<'a>(&'a self, peers: &'a Arc<PeerManager>, cloud: &'a Arc<CloudRuntime>, id: &'a str, action: UploadAction) -> BoxFuture<'a, Result<FileUploadDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            if matches!(action, UploadAction::Start) {
                let prefix = self.uploads.lock().await.get(id).ok_or_else(super::service::invalid)?.status.prefix.clone();
                self.admit_write(peers, cloud, &prefix).await?;
            }
            let adapter = OfficialUploadAdapter { backend: self, cloud };
            crate::local_upload::queue_action(&adapter, id, action).await.map(FileUploadDto::Cloud)
        })
    }
    fn download<'a>(&'a self, peers: &'a Arc<PeerManager>, cloud: &'a Arc<CloudRuntime>) -> BoxFuture<'a, Result<(crate::download_types::DownloadBackend, Arc<dyn crate::traits::TransferQueueAdapter>), AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            if !self.capabilities().download { return Err(unsupported()); }
            let files = self.files.lock().await.clone();
            Ok((crate::download_types::DownloadBackend::Official { connection_id: self.connection_id.clone() },
                Arc::new(crate::download_types::OfficialDownloadAdapter {
                    parents: self.parents.lock().await.clone(),
                    content_types: tokio::sync::Mutex::new(self.content_types.lock().await.clone()),
                    #[cfg(test)]
                    pages: None,
                    runtime: cloud.clone(), connection: self.connection_id.clone(), revision: self.revision,
                    files: tokio::sync::Mutex::new(files), directories: self.directories.lock().await.clone(), names: self.names.lock().await.clone(),
                }) as Arc<dyn crate::traits::TransferQueueAdapter>))
        })
    }
}
