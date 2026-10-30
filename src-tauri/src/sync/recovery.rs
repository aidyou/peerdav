use crate::sync::apply;
use crate::sync::apply_types::{ApplyAction, ApplyIntent, ApplyPhase, FileIdentity};
use crate::sync::index::SyncIndex;
use crate::sync::recovery_types::{RecoveryError, RecoverySummary};

#[cfg(unix)]
mod unix {
    use super::*;
    use cap_std::fs::{Dir, MetadataExt as CapMetadataExt, PermissionsExt as CapPermissionsExt};
    use rustix::fs::OFlags;
    use sha2::{Digest, Sha256};

    use std::fs::{self, OpenOptions};
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Path;
    use zeroize::Zeroizing;

    fn identity_from_metadata(m: &fs::Metadata) -> FileIdentity {
        FileIdentity {
            dev: m.dev(),
            ino: m.ino(),
        }
    }

    fn digest(
        path: &Path,
        expected: &Option<String>,
        size: &Option<u64>,
        id: &Option<FileIdentity>,
    ) -> Result<(), RecoveryError> {
        let mut f = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(path)?;
        let m = f.metadata()?;
        if !m.is_file() || id.as_ref() != Some(&identity_from_metadata(&m)) {
            return Err(RecoveryError::Concurrent);
        }
        let limit = size
            .ok_or(RecoveryError::InvalidRecord)?
            .checked_add(1)
            .ok_or(RecoveryError::InvalidRecord)?;
        let mut h = Sha256::new();
        let mut n = 0u64;
        let mut buf = Zeroizing::new([0u8; 64 * 1024]);
        loop {
            let got = f.read(&mut *buf)?;
            if got == 0 {
                break;
            }
            n = n.checked_add(got as u64).ok_or(RecoveryError::Concurrent)?;
            if n > limit {
                return Err(RecoveryError::Concurrent);
            }
            h.update(&buf[..got]);
        }
        if expected.as_deref() != Some(&hex::encode(h.finalize())) || *size != Some(n) {
            return Err(RecoveryError::Concurrent);
        }
        Ok(())
    }

    fn trusted_layout(
        root: &Path,
        private: &Path,
        intent: &ApplyIntent,
    ) -> Result<(), RecoveryError> {
        if !root.is_absolute()
            || !private.is_absolute()
            || root == private
            || root.starts_with(private)
            || private.starts_with(root)
        {
            return Err(RecoveryError::UntrustedPath);
        }
        let root_dir =
            Dir::open_ambient_dir(root, cap_std::ambient_authority()).map_err(RecoveryError::Io)?;
        let root_meta = root_dir.dir_metadata().map_err(RecoveryError::Io)?;
        if !root_meta.is_dir()
            || (FileIdentity {
                dev: root_meta.dev(),
                ino: root_meta.ino(),
            }) != intent.root_identity
        {
            return Err(RecoveryError::Concurrent);
        }
        let private_dir = Dir::open_ambient_dir(private, cap_std::ambient_authority())
            .map_err(RecoveryError::Io)?;
        let private_meta = private_dir.dir_metadata().map_err(RecoveryError::Io)?;
        if !private_meta.is_dir() || private_meta.permissions().mode() & 0o777 != 0o700 {
            return Err(RecoveryError::UntrustedPath);
        }
        for p in [&intent.staging_path, &intent.recovery_path] {
            if p.as_os_str().is_empty() || p.parent() != Some(private) || p.file_name().is_none() {
                return Err(RecoveryError::UntrustedPath);
            }
        }
        Ok(())
    }

    fn stage_exists(intent: &ApplyIntent) -> Result<bool, RecoveryError> {
        match fs::symlink_metadata(&intent.staging_path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn validate_target(intent: &ApplyIntent, root: &Path) -> Result<(), RecoveryError> {
        digest(
            &root.join(&intent.relative_target),
            &intent.staging_digest,
            &intent.staging_size,
            &intent.staging_identity,
        )
    }

    fn validate_stage(intent: &ApplyIntent) -> Result<(), RecoveryError> {
        if matches!(
            intent.action,
            ApplyAction::Delete | ApplyAction::DeleteDirectory
        ) {
            return Ok(());
        }
        if intent.staging_size.is_none()
            || intent.staging_digest.is_none()
            || intent.staging_identity.is_none()
        {
            return Err(RecoveryError::InvalidRecord);
        }
        digest(
            &intent.staging_path,
            &intent.staging_digest,
            &intent.staging_size,
            &intent.staging_identity,
        )
    }

    pub fn recover_pending(
        index: &mut SyncIndex,
        root: &Path,
        private: &Path,
    ) -> Result<RecoverySummary, RecoveryError> {
        let completed: Vec<String> = index
            .load("engine", "completed_apply_operations")?
            .unwrap_or_default();
        let records: Vec<(String, ApplyIntent)> = index.apply_records()?;
        let mut out = RecoverySummary::default();
        for (op, intent) in records {
            if completed.iter().any(|x| x == &op) {
                continue;
            }
            if matches!(intent.phase, ApplyPhase::Paused) {
                out.needs_user_confirmation = true;
                out.requires_remote_reconcile = true;
                out.operation = Some(op);
                continue;
            }
            trusted_layout(root, private, &intent)?;
            let before = intent.phase.clone();
            if matches!(before, ApplyPhase::Prepared | ApplyPhase::OriginalPreserved)
                && stage_exists(&intent)?
            {
                validate_stage(&intent)?;
            }
            if matches!(before, ApplyPhase::Prepared | ApplyPhase::OriginalPreserved)
                && !matches!(
                    intent.action,
                    ApplyAction::Delete | ApplyAction::DeleteDirectory
                )
                && !stage_exists(&intent)?
            {
                validate_target(&intent, root)?;
                if matches!(before, ApplyPhase::OriginalPreserved) {
                    digest(
                        &intent.recovery_path,
                        &intent.expected_old_digest,
                        &intent.expected_old_size,
                        &intent.expected_old_identity,
                    )?;
                }
            }
            match before {
                ApplyPhase::Prepared => {
                    let phase = apply::recover(&intent, root)?;
                    if phase == ApplyPhase::Prepared {
                        let mut paused = intent.clone();
                        paused.phase = ApplyPhase::Paused;
                        index.record_apply(&op, &paused)?;
                        out.needs_user_confirmation = true;
                        out.operation = Some(op.clone());
                    } else {
                        let mut next = intent.clone();
                        next.phase = phase;
                        index.record_apply(&op, &next)?;
                    }
                    out.requires_remote_reconcile = true;
                    out.recovered_operations.push(op);
                }
                ApplyPhase::OriginalPreserved => {
                    let phase = apply::recover(&intent, root)?;
                    let mut next = intent.clone();
                    next.phase = phase;
                    index.record_apply(&op, &next)?;
                    out.requires_remote_reconcile = true;
                    out.recovered_operations.push(op);
                }
                ApplyPhase::Installed => {
                    if intent.staging_size.is_none()
                        || intent.staging_digest.is_none()
                        || intent.staging_identity.is_none()
                    {
                        return Err(RecoveryError::InvalidRecord);
                    }
                    digest(
                        &root.join(&intent.relative_target),
                        &intent.staging_digest,
                        &intent.staging_size,
                        &intent.staging_identity,
                    )?;
                    out.requires_remote_reconcile = true;
                    out.recovered_operations.push(op);
                }
                ApplyPhase::Deleted => {
                    if root.join(&intent.relative_target).try_exists()? {
                        return Err(RecoveryError::Concurrent);
                    }
                    out.requires_remote_reconcile = true;
                    out.recovered_operations.push(op);
                }
                ApplyPhase::Paused => {
                    out.needs_user_confirmation = true;
                    out.requires_remote_reconcile = true;
                    out.operation = Some(op);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(unix)]
pub use unix::recover_pending;

#[cfg(not(unix))]
pub fn recover_pending(
    _: &mut SyncIndex,
    _: &std::path::Path,
    _: &std::path::Path,
) -> Result<RecoverySummary, RecoveryError> {
    Err(RecoveryError::Unsupported)
}
