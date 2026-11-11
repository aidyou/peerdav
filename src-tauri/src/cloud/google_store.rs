#[cfg(test)]
#[path = "google_store_tests.rs"]
mod tests;

use super::{official_types::GoogleObjectStore, traits::ObjectStore, types::*};
use futures_util::{future::BoxFuture, StreamExt};
use std::sync::Arc;

fn app_error(error: crate::AppError) -> CloudError {
    CloudError::Service { status: 0, detail: error.detail.unwrap_or(error.message) }
}

fn error(error: impl std::error::Error + Send + Sync + 'static) -> CloudError {
    CloudError::Storage { source: object_store::Error::Generic { store: "Google Drive", source: Box::new(error) } }
}
impl GoogleObjectStore {
    async fn api(&self) -> Result<super::official_api_types::OfficialApi, CloudError> {
        let runtime = self.runtime.upgrade().ok_or(CloudError::UploadCancelled)?;
        runtime.admit().map_err(app_error)?;
        let snapshot = runtime.config.snapshot().map_err(error)?;
        if !snapshot.official_connections.iter().any(|c| c.id == self.connection && c.revision == self.revision && c.root_id == self.root) { return Err(CloudError::PrefixViolation); }
        runtime.official_api(&self.connection).await.map(|(_, api)| api).map_err(app_error)
    }
    async fn children(&self, parent: &str) -> Result<Vec<super::official_api_types::OfficialEntry>, CloudError> {
        let mut result = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self.api().await?.list(parent, cursor.as_deref()).await.map_err(error)?;
            if result.len().saturating_add(page.entries.len()) > 100_000 || seen.len() > 1000 { return Err(CloudError::UploadBudgetExceeded); }
            result.extend(page.entries);
            match page.next {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err(CloudError::UploadBudgetExceeded),
                None if page.complete => return Ok(result),
                None => return Err(CloudError::Unsupported),
            }
        }
    }
    async fn resolve(&self, key: &str) -> Result<super::official_api_types::OfficialEntry, CloudError> {
        super::object_service::valid_relative_path(key).map_err(error)?;
        let mut parent = self.root.clone();
        let parts: Vec<_> = key.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            let mut matches = self.children(&parent).await?.into_iter().filter(|e| e.name == *part);
            let entry = matches.next().ok_or(CloudError::NotFound)?;
            if matches.next().is_some() { return Err(CloudError::Precondition); }
            if index == parts.len() - 1 { return Ok(entry); }
            if !entry.is_dir { return Err(CloudError::PrefixViolation); }
            parent = entry.id;
        }
        Err(CloudError::PrefixViolation)
    }
    async fn parent(&self, key: &str) -> Result<(String, String), CloudError> {
        super::object_service::valid_relative_path(key).map_err(error)?;
        let mut parts: Vec<_> = key.split('/').collect();
        let name = parts.pop().ok_or(CloudError::PrefixViolation)?.to_owned();
        let mut parent = self.root.clone();
        for part in parts {
            let mut matches = self.children(&parent).await?.into_iter().filter(|e| e.name == part);
            let entry = matches.next();
            if matches.next().is_some() { return Err(CloudError::Precondition); }
            parent = match entry {
                Some(entry) if entry.is_dir => entry.id,
                Some(_) => return Err(CloudError::PrefixViolation),
                None => self.api().await?.create_folder_id(&parent, part).await.map_err(error)?,
            };
        }
        Ok((parent, name))
    }
}
impl ObjectStore for GoogleObjectStore {
    fn capabilities(&self) -> StorageCapabilities { StorageCapabilities::default() }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        futures_util::stream::try_unfold((false, Vec::new()), move |(loaded, mut buffered): (bool, Vec<ObjectMetadata>)| async move {
            if let Some(item) = buffered.pop() { return Ok(Some((item, (loaded, buffered)))); }
            if loaded { return Ok(None); }
            let page = self.list_page(prefix, None).await?;
            let mut objects = page.objects; objects.reverse();
            Ok(objects.pop().map(|item| (item, (true, objects))))
        }).boxed()
    }
    fn list_page<'a>(&'a self, prefix: &'a str, token: Option<&'a str>) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move {
            super::object_service::valid_prefix(prefix).map_err(error)?;
            if token.is_some() { return Err(CloudError::Unsupported); }
            let mut pending = vec![(self.root.clone(), String::new(), 0usize)];
            let mut objects = Vec::new();
            let mut visited = std::collections::HashSet::new();
            let mut observed = 0usize;
            while let Some((parent, base, depth)) = pending.pop() {
                if depth > 64 || !visited.insert(parent.clone()) || visited.len() > 1000 { return Err(CloudError::UploadBudgetExceeded); }
                let mut names = std::collections::HashSet::new();
                for entry in self.children(&parent).await? {
                    observed += 1;
                    if observed > 100_000 { return Err(CloudError::UploadBudgetExceeded); }
                    let key = format!("{base}{}", entry.name);
                    let relevant = key.starts_with(prefix) || (entry.is_dir && prefix.starts_with(&format!("{key}/")));
                    if !relevant { continue; }
                    if !names.insert(entry.name.clone()) { return Err(CloudError::Precondition); }
                    super::object_service::valid_relative_path(&entry.name).map_err(error)?;
                    if entry.name.contains('/') { return Err(CloudError::PrefixViolation); }
                    if entry.is_dir {
                        let dir = format!("{key}/");
                        if dir.starts_with(prefix) || prefix.starts_with(&dir) { pending.push((entry.id, dir, depth + 1)); }
                    } else if key.starts_with(prefix) {
                        objects.push(ObjectMetadata { key, size: entry.size, etag: entry.revision });
                    }
                    if objects.len() + pending.len() > 100_000 { return Err(CloudError::UploadBudgetExceeded); }
                }
            }
            objects.sort_by(|a, b| a.key.cmp(&b.key));
            if objects.windows(2).any(|pair| pair[0].key == pair[1].key) { return Err(CloudError::Precondition); }
            Ok(ObjectPage { objects, next_token: None })
        })
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move { let entry = self.resolve(key).await?; if entry.is_dir { return Err(CloudError::NotFound); } Ok(ObjectMetadata { key: key.into(), size: entry.size, etag: entry.revision }) })
    }
    fn get<'a>(&'a self, key: &'a str, range: Option<std::ops::Range<u64>>) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            if range.is_some() { return Err(CloudError::Unsupported); }
            let entry = self.resolve(key).await?;
            let response = self.api().await?.media(&entry.id).await.map_err(error)?;
            Ok(response.bytes_stream().map(|chunk| chunk.map_err(error)).boxed())
        })
    }
    fn put<'a>(&'a self, key: &'a str, mut body: ObjectStream, condition: PutCondition) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            if !key.starts_with("_peerdav_encrypt/") && !key.starts_with("_peerdav_mirror/") { return Err(CloudError::PrefixViolation); }
            if !matches!(condition, PutCondition::Any) { return Err(CloudError::Unsupported); }
            // Append objects are immutable: never simulate atomic replacement using HEAD+PUT.
            let existing = match self.stat(key).await { Ok(meta) => Some(meta), Err(CloudError::NotFound) => None, Err(e) => return Err(e) };
            let file = tempfile::tempfile().map_err(error)?;
            let mut file = tokio::fs::File::from_std(file);
            let mut size = 0u64;
            use tokio::io::AsyncWriteExt;
            while let Some(chunk) = body.next().await {
                let chunk = chunk?; size = size.checked_add(chunk.len() as u64).ok_or(CloudError::PayloadTooLarge)?;
                if size > crate::types::MAX_TRANSFER_BYTES { return Err(CloudError::PayloadTooLarge); }
                file.write_all(&chunk).await.map_err(error)?;
            }
            if let Some(meta) = existing {
                if key.starts_with("_peerdav_mirror/") {
                    use tokio::io::AsyncSeekExt;
                    file.rewind().await.map_err(error)?;
                    let entry = self.resolve(key).await?;
                    // Mirror's existing Any-write contract is explicitly unconditional,
                    // unlike immutable append objects and CAS repositories.
                    self.api().await?.replace_file_stream(&entry.id, file.into_std().await, size, None).await.map_err(error)?;
                    return self.stat(key).await;
                }
                if meta.size != size { return Err(CloudError::Precondition); }
                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                file.rewind().await.map_err(error)?;
                let mut remote = self.get(key, None).await?;
                let mut offset = 0u64;
                while let Some(chunk) = remote.next().await {
                    let chunk = chunk?;
                    offset = offset.checked_add(chunk.len() as u64).ok_or(CloudError::PayloadTooLarge)?;
                    if offset > size { return Err(CloudError::Precondition); }
                    let mut local = vec![0; chunk.len()];
                    file.read_exact(&mut local).await.map_err(error)?;
                    if local.as_slice() != chunk.as_ref() { return Err(CloudError::Precondition); }
                }
                if offset != size { return Err(CloudError::Precondition); }
                return Ok(meta);
            }
            use tokio::io::AsyncSeekExt;
            file.rewind().await.map_err(error)?;
            let (parent, name) = self.parent(key).await?;
            self.api().await?.upload_object(&parent, &name, file.into_std().await, size).await.map_err(error)?;
            self.stat(key).await
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            if !key.starts_with("_peerdav_mirror/") { return Err(CloudError::PrefixViolation); }
            let entry = self.resolve(key).await?;
            if entry.is_dir || entry.id == self.root { return Err(CloudError::PrefixViolation); }
            let version = entry.revision.ok_or(CloudError::Precondition)?;
            self.api().await?.delete_item(&entry.id, &format!("google-version:{version}")).await.map_err(error)
        })
    }
}

impl super::runtime::CloudRuntime {
    pub(crate) fn google_store(&self, connection: &str) -> Result<Option<Arc<dyn ObjectStore>>, crate::AppError> {
        let snapshot = self.config.snapshot().map_err(super::runtime::config_error)?;
        let Some(entry) = snapshot.official_connections.into_iter().find(|entry| entry.id == connection) else { return Ok(None); };
        if entry.provider != super::oauth_types::OfficialProvider::GoogleDrive { return Err(crate::AppError { code: "CLOUD_UNSUPPORTED".into(), message: "此协议的同步尚未实现 / Sync is not implemented for this protocol".into(), detail: None, context: None }); }
        Ok(Some(Arc::new(GoogleObjectStore { runtime: self.weak_self.clone(), connection: entry.id, revision: entry.revision, root: entry.root_id })))
    }
}
