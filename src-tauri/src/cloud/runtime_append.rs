use super::*;
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
    closure: &ValidatedClosure,
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
fn roots(closure: &ValidatedClosure) -> Result<Vec<String>, AppError> {
    let projection = merge::project(&revisions(closure)?).map_err(append_error)?;
    let mut roots = std::collections::BTreeSet::new();
    for entry in projection.entries {
        if let Some(root) = entry.logical_path.first() {
            roots.insert(root.clone());
        }
    }
    Ok(roots.into_iter().collect())
}
impl CloudRuntime {
    fn append_store(&self, connection: &str) -> Result<Arc<dyn ObjectStore>, AppError> {
        Ok(Arc::new(
            S3Store::new(self.connection(connection)?.config).map_err(cloud_error)?,
        ))
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
        if password.is_empty() || password.as_str() != confirmation.as_str() {
            return Err(runtime_error(
                "CLOUD_APPEND_PASSWORD",
                "密码不能为空且确认必须一致 / Password is required and confirmation must match",
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
        let store = self.append_store(&input.connection_id)?;
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
        let store = self.append_store(&input.connection_id)?;
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
        let closure = append_discovery::discover(
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
                repo_id: selected.repo_id,
                anchor_key: selected.key,
                anchor_fingerprint: selected.fingerprint,
                append_mode: selected.mode,
                trust_source: selected.source,
                root_candidates: roots(&closure)?,
                selected_root: None,
            },
        };
        let result = cached.redacted();
        self.config
            .cache_append_repository(cached)
            .map_err(config_error)?;
        Ok(result)
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
        let mut mapping = input.mapping;
        if let Some(id) = input.mapping_id.as_deref() {
            self.manager.stop_mapping(id).await?;
            if self.mapping(id)?.local_root != mapping.local_root {
                mapping.schedule.enabled = false;
            }
        } else {
            mapping.schedule.enabled = false;
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
            || binding.selected_root.as_deref() != Some(mapping.logical_path.as_str())
        {
            return Err(append_error(AppendError::TrustRejected));
        }
        Ok(cached)
    }
    pub(super) async fn append_preview_state(
        &self,
        mapping: &CloudMapping,
        epoch: &str,
    ) -> Result<(PreviewCounts, String), AppError> {
        let cached = self.append_cached(mapping)?;
        let mut index = self.open_append_index(mapping, epoch)?;
        let operation = index
            .begin_run("preview", &hex::encode(rand::random::<[u8; 16]>()))
            .map_err(append_error)?;
        let closure = append_discovery::discover(
            self.append_store(&mapping.connection_id)?,
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
            .complete_run("preview", &operation)
            .map_err(append_error)?;
        Ok((preview_counts(&report, &base, &projection), signature))
    }
    pub(crate) async fn start_append_worker(
        &self,
        mapping: CloudMapping,
        epoch: &str,
        automatic: bool,
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
            store: self.append_store(&mapping.connection_id)?,
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
        Box::pin(async move {
            let mut index = index?;
            let result = append_engine::run_with_deletion_policy(
                store,
                &trust(&cached.binding),
                cached.password.as_bytes(),
                &mut index,
                &AppendPullRequest {
                    root: &root,
                    private: &private,
                    mapping: &mapping,
                    run_id: &hex::encode(rand::random::<[u8; 16]>()),
                },
                context.cancel,
                automatic,
            )
            .await;
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
