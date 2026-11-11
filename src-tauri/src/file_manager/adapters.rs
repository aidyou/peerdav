use super::{
    traits::BrowserBackend,
    types::{CloudBrowserBackend, FileEntryDto, FilePageDto, PeerBrowserBackend, PeerBrowserError},
};
use crate::{
    AppError, PeerListing, PeerManager, PreviewPayload,
    cloud::{object_runtime_types::ObjectPageDto, runtime::CloudRuntime},
};
use futures_util::future::BoxFuture;

pub async fn cloud_list(
    runtime: &CloudRuntime,
    connection: &str,
    prefix: &str,
    cursor: Option<&str>,
) -> Result<ObjectPageDto, AppError> {
    runtime.object_list(connection, prefix, cursor).await
}
pub async fn cloud_preview(
    runtime: &CloudRuntime,
    connection: &str,
    key: &str,
) -> Result<PreviewPayload, AppError> {
    runtime.object_preview(connection, key).await
}
pub async fn peer_list(
    peers: &PeerManager,
    peer: &str,
    share: &str,
    path: &str,
) -> Result<PeerListing, PeerBrowserError> {
    peers
        .list_dir(peer, share, path)
        .await
        .map_err(PeerBrowserError::from)
}
pub async fn peer_preview(
    peers: &PeerManager,
    peer: &str,
    share: &str,
    path: &str,
) -> Result<PreviewPayload, PeerBrowserError> {
    peers
        .preview(peer, share, path)
        .await
        .map_err(PeerBrowserError::from)
}

impl BrowserBackend for PeerBrowserBackend {
    fn prepare<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime) -> BoxFuture<'a, Result<(), AppError>> {
        self.admit(peers, cloud)
    }
    fn edit_read<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str) -> BoxFuture<'a, Result<super::types::EditContent, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let content = peers.read_editor_content(&self.peer_id, &self.share_id, id).await.map_err(AppError::from)?;
            self.admit(peers, cloud).await?;
            Ok(content)
        })
    }
    fn edit_write<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str, revision: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<String, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            peers.write_editor_content(&self.peer_id, &self.share_id, id, revision, bytes).await.map_err(AppError::from)
        })
    }
    fn online_preview<'a>(&'a self, _: &'a PeerManager, _: &'a CloudRuntime, _: &'a str) -> BoxFuture<'a, Result<String, AppError>> {
        Box::pin(async { Err(super::service::unavailable()) })
    }
    fn upload_resolve<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, conflict_id: &'a str, decision: crate::cloud::directory_types::DirectoryConflictPolicy, apply_to_remaining: bool) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move { self.admit(peers, cloud).await?; crate::traits::UploadQueueAdapter::resolve(&crate::peer_upload_types::PeerUploadAdapter { manager: peers, peer: &self.peer_id }, id, conflict_id, decision, apply_to_remaining).await })
    }
    fn upload_action<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, action: super::types::UploadAction) -> BoxFuture<'a, Result<super::types::FileUploadDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let adapter = crate::peer_upload_types::PeerUploadAdapter { manager: peers, peer: &self.peer_id };
            crate::local_upload::queue_action(&adapter, id, action).await.map(super::types::FileUploadDto::Peer)
        })
    }
    fn upload_page<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> BoxFuture<'a, Result<crate::local_upload_types::DirectoryPageDto, AppError>> {
        Box::pin(async move { self.admit(peers, cloud).await?; crate::traits::UploadQueueAdapter::page(&crate::peer_upload_types::PeerUploadAdapter { manager: peers, peer: &self.peer_id }, id, directory_token, cursor, limit).await })
    }
    fn create_dir<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, name: &'a str) -> BoxFuture<'a, Result<FileEntryDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let shares = peers.shares(&self.peer_id).await.map_err(AppError::from)?;
            if !shares.iter().any(|share| share.id == self.share_id && share.permissions.can_write) { return Err(crate::DomainError::PeerAuthorizationRejected.into()); }
            let target = format!("{}/{name}", path.trim_end_matches('/'));
            peers.create_directory(&self.peer_id, &self.share_id, &target).await.map_err(AppError::from)?;
            self.admit(peers, cloud).await?;
            Ok(super::service::directory_entry(target, path.into(), name.into()))
        })
    }
    fn release_upload<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
        id: &'a str,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(peers.release_upload_batch(&self.peer_id, id))
    }
    fn select_upload<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
        path: String,
        paths: Vec<std::path::PathBuf>,
        mode: super::types::UploadMode,
        options: crate::types::IgnoreOptions,
    ) -> BoxFuture<'a, Result<super::types::FileUploadDto, AppError>> {
        Box::pin(async move {
            if peers.download_scope(&self.peer_id)? != self.authorization {
                return Err(super::service::invalid());
            }
            let scope = peers.upload_scope(&self.peer_id)?;
            let shares = peers.shares(&self.peer_id).await.map_err(AppError::from)?;
            if !shares
                .iter()
                .any(|share| share.id == self.share_id && share.permissions.can_write)
            {
                return Err(AppError::from(
                    crate::DomainError::PeerAuthorizationRejected,
                ));
            }
            peers
                .select_upload_batch(
                    self.peer_id.clone(),
                    self.share_id.clone(),
                    path,
                    paths,
                    match mode { super::types::UploadMode::Single => "single", super::types::UploadMode::Files => "files", super::types::UploadMode::Folder => "folder" },
                    options,
                    scope,
                )
                .await
                .map(super::types::FileUploadDto::Peer)
        })
    }
    fn download<'a>(
        &'a self,
        peers: &'a std::sync::Arc<PeerManager>,
        _: &'a std::sync::Arc<CloudRuntime>,
    ) -> BoxFuture<
        'a,
        Result<
            (
                crate::download_types::DownloadBackend,
                std::sync::Arc<dyn crate::traits::TransferQueueAdapter>,
            ),
            AppError,
        >,
    > {
        Box::pin(async move {
            if peers.download_scope(&self.peer_id)? != self.authorization {
                return Err(super::service::invalid());
            }
            Ok((
                crate::download_types::DownloadBackend::Peer {
                    peer_id: self.peer_id.clone(),
                    share_id: self.share_id.clone(),
                },
                std::sync::Arc::new(crate::download_types::PeerDownloadAdapter {
                    manager: std::sync::Arc::clone(peers),
                    peer: self.peer_id.clone(),
                    share: self.share_id.clone(),
                    scope: self.authorization.clone(),
                }) as std::sync::Arc<dyn crate::traits::TransferQueueAdapter>,
            ))
        })
    }
    fn capabilities(&self) -> super::types::FileCapabilities {
        super::types::FileCapabilities {
            delete_confirmation: super::types::DeleteConfirmation::Entry,
            list: true,
            preview: true,
            upload: true,
            download: true,
            download_directory: true,
            delete: true,
            create_dir: true,
            rename: false,
            online_preview: false,
            external_edit: true,
        }
    }
    fn admit<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            if peers.download_scope(&self.peer_id)? != self.authorization {
                return Err(super::service::invalid());
            }
            Ok(())
        })
    }
    fn list<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
        path: &'a str,
        next: Option<&'a str>,
    ) -> BoxFuture<'a, Result<FilePageDto, AppError>> {
        Box::pin(async move {
            if next.is_some() {
                return Err(super::service::invalid());
            }
            let listing = peers
                .list_dir(&self.peer_id, &self.share_id, path)
                .await
                .map_err(AppError::from)?;
            Ok(FilePageDto {
                breadcrumbs: super::service::path_breadcrumbs(&listing.path),
                id: listing.path,
                entries: listing
                    .entries
                    .into_iter()
                    .map(|entry| FileEntryDto {
                        id: entry.path,
                        parent_id: path.into(),
                        name: entry.name,
                        is_dir: entry.is_dir,
                        size: entry.size,
                        modified: entry.modified,
                        content_type: entry.content_type,
                        online_preview: false,
                        protected: false,
                    })
                    .collect(),
                next: None,
                complete: !listing.truncated,
                protected: false,
            })
        })
    }
    fn delete_plan<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
        path: &'a str,
    ) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeletePlanDto, AppError>>
    {
        Box::pin(async move {
            if path.is_empty() || path == "/" {
                return Err(super::service::invalid());
            }
            let parent = path
                .rsplit_once('/')
                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                .ok_or_else(super::service::invalid)?;
            let listing = peers
                .list_dir(&self.peer_id, &self.share_id, parent)
                .await
                .map_err(AppError::from)?;
            let entry = listing
                .entries
                .into_iter()
                .find(|entry| entry.path == path)
                .ok_or_else(super::service::invalid)?;
            let expires = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| super::service::invalid())?
                .as_secs()
                .saturating_add(300);
            Ok(crate::cloud::object_runtime_types::ObjectDeletePlanDto {
                plan_id: String::new(),
                expires_at_unix: expires,
                object_count: 1,
                connection_revision: 0,
                entries: vec![crate::cloud::object_runtime_types::ObjectEntryDto {
                    key: entry.path,
                    size: entry.size,
                    etag: None,
                    protected: false,
                }],
            })
        })
    }
    fn delete_execute<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
        target: &'a str,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeleteResultDto, AppError>>
    {
        Box::pin(async move {
            peers
                .delete_entry(&self.peer_id, &self.share_id, target)
                .await
                .map_err(AppError::from)?;
            Ok(crate::cloud::object_runtime_types::ObjectDeleteResultDto {
                deleted: vec![target.to_owned()],
                failed: Vec::new(),
            })
        })
    }
    fn preview<'a>(
        &'a self,
        peers: &'a PeerManager,
        _: &'a CloudRuntime,
        path: &'a str,
    ) -> BoxFuture<'a, Result<PreviewPayload, AppError>> {
        Box::pin(async move {
            peers
                .preview(&self.peer_id, &self.share_id, path)
                .await
                .map_err(AppError::from)
        })
    }
}

impl BrowserBackend for CloudBrowserBackend {
    fn supports_move(&self) -> bool { true }
    fn move_entry<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str, destination: &'a str) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            cloud.object_move(&self.connection_id, self.revision, &self.identity, id, destination).await
        })
    }
    fn prepare<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            cloud.object_prepare(&self.connection_id).await?;
            self.admit(peers, cloud).await
        })
    }
    fn online_preview<'a>(&'a self, _: &'a PeerManager, _: &'a CloudRuntime, _: &'a str) -> BoxFuture<'a, Result<String, AppError>> {
        Box::pin(async { Err(super::service::unavailable()) })
    }
    fn upload_resolve<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, conflict_id: &'a str, decision: crate::cloud::directory_types::DirectoryConflictPolicy, apply_to_remaining: bool) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move { self.admit(peers, cloud).await?; crate::traits::UploadQueueAdapter::resolve(&crate::cloud::directory_types::S3UploadAdapter { runtime: cloud, connection: &self.connection_id }, id, conflict_id, decision, apply_to_remaining).await })
    }
    fn upload_action<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, action: super::types::UploadAction) -> BoxFuture<'a, Result<super::types::FileUploadDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let adapter = crate::cloud::directory_types::S3UploadAdapter { runtime: cloud, connection: &self.connection_id };
            crate::local_upload::queue_action(&adapter, id, action).await.map(super::types::FileUploadDto::Cloud)
        })
    }
    fn upload_page<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> BoxFuture<'a, Result<crate::local_upload_types::DirectoryPageDto, AppError>> {
        Box::pin(async move { self.admit(peers, cloud).await?; crate::traits::UploadQueueAdapter::page(&crate::cloud::directory_types::S3UploadAdapter { runtime: cloud, connection: &self.connection_id }, id, directory_token, cursor, limit).await })
    }
    fn create_dir<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, name: &'a str) -> BoxFuture<'a, Result<FileEntryDto, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            let session = cloud.object_session(&self.connection_id).await?;
            let prefix = format!("{}{name}/", if path.is_empty() { String::new() } else { format!("{}/", path.trim_end_matches('/')) });
            session.service.create_directory(&prefix).await.map_err(|error| AppError::from(crate::DomainError::from(error)))?;
            self.admit(peers, cloud).await?;
            Ok(super::service::directory_entry(prefix, path.into(), name.into()))
        })
    }
    fn edit_read<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str) -> BoxFuture<'a, Result<super::types::EditContent, AppError>> {
        Box::pin(async move {
            use futures_util::StreamExt;
            self.admit(peers, cloud).await?;
            let _permit = cloud.network.read().await;
            let session = cloud.object_session(&self.connection_id).await?;
            session.service.preflight_upload(path).map_err(|e| AppError::from(crate::DomainError::from(e)))?;
            let meta = session.store.stat(path).await.map_err(|e| AppError::from(crate::DomainError::from(e)))?;
            if meta.size > super::types::MAX_EDIT_BYTES as u64 { return Err(super::service::unavailable()); }
            let revision = meta.etag.ok_or_else(super::service::unavailable)?;
            let mut body = session.store.get(path, None).await.map_err(|e| AppError::from(crate::DomainError::from(e)))?;
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|e| AppError::from(crate::DomainError::from(e)))?;
                if bytes.len().saturating_add(chunk.len()) > super::types::MAX_EDIT_BYTES { return Err(super::service::unavailable()); }
                bytes.extend_from_slice(&chunk);
            }
            let after = session.store.stat(path).await.map_err(|e| AppError::from(crate::DomainError::from(e)))?;
            if after.etag.as_deref() != Some(&revision) { return Err(super::service::invalid()); }
            drop(_permit);
            self.admit(peers, cloud).await?;
            Ok(super::types::EditContent { name: path.rsplit('/').next().ok_or_else(super::service::invalid)?.into(), bytes, revision })
        })
    }
    fn edit_write<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, path: &'a str, revision: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<String, AppError>> {
        Box::pin(async move {
            self.admit(peers, cloud).await?;
            if bytes.len() > super::types::MAX_EDIT_BYTES { return Err(super::service::unavailable()); }
            let _permit = cloud.network.read().await;
            let session = cloud.object_session(&self.connection_id).await?;
            if session.revision != self.revision || session.identity != self.identity { return Err(super::service::invalid()); }
            let result = session.service.replace_edit(path, bytes, revision).await.map_err(|e| AppError::from(crate::DomainError::from(e)))?;
            Ok(result.etag.unwrap_or_default())
        })
    }
    fn release_upload<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
        id: &'a str,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(cloud.object_release_upload_directory(&self.connection_id, id))
    }
    fn select_upload<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
        path: String,
        paths: Vec<std::path::PathBuf>,
        mode: super::types::UploadMode,
        _: crate::types::IgnoreOptions,
    ) -> BoxFuture<'a, Result<super::types::FileUploadDto, AppError>> {
        Box::pin(async move {
            if paths.is_empty()
                || paths.len() > 256
                || (!matches!(mode, super::types::UploadMode::Files) && paths.len() != 1)
            {
                return Err(crate::local_upload::invalid());
            }
            let result = if matches!(mode, super::types::UploadMode::Folder) {
                cloud
                    .object_select_upload_directory_bound(
                        &self.connection_id,
                        self.revision,
                        path,
                        &paths[0],
                        Some(&self.identity),
                    )
                    .await?
            } else {
                cloud
                    .object_select_upload_files_bound(
                        &self.connection_id,
                        self.revision,
                        path,
                        paths,
                        Some(&self.identity),
                    )
                    .await?
            };
            Ok(super::types::FileUploadDto::Cloud(result))
        })
    }
    fn download<'a>(
        &'a self,
        _: &'a std::sync::Arc<PeerManager>,
        cloud: &'a std::sync::Arc<CloudRuntime>,
    ) -> BoxFuture<
        'a,
        Result<
            (
                crate::download_types::DownloadBackend,
                std::sync::Arc<dyn crate::traits::TransferQueueAdapter>,
            ),
            AppError,
        >,
    > {
        Box::pin(async move {
            let session = cloud.object_session(&self.connection_id).await?;
            if session.revision != self.revision || session.identity != self.identity {
                return Err(super::service::invalid());
            }
            Ok((
                crate::download_types::DownloadBackend::S3 {
                    connection_id: self.connection_id.clone(),
                },
                std::sync::Arc::new(crate::download_types::S3DownloadAdapter {
                    runtime: std::sync::Arc::clone(cloud),
                    connection: self.connection_id.clone(),
                    session,
                }) as std::sync::Arc<dyn crate::traits::TransferQueueAdapter>,
            ))
        })
    }
    fn capabilities(&self) -> super::types::FileCapabilities {
        super::types::FileCapabilities {
            delete_confirmation: super::types::DeleteConfirmation::Snapshot,
            list: true,
            preview: true,
            upload: true,
            download: true,
            download_directory: true,
            delete: true,
            create_dir: true,
            rename: false,
            online_preview: false,
            external_edit: true,
        }
    }
    fn admit<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let (revision, identity) = cloud.object_selection_scope(&self.connection_id).await?;
            if revision != self.revision || identity != self.identity {
                return Err(super::service::invalid());
            }
            Ok(())
        })
    }
    fn list<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
        path: &'a str,
        next: Option<&'a str>,
    ) -> BoxFuture<'a, Result<FilePageDto, AppError>> {
        Box::pin(async move {
            let page = cloud
                .object_list_bound(
                    &self.connection_id,
                    path,
                    next,
                    Some((self.revision, &self.identity)),
                )
                .await?;
            let mut entries = std::collections::BTreeMap::<String, FileEntryDto>::new();
            for object in page.objects {
                let relative = object
                    .key
                    .strip_prefix(path)
                    .ok_or_else(super::service::invalid)?;
                if relative.is_empty() {
                    continue;
                }
                let (key, name, is_dir) = match relative.find('/') {
                    Some(index) => (
                        format!("{}{}", path, &relative[..=index]),
                        relative[..index].to_owned(),
                        true,
                    ),
                    None => (object.key.clone(), relative.to_owned(), false),
                };
                let entry = entries.entry(key.clone()).or_insert(FileEntryDto {
                    id: key,
                    parent_id: path.into(),
                    name,
                    is_dir,
                    size: if is_dir { 0 } else { object.size },
                    modified: None,
                    content_type: None,
                    online_preview: false,
                    protected: false,
                });
                entry.protected |= object.protected;
            }
            let mut entries: Vec<_> = entries.into_values().collect();
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(FilePageDto {
                breadcrumbs: super::service::path_breadcrumbs(&page.prefix),
                id: page.prefix,
                entries,
                next: page.next,
                complete: page.complete,
                protected: page.prefix_protected,
            })
        })
    }
    fn delete_plan<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
        path: &'a str,
    ) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeletePlanDto, AppError>>
    {
        Box::pin(async move {
            cloud
                .object_delete_plan_bound(
                    &self.connection_id,
                    path,
                    Some((self.revision, &self.identity)),
                )
                .await
        })
    }
    fn delete_execute<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
        _: &'a str,
        plan: Option<&'a str>,
    ) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeleteResultDto, AppError>>
    {
        Box::pin(async move {
            cloud
                .object_delete_execute_bound(
                    &self.connection_id,
                    plan.ok_or_else(super::service::invalid)?,
                    true,
                    Some((self.revision, &self.identity)),
                )
                .await
        })
    }
    fn preview<'a>(
        &'a self,
        _: &'a PeerManager,
        cloud: &'a CloudRuntime,
        path: &'a str,
    ) -> BoxFuture<'a, Result<PreviewPayload, AppError>> {
        Box::pin(cloud.object_preview_bound(
            &self.connection_id,
            path,
            Some((self.revision, &self.identity)),
        ))
    }
}

impl From<PeerBrowserError> for AppError {
    fn from(error: PeerBrowserError) -> Self {
        match error {
            PeerBrowserError::Peer(source) => source.into(),
        }
    }
}
