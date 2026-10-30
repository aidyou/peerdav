use std::path::Path;

use crate::sync::index::SyncIndex;
use crate::sync::root_state::{self};
use crate::sync::root_types::{RootBindingStatus, RootStatus};
use crate::{AppError, DomainError};

use super::config_types::CloudMapping;
use super::runtime::CloudRuntime;

fn guard_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> AppError {
    AppError::from(DomainError::Cloud {
        code: "CLOUD_INDEX",
        user_reason: "同步根目录状态不可用 / Sync root state unavailable",
        source: Box::new(error),
    })
}

impl CloudRuntime {
    /// Open the durable root guard.  This deliberately has a stable scope:
    /// root epochs belong in the encrypted binding, not in the index scope.
    pub(crate) fn root_guard_index(&self, mapping: &CloudMapping) -> Result<SyncIndex, AppError> {
        let key = self.config.get_or_create_index_key().map_err(|error| {
            AppError::from(DomainError::Cloud {
                code: "CLOUD_INDEX",
                user_reason: "读取同步索引密钥失败 / Reading sync index key failed",
                source: Box::new(error),
            })
        })?;
        SyncIndex::open(
            &self.data_dir.join("cloud-sync.sqlite"),
            key,
            mapping.repository_id.clone(),
            format!("guard:{}", mapping.id),
        )
        .map_err(guard_error)
    }

    /// Inspect and persist the guard before a scheduled run.  In particular,
    /// a missing root that later reappears with the same inode remains paused.
    pub fn validate_scheduled_root(&self, mapping: &CloudMapping) -> Result<(), AppError> {
        let mut index = self.root_guard_index(mapping)?;
        let Some(mut binding) = index.load_binding().map_err(guard_error)? else {
            return Err(schedule_paused(
                "首次人工确认后才能定时同步 / Manual confirmation is required before scheduling",
            ));
        };
        let status = root_state::inspect_binding(&mut binding).map_err(guard_error)?;
        index.record_binding(&binding).map_err(guard_error)?;
        if !matches!(status, RootStatus::Ready)
            || !matches!(binding.status, RootBindingStatus::Ready)
            || binding.requires_preview
        {
            let phase = match &binding.status {
                RootBindingStatus::Paused {
                    reason:
                        crate::sync::root_types::RootPauseReason::Missing
                        | crate::sync::root_types::RootPauseReason::Unavailable,
                } => crate::sync::manager_types::SyncPhase::PausedRootMissing,
                RootBindingStatus::Paused {
                    reason: crate::sync::root_types::RootPauseReason::Replaced,
                } => crate::sync::manager_types::SyncPhase::PausedRootReplaced,
                _ => crate::sync::manager_types::SyncPhase::PausedNeedsManual,
            };
            self.manager.report_status(crate::sync::manager_types::SyncStatus { mapping_id: mapping.id.clone(), run_id: None, phase, detail: Some("同步目录已暂停，请重新选择目录，预览后手动同步 / Folder paused; reselect, preview and sync manually".into()) });
            return Err(schedule_paused(
                "同步根目录未就绪，定时同步已暂停 / Schedule paused because the sync root is not ready",
            ));
        }
        Ok(())
    }

    /// Persist a newly selected root as awaiting manual confirmation.  This
    /// is called after mapping configuration has been durably edited.
    pub(crate) fn reselect_root_guard(&self, mapping: &CloudMapping) -> Result<(), AppError> {
        let mut index = self.root_guard_index(mapping)?;
        let binding = index.load_binding().map_err(guard_error)?;
        let Some(old) = binding else {
            return Ok(());
        };
        if old.root_path == mapping.local_root
            && matches!(old.status, RootBindingStatus::Ready)
            && !old.requires_preview
        {
            return Ok(());
        }
        let next = root_state::reselect(&old, mapping.local_root.clone()).map_err(guard_error)?;
        index.record_binding(&next).map_err(guard_error)
    }

    /// Authorize a manual run only after its fresh preview has been checked.
    /// A first manual run creates the guard; schedules never create one.
    pub(crate) fn authorize_root_for_manual(
        &self,
        mapping: &CloudMapping,
        preview_root: &Path,
        explicit_confirm: bool,
    ) -> Result<(), AppError> {
        let mut index = self.root_guard_index(mapping)?;
        let mut binding = match index.load_binding().map_err(guard_error)? {
            Some(binding) => binding,
            None => root_state::bind_initial(mapping.local_root.clone()).map_err(guard_error)?,
        };
        if matches!(
            binding.status,
            RootBindingStatus::Paused {
                reason: crate::sync::root_types::RootPauseReason::Missing
                    | crate::sync::root_types::RootPauseReason::Replaced
                    | crate::sync::root_types::RootPauseReason::Unavailable
            }
        ) {
            return Err(schedule_paused(
                "同步目录曾不可用，请先重新选择目录，再预览并手动同步 / Reselect the unavailable root before previewing and syncing manually",
            ));
        }
        let expected_epoch = binding.epoch;
        root_state::authorize_manual_after_preview(
            &mut binding,
            expected_epoch,
            preview_root,
            explicit_confirm,
        )
        .map_err(guard_error)?;
        index.record_binding(&binding).map_err(guard_error)
    }
}

fn schedule_paused(message: &str) -> AppError {
    AppError {
        code: "CLOUD_SCHEDULE_PAUSED".into(),
        message: message.into(),
        detail: None,
        context: None,
    }
}
