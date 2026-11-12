use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use futures_util::{StreamExt, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cloud::traits::ObjectStore;
use crate::cloud::types::{CloudError, ObjectMetadata, PutCondition};
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
    pub confirmed_plan: Option<String>,
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

/// Read and validate a bounded local snapshot before publishing. Plain mirrors
/// use ordinary S3 keys with a last-moment remote snapshot check, not CAS.
async fn upload_file(
    store: &dyn ObjectStore,
    path: &Path,
    key: &str,
    expected: Option<&CloudFile>,
    scanned: &LocalFile,
    cancel: Arc<AtomicBool>,
) -> Result<ObjectMetadata, MirrorError> {
    if scanned.size > crate::cloud::types::MAX_PUT_BYTES as u64 {
        return Err(CloudError::PayloadTooLarge.into());
    }
    canceled(&cancel)?;
    let parent_path = path
        .parent()
        .ok_or_else(|| MirrorError::UnsafePath("missing parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| MirrorError::UnsafePath("missing file name".into()))?;
    let parent = scan::open_root_nofollow(parent_path)?;
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = parent.open_with(name, &options)?;
    if !file.metadata()?.is_file() || !local_state_matches(path, scanned) {
        return Err(CloudError::Precondition.into());
    }
    let mut file = tokio::fs::File::from_std(file.into_std());
    // Snapshot and validate before exposing any bytes to a mutating request.
    let mut data = Vec::new();
    loop {
        canceled(&cancel)?;
        let mut chunk = vec![0; READ_CHUNK];
        let count = file.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        if data.len().saturating_add(count) > crate::cloud::types::MAX_PUT_BYTES {
            return Err(CloudError::PayloadTooLarge.into());
        }
        data.extend_from_slice(&chunk[..count]);
    }
    if data.len() as u64 != scanned.size || !local_state_matches(path, scanned) {
        return Err(CloudError::Precondition.into());
    }
    verify_remote_snapshot(store, key, expected).await?;
    canceled(&cancel)?;
    let body = stream::once(async move { Ok(data.into()) });
    store
        .put(key, Box::pin(body), PutCondition::Any)
        .await
        .map_err(MirrorError::from)
}

async fn verify_remote_snapshot(
    store: &dyn ObjectStore,
    key: &str,
    expected: Option<&CloudFile>,
) -> Result<(), CloudError> {
    match (store.stat(key).await, expected) {
        (Err(CloudError::NotFound), None) => Ok(()),
        (Ok(current), Some(expected))
            if expected.etag.as_deref().is_some_and(|etag| !etag.is_empty())
                && current.etag == expected.etag && current.size == expected.size => Ok(()),
        (Err(CloudError::NotFound), Some(_)) | (Ok(_), _) => Err(CloudError::Precondition),
        (Err(error), _) => Err(error),
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
    expected_cloud: &CloudFile,
    expected_local: Option<&LocalFile>,
    cancel: &AtomicBool,
) -> Result<(i128, u64), MirrorError> {
    types::validate_relative_str(relative)
        .map_err(|reason| MirrorError::UnsafePath(format!("{relative}: {reason}")))?;
    let relative_path = Path::new(relative);
    async fn verify_cloud(
        store: &dyn ObjectStore,
        key: &str,
        expected: &CloudFile,
    ) -> Result<(), MirrorError> {
        let current = store.stat(key).await?;
        if expected.etag.is_none() || current.etag != expected.etag || current.size != expected.size
        {
            return Err(CloudError::Precondition.into());
        }
        Ok(())
    }
    let expected = expected_cloud;
    verify_cloud(store, key, expected).await?;
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
    let mut received = 0_u64;
    let result = (async {
        while let Some(chunk) = body.next().await {
            canceled(cancel)?;
            let chunk = chunk.map_err(MirrorError::from)?;
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or(CloudError::PayloadTooLarge)?;
            if received > expected.size {
                return Err(CloudError::Precondition.into());
            }
            file.write_all(&chunk).await.map_err(MirrorError::from)?;
        }
        if received != expected.size {
            return Err(CloudError::Precondition.into());
        }
        verify_cloud(store, key, expected).await?;
        canceled(cancel)?;
        file.sync_all().await.map_err(MirrorError::from)?;
        Ok::<(), MirrorError>(())
    })
    .await;
    drop(file);
    if let Err(error) = result {
        let _ = parent.remove_file(&temp_name);
        return Err(error);
    }
    let name = relative_path
        .file_name()
        .ok_or_else(|| MirrorError::UnsafePath(relative.into()))?;
    let publish = (|| {
        if let Some(scanned) = expected_local {
            if !local_state_matches(&path, scanned) {
                return Err(MirrorError::Cloud(CloudError::Precondition));
            }
            preserve_local(&parent, name)?;
        }
        rename_noreplace(&parent, std::ffi::OsStr::new(&temp_name), name)
    })();
    if let Err(error) = publish {
        if let Err(cleanup) = parent.remove_file(&temp_name) {
            log::warn!("mirror temporary cleanup failed: {cleanup:?}");
        }
        return Err(error);
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

/// Atomic no-clobber publication anchored to the opened directory, never a pathname rename.
fn rename_noreplace(
    parent: &Dir,
    from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
) -> Result<(), MirrorError> {
    #[cfg(target_os = "linux")]
    {
        rustix::fs::renameat_with(parent, from, parent, to, rustix::fs::RenameFlags::NOREPLACE)
            .map_err(std::io::Error::from)?;
        parent.open(".")?.sync_all()?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (parent, from, to);
        Err(CloudError::Unsupported.into())
    }
}

/// Move the actual entry (not a copied snapshot) into a unique recovery file.
/// Concurrent writers retain their inode and their writes remain recoverable.
fn preserve_local(parent: &Dir, name: &std::ffi::OsStr) -> Result<(), MirrorError> {
    let recovery = format!(
        "{}{}recovery-{}",
        name.to_string_lossy(),
        types::CONFLICT_MARKER,
        hex::encode(rand::random::<[u8; 16]>())
    );
    rename_noreplace(parent, name, std::ffi::OsStr::new(&recovery))
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
    meta.is_file() && mtime_ns == scanned.mtime_ns && meta.len() == scanned.size
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
/// 扫描或列举不完整时关闭删除传播；首次接入与异常删除必须确认当前计划。
pub async fn run_sync(
    store: Arc<dyn ObjectStore>,
    mapping: &MirrorMapping,
    data_dir: &Path,
    context: &MirrorRunContext,
    cancel: Arc<AtomicBool>,
) -> Result<MirrorSyncResult, MirrorError> {
    run_sync_mode(store, mapping, data_dir, context, cancel, true).await
}

pub(crate) async fn run_sync_mode(
    store: Arc<dyn ObjectStore>,
    mapping: &MirrorMapping,
    data_dir: &Path,
    context: &MirrorRunContext,
    cancel: Arc<AtomicBool>,
    manual: bool,
) -> Result<MirrorSyncResult, MirrorError> {
    let root = std::fs::canonicalize(&mapping.local_root)
        .map_err(|_| MirrorError::InvalidRoot(mapping.local_root.display().to_string()))?;
    let root_handle = scan::open_root_nofollow(&mapping.local_root)?;
    #[cfg(unix)]
    let root_identity = {
        use cap_std::fs::MetadataExt;
        let metadata = root_handle.dir_metadata()?;
        Some((metadata.dev(), metadata.ino()))
    };
    #[cfg(not(unix))]
    let root_identity = None;
    let identity = MirrorIdentity {
        connection_id: mapping.connection_id.clone(),
        endpoint: context.endpoint.clone(),
        bucket: context.bucket.clone(),
        cloud_prefix: mapping.cloud_prefix.clone(),
        local_root: root.display().to_string(),
        root_identity,
    };
    let mut baseline = baseline::load(data_dir, &mapping.id)?;
    if baseline.safety_version != 1 || baseline.identity.as_ref() != Some(&identity) {
        // A configured target change starts a new merge, never reuses deletion history.
        // Replacement of the same configured folder is not an intentional rebind.
        let replaced_root = baseline.identity.as_ref().is_some_and(|previous| {
            previous.connection_id == identity.connection_id
                && previous.endpoint == identity.endpoint
                && previous.bucket == identity.bucket
                && previous.cloud_prefix == identity.cloud_prefix
                && previous.local_root == identity.local_root
                && previous.root_identity != identity.root_identity
        });
        if baseline.safety_version == 1 && replaced_root {
            return Err(MirrorError::InvalidRoot(
                "目录实体已变化，请检查挂载或重新绑定目录 / Folder identity changed; check the mount or rebind the folder".into(),
            ));
        }
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
            safety_version: 1,
            initialized: false,
            identity: Some(identity.clone()),
            entries: BTreeMap::new(),
        };
    }

    // Exclusions are not user deletions. Retire only their comparison state;
    // never delete or restore existing local/remote temporary objects.
    baseline.retain(|path| !types::is_artifact(path));
    let mut result = MirrorSyncResult::default();
    let scan::ScanReport {
        files: local,
        skipped,
    } = scan::scan(&mapping.local_root, &cancel)?;
    let mut complete = skipped.is_empty();
    for (path, reason) in skipped {
        result.failures.push(MirrorFailure { path, reason });
    }
    let scope = format!("{}|{}|{}", context.endpoint, context.bucket, mapping.cloud_prefix);
    let ledger_root = format!("{}ledger/", types::mirror_root_prefix(&mapping.cloud_prefix));
    let writer_prefix = format!("{ledger_root}writers/");
    let writer = hex::encode(sha2::Sha256::digest(format!("{}|{}", data_dir.display(), mapping.id)));
    let writer_key = format!("{writer_prefix}{writer}.json");
    let snapshot_key = format!("{ledger_root}snapshots/{writer}.json");
    let mut cache = super::ledger::load(data_dir, &mapping.id, &scope)?;
    let (markers, mut healthy) = super::ledger::markers(store.as_ref(), &writer_prefix, &cancel).await?;
    // Only validate our own snapshot; another active writer may be publishing its own.
    // Stream size is bounded, and this is ledger I/O, never a full data listing.
    let own_snapshot_valid = super::ledger::snapshot_valid(store.as_ref(), &snapshot_key).await?;
    healthy &= own_snapshot_valid;
    let full = manual || !healthy || markers.is_empty() || cache.scope != scope
        || markers != cache.markers
        || (mapping.full_check && now_secs().saturating_sub(cache.checked_at) >= super::ledger_types::FULL_CHECK_SECONDS);
    let listed = if full {
        list_cloud(store.as_ref(), &mapping.cloud_prefix, &cancel).await?
    } else {
        cache.files.clone()
    };
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
    // Repair is a read-only reconciliation round: never apply file changes while
    // rebuilding an absent or damaged ledger. The next tick plans from repaired state.
    if baseline.initialized && (!own_snapshot_valid || markers.get(&writer_key).is_none_or(String::is_empty)) && complete {
        let mut nonce = [0u8; 16];
        rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut nonce)
            .map_err(|error| MirrorError::Baseline(error.to_string()))?;
        let token = hex::encode(nonce);
        super::ledger::mark(store.as_ref(), &writer_key, &token, true).await?;
        super::ledger::snapshot(store.as_ref(), &snapshot_key, &cloud).await?;
        super::ledger::mark(store.as_ref(), &writer_key, &token, false).await?;
        let (after, valid) = super::ledger::markers(store.as_ref(), &writer_prefix, &cancel).await?;
        let stable = markers.iter().filter(|(key, _)| *key != &writer_key)
            .eq(after.iter().filter(|(key, _)| *key != &writer_key));
        if valid && stable {
            super::ledger::save(data_dir, &mapping.id, &super::ledger_types::RemoteCache {
                scope, markers: after, files: cloud, checked_at: now_secs(),
            })?;
        }
        return Ok(result);
    }
    // Unknown state is never a common baseline, even for equal-sized files.
    // The planner preserves both versions as a conflict instead of inferring equality.
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
    let total = local.len().max(cloud.len()).max(baseline.entries.len());
    let emptied_side = (local.is_empty()
        && actions
            .iter()
            .any(|action| matches!(action, MirrorAction::DeleteCloud(_))))
        || (cloud.is_empty()
            && actions
                .iter()
                .any(|action| matches!(action, MirrorAction::DeleteLocal(_))));
    let unusual_deletion = emptied_side
        || deletions > types::MASS_DELETE_ABSOLUTE
        || (deletions >= types::MASS_DELETE_MINIMUM
            && deletions.saturating_mul(100) > total.saturating_mul(types::MASS_DELETE_PERCENT));
    use sha2::Digest;
    let plan_id = hex::encode(sha2::Sha256::digest(format!(
        "{identity:?}|{baseline:?}|{local:?}|{cloud:?}|{actions:?}"
    )));
    let confirmed = context.confirmed_plan.as_deref() == Some(plan_id.as_str());
    let initial_merge = !baseline.initialized && baseline.entries.is_empty();
    if !actions.is_empty()
        && !confirmed
        && (initial_merge || (context.confirmed_plan.is_some() && deletions == 0))
    {
        return Err(MirrorError::ConfirmMerge {
            plan_id,
            paths: actions.iter().map(|action| format!("{action:?}")).collect(),
        });
    }
    // Normal baseline-backed deletions propagate automatically, including timer runs.
    // Exceptional deletions still require the exact plan; `force` alone is insufficient.
    if deletions > 0
        && (unusual_deletion || context.confirmed_plan.is_some())
        && !(context.force_mass_delete && confirmed)
    {
        return Err(MirrorError::MassDelete {
            planned: deletions,
            total,
            plan_id,
            paths: actions
                .iter()
                .filter(|action| {
                    matches!(
                        action,
                        MirrorAction::DeleteCloud(_) | MirrorAction::DeleteLocal(_)
                    )
                })
                .map(|action| format!("{action:?}"))
                .collect(),
        });
    }

    let mut nonce = [0u8; 16];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut nonce)
        .map_err(|error| MirrorError::Baseline(error.to_string()))?;
    let token = hex::encode(nonce);
    let mutating = !actions.is_empty();
    if mutating || full {
        super::ledger::mark(store.as_ref(), &writer_key, &token, true).await?;
    }
    let machine = machine_label();
    let stamp = types::utc_stamp(now_secs());

    for action in actions {
        canceled(&cancel)?;
        let current_root = scan::open_root_nofollow(&mapping.local_root)?;
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            let metadata = current_root.dir_metadata()?;
            if Some((metadata.dev(), metadata.ino())) != identity.root_identity {
                return Err(MirrorError::InvalidRoot(
                    "folder identity changed during sync".into(),
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = current_root;
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
                match upload_file(
                    store.as_ref(),
                    &abs,
                    &key,
                    cloud.get(&path),
                    scanned,
                    Arc::clone(&cancel),
                )
                .await
                {
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
                let Some(expected_cloud) = cloud.get(&path) else {
                    continue;
                };
                match download_file(
                    store.as_ref(),
                    &mapping.local_root,
                    &path,
                    &key,
                    expected_cloud,
                    local.get(&path),
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
                if mapping.local_root.join(&path).try_exists()? {
                    result.failures.push(MirrorFailure { path, reason: "本地文件已重新出现，取消云端删除 / Local file reappeared; cloud deletion canceled".into() });
                    continue;
                }
                let key = types::cloud_key(&mapping.cloud_prefix, &path);
                let deletion = match verify_remote_snapshot(store.as_ref(), &key, cloud.get(&path)).await {
                    Ok(()) => {
                        canceled(&cancel)?;
                        if mapping.local_root.join(&path).try_exists()? {
                            Err(CloudError::Precondition)
                        } else {
                            store.delete(&key).await
                        }
                    }
                    Err(error) => Err(error),
                };
                match deletion {
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
                let key = types::cloud_key(&mapping.cloud_prefix, &path);
                match store.stat(&key).await {
                    Err(CloudError::NotFound) => {}
                    Ok(_) => {
                        result.failures.push(MirrorFailure { path, reason: "云端文件已重新出现，取消本地删除 / Cloud file reappeared; local deletion canceled".into() });
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
                let abs = mapping.local_root.join(&path);
                let deletion = (|| {
                    let scanned = local.get(&path).ok_or(CloudError::Precondition)?;
                    if !local_state_matches(&abs, scanned) {
                        return Err(MirrorError::Cloud(CloudError::Precondition));
                    }
                    let relative = Path::new(&path);
                    let parent = open_parent_dir(&mapping.local_root, relative)?;
                    let name = relative
                        .file_name()
                        .ok_or_else(|| MirrorError::UnsafePath(path.clone()))?;
                    preserve_local(&parent, name)
                })();
                match deletion {
                    Ok(()) => {
                        baseline.remove(&path);
                        result.deleted_local += 1;
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
                let Some(expected_cloud) = cloud.get(&path) else {
                    continue;
                };
                if let Err(error) = download_file(
                    store.as_ref(),
                    &mapping.local_root,
                    &conflict_relative,
                    &key,
                    expected_cloud,
                    None,
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
                match upload_file(
                    store.as_ref(),
                    &abs,
                    &key,
                    cloud.get(&path),
                    scanned,
                    Arc::clone(&cancel),
                )
                .await
                {
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
    if complete {
        baseline.initialized = true;
    }
    baseline::save(data_dir, &mapping.id, &baseline)?;
    if (mutating || full) && result.failures.is_empty() {
        let files = if mutating { list_cloud(store.as_ref(), &mapping.cloud_prefix, &cancel).await? } else { cloud };
        super::ledger::snapshot(store.as_ref(), &format!("{ledger_root}snapshots/{writer}.json"), &files).await?;
        super::ledger::mark(store.as_ref(), &writer_key, &token, false).await?;
        let (after, healthy) = super::ledger::markers(store.as_ref(), &writer_prefix, &cancel).await?;
        let stable = markers.iter().filter(|(key, _)| *key != &writer_key).eq(after.iter().filter(|(key, _)| *key != &writer_key));
        if healthy && stable {
            let checked_at = if full { now_secs() } else { cache.checked_at };
            cache = super::ledger_types::RemoteCache { scope, markers: after, files, checked_at };
            super::ledger::save(data_dir, &mapping.id, &cache)?;
        }
    }
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
        listings: Mutex<Vec<String>>,
        conditional_delete: bool,
        truncated_read: bool,
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
                listings: Mutex::new(Vec::new()),
                conditional_delete: true,
                truncated_read: false,
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
                .filter(|key| !key.contains("/ledger/"))
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
            StorageCapabilities {
                create_only: true,
                conditional_replace: true,
                conditional_writes_verified: true,
                ..Default::default()
            }
        }
        fn list<'a>(&'a self, prefix: &'a str) -> crate::cloud::types::ObjectListing<'a> {
            self.listings.lock().unwrap().push(prefix.into());
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
            let truncated = self.truncated_read;
            Box::pin(async move {
                match entry {
                    Some(mut value) => {
                        if truncated {
                            value.truncate(value.len().saturating_sub(1));
                        }
                        Ok(stream::once(async move { Ok(Bytes::from(value)) }).boxed())
                    }
                    None => Err(CloudError::NotFound),
                }
            })
        }
        fn put<'a>(
            &'a self,
            key: &'a str,
            mut body: crate::cloud::types::ObjectStream,
            condition: PutCondition,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                use sha2::Digest;
                let mut data = Vec::new();
                while let Some(chunk) = body.next().await {
                    data.extend_from_slice(&chunk?);
                }
                let etag = hex::encode(sha2::Sha256::digest(&data));
                let mut objects = self.objects.lock().unwrap();
                match condition {
                    PutCondition::CreateOnly if objects.contains_key(key) => {
                        return Err(CloudError::Precondition);
                    }
                    PutCondition::MatchEtag(ref expected)
                        if objects.get(key).map(|(_, etag)| etag) != Some(expected) =>
                    {
                        return Err(CloudError::Precondition);
                    }
                    _ => {}
                }
                objects.insert(key.into(), (data.clone(), etag.clone()));
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
        fn delete_if_match<'a>(
            &'a self,
            key: &'a str,
            etag: &'a str,
        ) -> BoxFuture<'a, Result<(), CloudError>> {
            Box::pin(async move {
                if !self.conditional_delete {
                    return Err(CloudError::Unsupported);
                }
                let mut objects = self.objects.lock().unwrap();
                if objects.get(key).map(|(_, current)| current.as_str()) != Some(etag) {
                    return Err(CloudError::Precondition);
                }
                objects.remove(key);
                Ok(())
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
            bucket: None,
            local_root: local_root.to_path_buf(),
            cloud_prefix: cloud_prefix.to_owned(),
            full_check: false,
            schedule: Default::default(),
        }
    }

    #[tokio::test]
    async fn ledger_fast_tick_repairs_missing_markers_and_manual_check_finds_external_edits() {
        let (root, data) = test_dirs();
        let mapping = mapping(&root, "docs");
        write(&root, "a.txt", "local");
        let store = Arc::new(MemoryStore::new(vec![]));
        run_sync(store.clone(), &mapping, &data, &context(), Arc::new(AtomicBool::new(false))).await.unwrap();
        let ctx = context();
        store.insert("_peerdav_mirror/docs/files/external.txt", b"external".to_vec(), "external-v1");
        store.listings.lock().unwrap().clear();
        let result = super::run_sync_mode(store.clone(), &mapping, &data, &ctx, Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert!(store.listings.lock().unwrap().iter().all(|prefix| !prefix.ends_with("/files/")), "steady tick must not enumerate remote files");
        assert_eq!(result.downloaded, 0, "external edits require the optional full check");
        assert!(!root.join("external.txt").exists());
        let result = run_sync(store.clone(), &mapping, &data, &ctx, Arc::new(AtomicBool::new(false))).await.unwrap();
        assert_eq!(result.downloaded, 1);
        store.objects.lock().unwrap().retain(|key, _| !key.contains("/ledger/"));
        super::run_sync_mode(store.clone(), &mapping, &data, &ctx, Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert!(store.objects.lock().unwrap().keys().any(|key| key.contains("/ledger/writers/")));
        let key = store.objects.lock().unwrap().keys().find(|key| key.contains("/ledger/writers/")).cloned().unwrap();
        store.insert(&key, b"corrupt".to_vec(), "broken");
        super::run_sync_mode(store.clone(), &mapping, &data, &ctx, Arc::new(AtomicBool::new(false)), false).await.unwrap();
        let state: super::super::ledger_types::WriterState = serde_json::from_slice(&store.get_value(&key)).unwrap();
        assert!(!state.pending);
        let snapshot = key.replacen("/writers/", "/snapshots/", 1);
        store.insert(&snapshot, b"corrupt snapshot".to_vec(), "broken-snapshot");
        write(&root, "a.txt", "deferred local change");
        let repair = super::run_sync_mode(store.clone(), &mapping, &data, &ctx, Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert_eq!(repair.uploaded, 0, "repair round freezes file mutations");
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"local");
        super::run_sync_mode(store.clone(), &mapping, &data, &ctx, Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"deferred local change");
        let mut enabled = mapping.clone(); enabled.full_check = true;
        let scope = format!("{}|{}|{}", ctx.endpoint, ctx.bucket, mapping.cloud_prefix);
        let mut cache = super::super::ledger::load(&data, &mapping.id, &scope).unwrap();
        cache.checked_at = now_secs().saturating_sub(600);
        super::super::ledger::save(&data, &mapping.id, &cache).unwrap();
        store.insert("_peerdav_mirror/docs/files/timed.txt", b"timed".to_vec(), "timed-v1");
        let result = super::run_sync_mode(store.clone(), &enabled, &data, &ctx, Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert_eq!(result.downloaded, 1, "opt-in full check runs at the ten-minute boundary");
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"deferred local change");
    }

    #[tokio::test]
    async fn interrupted_writer_forces_reconciliation_without_overwriting_other_writer_marker() {
        let (root, data) = test_dirs();
        let mapping = mapping(&root, "docs");
        write(&root, "a.txt", "local");
        let store = Arc::new(MemoryStore::new(vec![]));
        run_sync(store.clone(), &mapping, &data, &context(), Arc::new(AtomicBool::new(false))).await.unwrap();
        let other = "_peerdav_mirror/docs/ledger/writers/other.json";
        super::super::ledger::mark(store.as_ref(), other, "interrupted", true).await.unwrap();
        let before = store.get_value(other);
        store.insert("_peerdav_mirror/docs/files/other.txt", b"remote".to_vec(), "remote-v1");
        let result = super::run_sync_mode(store.clone(), &mapping, &data, &context(), Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert_eq!(result.downloaded, 1, "another offline writer must not freeze this client's sync indefinitely");
        assert_eq!(store.get_value(other), before, "never replace another writer's live marker");
        assert!(root.join("other.txt").exists());
        super::super::ledger::mark(store.as_ref(), other, "committed", false).await.unwrap();
        let result = super::run_sync_mode(store.clone(), &mapping, &data, &context(), Arc::new(AtomicBool::new(false)), false).await.unwrap();
        assert_eq!(result.downloaded, 0);
        assert_eq!(std::fs::read(root.join("other.txt")).unwrap(), b"remote");
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
            confirmed_plan: None,
        }
    }

    // Existing behavioral fixtures explicitly accept their computed plans. Safety
    // regressions call super::run_sync directly to exercise the refusal boundary.
    async fn run_sync(
        store: Arc<dyn ObjectStore>,
        mapping: &MirrorMapping,
        data: &Path,
        context: &MirrorRunContext,
        cancel: Arc<AtomicBool>,
    ) -> Result<MirrorSyncResult, MirrorError> {
        let first = super::run_sync(store.clone(), mapping, data, context, cancel.clone()).await;
        let plan_id = match first {
            Err(MirrorError::ConfirmMerge { plan_id, .. }) => plan_id,
            Err(MirrorError::MassDelete { plan_id, .. }) if context.force_mass_delete => plan_id,
            result => return result,
        };
        let approved = MirrorRunContext {
            confirmed_plan: Some(plan_id),
            endpoint: context.endpoint.clone(),
            bucket: context.bucket.clone(),
            force_mass_delete: context.force_mass_delete,
        };
        super::run_sync(store, mapping, data, &approved, cancel).await
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
    async fn temporary_files_never_transfer_or_propagate_old_baseline_deletes() {
        let local = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(local.path(), "keep.txt", "keep");
        let mapping = mapping(local.path(), "docs");
        let store = Arc::new(MemoryStore::new(vec![]));
        run_sync(store.clone(), &mapping, data.path(), &context(), Arc::new(AtomicBool::new(false))).await.unwrap();
        write(local.path(), ".notes.swp", "editor");
        write(local.path(), "local.tmp", "temporary local");
        write(local.path(), "folder.part/inside.txt", "temporary subtree");
        write(local.path(), "report.tmp.txt", "normal");
        // Simulate a pre-upgrade baseline: each side has lost a different old temporary file.
        let mut old = baseline::load(data.path(), &mapping.id).unwrap();
        for name in ["remote.part", "local.tmp"] {
            old.record_upload(name, 0, 5, Some("old".into()), 5);
        }
        baseline::save(data.path(), &mapping.id, &old).unwrap();
        store.objects.lock().unwrap().insert("_peerdav_mirror/docs/files/remote.part".into(), (b"remote".to_vec(), "old".into()));
        for _ in 0..2 {
            let result = super::run_sync(store.clone(), &mapping, data.path(), &context(), Arc::new(AtomicBool::new(false))).await.unwrap();
            assert!(result.failures.is_empty(), "{:?}", result.failures);
            assert_eq!((result.deleted_local, result.deleted_cloud, result.downloaded), (0, 0, 0));
            assert!(result.conflicts.is_empty());
        }
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/remote.part"), b"remote");
        assert!(!store.keys().contains(&"_peerdav_mirror/docs/files/local.tmp".into()));
        assert!(!store.keys().iter().any(|key| key.contains("folder.part") || key.ends_with(".swp")));
        assert!(!local.path().join("remote.part").exists());
        assert_eq!(std::fs::read(local.path().join("local.tmp")).unwrap(), b"temporary local");
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/report.tmp.txt"), b"normal");
    }

    #[tokio::test]
    async fn first_sync_uploads_and_downloads_then_reaches_steady_state() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "hello");
        write(&local, "sub/b.txt", "world");
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/files/remote.txt",
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
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"hello");
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/sub/b.txt"), b"world");
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
        write(&local, "anchor.txt", "anchor");
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/files/keep.txt",
            b"keep".to_vec(),
        )]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));

        // 第一轮建立三个文件的共同基线。
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
                .contains(&"_peerdav_mirror/docs/files/gone.txt".to_owned())
        );

        // 本地删除 gone.txt → 可信基线下自动传播，不携带确认令牌。
        std::fs::remove_file(local.join("gone.txt")).unwrap();
        let result = super::run_sync(
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
                .contains(&"_peerdav_mirror/docs/files/gone.txt".to_owned())
        );

        // 云端删除 keep.txt → 同步应删除本地副本。
        store
            .objects
            .lock()
            .expect("test store lock")
            .remove("_peerdav_mirror/docs/files/keep.txt");
        let result = super::run_sync(store.clone(), &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.deleted_local, 1);
        assert!(!local.join("keep.txt").exists());
        let recovery = std::fs::read_dir(&local)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("keep.txt.conflict.recovery-")
            })
            .unwrap();
        assert_eq!(std::fs::read(recovery).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn scheduled_mirror_runs_propagate_normal_deletions_but_refuse_empty_side() {
        use crate::cloud::runtime_schedule::{ScheduleHandle, TickSource};
        use std::time::Duration;

        for empty_side in [false, true] {
            let (local, data) = test_dirs();
            write(&local, "gone.txt", "gone");
            if !empty_side {
                write(&local, "keep.txt", "keep");
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
            std::fs::remove_file(local.join("gone.txt")).unwrap();
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let source: TickSource = {
                let store = store.clone();
                let data = data.clone();
                Arc::new(move |_| {
                    let store = store.clone();
                    let mapping = mapping.clone();
                    let data = data.clone();
                    let sender = sender.clone();
                    Box::pin(async move {
                        // Timer entry points never supply force or a confirmation token.
                        let result = super::run_sync(
                            store,
                            &mapping,
                            &data,
                            &context(),
                            Arc::new(AtomicBool::new(false)),
                        )
                        .await
                        .map_err(crate::AppError::from);
                        let notification = match &result {
                            Ok(result) => (result.deleted_cloud, None),
                            Err(error) => (0, Some(error.code.clone())),
                        };
                        sender.send(notification).unwrap();
                        result.map(|_| ())
                    })
                })
            };
            let schedule =
                ScheduleHandle::start_for_test("mirror:test", Duration::from_millis(20), source);
            let outcome = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            schedule.stop_and_join().await.unwrap();
            if empty_side {
                assert_eq!(outcome, (0, Some("MIRROR_MASS_DELETE".into())));
                assert_eq!(store.get_value("_peerdav_mirror/docs/files/gone.txt"), b"gone");
            } else {
                assert_eq!(outcome, (1, None));
                assert!(
                    !store
                        .keys()
                        .contains(&"_peerdav_mirror/docs/files/gone.txt".into())
                );
            }
            std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
        }
    }

    #[tokio::test]
    async fn empty_initialized_folders_do_not_repeat_initial_confirmation() {
        let (local, data) = test_dirs();
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));
        super::run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
            .await
            .unwrap();
        assert!(baseline::load(&data, &mapping.id).unwrap().initialized);
        write(&local, "new.txt", "new");
        let result = super::run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
            .await
            .unwrap();
        assert_eq!(result.uploaded, 1);
        // Both sides intentionally removed the only file: the empty state stays initialized.
        std::fs::remove_file(local.join("new.txt")).unwrap();
        store
            .objects
            .lock()
            .unwrap()
            .remove("_peerdav_mirror/docs/files/new.txt");
        super::run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
            .await
            .unwrap();
        assert!(
            baseline::load(&data, &mapping.id)
                .unwrap()
                .entries
                .is_empty()
        );
        store.insert(
            "_peerdav_mirror/docs/files/remote.txt",
            b"remote".to_vec(),
            "remote-v1",
        );
        let result = super::run_sync(store, &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.downloaded, 1);
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_suddenly_empty_side_requires_confirmation_in_both_directions() {
        for delete_local in [true, false] {
            let (local, data) = test_dirs();
            write(&local, "precious.txt", "keep");
            let store = Arc::new(MemoryStore::new(vec![]));
            let mapping = mapping(&local, "docs");
            let cancel = Arc::new(AtomicBool::new(false));
            run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
                .await
                .unwrap();
            if delete_local {
                std::fs::remove_file(local.join("precious.txt")).unwrap();
            } else {
                store
                    .objects
                    .lock()
                    .unwrap()
                    .remove("_peerdav_mirror/docs/files/precious.txt");
            }
            for context in [context(), forced_context()] {
                let result =
                    super::run_sync(store.clone(), &mapping, &data, &context, cancel.clone()).await;
                assert!(matches!(
                    result,
                    Err(MirrorError::MassDelete {
                        planned: 1,
                        total: 1,
                        ..
                    })
                ));
            }
            if delete_local {
                assert_eq!(
                    store.get_value("_peerdav_mirror/docs/files/precious.txt"),
                    b"keep"
                );
            } else {
                assert_eq!(std::fs::read(local.join("precious.txt")).unwrap(), b"keep");
            }
            std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
        }
    }

    #[tokio::test]
    async fn deletion_thresholds_allow_normal_batches_and_pause_unusual_batches() {
        for (count, removed, needs_confirmation) in [
            (12, 9, false),
            (40, 10, false),
            (39, 10, true),
            (500, 100, false),
            (500, 101, true),
        ] {
            let (local, data) = test_dirs();
            for index in 0..count {
                write(&local, &format!("file-{index}.txt"), "keep");
            }
            let store = Arc::new(MemoryStore::new(vec![]));
            let mapping = mapping(&local, "docs");
            let cancel = Arc::new(AtomicBool::new(false));
            run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
                .await
                .unwrap();
            for index in 0..removed {
                std::fs::remove_file(local.join(format!("file-{index}.txt"))).unwrap();
            }
            let result = super::run_sync(store.clone(), &mapping, &data, &context(), cancel).await;
            if needs_confirmation {
                assert!(
                    matches!(result, Err(MirrorError::MassDelete { planned, total, .. }) if planned == removed && total == count)
                );
                assert_eq!(store.keys().len(), count);
            } else {
                let result = result.unwrap();
                assert_eq!(result.deleted_cloud, removed as u64);
                assert!(result.failures.is_empty());
                assert_eq!(store.keys().len(), count - removed);
            }
            std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
        }
    }

    #[tokio::test]
    async fn changing_the_local_folder_requires_a_new_merge() {
        let (local, data) = test_dirs();
        write(&local, "old.txt", "old");
        let store = Arc::new(MemoryStore::new(vec![]));
        let original = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));
        run_sync(store.clone(), &original, &data, &context(), cancel.clone())
            .await
            .unwrap();
        let new_root = local.with_file_name("new-local");
        std::fs::create_dir(&new_root).unwrap();
        write(&new_root, "new.txt", "new");
        let changed = MirrorMapping {
            local_root: new_root.clone(),
            ..original
        };
        let result =
            super::run_sync(store.clone(), &changed, &data, &context(), cancel.clone()).await;
        let Err(MirrorError::ConfirmMerge { plan_id, .. }) = result else {
            panic!("a changed local folder must require a new merge");
        };
        assert_eq!(store.keys().len(), 1);
        assert!(!new_root.join("old.txt").exists());
        let approved = MirrorRunContext {
            confirmed_plan: Some(plan_id),
            ..context()
        };
        let result = super::run_sync(store.clone(), &changed, &data, &approved, cancel)
            .await
            .unwrap();
        assert_eq!(result.uploaded, 1);
        assert_eq!(result.downloaded, 1);
        assert_eq!(result.deleted_cloud + result.deleted_local, 0);
        assert_eq!(std::fs::read(local.join("old.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(new_root.join("old.txt")).unwrap(), b"old");
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
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
            "_peerdav_mirror/docs/files/a.txt",
            b"remote-v2".to_vec(),
            "etag-other",
        );

        let result = run_sync(store.clone(), &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.conflicts, vec!["a.txt".to_owned()]);
        // 本地版本接管原路径，云端版本另存为本地冲突副本，两侧都保留且基线一致。
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"local-v2!");
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"local-v2!");
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
            "_peerdav_mirror/docs/files/a.txt",
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
            .remove("_peerdav_mirror/docs/files/a.txt");

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
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"v2-local");
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"v2-local");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn equal_size_different_files_preserve_both_versions_on_first_sync() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "local");
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/files/a.txt",
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
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.downloaded, 0);
        assert_eq!(result.conflicts, vec!["a.txt"]);
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"local");
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"local");
        let copies: Vec<_> = std::fs::read_dir(&local)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(types::CONFLICT_MARKER)
            })
            .collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read(&copies[0]).unwrap(), b"cloud");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[tokio::test]
    async fn unsafe_cloud_keys_are_skipped_without_writing_outside_the_root() {
        let (local, data) = test_dirs();
        let base = local.parent().unwrap().to_path_buf();
        let store = Arc::new(MemoryStore::new(vec![
            ("_peerdav_mirror/docs/files/../escape.txt", b"escaped".to_vec()),
            ("_peerdav_mirror/docs/files//absolute.txt", b"absolute".to_vec()),
            ("_peerdav_mirror/docs/files/valid.txt", b"valid".to_vec()),
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
    async fn changing_the_cloud_prefix_requires_a_new_merge_without_reusing_deletion_history() {
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
        let result = super::run_sync(
            store.clone(),
            &changed,
            &data,
            &context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let Err(MirrorError::ConfirmMerge { plan_id, .. }) = result else {
            panic!("changed prefix must require a new merge plan");
        };
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"keep me");
        assert!(
            !store
                .keys()
                .contains(&"_peerdav_mirror/work/files/a.txt".to_string())
        );
        let approved = MirrorRunContext {
            confirmed_plan: Some(plan_id),
            ..context()
        };
        let result = super::run_sync(
            store.clone(),
            &changed,
            &data,
            &approved,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.uploaded, 1);
        assert_eq!(result.deleted_local + result.deleted_cloud, 0);
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"keep me");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replaced_root_and_symlink_subtree_never_delete_cloud_files() {
        let (local, data) = test_dirs();
        write(&local, "sub/precious.txt", "keep");
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
        let moved = local.with_file_name("moved");
        std::fs::rename(&local, &moved).unwrap();
        std::fs::create_dir(&local).unwrap();
        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &forced_context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert!(matches!(result, Err(MirrorError::InvalidRoot(_))));
        assert_eq!(
            store.get_value("_peerdav_mirror/docs/files/sub/precious.txt"),
            b"keep"
        );
        std::fs::remove_dir(&local).unwrap();
        std::fs::rename(&moved, &local).unwrap();
        let sub = local.join("sub");
        std::fs::rename(&sub, local.join("original-sub")).unwrap();
        std::os::unix::fs::symlink(local.join("original-sub"), &sub).unwrap();
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
        assert!(!result.failures.is_empty());
        assert_eq!(
            store.get_value("_peerdav_mirror/docs/files/sub/precious.txt"),
            b"keep"
        );
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn legacy_baseline_never_authorizes_deletion() {
        let (local, data) = test_dirs();
        let store = Arc::new(MemoryStore::new(vec![(
            "_peerdav_mirror/docs/files/precious.txt",
            b"keep".to_vec(),
        )]));
        let mapping = mapping(&local, "docs");
        let mut legacy = MirrorBaseline::default();
        legacy.record_upload("precious.txt", 1, 4, Some("etag".into()), 4);
        baseline::save(&data, &mapping.id, &legacy).unwrap();
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
        assert_eq!(std::fs::read(local.join("precious.txt")).unwrap(), b"keep");
        assert_eq!(
            store.get_value("_peerdav_mirror/docs/files/precious.txt"),
            b"keep"
        );
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
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
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"keep");
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
    async fn confirmation_is_required_and_stale_plans_never_authorize_deletion() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "precious");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));
        let first =
            super::run_sync(store.clone(), &mapping, &data, &context(), cancel.clone()).await;
        let Err(MirrorError::ConfirmMerge { plan_id, .. }) = first else {
            panic!("merge must be confirmed");
        };
        assert!(store.keys().is_empty());
        let approved = MirrorRunContext {
            confirmed_plan: Some(plan_id),
            ..context()
        };
        super::run_sync(store.clone(), &mapping, &data, &approved, cancel.clone())
            .await
            .unwrap();
        std::fs::remove_file(local.join("a.txt")).unwrap();
        let error = super::run_sync(
            store.clone(),
            &mapping,
            &data,
            &forced_context(),
            cancel.clone(),
        )
        .await
        .unwrap_err();
        let MirrorError::MassDelete { plan_id, .. } = error else {
            panic!("force without a token is unsafe");
        };
        write(&local, "new.txt", "new");
        let stale = MirrorRunContext {
            confirmed_plan: Some(plan_id),
            ..forced_context()
        };
        assert!(matches!(
            super::run_sync(store.clone(), &mapping, &data, &stale, cancel).await,
            Err(MirrorError::MassDelete { .. })
        ));
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"precious");
        assert!(
            !store
                .keys()
                .contains(&"_peerdav_mirror/docs/files/new.txt".into())
        );
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn stale_bulk_approval_never_authorizes_a_changed_normal_deletion() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "precious");
        write(&local, "b.txt", "keep");
        let store = Arc::new(MemoryStore::new(vec![]));
        let mapping = mapping(&local, "docs");
        let cancel = Arc::new(AtomicBool::new(false));
        run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
            .await
            .unwrap();
        std::fs::remove_file(local.join("a.txt")).unwrap();
        std::fs::remove_file(local.join("b.txt")).unwrap();
        let error = super::run_sync(store.clone(), &mapping, &data, &context(), cancel.clone())
            .await
            .unwrap_err();
        let MirrorError::MassDelete { plan_id, .. } = error else {
            panic!("empty side must pause");
        };
        write(&local, "b.txt", "keep");
        let stale = MirrorRunContext {
            confirmed_plan: Some(plan_id),
            ..forced_context()
        };
        assert!(matches!(
            super::run_sync(store.clone(), &mapping, &data, &stale, cancel).await,
            Err(MirrorError::MassDelete { .. })
        ));
        assert_eq!(store.get_value("_peerdav_mirror/docs/files/a.txt"), b"precious");
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn downloads_never_clobber_new_files_and_reject_changed_remote() {
        let (local, _) = test_dirs();
        let store = MemoryStore::new(vec![("a", b"cloud".to_vec())]);
        let meta = store.stat("a").await.unwrap();
        let expected = CloudFile {
            etag: meta.etag,
            size: meta.size,
        };
        write(&local, "a.txt", "new local data");
        let result = download_file(
            &store,
            &local,
            "a.txt",
            "a",
            &expected,
            None,
            &AtomicBool::new(false),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(local.join("a.txt")).unwrap(),
            b"new local data"
        );
        store.insert("a", b"changed".to_vec(), "new revision");
        let result = download_file(
            &store,
            &local,
            "missing.txt",
            "a",
            &expected,
            None,
            &AtomicBool::new(false),
        )
        .await;
        assert!(result.is_err());
        assert!(!local.join("missing.txt").exists());
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn key_delete_without_cas_and_truncated_download_preserve_safety() {
        let (local, data) = test_dirs();
        write(&local, "a.txt", "precious");
        let mut store = MemoryStore::new(vec![]);
        store.conditional_delete = false;
        let store = Arc::new(store);
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
        std::fs::remove_file(local.join("a.txt")).unwrap();
        let result = run_sync(
            store.clone(),
            &mapping,
            &data,
            &forced_context(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.deleted_cloud, 1);
        assert!(result.failures.is_empty());
        assert!(store.keys().is_empty());
        let mut truncated = MemoryStore::new(vec![("a", b"remote".to_vec())]);
        truncated.truncated_read = true;
        let meta = truncated.stat("a").await.unwrap();
        write(&local, "a.txt", "keep local");
        let scanned = scan::scan(&local, &AtomicBool::new(false)).unwrap();
        let expected = CloudFile {
            etag: meta.etag,
            size: meta.size,
        };
        assert!(
            download_file(
                &truncated,
                &local,
                "a.txt",
                "a",
                &expected,
                scanned.files.get("a.txt"),
                &AtomicBool::new(false)
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"keep local");
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn mirror_upload_rejects_stale_missing_and_new_remote_snapshots() {
        let (local, _) = test_dirs();
        write(&local, "a.txt", "local");
        let scanned = scan::scan(&local, &AtomicBool::new(false)).unwrap();
        let store = MemoryStore::new(vec![("a", b"remote".to_vec())]);
        let expected = CloudFile { etag: Some("stale".into()), size: 6 };
        for expected in [None, Some(&expected)] {
            assert!(matches!(upload_file(&store, &local.join("a.txt"), "a", expected,
                &scanned.files["a.txt"], Arc::new(AtomicBool::new(false))).await,
                Err(MirrorError::Cloud(CloudError::Precondition))));
            assert_eq!(store.get_value("a"), b"remote");
        }
        assert!(matches!(verify_remote_snapshot(&store, "missing", Some(&expected)).await, Err(CloudError::Precondition)));
        let missing_revision = CloudFile { etag: None, size: 6 };
        assert!(matches!(verify_remote_snapshot(&store, "a", Some(&missing_revision)).await, Err(CloudError::Precondition)));
        let mut oversized = scanned.files["a.txt"].clone();
        oversized.size = crate::cloud::types::MAX_PUT_BYTES as u64 + 1;
        assert!(matches!(upload_file(&store, &local.join("a.txt"), "new", None, &oversized,
            Arc::new(AtomicBool::new(false))).await, Err(MirrorError::Cloud(CloudError::PayloadTooLarge))));
        std::fs::remove_dir_all(local.parent().unwrap()).unwrap();
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
                total: 10,
                ..
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
        store.insert("_peerdav_mirror/docs/files/a.txt", b"v2".to_vec(), "etag-v2");

        let result = run_sync(store.clone(), &mapping, &data, &context(), cancel)
            .await
            .unwrap();
        assert_eq!(result.restored, 1);
        assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"v2");
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
    }
}
