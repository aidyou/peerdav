//! Object-store adapter for official drive file APIs.
//!
//! Google Drive keeps its dedicated adapter; this adapter exposes the same
//! append/mirror primitives for Baidu Netdisk, Dropbox and OneDrive. These
//! providers do not offer atomic conditional writes, so immutable append
//! objects are verified by content and mirror objects use the provider's
//! ordinary replace operation.
use super::{
    oauth_types::OfficialProvider,
    official_api_types::{OfficialApi, OfficialApiError, OfficialEntry},
    runtime::CloudRuntime,
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectPage, ObjectStoreScope, ObjectStream,
        PutCondition, StorageCapabilities,
    },
};
use crate::AppError;
use futures_util::{StreamExt, future::BoxFuture};
use std::{
    collections::HashSet,
    ops::Range,
    sync::Weak,
};
#[cfg(test)]
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

fn app_error(error: AppError) -> CloudError {
    CloudError::Service {
        status: 0,
        detail: error.detail.unwrap_or(error.message),
    }
}

fn api_error(provider: OfficialProvider, error: OfficialApiError) -> CloudError {
    let store = match provider {
        OfficialProvider::BaiduNetdisk => "Baidu Netdisk",
        OfficialProvider::Dropbox => "Dropbox",
        OfficialProvider::OneDrive => "OneDrive",
        _ => "Official drive",
    };
    CloudError::Storage {
        source: object_store::Error::Generic {
            store,
            source: Box::new(error),
        },
    }
}

fn config_error(error: impl std::error::Error + Send + Sync + 'static) -> CloudError {
    CloudError::Storage {
        source: object_store::Error::Generic {
            store: "official config",
            source: Box::new(error),
        },
    }
}

#[cfg(test)]
type ApiFactory = Arc<dyn Fn() -> OfficialApi + Send + Sync>;

pub(crate) struct OfficialObjectStore {
    runtime: Weak<CloudRuntime>,
    connection: String,
    revision: u64,
    root: String,
    provider: OfficialProvider,
    endpoint: String,
    parents: tokio::sync::Mutex<()>,
    #[cfg(test)]
    api_factory: Option<ApiFactory>,
}

impl OfficialObjectStore {
    pub(crate) fn new(
        runtime: Weak<CloudRuntime>,
        connection: String,
        revision: u64,
        root: String,
        provider: OfficialProvider,
        endpoint: String,
    ) -> Self {
        Self {
            runtime,
            connection,
            revision,
            root,
            provider,
            endpoint,
            parents: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            api_factory: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        factory: ApiFactory,
        root: String,
        provider: OfficialProvider,
        endpoint: String,
    ) -> Self {
        Self {
            runtime: Weak::new(),
            connection: "test".into(),
            revision: 1,
            root,
            provider,
            endpoint,
            parents: tokio::sync::Mutex::new(()),
            api_factory: Some(factory),
        }
    }

    async fn api(&self) -> Result<OfficialApi, CloudError> {
        #[cfg(test)]
        if let Some(factory) = &self.api_factory {
            return Ok(factory());
        }
        let runtime = self.runtime.upgrade().ok_or(CloudError::UploadCancelled)?;
        runtime.admit().map_err(app_error)?;
        let snapshot = runtime.config.snapshot().map_err(config_error)?;
        let entry = snapshot
            .official_connections
            .into_iter()
            .find(|entry| entry.id == self.connection)
            .ok_or(CloudError::PrefixViolation)?;
        if entry.revision != self.revision
            || entry.root_id != self.root
            || entry.provider != self.provider
        {
            return Err(CloudError::PrefixViolation);
        }
        let (_, api) = runtime
            .official_api(&self.connection)
            .await
            .map_err(app_error)?;
        Ok(api)
    }

    async fn children(
        &self,
        api: &OfficialApi,
        parent: &str,
    ) -> Result<Vec<OfficialEntry>, CloudError> {
        let mut result = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let page = api
                .list(parent, cursor.as_deref())
                .await
                .map_err(|error| api_error(self.provider, error))?;
            if result.len().saturating_add(page.entries.len()) > 100_000 || seen.len() > 1000 {
                return Err(CloudError::UploadBudgetExceeded);
            }
            result.extend(page.entries);
            match page.next {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err(CloudError::Precondition),
                None if page.complete => return Ok(result),
                None => return Err(CloudError::Precondition),
            }
        }
    }

    async fn resolve(
        &self,
        api: &OfficialApi,
        key: &str,
    ) -> Result<OfficialEntry, CloudError> {
        super::object_service::valid_relative_path(key).map_err(config_error)?;
        let mut parent = self.root.clone();
        let parts: Vec<_> = key.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            let mut matches = self
                .children(api, &parent)
                .await?
                .into_iter()
                .filter(|entry| entry.name == *part);
            let entry = matches.next().ok_or(CloudError::NotFound)?;
            if matches.next().is_some() {
                return Err(CloudError::Precondition);
            }
            if index == parts.len() - 1 {
                return Ok(entry);
            }
            if !entry.is_dir {
                return Err(CloudError::PrefixViolation);
            }
            parent = entry.id;
        }
        Err(CloudError::PrefixViolation)
    }

    async fn parent_and_name(
        &self,
        api: &OfficialApi,
        key: &str,
    ) -> Result<(String, String), CloudError> {
        // Concurrent append blocks may share a not-yet-created folder.
        let _parent = self.parents.lock().await;
        super::object_service::valid_relative_path(key).map_err(config_error)?;
        let mut parts: Vec<_> = key.split('/').collect();
        let name = parts.pop().ok_or(CloudError::PrefixViolation)?.to_owned();
        let mut parent = self.root.clone();
        for part in parts {
            let mut matches = self
                .children(api, &parent)
                .await?
                .into_iter()
                .filter(|entry| entry.name == part);
            let entry = matches.next();
            if matches.next().is_some() {
                return Err(CloudError::Precondition);
            }
            parent = match entry {
                Some(entry) if entry.is_dir => entry.id,
                Some(_) => return Err(CloudError::PrefixViolation),
                None => api
                    .create_folder_id(&parent, part)
                    .await
                    .map_err(|error| api_error(self.provider, error))?,
            };
        }
        Ok((parent, name))
    }

    fn ensure_sync_key(key: &str) -> Result<(), CloudError> {
        if key.starts_with("_peerdav_encrypt/") || key.starts_with("_peerdav_mirror/") {
            Ok(())
        } else {
            Err(CloudError::PrefixViolation)
        }
    }
}

impl ObjectStore for OfficialObjectStore {
    fn scope(&self) -> Option<ObjectStoreScope> {
        Some(ObjectStoreScope {
            endpoint: self.endpoint.clone(),
            bucket: self.root.clone(),
            prefix: String::new(),
        })
    }

    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::default()
    }

    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        futures_util::stream::try_unfold((false, Vec::new()), move |(loaded, mut buffered): (bool, Vec<ObjectMetadata>)| async move {
            if let Some(item) = buffered.pop() {
                return Ok(Some((item, (loaded, buffered))));
            }
            if loaded {
                return Ok(None);
            }
            let page = self.list_page(prefix, None).await?;
            let mut objects = page.objects;
            objects.reverse();
            Ok(objects.pop().map(|item| (item, (true, objects))))
        })
        .boxed()
    }

    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move {
            super::object_service::valid_prefix(prefix).map_err(config_error)?;
            if token.is_some() {
                return Err(CloudError::Unsupported);
            }
            let api = self.api().await?;
            let mut pending = vec![(self.root.clone(), String::new(), 0usize)];
            let mut objects = Vec::new();
            let mut visited = HashSet::new();
            let mut observed = 0usize;
            while let Some((parent, base, depth)) = pending.pop() {
                if depth > 64 || !visited.insert(parent.clone()) || visited.len() > 1000 {
                    return Err(CloudError::UploadBudgetExceeded);
                }
                let mut names = HashSet::new();
                for entry in self.children(&api, &parent).await? {
                    observed += 1;
                    if observed > 100_000 {
                        return Err(CloudError::UploadBudgetExceeded);
                    }
                    let key = format!("{base}{}", entry.name);
                    let relevant = key.starts_with(prefix)
                        || (entry.is_dir && prefix.starts_with(&format!("{key}/")));
                    if !relevant {
                        continue;
                    }
                    if !names.insert(entry.name.clone()) {
                        return Err(CloudError::Precondition);
                    }
                    super::object_service::valid_relative_path(&entry.name).map_err(config_error)?;
                    if entry.is_dir {
                        let dir = format!("{key}/");
                        if dir.starts_with(prefix) || prefix.starts_with(&dir) {
                            pending.push((entry.id, dir, depth + 1));
                        }
                    } else if key.starts_with(prefix) {
                        objects.push(ObjectMetadata {
                            key,
                            size: entry.size,
                            etag: entry.revision,
                        });
                    }
                    if objects.len() + pending.len() > 100_000 {
                        return Err(CloudError::UploadBudgetExceeded);
                    }
                }
            }
            objects.sort_by(|a, b| a.key.cmp(&b.key));
            if objects.windows(2).any(|pair| pair[0].key == pair[1].key) {
                return Err(CloudError::Precondition);
            }
            Ok(ObjectPage {
                objects,
                next_token: None,
            })
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let api = self.api().await?;
            let entry = self.resolve(&api, key).await?;
            if entry.is_dir {
                return Err(CloudError::NotFound);
            }
            Ok(ObjectMetadata {
                key: key.into(),
                size: entry.size,
                etag: entry.revision,
            })
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            if range.is_some() {
                return Err(CloudError::Unsupported);
            }
            let api = self.api().await?;
            let entry = self.resolve(&api, key).await?;
            let response = api
                .media(&entry.id)
                .await
                .map_err(|error| api_error(self.provider, error))?;
            Ok(response
                .bytes_stream()
                .map(|chunk| chunk.map_err(|error| CloudError::Transport { source: error.without_url() }))
                .boxed())
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            Self::ensure_sync_key(key)?;
            if !matches!(condition, PutCondition::Any) {
                return Err(CloudError::Unsupported);
            }
            let api = self.api().await?;
            let existing = match self.resolve(&api, key).await {
                Ok(entry) if !entry.is_dir => Some(entry),
                Ok(_) => return Err(CloudError::Precondition),
                Err(CloudError::NotFound) => None,
                Err(error) => return Err(error),
            };
            let file = tempfile::tempfile().map_err(config_error)?;
            let mut file = tokio::fs::File::from_std(file);
            let mut size = 0u64;
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or(CloudError::PayloadTooLarge)?;
                if size > crate::types::MAX_TRANSFER_BYTES {
                    return Err(CloudError::PayloadTooLarge);
                }
                file.write_all(&chunk).await.map_err(config_error)?;
            }
            if let Some(entry) = existing {
                if key.starts_with("_peerdav_mirror/") {
                    use tokio::io::AsyncSeekExt;
                    file.rewind().await.map_err(config_error)?;
                    api.replace_file_stream(&entry.id, file.into_std().await, size, None)
                        .await
                        .map_err(|error| api_error(self.provider, error))?;
                    return self.stat(key).await;
                }
                if entry.size != size {
                    return Err(CloudError::Precondition);
                }
                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                file.rewind().await.map_err(config_error)?;
                let mut remote = api
                    .media(&entry.id)
                    .await
                    .map_err(|error| api_error(self.provider, error))?
                    .bytes_stream()
                    .map(|chunk| chunk.map_err(|error| CloudError::Transport { source: error.without_url() }));
                let mut offset = 0u64;
                while let Some(chunk) = remote.next().await {
                    let chunk = chunk?;
                    offset = offset
                        .checked_add(chunk.len() as u64)
                        .ok_or(CloudError::PayloadTooLarge)?;
                    if offset > size {
                        return Err(CloudError::Precondition);
                    }
                    let mut local = vec![0; chunk.len()];
                    file.read_exact(&mut local).await.map_err(config_error)?;
                    if local.as_slice() != chunk.as_ref() {
                        return Err(CloudError::Precondition);
                    }
                }
                if offset != size {
                    return Err(CloudError::Precondition);
                }
                return Ok(ObjectMetadata {
                    key: key.into(),
                    size: entry.size,
                    etag: entry.revision,
                });
            }
            use tokio::io::AsyncSeekExt;
            file.rewind().await.map_err(config_error)?;
            let (parent, name) = self.parent_and_name(&api, key).await?;
            api.upload_object(&parent, &name, file.into_std().await, size)
                .await
                .map_err(|error| api_error(self.provider, error))?;
            self.stat(key).await
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            if !key.starts_with("_peerdav_mirror/") {
                return Err(CloudError::PrefixViolation);
            }
            let api = self.api().await?;
            let entry = self.resolve(&api, key).await?;
            if entry.is_dir || entry.id == self.root {
                return Err(CloudError::PrefixViolation);
            }
            let revision = entry.revision.unwrap_or_else(|| "*".into());
            api.delete_item(&entry.id, &revision)
                .await
                .map_err(|error| api_error(self.provider, error))
        })
    }

    fn upload_stream<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.put(key, body, PutCondition::Any)
    }

    fn copy_move_stream<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.put(key, body, PutCondition::Any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::official_api_types::{OfficialApi, OfficialApiError, OfficialEntry, OfficialPage};
    use crate::cloud::official_drives::OfficialDrive;
    use futures_util::future::BoxFuture;
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    use zeroize::Zeroizing;

    #[derive(Default)]
    struct State {
        entries: BTreeMap<String, Node>,
        next: u64,
    }
    struct Node {
        name: String,
        is_dir: bool,
        data: Vec<u8>,
        revision: String,
    }
    struct FakeDrive {
        state: Arc<Mutex<State>>,
    }

    fn join(parent: &str, name: &str) -> String {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }

    fn entry(path: &str, node: &Node) -> OfficialEntry {
        OfficialEntry {
            id: path.into(),
            name: node.name.clone(),
            is_dir: node.is_dir,
            size: node.data.len() as u64,
            content_type: None,
            online_preview: false,
            revision: (!node.is_dir).then(|| node.revision.clone()),
        }
    }

    impl OfficialDrive for FakeDrive {
        fn provider(&self) -> OfficialProvider { OfficialProvider::Dropbox }

        fn list<'a>(&'a self, parent: &'a str, _cursor: Option<&'a str>) -> BoxFuture<'a, Result<OfficialPage, OfficialApiError>> {
            Box::pin(async move {
                let state = self.state.lock().map_err(|_| OfficialApiError::InvalidResponse)?;
                let mut entries: Vec<_> = state.entries.iter()
                    .filter(|(path, _)| path.rsplit_once('/').map(|(dir, _)| dir == parent.trim_end_matches('/') || (parent == "/" && dir.is_empty())).unwrap_or(false))
                    .map(|(path, node)| entry(path, node))
                    .collect();
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(OfficialPage { entries, next: None, complete: true })
            })
        }

        fn media<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<reqwest::Response, OfficialApiError>> {
            Box::pin(async { Err(OfficialApiError::Unsupported) })
        }

        fn delete_item<'a>(&'a self, id: &'a str, _revision: &'a str) -> BoxFuture<'a, Result<(), OfficialApiError>> {
            Box::pin(async move {
                let mut state = self.state.lock().map_err(|_| OfficialApiError::InvalidResponse)?;
                state.entries.remove(id);
                let prefix = format!("{}/", id.trim_end_matches('/'));
                state.entries.retain(|path, _| !path.starts_with(&prefix));
                Ok(())
            })
        }

        fn create_folder<'a>(&'a self, parent: &'a str, name: &'a str) -> BoxFuture<'a, Result<String, OfficialApiError>> {
            Box::pin(async move {
                let path = join(parent, name);
                let mut state = self.state.lock().map_err(|_| OfficialApiError::InvalidResponse)?;
                state.entries.insert(path.clone(), Node { name: name.into(), is_dir: true, data: Vec::new(), revision: String::new() });
                Ok(path)
            })
        }

        fn create_folder_id<'a>(&'a self, parent: &'a str, name: &'a str) -> BoxFuture<'a, Result<String, OfficialApiError>> {
            self.create_folder(parent, name)
        }

        fn upload_new<'a>(&'a self, parent: &'a str, name: &'a str, mut file: std::fs::File, _size: u64, _progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>) -> BoxFuture<'a, Result<String, OfficialApiError>> {
            Box::pin(async move {
                let mut data = Vec::new();
                file.read_to_end(&mut data).map_err(|_| OfficialApiError::InvalidInput)?;
                let path = join(parent, name);
                let mut state = self.state.lock().map_err(|_| OfficialApiError::InvalidResponse)?;
                state.next += 1;
                let revision = format!("rev-{}", state.next);
                state.entries.insert(path.clone(), Node { name: name.into(), is_dir: false, data, revision });
                Ok(path)
            })
        }

        fn replace_file_stream<'a>(&'a self, id: &'a str, mut file: std::fs::File, _size: u64, _expected: Option<&'a str>, _progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>) -> BoxFuture<'a, Result<(), OfficialApiError>> {
            Box::pin(async move {
                let mut data = Vec::new();
                file.read_to_end(&mut data).map_err(|_| OfficialApiError::InvalidInput)?;
                let mut state = self.state.lock().map_err(|_| OfficialApiError::InvalidResponse)?;
                state.next += 1;
                let revision = format!("rev-{}", state.next);
                let node = state.entries.get_mut(id).ok_or(OfficialApiError::InvalidInput)?;
                node.data = data;
                node.revision = revision;
                Ok(())
            })
        }

        fn metadata<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<serde_json::Value, OfficialApiError>> {
            Box::pin(async move {
                let state = self.state.lock().map_err(|_| OfficialApiError::InvalidResponse)?;
                let node = state.entries.get(id).ok_or(OfficialApiError::InvalidInput)?;
                Ok(serde_json::json!({ "rev": node.revision, "path_display": id }))
            })
        }
    }

    fn api(state: Arc<Mutex<State>>) -> OfficialApi {
        OfficialApi {
            #[cfg(test)]
            fixture: None,
            client: reqwest::Client::new(),
            provider: OfficialProvider::Dropbox,
            access_token: Zeroizing::new("test".into()),
            drive: Some(Box::new(FakeDrive { state })),
        }
    }

    fn store(state: Arc<Mutex<State>>) -> OfficialObjectStore {
        let factory_state = Arc::clone(&state);
        OfficialObjectStore::for_test(
            Arc::new(move || api(Arc::clone(&factory_state))),
            "/".into(),
            OfficialProvider::Dropbox,
            "https://api.dropboxapi.com/2".into(),
        )
    }

    fn body(bytes: &'static [u8]) -> ObjectStream {
        futures_util::stream::once(async move { Ok(bytes::Bytes::from_static(bytes)) }).boxed()
    }

    #[tokio::test]
    async fn mirror_roundtrip_and_append_immutability() {
        let state = Arc::new(Mutex::new(State::default()));
        let store = store(Arc::clone(&state));
        let mirror = "_peerdav_mirror/docs/a.txt";
        store.put(mirror, body(b"hello"), PutCondition::Any).await.expect("mirror create");
        assert_eq!(store.stat(mirror).await.expect("stat").size, 5);
        assert!(store.list_page("_peerdav_mirror/", None).await.expect("list").objects.iter().any(|entry| entry.key == mirror));
        store.put(mirror, body(b"world"), PutCondition::Any).await.expect("mirror replace");
        assert_eq!(state.lock().expect("state").entries["/_peerdav_mirror/docs/a.txt"].data, b"world");
        store.delete(mirror).await.expect("mirror delete");
        assert!(matches!(store.stat(mirror).await, Err(CloudError::NotFound)));

        let append = "_peerdav_encrypt/objects/abc";
        store.put(append, body(b"payload"), PutCondition::Any).await.expect("append create");
        assert_eq!(store.stat(append).await.expect("append stat").size, 7);
        assert!(matches!(store.put(append, body(b"payload"), PutCondition::CreateOnly).await, Err(CloudError::Unsupported)));
        assert!(matches!(store.get(append, Some(0..1)).await, Err(CloudError::Unsupported)));
        assert!(matches!(store.list_page("_peerdav_encrypt/", Some("next")).await, Err(CloudError::Unsupported)));
        assert!(matches!(store.put("ordinary/file", body(b"x"), PutCondition::Any).await, Err(CloudError::PrefixViolation)));
    }
}