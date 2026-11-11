use super::*;
use crate::cloud::types::S3Config;
use crate::sync::{
    append_discovery, append_engine, append_engine_types::AppendPullRequest,
    append_index::AppendIndex, append_repository::AppendRepository, append_types::*,
};
use std::collections::BTreeMap;

pub(crate) fn append_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> AppError {
    let any = &error as &dyn std::any::Any;
    let code = match any.downcast_ref::<AppendError>() {
        Some(AppendError::TrustRejected) => "CLOUD_APPEND_TRUST_REQUIRED",
        Some(AppendError::FingerprintMismatch) => "CLOUD_APPEND_FINGERPRINT",
        Some(AppendError::Authentication) => "CLOUD_APPEND_AUTHENTICATION",
        Some(AppendError::PendingDependency(_)) => "CLOUD_APPEND_DEPENDENCY",
        _ => "CLOUD_APPEND",
    };
    let code = match any.downcast_ref::<crate::sync::append_engine_types::AppendEngineError>() {
        Some(crate::sync::append_engine_types::AppendEngineError::Local(
            crate::sync::engine_types::EngineError::DeletionConfirmation,
        )) => "CLOUD_DELETE_CONFIRMATION",
        _ => code,
    };
    AppError::from(DomainError::Cloud {
        code,
        user_reason: "追加仓库操作失败 / Append repository operation failed",
        source: Box::new(error),
    })
}
fn trust(binding: &AppendRepositoryBinding) -> TrustBinding {
    TrustBinding {
        key: binding.anchor_key.clone(),
        repo_id: binding.repo_id.clone(),
        fingerprint: binding.anchor_fingerprint.clone(),
        mode: binding.append_mode.clone(),
        source: binding.trust_source.clone(),
    }
}
fn revisions(
    closure: &AuthenticatedMetadata,
) -> Result<crate::sync::merge_types::RevisionMap, AppError> {
    let mut revisions = BTreeMap::new();
    for tree in closure.trees().values() {
        for revision in &tree.entries {
            if let Some(old) = revisions.insert(revision.revision_id.clone(), revision.clone()) {
                if old != *revision {
                    return Err(append_error(AppendError::Invalid("revision collision")));
                }
            }
        }
    }
    Ok(revisions)
}
impl CloudRuntime {
    pub(super) fn append_config(&self, connection: &str, bucket: Option<&str>) -> Result<S3Config, AppError> {
        let mut config = self.connection(connection)?.config;
        if let Some(bucket) = bucket {
            if bucket != config.bucket && !config.buckets.iter().any(|item| item == bucket) {
                return Err(runtime_error("CLOUD_CONFIG_INVALID", "请选择已配置的存储桶 / Select a configured bucket"));
            }
            config.bucket = bucket.to_owned();
        }
        Ok(config)
    }
    pub(super) fn append_storage(&self, connection: &str, bucket: Option<&str>) -> Result<AppendStorage, AppError> {
        if let Some(entry) = self.config.snapshot().map_err(config_error)?.official_connections.into_iter().find(|entry| entry.id == connection) {
            if bucket.is_some_and(|bucket| bucket != entry.root_id) { return Err(runtime_error("CLOUD_CONFIG_INVALID", "网盘根范围不匹配 / Drive root scope mismatch")); }
            return Ok(AppendStorage { endpoint: "https://www.googleapis.com/drive/v3".into(), bucket: entry.root_id, prefix: String::new() });
        }
        let config = self.append_config(connection, bucket)?;
        Ok(AppendStorage { endpoint: config.endpoint, bucket: config.bucket, prefix: config.prefix })
    }

    fn append_store(&self, connection: &str) -> Result<Arc<dyn ObjectStore>, AppError> {
        if let Some(store) = self.google_store(connection)? { return Ok(store); }
        Ok(Arc::new(S3Store::new(self.append_config(connection, None)?).map_err(cloud_error)?))
    }
    fn bound_append_store(&self, cached: &CachedAppendRepository) -> Result<Arc<dyn ObjectStore>, AppError> {
        if let Some(store) = self.google_store(&cached.connection_id)? {
            let binding = self.append_storage(&cached.connection_id, None)?;
            if cached.binding.storage.as_ref() != Some(&binding) { return Err(runtime_error("CLOUD_PREVIEW_STALE", "同步盘存储范围已变化 / Sync storage scope changed")); }
            return Ok(store);
        }
        let mut config = self.connection(&cached.connection_id)?.config;
        if let Some(storage) = &cached.binding.storage {
            if config.endpoint != storage.endpoint {
                return Err(runtime_error("CLOUD_CONFIG_INVALID", "同步盘端点已变化，请重新连接 / Drive endpoint changed; reconnect the drive"));
            }
            config.bucket = storage.bucket.clone();
            config.prefix = storage.prefix.clone();
        }
        Ok(Arc::new(S3Store::new(config).map_err(cloud_error)?))
    }
    pub async fn discover_append_in_bucket(&self, connection: &str, bucket: Option<&str>) -> Result<Vec<Candidate>, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        self.append_storage(connection, bucket)?;
        let store: Arc<dyn ObjectStore> = if let Some(store) = self.google_store(connection)? { store }
            else { Arc::new(S3Store::new(self.append_config(connection, bucket)?).map_err(cloud_error)?) };
        AppendRepository::discover_anchors(store.as_ref()).await.map_err(append_error)
    }
    pub async fn discover_append_repositories(
        &self,
        connection: &str,
    ) -> Result<Vec<Candidate>, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        AppendRepository::discover_anchors(self.append_store(connection)?.as_ref())
            .await
            .map_err(append_error)
    }
    /// New sync drives use Append v2 without probing conditional-write support.
    /// Existing CAS repositories and mappings retain their stored protocol.
    pub async fn create_sync_drive(
        &self,
        input: CreateAppendRepositoryInput,
    ) -> Result<AppendRepositoryRedacted, AppError> {
        self.create_append_repository(input).await
    }

    pub async fn create_append_repository(
        &self,
        input: CreateAppendRepositoryInput,
    ) -> Result<AppendRepositoryRedacted, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        let password = Zeroizing::new(input.password);
        let confirmation = Zeroizing::new(input.password_confirmation);
        if password.chars().count() < 6 || password.as_str() != confirmation.as_str() {
            return Err(runtime_error(
                "CLOUD_APPEND_PASSWORD",
                "新密码至少需要 6 位且确认必须一致 / New passwords require at least 6 characters and matching confirmation",
            ));
        }
        let descriptor = Descriptor {
            version: 2,
            repo_id: hex::encode(rand::random::<[u8; 16]>()),
            mode: input.mode,
            chunker: Chunker {
                name: "fastcdc".into(),
                version: "5.0.0".into(),
                normalization: "level1".into(),
                level: 1,
                min: 1048576,
                average: 4194304,
                max: 8388608,
                seed: 0,
            },
        };
        let (object, mut selected) =
            AppendRepository::prepare_anchor(descriptor, password.as_bytes())
                .map_err(append_error)?;
        selected.source = TrustSource::AcceptedTofu;
        let storage = Some(self.append_storage(&input.connection_id, input.bucket.as_deref())?);
        let store: Arc<dyn ObjectStore> = if let Some(store) = self.google_store(&input.connection_id)? { store }
            else { Arc::new(S3Store::new(self.append_config(&input.connection_id, input.bucket.as_deref())?).map_err(cloud_error)?) };
        store
            .put(
                object.key(),
                crate::cloud::bytes_stream(object.bytes().to_vec().into()),
                crate::cloud::types::PutCondition::Any,
            )
            .await
            .map_err(cloud_error)?;
        AppendRepository::open_selected(store, selected.clone(), password.as_bytes())
            .await
            .map_err(append_error)?;
        let cached = CachedAppendRepository {
            connection_id: input.connection_id,
            password: password.to_string(),
            binding: AppendRepositoryBinding {
                storage,
                repo_id: selected.repo_id,
                anchor_key: selected.key,
                anchor_fingerprint: selected.fingerprint,
                append_mode: selected.mode,
                trust_source: selected.source,
                root_candidates: Vec::new(),
                selected_root: None,
            },
        };
        let redacted = cached.redacted();
        self.config
            .cache_append_repository(cached)
            .map_err(config_error)?;
        Ok(redacted)
    }
    pub async fn join_append_repository(
        &self,
        input: JoinAppendRepositoryInput,
    ) -> Result<AppendRepositoryRedacted, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        let source = match input.trusted_fingerprint.as_deref() {
            Some(expected) if expected == input.anchor_fingerprint => {
                TrustSource::TrustedFingerprint
            }
            Some(_) => return Err(append_error(AppendError::FingerprintMismatch)),
            None if input.accept_tofu => TrustSource::AcceptedTofu,
            None => return Err(append_error(AppendError::TrustRejected)),
        };
        let password = Zeroizing::new(input.password);
        let selected = TrustBinding {
            key: input.anchor_key,
            fingerprint: input.anchor_fingerprint,
            repo_id: input.repository_id,
            mode: input.mode,
            source,
        };
        let storage = Some(self.append_storage(&input.connection_id, input.bucket.as_deref())?);
        let store: Arc<dyn ObjectStore> = if let Some(store) = self.google_store(&input.connection_id)? { store }
            else { Arc::new(S3Store::new(self.append_config(&input.connection_id, input.bucket.as_deref())?).map_err(cloud_error)?) };
        AppendRepository::open_selected(store.clone(), selected.clone(), password.as_bytes())
            .await
            .map_err(append_error)?;
        let temp = tempfile::tempdir_in(&self.data_dir).map_err(append_error)?;
        let mut index = AppendIndex::open(
            &temp.path().join("discovery.sqlite"),
            self.config
                .get_or_create_index_key()
                .map_err(config_error)?,
            selected.repo_id.clone(),
        )
        .map_err(append_error)?;
        let root_candidates = append_discovery::discover_roots(
            store,
            &selected,
            password.as_bytes(),
            &mut index,
            &hex::encode(rand::random::<[u8; 16]>()),
        )
        .await
        .map_err(append_error)?;
        let cached = CachedAppendRepository {
            connection_id: input.connection_id,
            password: password.to_string(),
            binding: AppendRepositoryBinding {
                storage,
                repo_id: selected.repo_id,
                anchor_key: selected.key,
                anchor_fingerprint: selected.fingerprint,
                append_mode: selected.mode,
                trust_source: selected.source,
                root_candidates,
                selected_root: None,
            },
        };
        let result = cached.redacted();
        self.config
            .cache_append_repository(cached)
            .map_err(config_error)?;
        Ok(result)
    }
    pub async fn prepare_append_mapping(
        &self, input: SaveAppendMappingInput,
    ) -> Result<CloudMapping, AppError> {
        self.admit()?;
        if input.mapping_id.is_some() { return Err(runtime_error("CLOUD_CONFIG_INVALID", "预绑定仅适用于新映射 / Prebinding is only available for new mappings")); }
        let mut cached = self.config.load_append_repository(&input.mapping.connection_id, &input.mapping.repository_id).map_err(config_error)?;
        if !cached.binding.is_trusted() || input.selected_root != input.mapping.logical_path
            || (!cached.binding.root_candidates.is_empty() && !cached.binding.root_candidates.contains(&input.selected_root)) {
            return Err(append_error(AppendError::TrustRejected));
        }
        crate::sync::namespace::components(&input.selected_root).map_err(append_error)?;
        if cached.binding.root_candidates.is_empty() { cached.binding.root_candidates.push(input.selected_root.clone()); }
        cached.binding.selected_root = Some(input.selected_root);
        self.config.prepare_mapping(input.mapping, &[self.data_dir.as_path()], Some(cached.binding.clone())).map_err(config_error)
    }

    pub async fn inspect_mapping_preparation(&self, mapping: CloudMapping) -> Result<MappingPreparation, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        let revision = self.connection_revision(&mapping.connection_id)?;
        let epoch = Self::root_epoch(&mapping.local_root)?;
        let root = mapping.local_root.clone();
        let cancel = Arc::clone(&self.cancel);
        let report = tokio::task::spawn_blocking(move || scan(&root, &ScanOptions { cancel, ..Default::default() })).await.map_err(append_error)?.map_err(append_error)?;
        if !report.complete || !report.ignored_paths.is_empty() { return Err(runtime_error("CLOUD_SCAN_FAILED", "目录扫描不完整 / Directory scan is incomplete")); }
        let binding = mapping.append_binding.as_ref().ok_or_else(|| append_error(AppendError::TrustRejected))?;
        let cached = self.config.load_append_repository(&mapping.connection_id, &mapping.repository_id).map_err(config_error)?;
        if !binding.is_trusted() || trust(binding) != trust(&cached.binding) || binding.storage != cached.binding.storage || binding.selected_root.as_deref() != Some(mapping.logical_path.as_str()) { return Err(append_error(AppendError::TrustRejected)); }
        let mut index = self.open_append_index(&mapping, &epoch)?;
        let operation = index.begin_run("preview-metadata-v1", &hex::encode(rand::random::<[u8; 16]>())).map_err(append_error)?;
        let closure = append_discovery::discover_metadata(self.bound_append_store(&cached)?, &trust(binding), cached.password.as_bytes(), &mut index, &operation).await.map_err(append_error)?;
        let remote = revisions(&closure)?;
        let projection = crate::sync::namespace::project_scoped(&merge::project(&remote).map_err(append_error)?, &mapping.logical_path).map_err(append_error)?;
        let remote_empty = projection.entries.iter().all(|entry| !matches!(entry.value, RevisionValue::Live { .. }));
        let remote_digest = hex::encode(Sha256::digest(serde_json::to_vec(&projection).map_err(append_error)?));
        self.admit()?;
        if Self::root_epoch(&mapping.local_root)? != epoch || self.connection_revision(&mapping.connection_id)? != revision { return Err(runtime_error("CLOUD_PREVIEW_STALE", "目录或配置已变化 / Folder or configuration changed")); }
        index.complete_run("preview-metadata-v1", &operation).map_err(append_error)?;
        let remote_paths: BTreeMap<_, _> = projection.entries.iter().map(|entry| (entry.projection_path.join("/"), &entry.value)).collect();
        let mut entries = Vec::new();
        let mut observed = std::collections::BTreeSet::new();
        for file in &report.files {
            observed.insert(file.path.clone());
            let action = match remote_paths.get(&file.path) {
                Some(RevisionValue::Live { node: Node::File(saved), .. }) if saved.digest == file.whole_file_hash && saved.length == file.size => continue,
                Some(RevisionValue::Live { .. }) => "conflict",
                _ => "upload",
            };
            entries.push(MappingPlanEntry { path: file.path.clone(), action: action.into(), is_dir: false });
        }
        for path in report.directories.iter().filter(|path| !path.is_empty()) {
            observed.insert(path.clone());
            if !matches!(remote_paths.get(path), Some(RevisionValue::Live { node: Node::Dir, .. })) {
                entries.push(MappingPlanEntry { path: path.clone(), action: if remote_paths.contains_key(path) { "conflict" } else { "upload" }.into(), is_dir: true });
            }
        }
        for (path, value) in &remote_paths {
            if observed.contains(path) || path.is_empty() { continue; }
            if let RevisionValue::Live { node, .. } = value {
                entries.push(MappingPlanEntry { path: path.clone(), action: "download".into(), is_dir: matches!(node, Node::Dir) });
            }
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(MappingPreparation { mapping, entries, local_empty: report.files.is_empty() && report.directories.iter().all(|path| path.is_empty()), remote_empty, local_digest: Self::digest(&report)?, remote_digest, connection_revision: revision, root_epoch: epoch })
    }

    pub async fn prepare_mapping_bind(&self, input: SaveAppendMappingInput) -> Result<MappingBindResult, AppError> {
        let mapping = self.prepare_append_mapping(input).await?;
        let preparation = self.inspect_mapping_preparation(mapping).await?;
        let mut pending = self.pending_mappings.lock().await;
        pending.retain(|_, entry| entry.created.elapsed().as_secs() < 300);
        if pending.len() >= 64 { return Err(runtime_error("CLOUD_PREVIEW_STALE", "待确认计划过多 / Too many pending plans")); }
        let id = crate::local_upload::token()?;
        let entries = preparation.entries.clone();
        let automatic = preparation.local_empty || preparation.remote_empty;
        pending.insert(id.clone(), PendingMapping { preparation, created: Instant::now() });
        drop(pending);
        if automatic { return self.confirm_mapping_bind(&id, false).await; }
        Ok(MappingBindResult { plan_id: Some(id), entries, mapping: None, run_id: None })
    }

    pub async fn validate_mapping_bind(&self, plan_id: &str) -> Result<MappingPreparation, AppError> {
        self.admit()?;
        let preparation = {
            let pending = self.pending_mappings.lock().await;
            let plan = pending.get(plan_id).ok_or_else(|| runtime_error("CLOUD_PREVIEW_STALE", "绑定计划已失效 / Binding plan is stale"))?;
            if plan.created.elapsed().as_secs() >= 300 { return Err(runtime_error("CLOUD_PREVIEW_STALE", "绑定计划已过期 / Binding plan expired")); }
            plan.preparation.clone()
        };
        let current = self.inspect_mapping_preparation(preparation.mapping.clone()).await?;
        if current.local_digest != preparation.local_digest || current.remote_digest != preparation.remote_digest
            || current.root_epoch != preparation.root_epoch || current.connection_revision != preparation.connection_revision {
            return Err(runtime_error("CLOUD_PREVIEW_STALE", "目录或远端已变化，请重新准备 / Local or remote contents changed; prepare again"));
        }
        Ok(current)
    }

    pub async fn confirm_mapping_bind(&self, plan_id: &str, confirmed: bool) -> Result<MappingBindResult, AppError> {
        let preparation = self.validate_mapping_bind(plan_id).await?;
        if !confirmed && !preparation.local_empty && !preparation.remote_empty {
            return Err(runtime_error("CLOUD_CONFIRMATION_REQUIRED", "请确认同步计划 / Confirm the sync plan"));
        }
        let pending = self.pending_mappings.lock().await.remove(plan_id).ok_or_else(|| runtime_error("CLOUD_PREVIEW_STALE", "绑定计划已失效 / Binding plan is stale"))?;
        if pending.created.elapsed().as_secs() >= 300 { return Err(runtime_error("CLOUD_PREVIEW_STALE", "绑定计划已过期 / Binding plan expired")); }
        let saved = self.config.commit_prepared_mapping(&preparation.mapping, &[self.data_dir.as_path()]).map_err(config_error)?;
        let result = async {
            let plan = self.preview(&saved.id).await?;
            let current = self.inspect_mapping_preparation(saved.clone()).await?;
            if current.local_digest != preparation.local_digest || current.remote_digest != preparation.remote_digest || current.root_epoch != preparation.root_epoch || current.connection_revision != preparation.connection_revision {
                return Err(runtime_error("CLOUD_PREVIEW_STALE", "目录或远端已变化，请重新准备 / Local or remote contents changed; prepare again"));
            }
            self.manual_sync(&saved.id, &plan.preview_id).await
        }.await;
        match result {
            Ok(started) => Ok(MappingBindResult { plan_id: None, entries: preparation.entries, mapping: Some(saved), run_id: Some(started.run_id) }),
            Err(error) => {
                self.config.delete_mapping(&saved.id).map_err(config_error)?;
                Err(error)
            }
        }
    }

    pub async fn cancel_mapping_bind(&self, plan_id: &str) -> Result<(), AppError> {
        self.admit()?;
        self.pending_mappings.lock().await.remove(plan_id);
        Ok(())
    }

    pub async fn save_append_mapping(
        &self,
        input: SaveAppendMappingInput,
    ) -> Result<CloudMapping, AppError> {
        self.admit()?;
        // Empty repositories allow an explicitly named new root. Nonempty repositories
        // require selection from authenticated discovery, never candidate ordering.
        let mut cached = self
            .config
            .load_append_repository(&input.mapping.connection_id, &input.mapping.repository_id)
            .map_err(config_error)?;
        if input.selected_root != input.mapping.logical_path
            || (!cached.binding.root_candidates.is_empty()
                && !cached
                    .binding
                    .root_candidates
                    .contains(&input.selected_root))
        {
            return Err(runtime_error(
                "CLOUD_APPEND_ROOT_SELECTION",
                "必须显式选择已发现的根目录 / Explicit discovered root selection is required",
            ));
        }
        crate::sync::namespace::components(&input.selected_root).map_err(append_error)?;
        if cached.binding.root_candidates.is_empty() {
            cached
                .binding
                .root_candidates
                .push(input.selected_root.clone());
        }
        cached.binding.selected_root = Some(input.selected_root);
        let _permit = self.network.write().await;
        self.admit()?;
        let mapping = input.mapping;
        if let Some(id) = input.mapping_id.as_deref() {
            self.manager.stop_mapping(id).await?;
        }
        let saved = self
            .config
            .save_append_mapping(
                input.mapping_id.as_deref(),
                mapping,
                &[self.data_dir.as_path()],
                cached.binding.clone(),
            )
            .map_err(config_error)?;
        self.reselect_root_guard(&saved)?;
        Ok(saved)
    }
    pub(crate) fn open_append_index(
        &self,
        mapping: &CloudMapping,
        epoch: &str,
    ) -> Result<AppendIndex, AppError> {
        let name = hex::encode(Sha256::digest(Self::index_scope(mapping, epoch).as_bytes()));
        AppendIndex::open(
            &self.data_dir.join(format!("cloud-append-{name}.sqlite")),
            self.config
                .get_or_create_index_key()
                .map_err(config_error)?,
            mapping.repository_id.clone(),
        )
        .map_err(append_error)
    }
    fn append_cached(&self, mapping: &CloudMapping) -> Result<CachedAppendRepository, AppError> {
        let binding = self
            .config
            .load_trusted_append_binding(&mapping.id)
            .map_err(config_error)?;
        let cached = self
            .config
            .load_append_repository(&mapping.connection_id, &mapping.repository_id)
            .map_err(config_error)?;
        if trust(&cached.binding) != trust(&binding)
            || cached.binding.storage != binding.storage
            || binding.selected_root.as_deref() != Some(mapping.logical_path.as_str())
        {
            return Err(append_error(AppendError::TrustRejected));
        }
        Ok(cached)
    }
    /// Compare local contents with the last acknowledged epoch, without accessing
    /// the network or treating an in-flight publication as synchronized.
    /// Metadata-only listing: never reads file contents or opens the sync index.
    pub async fn local_directory_entries(&self, mapping_id: &str) -> Result<Vec<LocalSyncEntry>, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        let mapping = self.mapping(mapping_id)?;
        let epoch = Self::root_epoch(&mapping.local_root)?;
        let root = mapping.local_root.clone();
        let cancel = Arc::clone(&self.cancel);
        let entries = tokio::task::spawn_blocking(move || {
            fn walk(dir: &cap_std::fs::Dir, prefix: &str, cancel: &std::sync::atomic::AtomicBool, entries: &mut Vec<LocalSyncEntry>, depth: usize) -> Result<(), AppError> {
                use cap_fs_ext::DirExt;
                if depth > 128 || entries.len() >= 100_000 {
                    return Err(runtime_error("CLOUD_LIST_LIMIT", "目录过大，请选择更小的同步目录 / Directory listing limit exceeded"));
                }
                for item in dir.entries().map_err(append_error)? {
                    if cancel.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(runtime_error("CLOUD_CANCELLED", "目录读取已取消 / Directory listing cancelled"));
                    }
                    let item = item.map_err(append_error)?;
                    let name = item.file_name().into_string().map_err(|_| runtime_error("CLOUD_PATH_INVALID", "文件名称无效 / Invalid file name"))?;
                    let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                    if crate::ignore_rules::is_sync_temporary_path(&path) { continue; }
                    let metadata = dir.symlink_metadata(&name).map_err(append_error)?;
                    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) { continue; }
                    if entries.len() >= 100_000 { return Err(runtime_error("CLOUD_LIST_LIMIT", "目录条目过多 / Too many directory entries")); }
                    entries.push(LocalSyncEntry { path: path.clone(), name: name.clone(), is_dir: metadata.is_dir(), size: metadata.len(), synced: false, remote_only: false });
                    if metadata.is_dir() {
                        walk(&dir.open_dir_nofollow(&name).map_err(append_error)?, &path, cancel, entries, depth + 1)?;
                    }
                }
                Ok(())
            }
            let dir = crate::sync::materialize::open_root(&root).map_err(append_error)?;
            let mut entries = Vec::new();
            walk(&dir, "", &cancel, &mut entries, 0)?;
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.path.cmp(&b.path)));
            Ok::<_, AppError>(entries)
        }).await.map_err(append_error)??;
        if Self::root_epoch(&mapping.local_root)? != epoch {
            return Err(runtime_error("CLOUD_ROOT_CHANGED", "本地目录已变化，请刷新 / Local root changed; refresh the listing"));
        }
        Ok(entries)
    }

    pub async fn local_sync_entries(&self, mapping_id: &str) -> Result<Vec<LocalSyncEntry>, AppError> {
        self.admit()?;
        let _permit = self.network.read().await;
        self.admit()?;
        let mapping = self.mapping(mapping_id)?;
        let mapping_digest = Self::mapping_digest(&mapping)?;
        let connection_revision = self.connection_revision(&mapping.connection_id)?;
        let epoch = Self::root_epoch(&mapping.local_root)?;
        let revisions = if mapping.protocol == StorageProtocol::AppendV2 {
            self.append_cached(&mapping)?;
            let index = self.open_append_index(&mapping, &epoch)?;
            match index.active_epoch().map_err(append_error)? {
                Some(epoch) => index.epoch_revisions(&epoch).map_err(append_error)?,
                None => BTreeMap::new(),
            }
        } else {
            let index = SyncIndex::open(
                &self.data_dir.join("cloud-sync.sqlite"),
                self.config.get_or_create_index_key().map_err(config_error)?,
                mapping.repository_id.clone(), Self::index_scope(&mapping, &epoch),
            ).map_err(append_error)?;
            index.load::<MergeSet>("engine", "materialized").map_err(append_error)?
                .map(|set| set.revisions).unwrap_or_default()
        };
        let projection = crate::sync::namespace::project_scoped(
            &merge::project(&revisions).map_err(append_error)?, &mapping.logical_path,
        ).map_err(append_error)?;
        let root = mapping.local_root.clone();
        let cancel = Arc::clone(&self.cancel);
        let report = tokio::task::spawn_blocking(move || scan(&root, &ScanOptions { cancel, ..Default::default() }))
            .await.map_err(append_error)?.map_err(append_error)?;
        if Self::root_epoch(&mapping.local_root)? != epoch || !report.complete {
            return Err(runtime_error("CLOUD_ROOT_CHANGED", "本地目录已变化，请刷新 / Local root changed; refresh the listing"));
        }
        let base: BTreeMap<_, _> = projection.entries.iter()
            .map(|entry| (entry.projection_path.join("/"), &entry.value)).collect();
        let mut entries = Vec::new();
        for file in &report.files {
            let synced = matches!(base.get(&file.path), Some(RevisionValue::Live { node: Node::File(saved), .. })
                if saved.length == file.size && saved.digest == file.whole_file_hash);
            entries.push(LocalSyncEntry { path: file.path.clone(), name: file.path.rsplit('/').next().unwrap_or("").to_owned(), is_dir: false, size: file.size, synced, remote_only: false });
        }
        for path in report.directories.iter().filter(|path| !path.is_empty()) {
            let synced = matches!(base.get(path), Some(RevisionValue::Live { node: Node::Dir, .. }));
            entries.push(LocalSyncEntry { path: path.clone(), name: path.rsplit('/').next().unwrap_or("").to_owned(), is_dir: true, size: 0, synced, remote_only: false });
        }
        // Propagate changes (including locally deleted descendants) to folders.
        let local: std::collections::BTreeSet<_> = entries.iter().map(|e| e.path.as_str()).collect();
        let changed: Vec<_> = entries.iter().filter(|e| !e.synced).map(|e| e.path.clone())
            .chain(base.keys().filter(|path| !local.contains(path.as_str())).cloned()).collect();
        for entry in entries.iter_mut().filter(|e| e.is_dir) {
            let prefix = format!("{}/", entry.path);
            if changed.iter().any(|path| path.starts_with(&prefix)) { entry.synced = false; }
        }
        let remote_revisions = if mapping.protocol == StorageProtocol::AppendV2 {
            let cached = self.append_cached(&mapping)?;
            let mut index = self.open_append_index(&mapping, &epoch)?;
            let operation = index.begin_run("preview-metadata-v1", &hex::encode(rand::random::<[u8; 16]>())).map_err(append_error)?;
            let closure = append_discovery::discover_metadata(self.bound_append_store(&cached)?, &trust(&cached.binding), cached.password.as_bytes(), &mut index, &operation).await.map_err(append_error)?;
            let remote = self::revisions(&closure)?;
            index.complete_run("preview-metadata-v1", &operation).map_err(append_error)?;
            remote
        } else {
            let (repository, _) = self.repo(&mapping).await?;
            match repository.head().await.map_err(repo_error)?.commit {
                Some(commit) => {
                    let control = PublishControl { cancel: Arc::clone(&self.cancel), epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)), expected_epoch: 0 };
                    crate::sync::tree::load_commit_snapshot(&repository, &commit, &control).await.map_err(append_error)?.snapshot.revisions
                }
                None => BTreeMap::new(),
            }
        };
        let remote = crate::sync::namespace::project_scoped(&merge::project(&remote_revisions).map_err(append_error)?, &mapping.logical_path).map_err(append_error)?;
        let local_paths: std::collections::BTreeSet<_> = entries.iter().map(|entry| entry.path.clone()).collect();
        let remote_paths: BTreeMap<_, _> = remote.entries.iter().map(|entry| (entry.projection_path.join("/"), &entry.value)).collect();
        for entry in &mut entries {
            entry.synced &= base.get(&entry.path) == remote_paths.get(&entry.path);
        }
        let changed_remote: Vec<_> = remote_paths.keys().filter(|path| !local_paths.contains(*path)).cloned()
            .chain(entries.iter().filter(|entry| !entry.synced).map(|entry| entry.path.clone())).collect();
        for entry in entries.iter_mut().filter(|entry| entry.is_dir) {
            let prefix = format!("{}/", entry.path);
            if changed_remote.iter().any(|path| path.starts_with(&prefix)) { entry.synced = false; }
        }
        for remote_entry in &remote.entries {
            let path = remote_entry.projection_path.join("/");
            let value = &remote_entry.value;
            if path.is_empty() || local_paths.contains(&path) { continue; }
            let (is_dir, size) = match value {
                RevisionValue::Live { node: Node::File(file), .. } => (false, file.length),
                RevisionValue::Live { node: Node::Dir, .. } => (true, 0),
                RevisionValue::Tombstone => continue,
            };
            if entries.len() >= 100_000 { return Err(runtime_error("CLOUD_LIST_LIMIT", "目录条目过多 / Too many directory entries")); }
            entries.push(LocalSyncEntry { path: path.to_string(), name: path.rsplit('/').next().unwrap_or("").to_owned(), is_dir, size, synced: false, remote_only: true });
        }
        self.admit()?;
        if Self::mapping_digest(&self.mapping(mapping_id)?)? != mapping_digest || self.connection_revision(&mapping.connection_id)? != connection_revision {
            return Err(runtime_error("CLOUD_PREVIEW_STALE", "远端或配置已变化 / Remote or configuration changed"));
        }
        if Self::root_epoch(&mapping.local_root)? != epoch { return Err(runtime_error("CLOUD_ROOT_CHANGED", "本地目录已变化，请刷新 / Local root changed; refresh the listing")); }
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.path.cmp(&b.path)));
        Ok(entries)
    }

    pub(super) async fn append_preview_state(
        &self,
        mapping: &CloudMapping,
        epoch: &str,
    ) -> Result<(PreviewCounts, String), AppError> {
        self.append_preview_selection(mapping, epoch, &[]).await
    }

    pub(super) async fn append_preview_selection(
        &self, mapping: &CloudMapping, epoch: &str, selection: &[String],
    ) -> Result<(PreviewCounts, String), AppError> {
        let cached = self.append_cached(mapping)?;
        let mut index = self.open_append_index(mapping, epoch)?;
        // Separate metadata-only previews from older full-discovery attempts:
        // an exhausted legacy payload budget must not poison metadata browsing.
        // Retries within this slot still retain every reservation.
        let operation = index
            .begin_run("preview-metadata-v1", &hex::encode(rand::random::<[u8; 16]>()))
            .map_err(append_error)?;
        let closure = append_discovery::discover_metadata(
            self.bound_append_store(&cached)?,
            &trust(&cached.binding),
            cached.password.as_bytes(),
            &mut index,
            &operation,
        )
        .await
        .map_err(append_error)?;
        let remote = revisions(&closure)?;
        let projection = crate::sync::namespace::project_scoped(
            &merge::project(&remote).map_err(append_error)?,
            &mapping.logical_path,
        )
        .map_err(append_error)?;
        let base = match index.active_epoch().map_err(append_error)? {
            Some(epoch) => index.epoch_revisions(&epoch).map_err(append_error)?,
            None => BTreeMap::new(),
        };
        let base = crate::sync::namespace::project_scoped(
            &merge::project(&base).map_err(append_error)?,
            &mapping.logical_path,
        )
        .map_err(append_error)?;
        let local_root = mapping.local_root.clone();
        let cancel = Arc::clone(&self.cancel);
        let mut report = tokio::task::spawn_blocking(move || {
            scan(
                &local_root,
                &ScanOptions {
                    cancel,
                    ..Default::default()
                },
            )
        })
        .await
        .map_err(append_error)?
        .map_err(append_error)?;
        report.directories.retain(|path| !path.is_empty());
        let signature = hex::encode(Sha256::digest(
            serde_json::to_vec(&closure).map_err(append_error)?,
        ));
        index
            .complete_run("preview-metadata-v1", &operation)
            .map_err(append_error)?;
        let includes = |path: &str| selection.is_empty() || selection.iter().any(|selected| path == selected || path.strip_prefix(selected).is_some_and(|suffix| suffix.starts_with('/')));
        report.files.retain(|file| includes(&file.path));
        report.directories.retain(|path| includes(path));
        let mut base = base;
        let mut projection = projection;
        base.entries.retain(|entry| includes(&entry.projection_path.join("/")) || includes(&entry.logical_path.join("/")));
        projection.entries.retain(|entry| includes(&entry.projection_path.join("/")) || includes(&entry.logical_path.join("/")));
        Ok((preview_counts(&report, &base, &projection), signature))
    }
    pub(crate) async fn start_append_worker(
        &self,
        mapping: CloudMapping,
        epoch: &str,
        automatic: bool,
        selection: Vec<String>,
    ) -> Result<CloudSyncResult, AppError> {
        let cached = self.append_cached(&mapping)?;
        let private = mapping
            .local_root
            .parent()
            .ok_or_else(|| {
                runtime_error(
                    "CLOUD_PRIVATE_DIR",
                    "无法创建恢复目录 / Cannot create recovery directory",
                )
            })?
            .join(format!(".peerdav-recovery-{}", mapping.id));
        if !private.exists() {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&private).map_err(append_error)?;
        }
        crate::sync::root_state::bind_initial(&private).map_err(append_error)?;
        let worker = Arc::new(AppendWorker {
            selection,
            store: self.bound_append_store(&cached)?,
            index: Mutex::new(Some(self.open_append_index(&mapping, epoch)?)),
            cached,
            root: mapping.local_root,
            private,
            mapping: mapping.logical_path,
            manager: Arc::downgrade(&self.manager),
            automatic,
        });
        match self.manager.start(mapping.id, worker)? {
            StartResult::Started(run_id) => Ok(CloudSyncResult { run_id }),
            StartResult::AlreadyRunning(_) => Err(runtime_error(
                "SYNC_ALREADY_RUNNING",
                "同步已在运行 / Sync already running",
            )),
        }
    }
}
struct AppendWorker {
    selection: Vec<String>,
    store: Arc<dyn ObjectStore>,
    index: Mutex<Option<AppendIndex>>,
    cached: CachedAppendRepository,
    root: PathBuf,
    private: PathBuf,
    mapping: String,
    manager: std::sync::Weak<SyncManager>,
    automatic: bool,
}
impl SyncWorker for AppendWorker {
    fn run(
        &self,
        context: crate::sync::manager_types::RunContext,
    ) -> futures_util::future::BoxFuture<
        'static,
        Result<crate::sync::manager_types::WorkerOutcome, AppError>,
    > {
        let index = self
            .index
            .lock()
            .map_err(|_| runtime_error("CLOUD_INDEX_LOCK", "索引锁损坏 / Index lock poisoned"))
            .and_then(|mut index| {
                index.take().ok_or_else(|| {
                    runtime_error("CLOUD_INDEX_MISSING", "索引不可用 / Index unavailable")
                })
            });
        let store = self.store.clone();
        let cached = self.cached.clone();
        let root = self.root.clone();
        let private = self.private.clone();
        let mapping = self.mapping.clone();
        let manager = self.manager.clone();
        let automatic = self.automatic;
        let selection = self.selection.clone();
        Box::pin(async move {
            let mut index = index?;
            let run_id = hex::encode(rand::random::<[u8; 16]>());
            let request = AppendPullRequest { root: &root, private: &private, mapping: &mapping, run_id: &run_id };
            let result = if selection.is_empty() {
                append_engine::run_with_deletion_policy(store, &trust(&cached.binding), cached.password.as_bytes(), &mut index, &request, context.cancel, automatic).await
            } else {
                append_engine::run_selected(store, &trust(&cached.binding), cached.password.as_bytes(), &mut index, &request, context.cancel, &selection).await
            };
            match result {
                Ok(_) => Ok(crate::sync::manager_types::WorkerOutcome::Completed),
                Err(error) => {
                    use crate::sync::{
                        append_engine_types::AppendEngineError,
                        manager_types::{SyncPhase, WorkerOutcome},
                    };
                    let phase = match &error {
                        AppendEngineError::Cancelled => Some(SyncPhase::Cancelled),
                        AppendEngineError::RecoveryPaused | AppendEngineError::Recovery(_) => {
                            Some(SyncPhase::PendingRecovery)
                        }
                        AppendEngineError::BindingChanged => Some(SyncPhase::PausedRootReplaced),
                        _ => None,
                    };
                    let error = append_error(error);
                    if let Some(phase) = phase {
                        if let Some(manager) = manager.upgrade() {
                            manager.report_background(error.clone());
                        }
                        Ok(WorkerOutcome::Phase {
                            phase,
                            detail: Some(error.message),
                        })
                    } else {
                        Err(error)
                    }
                }
            }
        })
    }
}
