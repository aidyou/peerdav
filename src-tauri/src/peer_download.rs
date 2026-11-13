use super::*;
use crate::{download_types::*, traits::TransferQueueAdapter};
use futures_util::future::BoxFuture;

impl PeerManager {
    pub(crate) fn download_scope(&self, peer: &str) -> Result<String, AppError> {
        self.vault.require_interface_unlocked().map_err(AppError::from)?;
        self.upload_scope(peer)
    }
}
impl TransferQueueAdapter for PeerDownloadAdapter {
    fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError> {
        self.manager.vault.transfer_cancellation().map_err(AppError::from)
    }
    fn report_error(&self, error: &AppError) {
        self.manager.events.error(error);
    }
    fn validate(&self) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async {
            self.manager
                .vault
                .require_interface_unlocked()
                .map_err(AppError::from)?;
            if self.manager.download_scope(&self.peer)? != self.scope {
                return Err(crate::local_upload::invalid());
            }
            Ok(())
        })
    }
    fn plan(
        &self,
        selection: DownloadSelection,
    ) -> BoxFuture<'_, Result<DownloadSpool, AppError>> {
        Box::pin(async move {
            self.validate().await?;
            let mut result = DownloadSpool::new()?;
            let (paths, required_kind) = match selection {
                DownloadSelection::Single { path } => (vec![path], Some(EntryKind::File)),
                DownloadSelection::Files { paths } => (paths, None),
                DownloadSelection::Folder { path } => (vec![path], Some(EntryKind::Directory)),
            };
            let backend = self.manager.read_backend(&self.peer, &self.share).map_err(AppError::from)?;
            let mut pending = Vec::new();
            let mut targets = std::collections::HashSet::new();
            for path in paths {
                self.validate().await?;
                let (path, _, name) = remote_file_target(&self.share, &path).map_err(AppError::from)?;
                let entry = backend.stat(read_context(wire::LISTING_BODY_LIMIT, NonZeroUsize::MIN), backend.scope().entry(path.clone()))
                    .await.map_err(|source| AppError::from(DomainError::PeerFile { operation: "inspect download selection", source }))?;
                if entry.id.opaque() != path || required_kind.as_ref().is_some_and(|kind| *kind != entry.kind) {
                    return Err(crate::local_upload::invalid());
                }
                match entry.kind {
                    EntryKind::Directory => pending.push((path, name, 0usize)),
                    EntryKind::File => {
                        if !targets.insert(name.clone()) { return Err(crate::local_upload::invalid()); }
                        result.push(crate::download::item(path, name, entry.size.unwrap_or(0))?)?;
                    }
                }
            }
            let mut visited = std::collections::HashSet::new();
            while let Some((path, relative, depth)) = pending.pop() {
                self.validate().await?;
                if depth > MAX_DOWNLOAD_DEPTH || !visited.insert(path.clone()) {
                    return Err(crate::local_upload::invalid());
                }
                let listing = self.manager.list_dir(&self.peer, &self.share, &path).await.map_err(AppError::from)?;
                if listing.truncated { return Err(crate::local_upload::invalid()); }
                for entry in listing.entries {
                    let name = sanitize_file_name(&entry.name).map_err(AppError::from)?;
                    let expected = format!("{}/{}", path.trim_end_matches('/'), name);
                    if entry.path != expected { return Err(crate::local_upload::invalid()); }
                    let target = format!("{relative}/{name}");
                    if entry.is_dir { pending.push((entry.path, target, depth + 1)); }
                    else {
                        if !targets.insert(target.clone()) { return Err(crate::local_upload::invalid()); }
                        result.push(crate::download::item(entry.path, target, entry.size)?)?;
                    }
                }
            }
            Ok(result)
        })
    }
    fn execute<'a>(
        &'a self,
        item: &'a DownloadItem,
        root: &'a crate::cloud::PhysicalRoot,
        overwrite: bool,
        cancel: tokio_util::sync::CancellationToken,
        progress: Option<crate::peer::TransferProgressCallback>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            self.validate().await?;
            let identity = root
                .relative_target_identity(&item.relative_path)
                .map_err(|e| AppError::from(DomainError::from(e)))?;
            let (_, client) = self
                .manager
                .peer_client(&self.peer)
                .map_err(AppError::from)?;
            let (display, remote, _) =
                remote_file_target(&self.share, &item.remote_path).map_err(AppError::from)?;
            // Preserve resumable parts and ETag checks in a private backend-owned directory.
            tokio::fs::create_dir_all(&self.manager.temp_root)
                .await
                .map_err(crate::local_upload::io)?;
            let key = hex::encode(Sha256::digest(
                format!("{}\n{}\n{}", self.scope, self.share, remote).as_bytes(),
            ));
            let destination = self.manager.temp_root.join(format!("queue-{key}"));
            let _payload_guard = self.manager.download_payload_lock.lock().await;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::from(DomainError::TransferCanceled)),
                r = self.manager.resumable_download(
                    &client, &remote, &display, &destination, ConflictPolicy::Overwrite, progress,
                ) => { r.map_err(AppError::from)?; },
            }
            self.validate().await?;
            if overwrite
                && root
                    .relative_target_identity(&item.relative_path)
                    .map_err(|e| AppError::from(DomainError::from(e)))?
                    != identity
            {
                return Err(AppError::from(DomainError::TransferConflict {
                    path: item.relative_path.clone(),
                }));
            }
            let install_root = root.try_clone().map_err(|e| AppError::from(DomainError::from(e)))?;
            let relative = item.relative_path.clone();
            let source = destination.clone();
            tokio::task::spawn_blocking(move || install_root.install_download(&relative, &source, overwrite, &cancel))
                .await.map_err(|e| AppError::from(DomainError::Internal { detail: e.to_string() }))?
                .map_err(|e| AppError::from(DomainError::from(e)))?;
            tokio::fs::remove_file(destination)
                .await
                .map_err(crate::local_upload::io)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn stat_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/").trim_end_matches('/');
                let paths = if path.ends_with("/tree") && request.to_lowercase().contains("depth: 1") { vec![path.to_owned(), format!("{path}/nested.txt")] } else { vec![path.to_owned()] };
                let mut body = String::from("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">");
                for path in paths {
                    let kind = if path.ends_with("/tree") { "<d:collection/>" } else { "" };
                    body.push_str(&format!("<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype><d:getcontentlength>1</d:getcontentlength><d:getcontenttype>text/plain</d:getcontenttype><d:getlastmodified>Sun, 01 Nov 2026 00:00:00 GMT</d:getlastmodified><d:getetag>\"v1\"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                }
                body.push_str("</d:multistatus>");
                socket.write_all(format!("HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        (url, handle)
    }
    #[tokio::test]
    async fn download_fast_lock_unlock_invalidates_selected_and_conflict_reveal() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::open(&dir.path().join("vault")).unwrap());
        vault.initialize(dir.path().to_str().unwrap(), "owner", "test password 2026").unwrap();
        vault.admin_login("owner", "test password 2026").unwrap();
        let (stat_url, stat_task) = stat_server().await;
        vault.save_peer_code("abcd", &stat_url, "secret").unwrap();
        vault.save_peer_record(&crate::types::PeerRecord { id: "abcd".into(), label: "Peer".into(), url: stat_url, authorization_id: None, machine_id: None, created_at: 0, last_connected_at: None }).unwrap();
        let manager = Arc::new(PeerManager::new(vault.clone(), dir.path().join("temp")).unwrap());
        std::fs::write(dir.path().join("a"), "old").unwrap();
        let queues = Arc::new(DownloadQueues::default());
        for start in [false, true] {
            let adapter = Arc::new(PeerDownloadAdapter { manager: manager.clone(), peer: "abcd".into(), share: "share".into(), scope: manager.download_scope("abcd").unwrap() });
            let q = queues.select(DownloadBackend::Peer { peer_id: "abcd".into(), share_id: "share".into() }, DownloadSelection::Single { path: "/a".into() }, crate::cloud::PhysicalRoot::open(dir.path()).unwrap(), adapter).await.unwrap();
            if start {
                queues.start(&q.queue_id).await.unwrap();
                tokio::time::timeout(Duration::from_secs(3), async {
                    while queues.status(&q.queue_id).await.unwrap().phase != "conflict" { tokio::task::yield_now().await; }
                }).await.unwrap();
            }
            vault.lock_interface().unwrap();
            vault.unlock_interface("test password 2026").unwrap();
            assert!(queues.reveal_root(&q.queue_id).await.is_err());
            if start {
                tokio::time::timeout(Duration::from_secs(3), queues.cancel(&q.queue_id)).await.unwrap().unwrap();
                assert_ne!(queues.status(&q.queue_id).await.unwrap().phase, "completed");
            } else { assert!(queues.start(&q.queue_id).await.is_err()); }
            queues.release(&q.queue_id).await.unwrap();
        }
        stat_task.abort();
        let _joined = stat_task.await;
    }

    #[tokio::test]
    async fn shared_download_peer_adapter_skip_has_no_head_get_and_scope_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::open(&dir.path().join("vault")).unwrap());
        vault
            .initialize(dir.path().to_str().unwrap(), "owner", "test password 2026")
            .unwrap();
        let (stat_url, stat_task) = stat_server().await;
        vault
            .save_peer_code("abcd", &stat_url, "secret")
            .unwrap();
        vault
            .save_peer_record(&crate::types::PeerRecord {
                id: "abcd".into(),
                label: "Peer".into(),
                url: stat_url,
                authorization_id: None,
                machine_id: None,
                created_at: 0,
                last_connected_at: None,
            })
            .unwrap();
        let manager = Arc::new(PeerManager::new(vault, dir.path().join("temp")).unwrap());
        let adapter = Arc::new(PeerDownloadAdapter {
            scope: manager.download_scope("abcd").unwrap(),
            manager: manager.clone(),
            peer: "abcd".into(),
            share: "share".into(),
        });
        let mixed = adapter.plan(DownloadSelection::Files { paths: vec!["/a".into(), "/tree".into()] }).await.unwrap().page(0, 100).unwrap();
        assert_eq!(mixed.iter().map(|i| i.relative_path.as_str()).collect::<Vec<_>>(), vec!["a", "tree/nested.txt"]);
        assert!(adapter.plan(DownloadSelection::Single { path: "/tree".into() }).await.is_err());
        assert!(adapter.plan(DownloadSelection::Files { paths: vec!["/a".into(), "/a".into()] }).await.is_err());
        std::fs::write(dir.path().join("a"), "old").unwrap();
        std::fs::write(dir.path().join("b"), "old").unwrap();
        let queues = Arc::new(DownloadQueues::default());
        let baseline = std::fs::read_dir(dir.path().join("temp")).unwrap().count();
        for _ in 0..2 {
            let q = queues
                .select(
                    DownloadBackend::Peer {
                        peer_id: "abcd".into(),
                        share_id: "share".into(),
                    },
                    DownloadSelection::Files {
                        paths: vec!["/a".into(), "/b".into()],
                    },
                    crate::cloud::PhysicalRoot::open(dir.path()).unwrap(),
                    adapter.clone(),
                )
                .await
                .unwrap();
            queues.start(&q.queue_id).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let s = queues.status(&q.queue_id).await.unwrap();
                    if let Some(c) = s.conflict {
                        queues
                            .resolve(&q.queue_id, &c.conflict_id, ConflictResolution::Skip, true)
                            .await
                            .unwrap();
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                loop {
                    let s = queues.status(&q.queue_id).await.unwrap();
                    if s.phase == "completed" {
                        assert_eq!(s.skipped_files, 2);
                        break;
                    }
                    assert_ne!(s.phase, "failed");
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            queues.release(&q.queue_id).await.unwrap();
        }
        assert_eq!(
            std::fs::read_dir(dir.path().join("temp")).unwrap().count(),
            baseline
        );
        // Real range transport through the same required adapter and queue engine.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        manager
            .vault
            .save_peer_record(&crate::types::PeerRecord {
                id: "abcd".into(),
                label: "Peer".into(),
                url: format!("http://{address}"),
                authorization_id: None,
                machine_id: None,
                created_at: 0,
                last_connected_at: None,
            })
            .unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for _ in 0..6 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 8192];
                let n = socket.read(&mut request).await.unwrap();
                if request[..n].starts_with(b"PROPFIND ") {
                    let request = String::from_utf8_lossy(&request[..n]);
                    let path = request.split_whitespace().nth(1).unwrap().trim_end_matches('/');
                    let body = format!("<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1</d:getcontentlength><d:getcontenttype>text/plain</d:getcontenttype><d:getlastmodified>Sun, 01 Nov 2026 00:00:00 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>");
                    socket.write_all(format!("HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    continue;
                }
                let head = request[..n].starts_with(b"HEAD ");
                let response = if head {
                    "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/1\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nx"
                };
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let fresh = Arc::new(PeerDownloadAdapter {
            scope: manager.download_scope("abcd").unwrap(),
            manager: manager.clone(),
            peer: "abcd".into(),
            share: "share".into(),
        });
        let q = queues
            .select(
                DownloadBackend::Peer {
                    peer_id: "abcd".into(),
                    share_id: "share".into(),
                },
                DownloadSelection::Files {
                    paths: vec!["/a".into(), "/b".into()],
                },
                crate::cloud::PhysicalRoot::open(dir.path()).unwrap(),
                fresh,
            )
            .await
            .unwrap();
        queues.start(&q.queue_id).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let s = queues.status(&q.queue_id).await.unwrap();
                if let Some(c) = s.conflict {
                    queues
                        .resolve(
                            &q.queue_id,
                            &c.conflict_id,
                            ConflictResolution::Overwrite,
                            true,
                        )
                        .await
                        .unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
            loop {
                let s = queues.status(&q.queue_id).await.unwrap();
                if s.phase == "completed" || s.phase == "failed" {
                    return s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        server.abort();
        assert!(server.await.is_ok() || result.is_ok());
        let end = result.unwrap();
        assert_eq!(end.phase, "completed", "{:?}", end.error);
        assert_eq!(end.completed_files, 2);
        assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), b"x");
        queues.release(&q.queue_id).await.unwrap();
        manager
            .upload_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        assert!(adapter.validate().await.is_err());
        stat_task.abort();
        let _joined = stat_task.await;
    }
}
