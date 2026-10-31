use std::path::{Component, Path, PathBuf};

use crate::cloud::runtime_types::RecoveryRecord;
use crate::sync::apply;
use crate::sync::apply_types::{ApplyIntent, ApplyPhase};
use crate::sync::index::SyncIndex;
use crate::{AppError, DomainError};

use super::runtime::CloudRuntime;

impl CloudRuntime {
    /// Lists durable apply intents for a mapping.  Only the relative target and
    /// a stable, lower-case phase are exposed; staging and recovery filenames
    /// are deliberately never returned to the caller.
    pub async fn recovery_records(
        &self,
        mapping_id: &str,
    ) -> Result<Vec<RecoveryRecord>, AppError> {
        self.admit()?;
        let _network = self.network.read().await;
        self.admit()?;
        let mapping = self.mapping(mapping_id)?;
        let epoch = Self::root_epoch(&mapping.local_root)?;
        let records = if mapping.protocol == super::config_types::StorageProtocol::AppendV2 {
            let index = self.open_append_index(&mapping, &epoch)?;
            let mut records = Vec::new();
            let mut after = String::new();
            loop {
                let ids = index
                    .recovery_record_ids(&after, 128)
                    .map_err(|error| recovery_error("CLOUD_RECOVERY_INDEX", error.to_string()))?;
                if ids.is_empty() {
                    break;
                }
                for id in ids {
                    after = id.clone();
                    if let Some(intent) = index.recovery_record(&id).map_err(|error| {
                        recovery_error("CLOUD_RECOVERY_INDEX", error.to_string())
                    })? {
                        records.push((id, intent));
                    }
                }
            }
            records
        } else {
            self.open_recovery_index(&mapping, &epoch)?
                .apply_records::<ApplyIntent>()
                .map_err(|error| recovery_error("CLOUD_RECOVERY_INDEX", error.to_string()))?
        };
        records
            .into_iter()
            .map(|(id, intent)| {
                validate_intent(
                    &mapping.local_root,
                    &self.recovery_dir(mapping_id, &mapping.local_root),
                    &intent,
                    &epoch,
                )?;
                let relative_path = safe_relative(&intent.relative_target)?;
                Ok(RecoveryRecord {
                    id,
                    relative_path,
                    phase: phase_name(&intent.phase).to_owned(),
                })
            })
            .collect()
    }

    /// Restores one preserved original into a caller-selected, non-existing
    /// path below the mapping root.  The network admission write guard is held
    /// while the mapping is stopped and while the confined restore runs.
    pub async fn restore(
        &self,
        mapping_id: &str,
        record_id: &str,
        destination: PathBuf,
    ) -> Result<(), AppError> {
        self.admit()?;
        let _network = self.network.write().await;
        self.admit()?;
        self.manager.stop_mapping(mapping_id).await?;
        let mapping = self.mapping(mapping_id)?;
        let epoch = Self::root_epoch(&mapping.local_root)?;
        let intent = if mapping.protocol == super::config_types::StorageProtocol::AppendV2 {
            self.open_append_index(&mapping, &epoch)?
                .recovery_record(record_id)
                .map_err(|error| recovery_error("CLOUD_RECOVERY_INDEX", error.to_string()))?
        } else {
            self.open_recovery_index(&mapping, &epoch)?
                .apply_records::<ApplyIntent>()
                .map_err(|error| recovery_error("CLOUD_RECOVERY_INDEX", error.to_string()))?
                .into_iter()
                .find(|(id, _)| id == record_id)
                .map(|(_, intent)| intent)
        }
        .ok_or_else(|| {
            recovery_error(
                "CLOUD_RECOVERY_NOT_FOUND",
                "恢复记录不存在 / Recovery record not found",
            )
        })?;
        validate_intent(
            &mapping.local_root,
            &self.recovery_dir(mapping_id, &mapping.local_root),
            &intent,
            &epoch,
        )?;
        let relative = safe_relative(&destination)?;
        apply::restore(&intent, &mapping.local_root, Path::new(&relative))
            .map_err(|error| recovery_error("CLOUD_RECOVERY_RESTORE", error.to_string()))
    }

    fn recovery_dir(&self, mapping_id: &str, root: &Path) -> PathBuf {
        root.parent()
            .map(|parent| parent.join(format!(".peerdav-recovery-{mapping_id}")))
            .unwrap_or_else(|| {
                self.data_dir
                    .join(format!(".peerdav-recovery-{mapping_id}"))
            })
    }

    fn open_recovery_index(
        &self,
        mapping: &crate::cloud::config_types::CloudMapping,
        epoch: &str,
    ) -> Result<SyncIndex, AppError> {
        let key = self
            .config
            .get_or_create_index_key()
            .map_err(|error| recovery_error("CLOUD_RECOVERY_INDEX", error.to_string()))?;
        SyncIndex::open(
            &self.data_dir.join("cloud-sync.sqlite"),
            key,
            mapping.repository_id.clone(),
            Self::index_scope(mapping, epoch),
        )
        .map_err(|error| recovery_error("CLOUD_RECOVERY_INDEX", error.to_string()))
    }
}

fn safe_relative(path: &Path) -> Result<String, AppError> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(recovery_error(
            "CLOUD_RECOVERY_PATH",
            "恢复目标必须是安全的相对路径 / Recovery target must be a safe relative path",
        ));
    }
    path.to_str().map(str::to_owned).ok_or_else(|| {
        recovery_error(
            "CLOUD_RECOVERY_PATH",
            "恢复目标路径无效 / Recovery target path is invalid",
        )
    })
}

fn phase_name(phase: &ApplyPhase) -> &'static str {
    match phase {
        ApplyPhase::Prepared => "prepared",
        ApplyPhase::OriginalPreserved => "originalpreserved",
        ApplyPhase::Installed => "installed",
        ApplyPhase::Deleted => "deleted",
        ApplyPhase::Paused => "paused",
    }
}

fn validate_intent(
    root: &Path,
    private: &Path,
    intent: &ApplyIntent,
    epoch: &str,
) -> Result<(), AppError> {
    let _ = safe_relative(&intent.relative_target)?;
    if intent.recovery_path.parent() != Some(private)
        || intent.staging_path.parent() != Some(private)
        || private.starts_with(root)
        || root.starts_with(private)
    {
        return Err(recovery_error(
            "CLOUD_RECOVERY_UNTRUSTED",
            "恢复记录不属于此同步根目录 / Recovery record is not bound to this mapping",
        ));
    }
    let expected = format!("{}:{}", intent.root_identity.dev, intent.root_identity.ino);
    if expected != epoch {
        return Err(recovery_error(
            "CLOUD_RECOVERY_STALE",
            "同步根目录已变化 / Sync root changed",
        ));
    }
    Ok(())
}

fn recovery_error(code: &str, message: impl Into<String>) -> AppError {
    let user_reason = match code {
        "CLOUD_RECOVERY_INDEX" => "恢复索引操作失败 / Recovery index operation failed",
        "CLOUD_RECOVERY_NOT_FOUND" => "恢复记录不存在 / Recovery record not found",
        "CLOUD_RECOVERY_PATH" => "恢复目标路径无效 / Recovery target path is invalid",
        "CLOUD_RECOVERY_UNTRUSTED" => {
            "恢复记录不属于此同步根目录 / Recovery record is not bound to this mapping"
        }
        "CLOUD_RECOVERY_STALE" => "同步根目录已变化 / Sync root changed",
        "CLOUD_RECOVERY_RESTORE" => "恢复文件失败 / File restore failed",
        _ => "云恢复操作失败 / Cloud recovery operation failed",
    };
    let detail = message.into();
    DomainError::Cloud {
        code: match code {
            "CLOUD_RECOVERY_INDEX" => "CLOUD_RECOVERY_INDEX",
            "CLOUD_RECOVERY_NOT_FOUND" => "CLOUD_RECOVERY_NOT_FOUND",
            "CLOUD_RECOVERY_PATH" => "CLOUD_RECOVERY_PATH",
            "CLOUD_RECOVERY_UNTRUSTED" => "CLOUD_RECOVERY_UNTRUSTED",
            "CLOUD_RECOVERY_STALE" => "CLOUD_RECOVERY_STALE",
            "CLOUD_RECOVERY_RESTORE" => "CLOUD_RECOVERY_RESTORE",
            _ => "CLOUD_RECOVERY",
        },
        user_reason,
        source: Box::new(std::io::Error::new(std::io::ErrorKind::Other, detail)),
    }
    .into()
}
