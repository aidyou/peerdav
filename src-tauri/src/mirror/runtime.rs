use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{StreamExt, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cloud::traits::ObjectStore;
use crate::cloud::types::{CloudError, ObjectMetadata};
use crate::machine;

use super::baseline;
use super::scan;
use super::sync;
use super::types::{
    self, CloudFile, LocalFile, MirrorAction, MirrorError, MirrorFailure, MirrorMapping,
    MirrorSyncResult,
};

/// 流式上传的读取分片大小：本地文件按此大小分片读出，整文件不载入内存。
const READ_CHUNK: usize = 256 * 1024;
/// 单个镜像目录最多容纳的对象数，超过即拒绝同步。
const MAX_CLOUD_ENTRIES: usize = 500_000;

fn canceled(cancel: &AtomicBool) -> Result<(), MirrorError> {
    if cancel.load(Ordering::Relaxed) {
        Err(MirrorError::Canceled)
    } else {
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 本机标识（用于冲突副本命名）；取不到稳定机器码时退回到 `local`。
fn machine_label() -> String {
    machine::machine_code()
        .map(|code| machine::short_code(&code))
        .unwrap_or_else(|| "local".to_owned())
}

/// 列举镜像目录下的云端对象，返回「相对路径 → 云端文件快照」。
/// 冲突副本与目录占位键（以 `/` 结尾）被跳过；分页由底层 `list` 流处理。
pub async fn list_cloud(
    store: &dyn ObjectStore,
    cloud_prefix: &str,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, CloudFile>, MirrorError> {
    let prefix = types::cloud_key_prefix(cloud_prefix);
    let mut listing = store.list(&prefix);
    let mut files = BTreeMap::new();
    while let Some(item) = listing.next().await {
        canceled(cancel)?;
        let meta = item.map_err(MirrorError::from)?;
        let Some(relative) = meta.key.strip_prefix(&prefix) else {
            continue;
        };
        if relative.is_empty() || relative.ends_with('/') {
            continue;
        }
        if types::is_conflict_artifact(relative) {
            continue;
        }
        if files.len() >= MAX_CLOUD_ENTRIES {
            return Err(MirrorError::TooManyEntries);
        }
        files.insert(
            relative.to_owned(),
            CloudFile {
                etag: meta.etag,
                size: meta.size,
            },
        );
    }
    Ok(files)
}

/// 把本地文件流式上传到云端键。本地读取失败记入共享备注并在结果里转回本地错误，
/// 避免把本地 IO 错误误报成云存储错误。
async fn upload_file(
    store: &dyn ObjectStore,
    path: &Path,
    key: &str,
    cancel: Arc<AtomicBool>,
) -> Result<ObjectMetadata, MirrorError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(MirrorError::from)?;
    let failure: Arc<Mutex<Option<std::io::Error>>> = Arc::new(Mutex::new(None));
    let note = Arc::clone(&failure);
    let body = stream::unfold(file, move |mut file| {
        let note = Arc::clone(&note);
        async move {
            let mut buf = vec![0u8; READ_CHUNK];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => Some((Ok(buf[..n].to_vec().into()), file)),
                Err(error) => {
                    if let Ok(mut guard) = note.lock() {
                        *guard = Some(error);
                    }
                    Some((Err(CloudError::Unsupported), file))
                }
            }
        }
    });
    match store
        .upload_stream_with_control(key, Box::pin(body), cancel)
        .await
    {
        Ok(meta) => Ok(meta),
        Err(error) => {
            let local = failure
                .lock()
                .ok()
                .and_then(|mut guard| guard.take());
            if let Some(local) = local {
                return Err(MirrorError::Local(local));
            }
            Err(MirrorError::from(error))
        }
    }
}

/// 下载云端对象到本地（原子写：临时文件 + 同步 + 重命名），返回落盘后的修改时间与字节数。
async fn download_file(
    store: &dyn ObjectStore,
    local_root: &Path,
    relative: &str,
    key: &str,
    cancel: &AtomicBool,
) -> Result<(i128, u64), MirrorError> {
    let mut body = store
        .get(key, None)
        .await
        .map_err(MirrorError::from)?;
    let path = local_root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(MirrorError::from)?;
    }
    let tmp = PathBuf::from(format!("{}.peerdav-mirror-tmp", path.display()));
    let result = (async {
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(MirrorError::from)?;
        while let Some(chunk) = body.next().await {
            canceled(cancel)?;
            let chunk = chunk.map_err(MirrorError::from)?;
            file.write_all(&chunk)
                .await
                .map_err(MirrorError::from)?;
        }
        file.sync_all()
            .await
            .map_err(MirrorError::from)?;
        Ok::<(), MirrorError>(())
    })
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error);
    }
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(MirrorError::from)?;
    let meta = std::fs::symlink_metadata(&path).map_err(MirrorError::from)?;
    let mtime_ns = meta
        .modified()
        .map_err(MirrorError::from)?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i128;
    Ok((mtime_ns, meta.len()))
}

/// 同步开始前重新校验本地文件与扫描时一致，避免上传扫描过程中被改动的文件。
fn local_state_matches(path: &Path, scanned: &LocalFile) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let mtime_ns = meta
        .modified()
        .map(|t| {
            t.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i128
        })
        .unwrap_or(-1);
    mtime_ns == scanned.mtime_ns && meta.len() == scanned.size
}

fn action_path(action: &MirrorAction) -> &String {
    match action {
        MirrorAction::Upload(p)
        | MirrorAction::Download(p)
        | MirrorAction::DeleteCloud(p)
        | MirrorAction::DeleteLocal(p)
        | MirrorAction::Conflict(p)
        | MirrorAction::Restore(p) => p,
    }
}

/// 执行一次镜像同步：扫描本地 → 列举云端 → 三方比对 → 逐条执行 → 保存基线。
///
/// 单条失败不中断整体同步（记入 `failures`）；取消令牌在扫描、列举与每条操作前检查。
/// 基线在所有操作结束后保存一次，成功执行的操作会更新对应条目。
pub async fn run_sync(
    store: Arc<dyn ObjectStore>,
    mapping: &MirrorMapping,
    data_dir: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<MirrorSyncResult, MirrorError> {
    let mut baseline = baseline::load(data_dir, &mapping.id)?;
    let local = scan::scan(&mapping.local_root, &cancel)?;
    let cloud = list_cloud(store.as_ref(), &mapping.cloud_prefix, &cancel).await?;
    let actions = sync::plan(&local, &cloud, &baseline);
    let machine = machine_label();
    let stamp = types::utc_stamp(now_secs());
    let mut result = MirrorSyncResult::default();

    for action in actions {
        canceled(&cancel)?;
        let path = action_path(&action).clone();
        match action {
            MirrorAction::Upload(_) => {
                let Some(scanned) = local.get(&path) else {
                    continue;
                };
                let abs = mapping.local_root.join(&path);
                if !local_state_matches(&abs, scanned) {
                    result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: "本地文件在同步期间发生了变化，已跳过".into(),
                    });
                    continue;
                }
                let key = types::cloud_key(&mapping.cloud_prefix, &path);
                match upload_file(store.as_ref(), &abs, &key, Arc::clone(&cancel)).await {
                    Ok(meta) => {
                        baseline.record_upload(
                            &path,
                            scanned.mtime_ns,
                            scanned.size,
                            meta.etag,
                            meta.size,
                        );
                        result.uploaded += 1;
                    }
                    Err(error) => result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
            MirrorAction::Download(_) | MirrorAction::Restore(_) => {
                let key = types::cloud_key(&mapping.cloud_prefix, &path);
                match download_file(
                    store.as_ref(),
                    &mapping.local_root,
                    &path,
                    &key,
                    &cancel,
                )
                .await
                {
                    Ok((mtime_ns, size)) => {
                        let cloud_file = cloud.get(&path);
                        baseline.record_download(
                            &path,
                            mtime_ns,
                            cloud_file.and_then(|f| f.etag.clone()),
                            cloud_file.map(|f| f.size).unwrap_or(size),
                        );
                        if matches!(action, MirrorAction::Restore(_)) {
                            result.restored += 1;
                        } else {
                            result.downloaded += 1;
                        }
                    }
                    Err(error) => result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
            MirrorAction::DeleteCloud(_) => {
                let key = types::cloud_key(&mapping.cloud_prefix, &path);
                match store.delete(&key).await {
                    Ok(()) => {
                        baseline.remove(&path);
                        result.deleted_cloud += 1;
                    }
                    Err(error) => result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
            MirrorAction::DeleteLocal(_) => {
                let abs = mapping.local_root.join(&path);
                match tokio::fs::remove_file(&abs).await {
                    Ok(()) => {
                        baseline.remove(&path);
                        result.deleted_local += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        baseline.remove(&path);
                    }
                    Err(error) => result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
            MirrorAction::Conflict(_) => {
                let abs = mapping.local_root.join(&path);
                let conflict_key = types::cloud_key(
                    &mapping.cloud_prefix,
                    &types::conflict_name(&path, &machine, &stamp),
                );
                match upload_file(store.as_ref(), &abs, &conflict_key, Arc::clone(&cancel)).await {
                    Ok(_) => {
                        // 原路径记为「已同步到当前两侧状态」，避免下一轮重复冲突；
                        // 两个版本均已保留：云端原路径版本 + 冲突副本（本地版本）。
                        if let (Some(scanned), Some(cloud_file)) =
                            (local.get(&path), cloud.get(&path))
                        {
                            baseline.record_upload(
                                &path,
                                scanned.mtime_ns,
                                scanned.size,
                                cloud_file.etag.clone(),
                                cloud_file.size,
                            );
                        }
                        result.conflicts.push(path.clone());
                    }
                    Err(error) => result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
        }
    }

    // 清理两侧都不再存在的陈旧基线条目。
    baseline.retain(|p| local.contains_key(p) || cloud.contains_key(p));
    baseline::save(data_dir, &mapping.id, &baseline)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::types::{ObjectMetadata, PutCondition, StorageCapabilities};
    use bytes::Bytes;
    use futures_util::{StreamExt, future::BoxFuture, stream};
    use std::collections::HashMap;

    /// 内存版对象存储：键 -> (内容, etag)，用于端到端验证镜像同步管线。
    struct MemoryStore {
        objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
    }

    impl MemoryStore {
        fn new(objects: Vec<(&str, Vec<u8>)>) -> Self {
            Self {
                objects: Mutex::new(
                    objects
                        .into_iter()
                        .map(|(key, value)| {
                            let etag = format!("etag-{}", value.len());
                            (key.to_owned(), (value, etag))
                        })
                        .collect(),
                ),
            }
        }

        fn insert(&self, key: &str, value: Vec<u8>, etag: &str) {
            self.objects
                .lock()
                .expect("test store lock")
                .insert(key.to_owned(), (value, etag.to_owned()));
        }

        fn keys(&self) -> Vec<String> {
            self.objects
                .lock()
                .expect("test store lock")
                .keys()
                .cloned()
                .collect()
        }

        fn get_value(&self, key: &str) -> Vec<u8> {
            self.objects
                .lock()
                .expect("test store lock")
                .get(key)
                .map(|(value, _)| value.clone())
                .unwrap_or_default()
        }
    }

    impl ObjectStore for MemoryStore {
        fn capabilities(&self) -> StorageCapabilities {
            StorageCapabilities::default()
        }
        fn list<'a>(&'a self, prefix: &'a str) -> crate::cloud::types::ObjectListing<'a> {
            let items: Vec<ObjectMetadata> = self
                .objects
                .lock()
                .expect("test store lock")
                .iter()
                .filter(|(key, _)| key.starts_with(prefix))
                .map(|(key, (value, etag))| ObjectMetadata {
                    key: key.clone(),
                    size: value.len() as u64,
                    etag: Some(etag.clone()),
                })
                .collect();
            stream::iter(items.into_iter().map(Ok)).boxed()
        }
        fn stat<'a>(
            &'a self,
            key: &'a str,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            let entry = self
                .objects
                .lock()
                .expect("test store lock")
                .get(key)
                .map(|(value, etag)| (value.len() as u64, etag.clone()));
            Box::pin(async move {
                match entry {
                    Some((size, etag)) => Ok(ObjectMetadata {
                        key: key.to_owned(),
                        size,
                        etag: Some(etag),
                    }),
                    None => Err(CloudError::NotFound),
                }
            })
        }
        fn get<'a>(
            &'a self,
            key: &'a str,
            _range: Option<std::ops::Range<u64>>,
        ) -> BoxFuture<'a, Result<crate::cloud::types::ObjectStream, CloudError>> {
            let entry = self
                .objects
                .lock()
                .expect("test store lock")
                .get(key)
                .map(|(value, _)| value.clone());
            Box::pin(async move {
                match entry {
                    Some(value) => Ok(stream::once(async move { Ok(Bytes::from(value)) }).boxed()),
                    None => Err(CloudError::NotFound),
                }
            })
        }
        fn put<'a>(
            &'a self,
            key: &'a str,
            mut body: crate::cloud::types::ObjectStream,
            _condition: PutCondition,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                let mut data = Vec::new();
                while let Some(chunk) = body.next().await {
                    data.extend_from_slice(&chunk?);
                }
                let etag = format!("etag-{}", data.len());
                self.insert(key, data.clone(), &etag);
                Ok(ObjectMetadata {
                    key: key.to_owned(),
                    size: data.len() as u64,
                    etag: Some(etag),
                })
            })
        }
        fn upload_stream_with_control<'a>(
            &'a self,
            key: &'a str,
            mut body: crate::cloud::types::ObjectStream,
            _cancel: Arc<AtomicBool>,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                let mut data = Vec::new();
                while let Some(chunk) = body.next().await {
                    data.extend_from_slice(&chunk?);
                }
                let etag = format!("etag-{}", data.len());
                self.insert(key, data.clone(), &etag);
                Ok(ObjectMetadata {
                    key: key.to_owned(),
                    size: data.len() as u64,
                    etag: Some(etag),
                })
            })
        }
        fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
            self.objects
                .lock()
                .expect("test store lock")
                .remove(key);
            Box::pin(async move { Ok(()) })
        }
    }

    fn mapping(local_root: &Path, cloud_prefix: &str) -> MirrorMapping {
        MirrorMapping {
            id: "map1".to_owned(),
            connection_id: "c1".to_owned(),
            local_root: local_root.to_path_buf(),
            cloud_prefix: cloud_prefix.to_owned(),
            schedule: Default::default(),
        }
    }

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn test_dirs() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("peerdav-mirror-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let local = base.join("local");
        let data = base.join("data");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        (local, data)
    }

    #[tokio::test]
    async fn first_sync_uploads_and_downloads_then_reaches_steady_state() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "hello");
        write(&local, "sub/b.txt", "world");
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/remote.txt",
            b"remote".to_vec(),
        )]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = run_sync(store.clone(), &mapping, &data, Arc::clone(&cancel))
            .await
            .unwrap();
        assert_eq!(result.uploaded, 2);
        assert_eq!(result.downloaded, 1);
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"hello");
        assert_eq!(store.get_value("_peerdav_mirror/docs/sub/b.txt"), b"world");
        assert_eq!(
            std::fs::read(local.join("remote.txt")).unwrap(),
            b"remote"
        );

        // 第二轮：两侧均未变，不再有任何动作。
        let again = run_sync(store.clone(), &mapping, &data, cancel)
            .await
            .unwrap();
        assert_eq!(again.uploaded, 0);
        assert_eq!(again.downloaded, 0);
        assert_eq!(again.deleted_cloud, 0);
        assert_eq!(again.deleted_local, 0);
        assert!(again.conflicts.is_empty());
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn deletions_propagate_in_both_directions() {
        let (local, data) = test_dirs();
        write(&local, "keep.txt", "keep");
        write(&local, "gone.txt", "gone");
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/keep.txt",
            b"keep".to_vec(),
        )]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        // 第一轮把本地两个文件都上传。
        run_sync(store.clone(), &mapping, &data, Arc::clone(&cancel))
            .await
            .unwrap();
        assert!(store.keys().contains(&"_peerdav_mirror/docs/gone.txt".to_owned()));

        // 本地删除 gone.txt → 同步应删除云端副本。
        std::fs::remove_file(local.join("gone.txt")).unwrap();
        let result = run_sync(store.clone(), &mapping, &data, Arc::clone(&cancel))
            .await
            .unwrap();
        assert_eq!(result.deleted_cloud, 1);
        assert!(!store.keys().contains(&"_peerdav_mirror/docs/gone.txt".to_owned()));

        // 云端删除 keep.txt → 同步应删除本地副本。
        store
            .objects
            .lock()
            .expect("test store lock")
            .remove("_peerdav_mirror/docs/keep.txt");
        let result = run_sync(store.clone(), &mapping, &data, cancel)
            .await
            .unwrap();
        assert_eq!(result.deleted_local, 1);
        assert!(!local.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn both_sides_changed_keeps_both_as_conflict_copy() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "local-v1");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        // 第一轮建立基线。
        run_sync(store.clone(), &mapping, &data, Arc::clone(&cancel))
            .await
            .unwrap();

        // 本地与云端各自修改同一文件（云端直接改 etag 模拟另一台设备的修改）。
        std::fs::write(local.join("a.txt"), "local-v2").unwrap();
        store.insert("_peerdav_mirror/docs/a.txt", b"remote-v2".to_vec(), "etag-other");

        let result = run_sync(store.clone(), &mapping, &data, cancel)
            .await
            .unwrap();
        assert_eq!(result.conflicts, vec!["a.txt".to_owned()]);
        // 原路径云端版本保持不动，本地内容作为冲突副本上传。
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"remote-v2");
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"local-v2");
        let conflict = store
            .keys()
            .into_iter()
            .find(|key| key.contains(".conflict."))
            .unwrap();
        assert!(conflict.starts_with("_peerdav_mirror/docs/a.conflict."));
        assert_eq!(store.get_value(&conflict), b"local-v2");

        // 冲突记录入基线后，下一轮不再重复冲突。
        let again = run_sync(store.clone(), &mapping, &data, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert!(again.conflicts.is_empty());
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn local_delete_with_cloud_change_restores_the_file() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "v1");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        run_sync(store.clone(), &mapping, &data, Arc::clone(&cancel))
            .await
            .unwrap();

        // 本地删除，同时云端被另一台设备修改。
        std::fs::remove_file(local.join("a.txt")).unwrap();
        store.insert("_peerdav_mirror/docs/a.txt", b"v2".to_vec(), "etag-v2");

        let result = run_sync(store.clone(), &mapping, &data, cancel)
            .await
            .unwrap();
        assert_eq!(result.restored, 1);
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"v2");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }
}
