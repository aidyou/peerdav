#[cfg(target_os = "linux")]
mod linux {
    use crate::sync::apply_types::{
        ApplyAction, ApplyError, ApplyIntent, ApplyPhase, DurableJournal, FileIdentity,
    };
    use rustix::fs::{Mode, OFlags, RenameFlags, openat, renameat_with};
    use sha2::{Digest, Sha256};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Component, Path};
    use zeroize::Zeroizing;

    const MAX_FILE: u64 = 64 * 1024 * 1024 * 1024;
    fn identity(m: &fs::Metadata) -> FileIdentity {
        FileIdentity {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
    fn fd_identity(f: &File) -> Result<FileIdentity, ApplyError> {
        Ok(FileIdentity {
            dev: rustix::fs::fstat(f).map_err(ApplyError::from)?.st_dev,
            ino: rustix::fs::fstat(f).map_err(ApplyError::from)?.st_ino,
        })
    }
    fn checked_relative(p: &Path) -> Result<(), ApplyError> {
        crate::sync::paths::validate_relative(p).map_err(|_| ApplyError::InvalidPath)?;
        if p.as_os_str().is_empty() || p.is_absolute() {
            return Err(ApplyError::InvalidPath);
        }
        if p.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(ApplyError::InvalidPath);
        }
        Ok(())
    }
    fn root_fd(root: &Path) -> Result<File, ApplyError> {
        if !root.is_absolute() {
            return Err(ApplyError::InvalidPath);
        }
        let mut current = File::from(
            rustix::fs::open(
                "/",
                OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(ApplyError::from)?,
        );
        for component in root.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    current = File::from(
                        openat(
                            &current,
                            name,
                            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                            Mode::empty(),
                        )
                        .map_err(ApplyError::from)?,
                    )
                }
                _ => return Err(ApplyError::InvalidPath),
            }
        }
        Ok(current)
    }
    fn parent_fd(root: &File, relative: &Path) -> Result<(File, std::ffi::OsString), ApplyError> {
        checked_relative(relative)?;
        let mut parts = relative.components();
        let name = match parts.next_back() {
            Some(Component::Normal(n)) => n.to_os_string(),
            _ => return Err(ApplyError::InvalidPath),
        };
        let mut dir = root.try_clone()?;
        for c in parts {
            let Component::Normal(n) = c else {
                return Err(ApplyError::InvalidPath);
            };
            dir = File::from(
                openat(
                    &dir,
                    n,
                    OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(ApplyError::from)?,
            );
        }
        Ok((dir, name))
    }
    fn open_target(
        root: &File,
        rel: &Path,
    ) -> Result<(File, File, std::ffi::OsString), ApplyError> {
        let (parent, name) = parent_fd(root, rel)?;
        let target = File::from(
            openat(
                &parent,
                &name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(ApplyError::from)?,
        );
        Ok((target, parent, name))
    }
    fn digest_reader(mut f: impl Read) -> Result<(String, u64), ApplyError> {
        let mut h = Sha256::new();
        let mut n = 0;
        let mut buf = Zeroizing::new([0u8; 64 * 1024]);
        loop {
            let got = f.read(&mut *buf)?;
            if got == 0 {
                break;
            };
            n += got as u64;
            if n > MAX_FILE {
                return Err(ApplyError::TooLarge);
            }
            h.update(&buf[..got]);
        }
        Ok((hex::encode(h.finalize()), n))
    }
    fn unique_name(prefix: &str, op: &str) -> String {
        format!(
            "{prefix}-{}",
            op.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(80)
                .collect::<String>()
        )
    }
    fn move_fd(
        authorized_root: &File,
        trusted_private: &File,
        source_parent: &File,
        source: &std::ffi::OsStr,
        target_parent: &File,
        target: &std::ffi::OsStr,
    ) -> Result<(), ApplyError> {
        let root = authorized_root.try_clone()?;
        let private = trusted_private.try_clone()?;
        let source_parent = source_parent.try_clone()?;
        let target_parent = target_parent.try_clone()?;
        let source = source.to_os_string();
        let target = target.to_os_string();
        crate::sync::sandbox::run_confined(root, private, move |_, _| {
            Ok(renameat_with(
                &source_parent,
                &source,
                &target_parent,
                &target,
                RenameFlags::NOREPLACE,
            )
            .map_err(ApplyError::from))
        })?
    }
    fn libc_flags() -> i32 {
        (OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32
    }

    fn validate_file(
        path: &Path,
        expected: &Option<FileIdentity>,
        digest: &Option<String>,
        size: &Option<u64>,
    ) -> Result<(), ApplyError> {
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc_flags())
            .open(path)?;
        let m = f.metadata()?;
        if !m.is_file() {
            return Err(ApplyError::InvalidPath);
        };
        if expected.as_ref().is_none() || digest.is_none() || size.is_none() {
            return Err(ApplyError::IdentityChanged);
        }
        if expected.as_ref() != Some(&identity(&m)) {
            return Err(ApplyError::IdentityChanged);
        };
        let (d, n) = digest_reader(f)?;
        if digest.as_deref() != Some(&d) || size != &Some(n) {
            return Err(ApplyError::IdentityChanged);
        };
        Ok(())
    }
    fn validate_directory(path: &Path, expected: &Option<FileIdentity>) -> Result<(), ApplyError> {
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc_flags())
            .open(path)?;
        let m = f.metadata()?;
        if !m.is_dir() {
            return Err(ApplyError::InvalidPath);
        }
        if expected.as_ref() != Some(&identity(&m)) {
            return Err(ApplyError::IdentityChanged);
        }
        if fs::read_dir(path)?.next().transpose()?.is_some() {
            return Err(ApplyError::IdentityChanged);
        }
        Ok(())
    }
    fn validate_recovery(intent: &ApplyIntent) -> Result<(), ApplyError> {
        if matches!(intent.action, ApplyAction::DeleteDirectory) {
            validate_directory(&intent.recovery_path, &intent.expected_old_identity)
        } else {
            validate_file(
                &intent.recovery_path,
                &intent.expected_old_identity,
                &intent.expected_old_digest,
                &intent.expected_old_size,
            )
        }
    }

    pub fn apply_bytes<R: Read>(
        root: &Path,
        private: &Path,
        op_id: &str,
        relative: &Path,
        action: ApplyAction,
        mut input: R,
        expected_old: Option<FileIdentity>,
        expected_digest: Option<&str>,
        expected_size: Option<u64>,
        expected_new_digest: &str,
        expected_new_size: u64,
        journal: &mut DurableJournal<'_>,
    ) -> Result<ApplyIntent, ApplyError> {
        if op_id.is_empty()
            || op_id.len() > 80
            || !op_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(ApplyError::InvalidPath);
        }
        let rootf = root_fd(root)?;
        if private.starts_with(root) || root.starts_with(private) {
            return Err(ApplyError::InvalidPath);
        }
        let (parent, name) = parent_fd(&rootf, relative)?;
        let root_id = fd_identity(&rootf)?;
        let exists = openat(
            &parent,
            &name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .is_ok();
        if matches!(action, ApplyAction::Create) && exists {
            return Err(ApplyError::AlreadyExists);
        };
        if matches!(
            action,
            ApplyAction::Replace | ApplyAction::Delete | ApplyAction::DeleteDirectory
        ) && (!exists
            || expected_old.is_none()
            || (matches!(action, ApplyAction::DeleteDirectory)
                && (expected_digest.is_some() || expected_size.is_some()))
            || (!matches!(action, ApplyAction::DeleteDirectory)
                && (expected_digest.is_none() || expected_size.is_none())))
        {
            return Err(ApplyError::IdentityChanged);
        };
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(private) {
            Ok(()) => {
                if let Some(parent) = private.parent() {
                    root_fd(parent)?.sync_all()?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let private_fd = root_fd(private)?;
        if fd_identity(&private_fd)?.dev != root_id.dev {
            return Err(ApplyError::Unsupported);
        }
        private_fd.set_permissions(fs::Permissions::from_mode(0o700))?;
        private_fd.sync_all()?;
        let staging = private.join(unique_name("stage", op_id));
        let recovery = private.join(unique_name("recovery", op_id));
        let mut staging_identity = None;
        let mut staging_digest = None;
        let mut staging_size = None;
        if !matches!(action, ApplyAction::Delete | ApplyAction::DeleteDirectory) {
            let mut sf = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc_flags())
                .open(&staging)?;
            let mut h = Sha256::new();
            let mut n = 0;
            let mut buf = Zeroizing::new([0u8; 64 * 1024]);
            loop {
                let got = input.read(&mut *buf)?;
                if got == 0 {
                    break;
                };
                n += got as u64;
                if n > MAX_FILE {
                    return Err(ApplyError::TooLarge);
                };
                sf.write_all(&buf[..got])?;
                h.update(&buf[..got]);
            }
            sf.sync_all()?;
            let d = hex::encode(h.finalize());
            if n != expected_new_size || d != expected_new_digest {
                return Err(ApplyError::VerificationFailed);
            };
            let m = sf.metadata()?;
            staging_identity = Some(identity(&m));
            staging_digest = Some(d);
            staging_size = Some(n);
        }
        private_fd.sync_all()?;
        let mut intent = ApplyIntent {
            op_id: op_id.into(),
            action: action.clone(),
            relative_target: relative.into(),
            expected_old_identity: expected_old,
            expected_old_digest: expected_digest.map(str::to_owned),
            expected_old_size: expected_size,
            staging_identity,
            staging_digest,
            staging_size,
            root_identity: root_id,
            recovery_path: recovery.clone(),
            staging_path: staging.clone(),
            phase: ApplyPhase::Prepared,
        };
        journal(&intent)?;
        if fd_identity(&root_fd(root)?)? != intent.root_identity
            || fd_identity(&parent_fd(&root_fd(root)?, relative)?.0)? != fd_identity(&parent)?
        {
            return Err(ApplyError::IdentityChanged);
        }
        if exists {
            let (old, _, _) = open_target(&rootf, relative)?;
            let m = old.metadata()?;
            if !m.is_file() && !matches!(action, ApplyAction::DeleteDirectory) {
                return Err(ApplyError::InvalidPath);
            }
            if matches!(action, ApplyAction::DeleteDirectory) {
                if !m.is_dir() {
                    return Err(ApplyError::InvalidPath);
                }
                if intent.expected_old_identity.as_ref() != Some(&identity(&m)) {
                    return Err(ApplyError::IdentityChanged);
                }
                if fs::read_dir(&root.join(relative))?
                    .next()
                    .transpose()?
                    .is_some()
                {
                    return Err(ApplyError::IdentityChanged);
                }
            } else {
                if intent.expected_old_identity.as_ref() != Some(&identity(&m)) {
                    return Err(ApplyError::IdentityChanged);
                }
                let (d, n) = digest_reader(old)?;
                if intent.expected_old_digest.as_deref() != Some(&d)
                    || intent.expected_old_size != Some(n)
                {
                    return Err(ApplyError::IdentityChanged);
                }
            };
            let rp = recovery.parent().ok_or(ApplyError::InvalidPath)?;
            let rpf = File::open(rp)?;
            move_fd(
                &rootf,
                &private_fd,
                &parent,
                &name,
                &rpf,
                recovery.file_name().ok_or(ApplyError::InvalidPath)?,
            )?;
            parent.sync_all()?;
            private_fd.sync_all()?;
            validate_recovery(&intent)?;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc_flags())
                .open(&recovery)?
                .sync_all()?;
        }
        intent.phase = ApplyPhase::OriginalPreserved;
        journal(&intent)?;
        if !matches!(action, ApplyAction::Delete | ApplyAction::DeleteDirectory) {
            if fd_identity(&root_fd(root)?)? != intent.root_identity
                || fd_identity(&parent_fd(&root_fd(root)?, relative)?.0)? != fd_identity(&parent)?
            {
                return Err(ApplyError::IdentityChanged);
            }
            let sp = root_fd(private)?;
            move_fd(
                &rootf,
                &private_fd,
                &sp,
                staging.file_name().ok_or(ApplyError::InvalidPath)?,
                &parent,
                &name,
            )?;
            parent.sync_all()?;
            sp.sync_all()?;
            let (installed, _, _) = open_target(&rootf, relative)?;
            installed.sync_all()?;
            validate_file(
                &root.join(relative),
                &intent.staging_identity,
                &intent.staging_digest,
                &intent.staging_size,
            )?;
            intent.phase = ApplyPhase::Installed;
            journal(&intent)?;
        } else if matches!(action, ApplyAction::DeleteDirectory) {
            intent.phase = ApplyPhase::Deleted;
            journal(&intent)?;
            if openat(
                &parent,
                &name,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .is_ok()
            {
                return Err(ApplyError::IdentityChanged);
            }
        } else {
            intent.phase = ApplyPhase::Deleted;
            journal(&intent)?;
            match openat(
                &parent,
                &name,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(_) => return Err(ApplyError::IdentityChanged),
                Err(rustix::io::Errno::NOENT) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(intent)
    }
    pub fn recover(intent: &ApplyIntent, root: &Path) -> Result<ApplyPhase, ApplyError> {
        let rootf = root_fd(root)?;
        if fd_identity(&rootf)? != intent.root_identity {
            return Err(ApplyError::IdentityChanged);
        };
        let (parent, name) = parent_fd(&rootf, &intent.relative_target)?;
        let target = root.join(&intent.relative_target);
        match &intent.phase {
            ApplyPhase::Prepared => {
                if intent.recovery_path.exists() {
                    validate_recovery(intent)?;
                    return Ok(ApplyPhase::OriginalPreserved);
                }
                if !intent.staging_path.exists()
                    && !matches!(
                        intent.action,
                        ApplyAction::Delete | ApplyAction::DeleteDirectory
                    )
                    && target.exists()
                {
                    validate_file(
                        &target,
                        &intent.staging_identity,
                        &intent.staging_digest,
                        &intent.staging_size,
                    )?;
                    return Ok(ApplyPhase::Installed);
                }
                Ok(ApplyPhase::Prepared)
            }
            ApplyPhase::OriginalPreserved => {
                if target.exists() {
                    if matches!(intent.action, ApplyAction::DeleteDirectory) {
                        validate_directory(&target, &intent.expected_old_identity)?;
                    } else {
                        validate_file(
                            &target,
                            &intent.staging_identity,
                            &intent.staging_digest,
                            &intent.staging_size,
                        )?;
                    }
                    return Ok(ApplyPhase::Installed);
                }
                if matches!(
                    intent.action,
                    ApplyAction::Delete | ApplyAction::DeleteDirectory
                ) {
                    return Ok(ApplyPhase::Deleted);
                }
                let pp = File::open(
                    intent
                        .staging_path
                        .parent()
                        .ok_or(ApplyError::InvalidPath)?,
                )?;
                let private = root_fd(
                    intent
                        .staging_path
                        .parent()
                        .ok_or(ApplyError::InvalidPath)?,
                )?;
                move_fd(
                    &rootf,
                    &private,
                    &pp,
                    intent
                        .staging_path
                        .file_name()
                        .ok_or(ApplyError::InvalidPath)?,
                    &parent,
                    &name,
                )?;
                parent.sync_all()?;
                Ok(ApplyPhase::Installed)
            }
            p => Ok(p.clone()),
        }
    }
    pub fn restore(
        intent: &ApplyIntent,
        root: &Path,
        destination: &Path,
    ) -> Result<(), ApplyError> {
        let rootf = root_fd(root)?;
        let (parent, name) = parent_fd(&rootf, destination)?;
        if openat(
            &parent,
            &name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .is_ok()
        {
            return Err(ApplyError::AlreadyExists);
        };
        if matches!(intent.action, ApplyAction::DeleteDirectory) {
            return Err(ApplyError::Unsupported);
        }
        validate_file(
            &intent.recovery_path,
            &intent.expected_old_identity,
            &intent.expected_old_digest,
            &intent.expected_old_size,
        )?;
        let stage = intent.staging_path.with_extension("restore");
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stage)?;
        let mut src = File::open(&intent.recovery_path)?;
        let mut buf = Zeroizing::new([0u8; 64 * 1024]);
        let mut n = 0;
        loop {
            let got = src.read(&mut *buf)?;
            if got == 0 {
                break;
            };
            n += got as u64;
            if n > MAX_FILE {
                return Err(ApplyError::TooLarge);
            };
            out.write_all(&buf[..got])?;
        }
        out.sync_all()?;
        let pp = File::open(stage.parent().ok_or(ApplyError::InvalidPath)?)?;
        let private_fd = root_fd(stage.parent().ok_or(ApplyError::InvalidPath)?)?;
        move_fd(
            &rootf,
            &private_fd,
            &pp,
            stage.file_name().ok_or(ApplyError::InvalidPath)?,
            &parent,
            &name,
        )?;
        parent.sync_all()?;
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub use linux::{apply_bytes, recover, restore};
#[cfg(not(target_os = "linux"))]
pub fn apply_bytes<R: std::io::Read>(
    _: &std::path::Path,
    _: &std::path::Path,
    _: &str,
    _: &std::path::Path,
    _: super::apply_types::ApplyAction,
    _: R,
    _: Option<super::apply_types::FileIdentity>,
    _: Option<&str>,
    _: Option<u64>,
    _: &str,
    _: u64,
    _: &mut super::apply_types::DurableJournal<'_>,
) -> Result<super::apply_types::ApplyIntent, super::apply_types::ApplyError> {
    Err(super::apply_types::ApplyError::Unsupported)
}
#[cfg(not(target_os = "linux"))]
pub fn recover(
    _: &super::apply_types::ApplyIntent,
    _: &std::path::Path,
) -> Result<super::apply_types::ApplyPhase, super::apply_types::ApplyError> {
    Err(super::apply_types::ApplyError::Unsupported)
}
#[cfg(not(target_os = "linux"))]
pub fn restore(
    _: &super::apply_types::ApplyIntent,
    _: &std::path::Path,
    _: &std::path::Path,
) -> Result<(), super::apply_types::ApplyError> {
    Err(super::apply_types::ApplyError::Unsupported)
}
