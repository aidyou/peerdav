use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use futures_util::{StreamExt, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cloud::traits::ObjectStore;
use crate::cloud::types::{CloudError, ObjectMetadata};
use crate::machine;

use super::baseline;
use super::scan;
use super::sync;
use super::types::{
    self, CloudFile, LocalFile, MirrorAction, MirrorBaseline, MirrorError, MirrorFailure,
    MirrorIdentity, MirrorMapping, MirrorSyncResult, TEMP_MARKER,
};

/// 流式上传的读取分片大小：本地文件按此大小分片读出，整文件不载入内存。
const READ_CHUNK: usize = 256 * 1024;
/// 单个镜像目录最多容纳的对象数，超过即拒绝同步。
const MAX_CLOUD_ENTRIES: usize = 500_000;
/// 删除动作的绝对数量阈值：超过后必须显式确认。
const MASS_DELETE_ABSOLUTE: usize = 100;
/// 参与比例判断的最小删除数：小目录删一两个文件属于正常操作，不应被拦下。
const MASS_DELETE_MINIMUM: usize = 10;
/// 删除动作占总路径数的百分比阈值（按最大值向下取整比较），超过后必须显式确认。
const MASS_DELETE_PERCENT: usize = 25;
/// 下载临时文件的序号，避免同一目录内并发/重试时互相覆盖。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 一次镜像同步的目标信息与安全策略。
pub struct MirrorRunContext {
    /// 连接端点，纳入基线身份：端点变化意味着远端已换。
    pub endpoint: String,
    /// 连接使用的存储桶，纳入基线身份。
    pub bucket: String,
    /// 用户已确认本轮的大量删除计划。
    pub force_mass_delete: bool,
}

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
        if types::is_artifact(relative) {
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
            let local = failure.lock().ok().and_then(|mut guard| guard.take());
            if let Some(local) = local {
                return Err(MirrorError::Local(local));
            }
            Err(MirrorError::from(error))
        }
    }
}

/// 从本地根逐级打开相对路径的父目录；每级都 `nofollow`，缺失目录逐级创建。
/// 云端对象键同样会走到这里，因此父目录链上的符号链接一律拒绝，防止写出根目录。
fn open_parent_dir(root: &Path, relative: &Path) -> Result<Dir, MirrorError> {
    let mut dir = scan::open_root_nofollow(root)?;
    let mut components = relative.components();
    if !matches!(components.next_back(), Some(Component::Normal(_))) {
        return Err(MirrorError::UnsafePath(relative.display().to_string()));
    }
    for component in components {
        let Component::Normal(part) = component else {
            return Err(MirrorError::UnsafePath(relative.display().to_string()));
        };
        dir = match dir.open_dir_nofollow(part) {
            Ok(next) => next,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                dir.create_dir(part).map_err(MirrorError::from)?;
                dir.open_dir_nofollow(part).map_err(MirrorError::from)?
            }
            Err(error) => return Err(MirrorError::from(error)),
        };
    }
    Ok(dir)
}

/// 下载云端对象到本地（原子写：不跟随符号链接的父目录 + 临时文件 + 同步 + 重命名），
/// 返回落盘后的修改时间与字节数。`relative` 来自云端列举或本地扫描路径，必须先过
/// `validate_relative_str`，否则 `..`/绝对路径会把文件写到根目录之外。
async fn download_file(
    store: &dyn ObjectStore,
    local_root: &Path,
    relative: &str,
    key: &str,
    cancel: &AtomicBool,
) -> Result<(i128, u64), MirrorError> {
    types::validate_relative_str(relative)
        .map_err(|reason| MirrorError::UnsafePath(format!("{relative}: {reason}")))?;
    let relative_path = Path::new(relative);
    let mut body = store.get(key, None).await.map_err(MirrorError::from)?;
    let parent = open_parent_dir(local_root, relative_path)?;
    let path = local_root.join(relative_path);
    let temp_name = format!(
        ".{}.{}.{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed),
        TEMP_MARKER.trim_start_matches('.')
    );
    let mut options = CapOpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let temp_file = parent
        .open_with(&temp_name, &options)
        .map_err(MirrorError::from)?;
    let mut file = tokio::fs::File::from_std(temp_file.into_std());
    let result = (async {
        while let Some(chunk) = body.next().await {
            canceled(cancel)?;
            let chunk = chunk.map_err(MirrorError::from)?;
            file.write_all(&chunk).await.map_err(MirrorError::from)?;
        }
        file.sync_all().await.map_err(MirrorError::from)?;
        Ok::<(), MirrorError>(())
    })
    .await;
    drop(file);
    if let Err(error) = result {
        let _ = parent.remove_file(&temp_name);
        return Err(error);
    }
    let parent_relative = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let temp_path = local_root.join(parent_relative).join(&temp_name);
    if let Err(error) = tokio::fs::rename(&temp_path, &path).await {
        let _ = parent.remove_file(&temp_name);
        return Err(MirrorError::from(error));
    }
    let meta = std::fs::symlink_metadata(&path).map_err(MirrorError::from)?;
    if !meta.is_file() {
        return Err(MirrorError::UnsafePath(relative.to_owned()));
    }
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
        .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i128)
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

/// 执行一次镜像同步：校验目标身份 → 扫描本地 → 列举云端 → 三方比对 → 逐条执行 → 保存基线。
///
/// 单条失败不中断整体同步（记入 `failures`）；取消令牌在扫描、列举与每条操作前检查。
/// 基线在所有操作结束后保存一次，成功执行的操作会更新对应条目。
/// 扫描或列举不完整时关闭删除传播；删除量超过阈值时必须由调用方显式确认。
pub async fn run_sync(
    store: Arc<dyn ObjectStore>,
    mapping: &MirrorMapping,
    data_dir: &Path,
    context: &MirrorRunContext,
    cancel: Arc<AtomicBool>,
) -> Result<MirrorSyncResult, MirrorError> {
    let root = std::fs::canonicalize(&mapping.local_root)
        .map_err(|_| MirrorError::InvalidRoot(mapping.local_root.display().to_string()))?;
    let identity = MirrorIdentity {
        connection_id: mapping.connection_id.clone(),
        endpoint: context.endpoint.clone(),
        bucket: context.bucket.clone(),
        cloud_prefix: mapping.cloud_prefix.clone(),
        local_root: root.display().to_string(),
    };
    let mut baseline = baseline::load(data_dir, &mapping.id)?;
    if baseline.identity.as_ref() != Some(&identity) {
        if !baseline.entries.is_empty() {
            log::warn!(
                "mirror baseline for {} no longer matches its target; resetting before sync \
                 (connection={}, bucket={}, prefix={:?}, root={})",
                mapping.id,
                identity.connection_id,
                identity.bucket,
                identity.cloud_prefix,
                identity.local_root
            );
        }
        baseline = MirrorBaseline {
            identity: Some(identity),
            entries: BTreeMap::new(),
        };
    }

    let mut result = MirrorSyncResult::default();
    let scan::ScanReport {
        files: local,
        skipped,
    } = scan::scan(&mapping.local_root, &cancel)?;
    let mut complete = skipped.is_empty();
    for (path, reason) in skipped {
        result.failures.push(MirrorFailure { path, reason });
    }
    let listed = list_cloud(store.as_ref(), &mapping.cloud_prefix, &cancel).await?;
    let mut cloud = BTreeMap::new();
    for (path, file) in listed {
        match types::validate_relative_str(&path) {
            Ok(()) => {
                cloud.insert(path, file);
            }
            Err(reason) => {
                complete = false;
                result.failures.push(MirrorFailure {
                    path,
                    reason: format!("跳过不安全的云端对象键 / unsafe cloud key skipped: {reason}"),
                });
            }
        }
    }
    // 首次同步（或目标变化后重置）时，把两侧同名且大小相同的文件视为已建立基线：
    // 重新接入一个已有的镜像目录不应把所有文件都变成冲突副本。
    if baseline.entries.is_empty() {
        for (path, file) in &local {
            if let Some(cloud_file) = cloud.get(path)
                && cloud_file.size == file.size
            {
                baseline.record_upload(
                    path,
                    file.mtime_ns,
                    file.size,
                    cloud_file.etag.clone(),
                    cloud_file.size,
                );
            }
        }
    }
    let mut actions = sync::plan(&local, &cloud, &baseline);
    if !complete {
        let before = actions.len();
        actions.retain(|action| {
            !matches!(
                action,
                MirrorAction::DeleteCloud(_) | MirrorAction::DeleteLocal(_)
            )
        });
        if actions.len() != before {
            result.failures.push(MirrorFailure {
                path: "*".into(),
                reason: "存在未能扫描/列举的条目，本轮已跳过删除传播 / scan incomplete; deletion propagation skipped"
                    .into(),
            });
        }
    }
    let deletions = actions
        .iter()
        .filter(|action| {
            matches!(
                action,
                MirrorAction::DeleteCloud(_) | MirrorAction::DeleteLocal(_)
            )
        })
        .count();
    let total = local.len().max(cloud.len());
    if deletions > MASS_DELETE_ABSOLUTE
        || (deletions >= MASS_DELETE_MINIMUM && deletions * 100 > total * MASS_DELETE_PERCENT)
    {
        if !context.force_mass_delete {
            return Err(MirrorError::MassDelete {
                planned: deletions,
                total,
            });
        }
        log::warn!("mirror mass delete confirmed by caller: {deletions} of {total} paths");
    }

    let machine = machine_label();
    let stamp = types::utc_stamp(now_secs());

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
                        reason:
                            "本地文件在同步期间发生了变化，已跳过 / local file changed during sync"
                                .into(),
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
                match download_file(store.as_ref(), &mapping.local_root, &path, &key, &cancel).await
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
                let Some(scanned) = local.get(&path) else {
                    continue;
                };
                let abs = mapping.local_root.join(&path);
                if !local_state_matches(&abs, scanned) {
                    result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: "本地文件在同步期间发生了变化，已跳过冲突处理 / local file changed during sync"
                            .into(),
                    });
                    continue;
                }
                if !cloud.contains_key(&path) {
                    result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: "冲突缺少云端版本 / conflict without a cloud version".into(),
                    });
                    continue;
                }
                // 先把云端版本固定为本地冲突副本，再让本地版本接管原路径：
                // 两个版本都保留，原路径恢复为两侧一致，基线可以如实记录。
                let key = types::cloud_key(&mapping.cloud_prefix, &path);
                let conflict_relative = types::conflict_name(&path, &machine, &stamp);
                if let Err(error) = download_file(
                    store.as_ref(),
                    &mapping.local_root,
                    &conflict_relative,
                    &key,
                    &cancel,
                )
                .await
                {
                    result.failures.push(MirrorFailure {
                        path: path.clone(),
                        reason: format!(
                            "保留云端冲突副本失败 / failed to keep the cloud version: {error}"
                        ),
                    });
                    continue;
                }
                match upload_file(store.as_ref(), &abs, &key, Arc::clone(&cancel)).await {
                    Ok(meta) => {
                        baseline.record_upload(
                            &path,
                            scanned.mtime_ns,
                            scanned.size,
                            meta.etag,
                            meta.size,
                        );
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
    use std::path::PathBuf;

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
        fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
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
            self.objects.lock().expect("test store lock").remove(key);
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

    fn context() -> MirrorRunContext {
        MirrorRunContext {
            endpoint: "https://s3.example.test".to_owned(),
            bucket: "bucket".to_owned(),
            force_mass_delete: false,
        }
    }

    fn forced_context() -> MirrorRunContext {
        MirrorRunContext {
            force_mass_delete: true,
            ..context()
        }
    }

    fn test_dirs() -> (PathBuf, PathBuf) {
        use std::sync::atomic::AtomicUsize;
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "peerdav-mirror-e2e-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
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

        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::clone(&cancel),
        )
        .await
        .unwrap();
        assert_eq!(result.uploaded, 2);
        assert_eq!(result.downloaded, 1);
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"hello");
        assert_eq!(store.get_value("_peerdav_mirror/docs/sub/b.txt"), b"world");
        assert_eq!(std::fs::read(local.join("remote.txt")).unwrap(), b"remote");

        // 第二轮：两侧均未变，不再有任何动作。
        let again = run_sync(store.clone(), &mapping, &data, &context(), cancel)
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
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::clone(&cancel),
        )
        .await
        .unwrap();
        assert!(
            store
                .keys()
                .contains(&"_peerdav_mirror/docs/gone.txt".to_owned())
        );

        // 本地删除 gone.txt → 同步应删除云端副本。
        std::fs::remove_file(local.join("gone.txt")).unwrap();
        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::clone(&cancel),
        )
        .await
        .unwrap();
        assert_eq!(result.deleted_cloud, 1);
        assert!(
            !store
                .keys()
                .contains(&"_peerdav_mirror/docs/gone.txt".to_owned())
        );

        // 云端删除 keep.txt → 同步应删除本地副本。
        store
            .objects
            .lock()
            .expect("test store lock")
            .remove("_peerdav_mirror/docs/keep.txt");
        let result = run_sync(store.clone(), &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.deleted_local, 1);
        assert!(!local.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn both_sides_changed_keeps_both_versions() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "local-v1");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        // 第一轮建立基线。
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::clone(&cancel),
        )
        .await
        .unwrap();

        // 本地与云端各自修改同一文件（云端直接改 etag 模拟另一台设备的修改）。
        std::fs::write(local.join("a.txt"), "local-v2!").unwrap();
        store.insert(
            "_peerdav_mirror/docs/a.txt",
            b"remote-v2".to_vec(),
            "etag-other",
        );

        let result = run_sync(store.clone(), &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.conflicts, vec!["a.txt".to_owned()]);
        // 本地版本接管原路径，云端版本另存为本地冲突副本，两侧都保留且基线一致。
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"local-v2!");
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"local-v2!");
        let conflict = std::fs::read_dir(&local)
            .unwrap()
            .filter_map(|item| item.ok())
            .find(|item| item.file_name().to_string_lossy().contains(".conflict."))
            .expect("local conflict copy");
        assert_eq!(std::fs::read(conflict.path()).unwrap(), b"remote-v2");
        // 云端不再保留旧式冲突对象（标记会随本地副本走）。
        assert!(!store.keys().iter().any(|key| key.contains(".conflict.")));

        // 冲突解决后原路径两侧一致，下一轮不再重复冲突。
        let again = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(again.conflicts.is_empty());
        assert!(again.failures.is_empty());
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn unchanged_conflict_copy_is_not_uploaded_again() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "local-v1");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        std::fs::write(local.join("a.txt"), "local-v2!").unwrap();
        store.insert(
            "_peerdav_mirror/docs/a.txt",
            b"remote-v2".to_vec(),
            "etag-other",
        );
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        // 本地冲突副本本身不参与同步：后续轮次既不上传也不删除。
        let keys_before = store.keys();
        let again = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(again.uploaded, 0);
        assert_eq!(again.deleted_cloud, 0);
        assert_eq!(store.keys(), keys_before);
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn local_change_after_cloud_delete_restores_the_cloud_copy() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "v1");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        std::fs::write(local.join("a.txt"), "v2-local").unwrap();
        store
            .objects
            .lock()
            .expect("test store lock")
            .remove("_peerdav_mirror/docs/a.txt");

        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.conflicts, Vec::<String>::new());
        assert_eq!(result.uploaded, 1);
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"v2-local");
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"v2-local");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn equal_size_files_are_adopted_on_the_first_sync() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "local");
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/a.txt",
            b"cloud".to_vec(),
        )]));
        let mapping = mapping(&local, "docs");
        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        // 接入已有镜像目录时，同名同大小视为同一状态，不复制、不覆盖。
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.downloaded, 0);
        assert!(result.conflicts.is_empty());
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"local");
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"cloud");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn unsafe_cloud_keys_are_skipped_without_writing_outside_the_root() {
        let (local, data) = test_dirs();
        let base = local.parent().unwrap().to_path_buf();
        let store = Arc::new(MemoryStore::new(vec![
            ("_peerdav_mirror/docs/../escape.txt", b"escaped".to_vec()),
            ("_peerdav_mirror/docs//absolute.txt", b"absolute".to_vec()),
            ("_peerdav_mirror/docs/valid.txt", b"valid".to_vec()),
        ]));
        let mapping = mapping(&local, "docs");
        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.downloaded, 1);
        assert!(!base.join("escape.txt").exists());
        assert!(!std::path::Path::new("/absolute.txt").exists());
        assert!(
            result
                .failures
                .iter()
                .any(|failure| failure.path == "../escape.txt")
        );
        assert!(
            result
                .failures
                .iter()
                .any(|failure| failure.path == "/absolute.txt")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn changing_the_cloud_prefix_resets_the_baseline_instead_of_deleting_local_files() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "keep me");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        let changed = MirrorMapping {
            cloud_prefix: "work".to_owned(),
            ..mapping.clone()
        };
        let result = run_sync(
            store.clone(),
            &changed,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.deleted_local, 0);
        assert_eq!(result.uploaded, 1);
        assert!(local.join("a.txt").exists());
        assert_eq!(store.get_value("_peerdav_mirror/work/a.txt"), b"keep me");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn incomplete_scan_disables_deletion_propagation() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "keep");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        // 「weird.」在本平台可创建但镜像不支持，扫描会报告不完整。
        std::fs::write(local.join("weird."), b"x").unwrap();
        std::fs::remove_file(local.join("a.txt")).unwrap();

        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.deleted_cloud, 0);
        assert_eq!(store.get_value("_peerdav_mirror/docs/a.txt"), b"keep");
        assert!(
            result
                .failures
                .iter()
                .any(|failure| failure.path == "weird.")
        );
        assert!(result.failures.iter().any(|failure| failure.path == "*"));
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn mass_delete_requires_explicit_confirmation() {
        let (local, data) = test_dirs();
        for index in 0..10 {
            write(&local, &format!("file-{index}.txt"), "x");
        }
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        for index in 0..10 {
            std::fs::remove_file(local.join(format!("file-{index}.txt"))).unwrap();
        }

        let error = run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            MirrorError::MassDelete {
                planned: 10,
                total: 10
            }
        ));
        assert_eq!(store.keys().len(), 10);

        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &forced_context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.deleted_cloud, 10);
        assert!(store.keys().is_empty());
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn local_delete_with_cloud_change_restores_the_file() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "v1");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        run_sync(
            store.clone(),
            &mapping,
            &data,
            &context(),
            Arc::clone(&cancel),
        )
        .await
        .unwrap();

        // 本地删除，同时云端被另一台设备修改。
        std::fs::remove_file(local.join("a.txt")).unwrap();
        store.insert("_peerdav_mirror/docs/a.txt", b"v2".to_vec(), "etag-v2");

        let result = run_sync(store.clone(), &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.restored, 1);
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"v2");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }
}
