use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use super::runtime_types::*;
use crate::cloud::config::CloudConfigStore;
use crate::cloud::config_types::ConfigError;
use crate::cloud::config_types::*;
use crate::cloud::{S3Store, traits::ObjectStore};
use crate::sync::merge;
use crate::sync::manager_types::{SyncPhase, SyncStatus};
use crate::sync::merge_types::{MergeSet, Node, RevisionValue};
use crate::sync::publisher_types::PublishControl;
use crate::sync::scan::scan;
use crate::sync::scan_types::ScanOptions;
use crate::sync::traits::{SyncEvents, SyncWorker};
use crate::sync::{
    engine,
    index::SyncIndex,
    manager::{StartResult, SyncManager},
    repository::Repository,
    repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode},
};
use crate::{AppError, DomainError, Vault};

#[path = "runtime_append.rs"]
pub mod append;

#[derive(Clone)]
struct Ticket {
    id: String,
    mapping: String,
    mapping_hash: String,
    root: PathBuf,
    identity: String,
    digest: String,
    revision: u64,
    head: Option<String>,
    epoch: String,
    created: Instant,
}
struct Worker {
    repository: Arc<Repository>,
    index: Arc<Mutex<Option<SyncIndex>>>,
    root: PathBuf,
    private: PathBuf,
    mapping: String,
    revision: u64,
    manager: std::sync::Weak<SyncManager>,
    data_dir: PathBuf,
}
impl SyncWorker for Worker {
    fn run(
        &self,
        context: crate::sync::manager_types::RunContext,
    ) -> futures_util::future::BoxFuture<
        'static,
        Result<crate::sync::manager_types::WorkerOutcome, AppError>,
    > {
        let repository = Arc::clone(&self.repository);
        let index = Arc::clone(&self.index);
        let root = self.root.clone();
        let private = self.private.clone();
        let mapping = self.mapping.clone();
        let revision = self.revision;
        let manager = self.manager.clone();
        let _data = self.data_dir.clone();
        Box::pin(async move {
            let control = PublishControl {
                cancel: Arc::clone(&context.cancel),
                epoch: Arc::clone(&context.epoch),
                expected_epoch: context.expected_epoch,
            };
            let mut idx = index
                .lock()
                .map_err(|_| {
                    runtime_error(
                        "CLOUD_INDEX_LOCK",
                        "同步索引锁已损坏 / Sync index lock is poisoned",
                    )
                })?
                .take()
                .ok_or_else(|| {
                    runtime_error(
                        "CLOUD_INDEX_MISSING",
                        "同步索引不可用 / Sync index unavailable",
                    )
                })?;
            let operation = hex::encode(rand::random::<[u8; 16]>());
            let progress = |phase| {
                if let Some(manager) = manager.upgrade() {
                    manager.report_status(crate::sync::manager_types::SyncStatus {
                        mapping_id: context.mapping_id.clone(),
                        run_id: Some(context.run_id),
                        phase,
                        detail: None,
                    });
                }
            };
            let result = engine::run_with_progress(
                repository, &mut idx, &root, &private, &mapping, &operation, revision, &control,
                &progress,
            )
            .await;
            let journal: Option<crate::sync::index_types::RunJournal> =
                idx.load("run", &operation).map_err(|error| {
                    AppError::from(DomainError::Cloud {
                        code: "CLOUD_INDEX",
                        user_reason: "读取同步日志失败 / Reading sync journal failed",
                        source: Box::new(error),
                    })
                })?;
            let pending = journal.is_some_and(|journal| {
                matches!(
                    journal.phase,
                    crate::sync::index_types::RunPhase::PublishedPendingApply
                )
            }) || ["pending", "active_intent"]
                .iter()
                .try_fold(false, |found, name| {
                    idx.load::<Option<crate::sync::engine_types::PendingRun>>("engine", name)
                        .map(|run| found || run.flatten().is_some())
                })
                .map_err(|error| {
                    AppError::from(DomainError::Cloud {
                        code: "CLOUD_INDEX",
                        user_reason: "读取恢复状态失败 / Reading recovery state failed",
                        source: Box::new(error),
                    })
                })?;
            *index.lock().map_err(|_| {
                runtime_error(
                    "CLOUD_INDEX_LOCK",
                    "同步索引锁已损坏 / Sync index lock is poisoned",
                )
            })? = Some(idx);
            use crate::sync::manager_types::{SyncPhase, WorkerOutcome};
            match result {
                Ok(report) if report.conflicts > 0 => Ok(WorkerOutcome::Phase {
                    phase: SyncPhase::Completed,
                    detail: Some(format!(
                        "同步完成，保留 {} 个冲突副本 / Synchronized; {} conflict copies preserved",
                        report.conflicts, report.conflicts
                    )),
                }),
                Ok(_) => Ok(WorkerOutcome::Completed),
                Err(error) => {
                    let phase = if pending {
                        Some(SyncPhase::PendingRecovery)
                    } else {
                        match &error {
                            crate::sync::engine_types::EngineError::Cancelled => {
                                Some(SyncPhase::Cancelled)
                            }
                            crate::sync::engine_types::EngineError::RootMissing(_) => {
                                Some(SyncPhase::PausedRootMissing)
                            }
                            crate::sync::engine_types::EngineError::Changed(_)
                            | crate::sync::engine_types::EngineError::Root(_) => {
                                Some(SyncPhase::PausedRootReplaced)
                            }
                            _ => None,
                        }
                    };
                    let manifest_error = match &error {
                        crate::sync::engine_types::EngineError::Tree(error) => Some(error),
                        crate::sync::engine_types::EngineError::Publish(
                            crate::sync::publisher_types::PubError::Tree(error),
                        ) => Some(error),
                        _ => None,
                    };
                    let code = match manifest_error {
                        Some(crate::sync::tree_types::TreeError::ResourceLimit(_)) => {
                            "CLOUD_MANIFEST_LIMIT"
                        }
                        Some(crate::sync::tree_types::TreeError::Unsupported) => {
                            "CLOUD_MANIFEST_UNSUPPORTED"
                        }
                        Some(_) => "CLOUD_MANIFEST_INVALID",
                        None if pending => "SYNC_PENDING_RECOVERY",
                        None => "CLOUD_SYNC_FAILED",
                    };
                    let error = AppError::from(DomainError::Cloud {
                        code,
                        user_reason: "同步未完成 / Synchronization did not complete",
                        source: Box::new(error),
                    });
                    match phase {
                        Some(phase) => {
                            if let Some(manager) = manager.upgrade() {
                                manager.report_background(error.clone());
                            }
                            Ok(WorkerOutcome::Phase {
                                phase,
                                detail: Some(error.message),
                            })
                        }
                        None => Err(error),
                    }
                }
            }
        })
    }
}

fn preview_counts(
    report: &crate::sync::scan_types::ScanReport,
    base: &crate::sync::merge_types::Projection,
    remote: &crate::sync::merge_types::Projection,
) -> PreviewCounts {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Value<'a> {
        File(&'a str, u64),
        Dir,
        Missing,
    }
    fn projection_value(entry: Option<&crate::sync::merge_types::ProjectionEntry>) -> Value<'_> {
        match entry.map(|e| &e.value) {
            Some(RevisionValue::Live {
                node: Node::File(file),
                ..
            }) => Value::File(&file.digest, file.length),
            Some(RevisionValue::Live {
                node: Node::Dir, ..
            }) => Value::Dir,
            _ => Value::Missing,
        }
    }
    fn local_value<'a>(
        path: &str,
        files: &'a std::collections::HashMap<&str, &crate::sync::scan_types::FileScan>,
        dirs: &std::collections::HashSet<&str>,
    ) -> Value<'a> {
        files.get(path).map_or_else(
            || {
                if dirs.contains(path) {
                    Value::Dir
                } else {
                    Value::Missing
                }
            },
            |file| Value::File(&file.whole_file_hash, file.size),
        )
    }
    let files: std::collections::HashMap<_, _> = report
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let dirs: std::collections::HashSet<_> =
        report.directories.iter().map(String::as_str).collect();
    fn by_path<'a>(
        projection: &'a crate::sync::merge_types::Projection,
    ) -> std::collections::HashMap<String, &'a crate::sync::merge_types::ProjectionEntry> {
        projection
            .entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .projection_path
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join("/"),
                    entry,
                )
            })
            .collect()
    }
    let baseline = by_path(base);
    let remote = by_path(remote);
    let mut paths = std::collections::HashSet::new();
    paths.extend(files.keys().copied());
    paths.extend(dirs.iter().copied());
    paths.extend(baseline.keys().map(String::as_str));
    paths.extend(remote.keys().map(String::as_str));
    let mut counts = PreviewCounts {
        uploads: 0,
        downloads: 0,
        deletes: 0,
        conflicts: remote.values().filter(|entry| entry.conflict).count() as u64,
    };
    for path in paths {
        let local = local_value(path, &files, &dirs);
        let b = projection_value(baseline.get(path).copied());
        let r = projection_value(remote.get(path).copied());
        if b == Value::Missing {
            match (local, r) {
                (Value::Missing, Value::Missing) | (Value::Dir, Value::Dir) => {}
                (Value::Missing, _) => counts.downloads += 1,
                (_, Value::Missing) => counts.uploads += 1,
                (left, right) if left != right => counts.conflicts += 1,
                _ => {}
            }
            continue;
        }
        let local_changed = local != b;
        let remote_changed = r != b;
        match (local_changed, remote_changed) {
            (false, false) => {}
            (true, false) => match local {
                Value::Missing => {
                    if r != Value::Missing {
                        counts.deletes += 1;
                    }
                }
                _ => counts.uploads += 1,
            },
            (false, true) => match r {
                Value::Missing => {
                    if local != Value::Missing {
                        counts.deletes += 1;
                    }
                }
                _ => counts.downloads += 1,
            },
            (true, true) if local == r => {}
            (true, true) => counts.conflicts += 1,
        }
    }
    counts
}
pub struct CloudRuntime {
    pub(crate) vault: Arc<Vault>,
    pub(crate) config: CloudConfigStore,
    pub(crate) manager: Arc<SyncManager>,
    pub(crate) data_dir: PathBuf,
    safe_apply: bool,
    gate: AtomicBool,
    cancel: Arc<AtomicBool>,
    pub(crate) network: Arc<RwLock<()>>,
    pub(crate) objects: tokio::sync::Mutex<super::object_runtime_types::ObjectState>,
    pub(crate) object_events: Arc<dyn SyncEvents>,
    pub(crate) object_lifecycle: tokio::sync::Mutex<()>,
    tickets: Mutex<HashMap<String, Ticket>>,
    pub(crate) schedules: super::runtime_schedule::CloudSchedules,
    /// 每个镜像映射的运行中取消令牌；同步结束后移除，用于实现「同一映射不重入」与取消。
    mirror_runs: Mutex<HashMap<String, Arc<AtomicBool>>>,
}
impl CloudRuntime {
    pub fn new(vault: Arc<Vault>, events: Arc<dyn SyncEvents>, data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            config: CloudConfigStore::new(Arc::clone(&vault)),
            manager: SyncManager::new(Arc::clone(&events)),
            object_events: events,
            object_lifecycle: tokio::sync::Mutex::new(()),
            objects: tokio::sync::Mutex::new(super::object_runtime_types::ObjectState::new()),
            vault,
            safe_apply: probe_apply_capability(&data_dir),
            data_dir,
            gate: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            network: Arc::new(RwLock::new(())),
            tickets: Mutex::new(HashMap::new()),
            schedules: super::runtime_schedule::CloudSchedules::new(),
            mirror_runs: Mutex::new(HashMap::new()),
        })
    }
    pub async fn stop_and_clear(&self) -> Result<(), AppError> {
        self.gate.store(false, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
        let _objects = self.object_lifecycle.lock().await;
        for result in [
            self.object_invalidate(None).await,
            self.schedules.stop_all().await,
            self.manager.block_new_and_stop_all().await,
        ] {
            if let Err(error) = result {
                log::error!("cloud shutdown error: {error:?}");
                if let Err(source) = self.object_events.error(error) {
                    log::error!("cloud shutdown error event failed: {source}");
                }
            }
        }
        let _p = self.network.write().await;
        match self.tickets.lock() {
            Ok(mut tickets) => tickets.clear(),
            Err(poisoned) => {
                poisoned.into_inner().clear();
                let error = runtime_error(
                    "CLOUD_TICKET_LOCK",
                    "预览状态不可用 / Preview state unavailable",
                );
                log::error!("cloud shutdown error: {error:?}");
                if let Err(source) = self.object_events.error(error) {
                    log::error!("cloud shutdown error event failed: {source}");
                }
            }
        }
        Ok(())
    }
    pub async fn refresh_schedules(self: &Arc<Self>) -> Result<(), AppError> {
        self.schedules.refresh(self).await
    }
    pub fn resume(&self) -> Result<(), AppError> {
        if !self.vault.status().map_err(AppError::from)?.unlocked {
            return Err(DomainError::VaultLocked.into());
        }
        self.cancel.store(false, Ordering::Release);
        self.gate.store(true, Ordering::Release);
        self.manager.open_gate();
        Ok(())
    }
    pub fn is_open(&self) -> bool {
        self.gate.load(Ordering::Acquire)
    }
    pub(crate) fn admit(&self) -> Result<(), AppError> {
        if !self.is_open() {
            return Err(runtime_error(
                "CLOUD_GATE_LOCKED",
                "云同步不可用：保险库已锁定 / Cloud sync is unavailable while the vault is locked",
            ));
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<CloudRuntimeSnapshot, AppError> {
        let s = self.config.snapshot().map_err(config_error)?;
        Ok(CloudRuntimeSnapshot {
            connections: s
                .connections
                .iter()
                .map(CloudConnection::redacted)
                .collect(),
            repositories: s
                .repositories
                .iter()
                .map(CachedRepository::redacted)
                .collect(),
            append_repositories: s.append_repositories,
            mappings: s.mappings,
            mirror_mappings: s.mirror_mappings,
            statuses: self.manager.snapshots(),
            capabilities: CloudCapabilities {
                safe_apply: self.safe_apply,
            },
        })
    }
    pub fn save_mirror_mapping(
        &self,
        input: crate::mirror::SaveMirrorMappingInput,
    ) -> Result<crate::mirror::MirrorMapping, AppError> {
        self.config.save_mirror_mapping(input).map_err(config_error)
    }
    pub fn delete_mirror_mapping(&self, id: &str) -> Result<(), AppError> {
        self.config.delete_mirror_mapping(id).map_err(config_error)
    }
    /// 读取单条镜像映射；不存在时返回 CLOUD_CONFIG_NOT_FOUND。
    pub(crate) fn mirror_mapping(
        &self,
        id: &str,
    ) -> Result<crate::mirror::MirrorMapping, AppError> {
        let snapshot = self.config.snapshot().map_err(config_error)?;
        snapshot
            .mirror_mappings
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| config_error(ConfigError::NotFound("mirror mapping".into())))
    }
    /// 取消正在运行的镜像同步；没有运行中的映射是空操作。
    pub fn mirror_cancel(&self, mapping_id: &str) -> Result<(), AppError> {
        let runs = self
            .mirror_runs
            .lock()
            .map_err(|_| runtime_error("MIRROR_BUSY", "镜像同步运行表已锁死 / Mirror run registry poisoned"))?;
        if let Some(cancel) = runs.get(mapping_id) {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    }
    /// 手动触发一次镜像同步。同一映射不重入（返回 MIRROR_BUSY），
    /// 进度与结束状态通过现有的 `app://cloud-progress` 事件发布。
    pub async fn mirror_sync(
        self: &Arc<Self>,
        mapping_id: &str,
    ) -> Result<crate::mirror::MirrorSyncResult, AppError> {
        let mapping = self.mirror_mapping(mapping_id)?;
        let cancel = {
            let mut runs = self
                .mirror_runs
                .lock()
                .map_err(|_| runtime_error("MIRROR_BUSY", "镜像同步运行表已锁死 / Mirror run registry poisoned"))?;
            if runs.contains_key(mapping_id) {
                return Err(AppError::from(crate::mirror::MirrorError::Busy));
            }
            let cancel = Arc::new(AtomicBool::new(false));
            runs.insert(mapping_id.to_owned(), Arc::clone(&cancel));
            cancel
        };
        let result = self.mirror_sync_inner(mapping, Arc::clone(&cancel)).await;
        if let Ok(mut runs) = self.mirror_runs.lock() {
            runs.remove(mapping_id);
        }
        result
    }
    async fn mirror_sync_inner(
        &self,
        mapping: crate::mirror::MirrorMapping,
        cancel: Arc<AtomicBool>,
    ) -> Result<crate::mirror::MirrorSyncResult, AppError> {
        let connection = self.connection(&mapping.connection_id)?;
        let store: Arc<dyn ObjectStore> =
            Arc::new(S3Store::new(connection.config).map_err(cloud_error)?);
        let _ = self.object_events.progress(SyncStatus {
            mapping_id: mapping.id.clone(),
            run_id: None,
            phase: SyncPhase::Scanning,
            detail: None,
        });
        let result = crate::mirror::run_sync(store, &mapping, &self.data_dir, cancel).await;
        let phase = match &result {
            Ok(_) => SyncPhase::Completed,
            Err(crate::mirror::MirrorError::Canceled) => SyncPhase::Cancelled,
            Err(_) => SyncPhase::Failed,
        };
        let _ = self.object_events.progress(SyncStatus {
            mapping_id: mapping.id.clone(),
            run_id: None,
            phase,
            detail: result.as_ref().err().map(|e| e.to_string()),
        });
        result.map_err(AppError::from)
    }
    pub fn save_connection(&self, input: SaveS3Input) -> Result<ConnectionRedacted, AppError> {
        self.config.save_connection(input).map_err(config_error)
    }
    pub async fn update_connection(
        &self,
        input: SaveS3Input,
    ) -> Result<ConnectionRedacted, AppError> {
        self.admit()?;
        let _objects = self.object_lifecycle.lock().await;
        self.object_invalidate(input.id.as_deref()).await?;
        let _permit = self.network.write().await;
        self.admit()?;
        if let Some(id) = input.id.as_deref() {
            for mapping in self
                .config
                .snapshot()
                .map_err(config_error)?
                .mappings
                .iter()
                .filter(|mapping| mapping.connection_id == id)
            {
                self.manager.stop_mapping(&mapping.id).await?;
            }
        }
        self.save_connection(input)
    }
    pub async fn delete_connection(&self, id: &str) -> Result<(), AppError> {
        self.admit()?;
        let _objects = self.object_lifecycle.lock().await;
        self.object_invalidate(Some(id)).await?;
        let _permit = self.network.write().await;
        self.admit()?;
        for mapping in self
            .config
            .snapshot()
            .map_err(config_error)?
            .mappings
            .iter()
            .filter(|mapping| mapping.connection_id == id)
        {
            self.manager.stop_mapping(&mapping.id).await?;
        }
        self.config.delete_connection(id).map_err(config_error)
    }
    pub async fn save_mapping(
        &self,
        mapping_id: Option<&str>,
        mut input: AddMappingInput,
    ) -> Result<CloudMapping, AppError> {
        self.admit()?;
        let _permit = self.network.write().await;
        self.admit()?;
        if let Some(id) = mapping_id {
            self.manager.stop_mapping(id).await?;
            let old = self.mapping(id)?;
            let root_changed = old.local_root != input.local_root;
            if root_changed {
                input.schedule.enabled = false;
            }
            let saved = self
                .config
                .edit_mapping(id, input, &[self.data_dir.as_path()])
                .map_err(config_error)?;
            self.reselect_root_guard(&saved)?;
            Ok(saved)
        } else {
            input.schedule.enabled = false;
            self.config
                .add_mapping(input, &[self.data_dir.as_path()])
                .map_err(config_error)
        }
    }
    pub async fn delete_mapping(&self, id: &str) -> Result<(), AppError> {
        self.admit()?;
        let _permit = self.network.write().await;
        self.admit()?;
        self.manager.stop_mapping(id).await?;
        self.config.delete_mapping(id).map_err(config_error)
    }
    pub async fn test_connection(&self, id: &str) -> Result<bool, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        let connection = self.connection(id)?;
        // A connection test only proves the address and credentials work. It must
        // not probe conditional writes: many S3-compatible providers reject them
        // even though the connection itself is perfectly usable, so listing the
        // service's buckets is the right proof.
        super::service::probe(
            &connection.config.endpoint,
            &connection.config.region,
            &connection.config.access_key,
            &connection.config.secret_key,
            connection.config.session_token.as_deref(),
            connection.config.allow_insecure_http,
            &connection.config.proxy,
        )
        .await
        .map_err(cloud_error)?;
        Ok(true)
    }
    fn connection(&self, id: &str) -> Result<CloudConnection, AppError> {
        self.config
            .snapshot()
            .map_err(config_error)?
            .connections
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| config_error(ConfigError::NotFound("connection".into())))
    }
    async fn repo(&self, m: &CloudMapping) -> Result<(Arc<Repository>, u64), AppError> {
        let c = self.connection(&m.connection_id)?;
        let cfg = self.config.snapshot().map_err(config_error)?;
        let cached = cfg
            .repositories
            .into_iter()
            .find(|r| r.connection_id == m.connection_id && r.descriptor.repo_id == m.repository_id)
            .ok_or_else(|| config_error(ConfigError::NotFound("repository".into())))?;
        let store_impl = S3Store::new(c.config).map_err(cloud_error)?;
        store_impl
            .verify_capabilities()
            .await
            .map_err(cloud_error)?;
        let store: Arc<dyn ObjectStore> = Arc::new(store_impl);
        let repo = if let Some(master) = cached.master {
            Repository::join_cached(store, Zeroizing::new(master))
                .await
                .map_err(repo_error)?
        } else {
            Repository::join(store, &[]).await.map_err(repo_error)?
        };
        Ok((Arc::new(repo), c.revision))
    }
    pub(crate) fn root_epoch(root: &Path) -> Result<String, AppError> {
        let m = std::fs::symlink_metadata(root).map_err(|_| {
            runtime_error(
                "CLOUD_ROOT_MISSING",
                "同步根目录不存在 / Sync root is missing",
            )
        })?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(runtime_error(
                "CLOUD_ROOT_UNSAFE",
                "同步根目录不安全 / Sync root is unsafe",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(format!("{}:{}", m.dev(), m.ino()))
        }
        #[cfg(not(unix))]
        {
            Err(runtime_error(
                "CLOUD_UNSUPPORTED",
                "平台不支持根目录身份 / Root identity is unsupported",
            ))
        }
    }
    fn mapping_digest(mapping: &CloudMapping) -> Result<String, AppError> {
        serde_json::to_vec(&(
            &mapping.connection_id,
            &mapping.repository_id,
            &mapping.logical_path,
            &mapping.local_root,
            &mapping.schedule,
            &mapping.protocol,
            &mapping.append_binding,
        ))
        .map(|value| hex::encode(Sha256::digest(value)))
        .map_err(|error| runtime_error("CLOUD_CONFIG_INVALID", &error.to_string()))
    }
    fn digest(report: &crate::sync::scan_types::ScanReport) -> Result<String, AppError> {
        serde_json::to_vec(report)
            .map(|v| hex::encode(Sha256::digest(v)))
            .map_err(|e| runtime_error("CLOUD_SCAN_FAILED", &e.to_string()))
    }
    pub async fn preview(&self, id: &str) -> Result<CloudPreview, AppError> {
        self.admit()?;
        let _p = self.network.read().await;
        self.admit()?;
        let m = self
            .config
            .snapshot()
            .map_err(config_error)?
            .mappings
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| config_error(ConfigError::NotFound("mapping".into())))?;
        let epoch = Self::root_epoch(&m.local_root)?;
        let cancel = Arc::clone(&self.cancel);
        let root = m.local_root.clone();
        let report = tokio::task::spawn_blocking(move || {
            scan(
                &root,
                &ScanOptions {
                    cancel,
                    ..Default::default()
                },
            )
        })
        .await
        .map_err(|_| runtime_error("CLOUD_SCAN_FAILED", "扫描任务异常退出 / Scan task failed"))?
        .map_err(|e| runtime_error("CLOUD_SCAN_FAILED", &e.to_string()))?;
        let digest = Self::digest(&report)?;
        let (total, rev, head_commit) = if m.protocol == StorageProtocol::AppendV2 {
            let (counts, signature) = self.append_preview_state(&m, &epoch).await?;
            (
                counts,
                self.connection(&m.connection_id)?.revision,
                Some(signature),
            )
        } else {
            let (repo, rev) = self.repo(&m).await?;
            let head = repo.head().await.map_err(repo_error)?;
            let remote = match head.commit.as_deref() {
                Some(commit) => {
                    let control = PublishControl {
                        cancel: Arc::clone(&self.cancel),
                        epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        expected_epoch: 0,
                    };
                    crate::sync::tree::load_commit_snapshot(&repo, commit, &control)
                    .await
                    .map_err(|error| {
                        AppError::from(DomainError::Cloud {
                            code: "CLOUD_REMOTE_METADATA",
                            user_reason: "读取目录清单失败 / Reading directory manifests failed",
                            source: Box::new(error),
                        })
                    })?
                    .snapshot
                }
                None => MergeSet {
                    revisions: std::collections::BTreeMap::new(),
                },
            };
            let remote_projection = crate::sync::namespace::project_scoped(
                &merge::project(&remote.revisions)
                    .map_err(|e| runtime_error("CLOUD_REMOTE_METADATA", &e.to_string()))?,
                &m.logical_path,
            )
            .map_err(|e| runtime_error("CLOUD_REMOTE_METADATA", &e.to_string()))?;
            let base = self
                .config
                .get_or_create_index_key()
                .map_err(config_error)
                .and_then(|key| {
                    SyncIndex::open(
                        &self.data_dir.join("cloud-sync.sqlite"),
                        key,
                        m.repository_id.clone(),
                        Self::index_scope(&m, &epoch),
                    )
                    .map_err(|e| runtime_error("CLOUD_INDEX", &e.to_string()))
                })?
                .load::<MergeSet>("engine", "materialized")
                .map_err(|e| runtime_error("CLOUD_INDEX", &e.to_string()))?
                .unwrap_or(MergeSet {
                    revisions: std::collections::BTreeMap::new(),
                });
            let base_projection = crate::sync::namespace::project_scoped(
                &merge::project(&base.revisions)
                    .map_err(|e| runtime_error("CLOUD_REMOTE_METADATA", &e.to_string()))?,
                &m.logical_path,
            )
            .map_err(|e| runtime_error("CLOUD_REMOTE_METADATA", &e.to_string()))?;
            let total = preview_counts(&report, &base_projection, &remote_projection);
            (total, rev, head.commit)
        };

        let id = hex::encode(rand::random::<[u8; 16]>());
        let ticket = Ticket {
            id: id.clone(),
            mapping: m.id.clone(),
            mapping_hash: Self::mapping_digest(&m)?,
            root: m.local_root.clone(),
            identity: epoch.clone(),
            digest,
            revision: rev,
            head: head_commit.clone(),
            epoch: epoch.clone(),
            created: Instant::now(),
        };
        let mut tickets = self.tickets.lock().map_err(|_| {
            runtime_error(
                "CLOUD_TICKET_LOCK",
                "预览状态不可用 / Preview state unavailable",
            )
        })?;
        if tickets.len() >= 256 {
            let oldest = tickets
                .iter()
                .min_by_key(|(_, ticket)| ticket.created)
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    runtime_error(
                        "CLOUD_TICKET_LOCK",
                        "预览状态不可用 / Preview state unavailable",
                    )
                })?;
            tickets.remove(&oldest);
        }
        tickets.insert(id.clone(), ticket);
        Ok(CloudPreview {
            preview_id: id,
            mapping_id: m.id,
            counts: total.clone(),
            summary: PreviewSummary {
                uploads: total.uploads,
                downloads: total.downloads,
                deletes: total.deletes,
                conflicts: total.conflicts,
            },
            requires_confirmation: true,
            head_commit,
            root_epoch: epoch,
        })
    }
    pub async fn manual_sync(
        &self,
        mapping_id: &str,
        preview_id: &str,
    ) -> Result<CloudSyncResult, AppError> {
        self.admit()?;
        if !self.safe_apply {
            return Err(runtime_error(
                "SYNC_SAFE_APPLY_UNSUPPORTED",
                "此系统不具备安全落盘能力，同步已暂停 / Safe local apply is unavailable; synchronization is paused",
            ));
        }
        let _p = self.network.read().await;
        self.admit()?;
        let ticket = self
            .tickets
            .lock()
            .map_err(|_| {
                runtime_error(
                    "CLOUD_TICKET_LOCK",
                    "预览状态不可用 / Preview state unavailable",
                )
            })?
            .remove(preview_id)
            .ok_or_else(|| {
                runtime_error(
                    "CLOUD_PREVIEW_STALE",
                    "预览不存在或已使用 / Preview is stale or already used",
                )
            })?;
        if ticket.mapping != mapping_id
            || ticket.id != preview_id
            || ticket.root != self.mapping_root(mapping_id)?
            || ticket.mapping_hash != Self::mapping_digest(&self.mapping(mapping_id)?)?
        {
            return Err(runtime_error(
                "CLOUD_PREVIEW_STALE",
                "预览已失效 / Preview is stale",
            ));
        }
        if Self::root_epoch(&ticket.root)? != ticket.identity {
            return Err(runtime_error(
                "CLOUD_PREVIEW_STALE",
                "同步根目录已变化 / Sync root changed",
            ));
        }
        let check_root = ticket.root.clone();
        let check = tokio::task::spawn_blocking(move || scan(&check_root, &ScanOptions::default()))
            .await
            .map_err(|_| runtime_error("CLOUD_SCAN_FAILED", "扫描任务异常退出 / Scan task failed"))?
            .map_err(|e| runtime_error("CLOUD_SCAN_FAILED", &e.to_string()))?;
        if Self::digest(&check)? != ticket.digest {
            return Err(runtime_error(
                "CLOUD_PREVIEW_STALE",
                "本地文件已变化 / Local files changed",
            ));
        }
        let m = self.mapping(mapping_id)?;
        self.authorize_root_for_manual(&m, &ticket.root, true)?;
        if m.protocol == StorageProtocol::AppendV2 {
            let (_, signature) = self.append_preview_state(&m, &ticket.epoch).await?;
            if self.connection(&m.connection_id)?.revision != ticket.revision
                || Some(signature) != ticket.head
            {
                return Err(runtime_error(
                    "CLOUD_PREVIEW_STALE",
                    "远端或配置已变化 / Remote or configuration changed",
                ));
            }
            return self.start_append_worker(m, &ticket.epoch).await;
        }
        let (repo, rev) = self.repo(&m).await?;
        if rev != ticket.revision || repo.head().await.map_err(repo_error)?.commit != ticket.head {
            return Err(runtime_error(
                "CLOUD_PREVIEW_STALE",
                "远端或配置已变化 / Remote or configuration changed",
            ));
        }
        let private = m.local_root.parent().ok_or_else(|| runtime_error("CLOUD_PRIVATE_DIR", "无法在同步目录所在文件系统创建恢复区 / Cannot create recovery storage on the same filesystem"))?.join(format!(".peerdav-recovery-{mapping_id}"));
        if !private.exists() {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(&private)
                .map_err(|e| runtime_error("CLOUD_PRIVATE_DIR", &e.to_string()))?;
        }
        crate::sync::root_state::bind_initial(&private)
            .map_err(|e| runtime_error("CLOUD_PRIVATE_DIR", &e.to_string()))?;
        let key = self
            .config
            .get_or_create_index_key()
            .map_err(config_error)?;
        let path = self.data_dir.join("cloud-sync.sqlite");
        let index = SyncIndex::open(
            &path,
            key,
            m.repository_id.clone(),
            Self::index_scope(&m, &ticket.epoch),
        )
        .map_err(|e| runtime_error("CLOUD_INDEX", &e.to_string()))?;
        let worker = Arc::new(Worker {
            repository: repo,
            index: Arc::new(Mutex::new(Some(index))),
            root: m.local_root,
            private,
            mapping: m.logical_path.clone(),
            revision: rev,
            manager: Arc::downgrade(&self.manager),
            data_dir: self.data_dir.clone(),
        });
        match self.manager.start(m.id, worker)? {
            StartResult::Started(run_id) => Ok(CloudSyncResult { run_id }),
            StartResult::AlreadyRunning(_) => Err(runtime_error(
                "SYNC_ALREADY_RUNNING",
                "同步已在运行 / Sync already running",
            )),
        }
    }
    pub(crate) fn index_scope(mapping: &CloudMapping, epoch: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        for value in [
            &mapping.connection_id,
            &mapping.repository_id,
            &mapping.logical_path,
        ] {
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        format!(
            "{}:{}:{}",
            mapping.id,
            epoch,
            hex::encode(digest.finalize())
        )
    }
    pub(crate) fn mapping(&self, id: &str) -> Result<CloudMapping, AppError> {
        self.config
            .snapshot()
            .map_err(config_error)?
            .mappings
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| config_error(ConfigError::NotFound("mapping".into())))
    }
    fn mapping_root(&self, id: &str) -> Result<PathBuf, AppError> {
        Ok(self.mapping(id)?.local_root)
    }
    pub async fn create_repository(
        &self,
        input: CreateRepositoryInput,
    ) -> Result<RepositoryRedacted, AppError> {
        self.admit()?;
        let _p = self.network.read().await;
        self.admit()?;
        let password = Zeroizing::new(input.password);
        let confirmation = Zeroizing::new(input.password_confirmation);
        if input.encrypted && (password.is_empty() || password.as_str() != confirmation.as_str()) {
            return Err(config_error(ConfigError::Validation(
                "encrypted repository password is required and confirmation must match".into(),
            )));
        }
        let cfg = self.connection(&input.connection_id)?;
        let s = S3Store::new(cfg.config).map_err(cloud_error)?;
        s.verify_capabilities().await.map_err(cloud_error)?;
        let store: Arc<dyn ObjectStore> = Arc::new(s);
        let id = input
            .repository_id
            .unwrap_or_else(|| hex::encode(rand::random::<[u8; 16]>()));
        let d = RepoDescriptor {
            version: 1,
            repo_id: id,
            mode: if input.encrypted {
                RepositoryMode::Encrypted
            } else {
                RepositoryMode::Plain
            },
            chunker: ChunkerProfile {
                name: "fastcdc".into(),
                version: "5.0.0".into(),
                normalization: "v2020".into(),
                level: 1,
                min: 1048576,
                average: 4194304,
                max: 8388608,
                seed: 0,
            },
        };
        let repo = Repository::create(store, d, password.as_bytes())
            .await
            .map_err(repo_error)?;
        let d = repo.descriptor().clone();
        self.config
            .cache_repository(&input.connection_id, d.clone(), repo.local_master())
            .map_err(config_error)?;
        Ok(RepositoryRedacted {
            id: d.repo_id,
            connection_id: input.connection_id,
            mode: d.mode,
        })
    }
    pub async fn join_repository(
        &self,
        input: JoinRepositoryInput,
    ) -> Result<RepositoryRedacted, AppError> {
        self.admit()?;
        let _p = self.network.read().await;
        self.admit()?;
        let cfg = self.connection(&input.connection_id)?;
        let s = S3Store::new(cfg.config).map_err(cloud_error)?;
        s.verify_capabilities().await.map_err(cloud_error)?;
        let store: Arc<dyn ObjectStore> = Arc::new(s);
        let password = Zeroizing::new(input.password);
        let repo = Repository::join(store, password.as_bytes())
            .await
            .map_err(repo_error)?;
        let d = repo.descriptor().clone();
        self.config
            .cache_repository(&input.connection_id, d.clone(), repo.local_master())
            .map_err(config_error)?;
        Ok(RepositoryRedacted {
            id: d.repo_id,
            connection_id: input.connection_id,
            mode: d.mode,
        })
    }
}
fn probe_apply_capability(data_dir: &Path) -> bool {
    let probe = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let area = tempfile::tempdir_in(data_dir)?;
        let root = area.path().join("root");
        let private = area.path().join("private");
        std::fs::create_dir(&root)?;
        std::fs::create_dir(&private)?;
        crate::sync::sandbox::run_confined(
            std::fs::File::open(root)?,
            std::fs::File::open(private)?,
            |_, _| Ok(()),
        )?;
        Ok(())
    })();
    probe.is_ok()
}
fn runtime_error(code: &str, msg: &str) -> AppError {
    AppError {
        code: code.into(),
        message: msg.into(),
        detail: None,
        context: None,
    }
}
fn config_error(e: ConfigError) -> AppError {
    let code = match &e {
        ConfigError::NotFound(_) => "CLOUD_CONFIG_NOT_FOUND",
        _ => "CLOUD_CONFIG_INVALID",
    };
    DomainError::Cloud {
        code,
        user_reason: "云配置操作失败 / Cloud configuration failed",
        source: Box::new(e),
    }
    .into()
}
fn cloud_error(e: crate::cloud::types::CloudError) -> AppError {
    DomainError::Cloud {
        code: "CLOUD_STORAGE",
        user_reason: "云存储连接失败 / Cloud storage connection failed",
        source: Box::new(e),
    }
    .into()
}
fn repo_error(e: crate::sync::repository::RepositoryError) -> AppError {
    DomainError::Cloud {
        code: "CLOUD_REPOSITORY",
        user_reason: "仓库操作失败 / Repository operation failed",
        source: Box::new(e),
    }
    .into()
}
#[cfg(test)]
mod preview_count_tests {
    use super::preview_counts;
    use crate::cloud::runtime_types::PreviewCounts;
    use crate::sync::merge_types::{
        EntityId, FileNode, Node, Projection, ProjectionEntry, RevisionId, RevisionValue,
    };
    use crate::sync::scan_types::{FileScan, ScanReport};

    fn report(files: &[(&str, &str)]) -> ScanReport {
        ScanReport {
            files: files
                .iter()
                .map(|(path, digest)| FileScan {
                    path: (*path).to_owned(),
                    inode: 1,
                    device: 1,
                    size: 1,
                    modified: 0,
                    changed: 0,
                    blocks: Vec::new(),
                    whole_file_hash: (*digest).to_owned(),
                })
                .collect(),
            directories: Vec::new(),
            ignored_paths: Vec::new(),
            complete: true,
        }
    }

    fn projection(path: &str, digest: &str, entity: &str, revision: &str) -> Projection {
        Projection {
            entries: vec![ProjectionEntry {
                entity: EntityId(entity.to_owned()),
                version: RevisionId(revision.to_owned()),
                logical_path: vec!["logical".to_owned(), path.to_owned()],
                projection_path: vec![path.to_owned()],
                value: RevisionValue::Live {
                    parent_dir: EntityId("root".to_owned()),
                    name: path.to_owned(),
                    node: Node::File(FileNode {
                        blocks: Vec::new(),
                        length: 1,
                        digest: digest.to_owned(),
                    }),
                },
                conflict: false,
            }],
        }
    }

    fn empty_projection() -> Projection {
        Projection {
            entries: Vec::new(),
        }
    }

    fn counts(report: &ScanReport, base: &Projection, remote: &Projection) -> PreviewCounts {
        preview_counts(report, base, remote)
    }

    fn assert_counts(
        actual: &PreviewCounts,
        uploads: u64,
        downloads: u64,
        deletes: u64,
        conflicts: u64,
    ) {
        assert_eq!(actual.uploads, uploads);
        assert_eq!(actual.downloads, downloads);
        assert_eq!(actual.deletes, deletes);
        assert_eq!(actual.conflicts, conflicts);
    }

    #[test]
    fn baseline_none_local_file_remote_none_uploads_one() {
        let actual = counts(
            &report(&[("a", "new")]),
            &empty_projection(),
            &empty_projection(),
        );
        assert_counts(&actual, 1, 0, 0, 0);
    }

    #[test]
    fn baseline_file_local_same_remote_changed_downloads_one() {
        let base = projection("a", "old", "entity-a", "rev-base");
        let remote = projection("a", "new", "entity-a", "rev-remote");
        let actual = counts(&report(&[("a", "old")]), &base, &remote);
        assert_counts(&actual, 0, 1, 0, 0);
    }

    #[test]
    fn baseline_file_local_missing_remote_same_deletes_one() {
        let base = projection("a", "old", "entity-a", "rev-base");
        let actual = counts(&report(&[]), &base, &base);
        assert_counts(&actual, 0, 0, 1, 0);
    }

    #[test]
    fn baseline_file_local_same_remote_missing_deletes_one() {
        let base = projection("a", "old", "entity-a", "rev-base");
        let actual = counts(&report(&[("a", "old")]), &base, &empty_projection());
        assert_counts(&actual, 0, 0, 1, 0);
    }

    #[test]
    fn baseline_file_local_and_remote_different_files_conflict_one() {
        let base = projection("a", "old", "entity-a", "rev-base");
        let remote = projection("a", "remote", "entity-a", "rev-remote");
        let actual = counts(&report(&[("a", "local")]), &base, &remote);
        assert_counts(&actual, 0, 0, 0, 1);
    }

    #[test]
    fn baseline_none_local_and_remote_different_files_conflict_one() {
        let base = empty_projection();
        let remote = projection("a", "remote", "entity-a", "rev-remote");
        let actual = counts(&report(&[("a", "local")]), &base, &remote);
        assert_counts(&actual, 0, 0, 0, 1);
    }

    #[test]
    fn baseline_unknown_empty_local_directory_remote_missing_uploads_one() {
        let local = ScanReport {
            directories: vec!["dir".to_owned()],
            ..report(&[])
        };
        let actual = counts(&local, &empty_projection(), &empty_projection());
        assert_counts(&actual, 1, 0, 0, 0);
    }

    #[test]
    fn projection_scope_uses_projection_path_not_logical_path() {
        let base = projection("a", "old", "same-entity", "same-revision");
        let mut remote = projection("a", "old", "different-entity", "different-revision");
        remote.entries[0].logical_path = vec!["private-scope".to_owned(), "unrelated".to_owned()];
        let actual = counts(&report(&[("a", "old")]), &base, &remote);
        assert_counts(&actual, 0, 0, 0, 0);
        let encoded = serde_json::to_string(&remote.entries[0].projection_path)
            .expect("serialize projection path");
        assert!(!encoded.contains("private-scope"));
    }
}
