use super::{
    engine_types::EngineError,
    materialize_types::{FinalVerifyMode, MaterializeError, MaterializeRequest, Materialized},
    merge_types::{Node, Projection, RevisionValue},
    scan,
    scan_types::ScanOptions,
    traits::BlockReader,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) fn open_root(root: &Path) -> Result<cap_std::fs::Dir, EngineError> {
    use cap_fs_ext::DirExt;
    if !root.is_absolute() {
        return Err(EngineError::UnsafePath(root.into()));
    }
    let mut dir = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())
        .map_err(EngineError::Io)?;
    for part in root.components() {
        match part {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                dir = dir.open_dir_nofollow(name).map_err(EngineError::Io)?
            }
            _ => return Err(EngineError::UnsafePath(root.into())),
        }
    }
    Ok(dir)
}
pub(crate) fn open_nofollow(root: &Path, relative: &Path) -> Result<std::fs::File, EngineError> {
    use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
    super::paths::validate_relative(relative)
        .map_err(|_| EngineError::UnsafePath(relative.into()))?;
    let mut dir = open_root(root)?;
    let mut parts = relative.components();
    let name = parts
        .next_back()
        .ok_or_else(|| EngineError::UnsafePath(relative.into()))?;
    for part in parts {
        dir = dir
            .open_dir_nofollow(part.as_os_str())
            .map_err(EngineError::Io)?;
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = dir
        .open_with(name.as_os_str(), &options)
        .map_err(EngineError::Io)?
        .into_std();
    if !file.metadata().map_err(EngineError::Io)?.is_file() {
        return Err(EngineError::UnsafePath(relative.into()));
    }
    Ok(file)
}
#[cfg(target_os = "linux")]
fn ensure_directory(root: &Path, private: &Path, relative: &Path) -> Result<(), EngineError> {
    let root_cap = open_root(root)?.into_std_file();
    let private_cap = open_root(private)?.into_std_file();
    // Capability directories may carry O_PATH descriptors, which cannot fsync.
    let flags =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC;
    let root = std::fs::File::from(
        rustix::fs::openat(&root_cap, ".", flags, rustix::fs::Mode::empty())
            .map_err(|e| EngineError::Io(e.into()))?,
    );
    let private = std::fs::File::from(
        rustix::fs::openat(&private_cap, ".", flags, rustix::fs::Mode::empty())
            .map_err(|e| EngineError::Io(e.into()))?,
    );
    let relative = relative.to_owned();
    super::sandbox::run_confined(root, private, move |mut dir, _| {
        let outcome = (|| -> Result<(), EngineError> {
            for part in relative.components() {
                let std::path::Component::Normal(name) = part else {
                    return Err(EngineError::UnsafePath(relative.clone()));
                };
                let flags = rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW;
                match rustix::fs::openat(&dir, name, flags, rustix::fs::Mode::empty()) {
                    Ok(fd) => dir = std::fs::File::from(fd),
                    Err(rustix::io::Errno::NOENT) => {
                        rustix::fs::mkdirat(&dir, name, rustix::fs::Mode::from_raw_mode(0o700))
                            .map_err(|e| EngineError::Io(e.into()))?;
                        dir.sync_all().map_err(EngineError::Io)?;
                        dir = std::fs::File::from(
                            rustix::fs::openat(&dir, name, flags, rustix::fs::Mode::empty())
                                .map_err(|e| EngineError::Io(e.into()))?,
                        );
                    }
                    Err(error) => return Err(EngineError::Io(error.into())),
                }
            }
            Ok(())
        })();
        Ok(outcome)
    })
    .map_err(EngineError::Sandbox)?
}
#[cfg(not(target_os = "linux"))]
fn ensure_directory(_: &Path, _: &Path, _: &Path) -> Result<(), EngineError> {
    Err(EngineError::Unsupported)
}

/// Timer runs must not turn an empty side or an abnormal batch into deletion authority.
/// Count only baseline files: namespace/directory anchors must not dilute the ratio.
pub(crate) fn check_automatic_deletions(
    base: &Projection,
    local: &super::scan_types::ScanReport,
    target: &Projection,
) -> Result<(), EngineError> {
    let known: BTreeSet<PathBuf> = base
        .entries
        .iter()
        .filter_map(|entry| {
            matches!(
                entry.value,
                RevisionValue::Live {
                    node: Node::File(_),
                    ..
                }
            )
            .then(|| entry.projection_path.iter().collect())
        })
        .collect();
    let local_paths: BTreeSet<PathBuf> = local
        .files
        .iter()
        .map(|file| PathBuf::from(&file.path))
        .collect();
    let target_paths: BTreeSet<PathBuf> = target
        .entries
        .iter()
        .filter_map(|entry| {
            matches!(
                entry.value,
                RevisionValue::Live {
                    node: Node::File(_),
                    ..
                }
            )
            .then(|| entry.projection_path.iter().collect())
        })
        .collect();
    let deleted: BTreeSet<_> = known
        .difference(&local_paths)
        .chain(known.difference(&target_paths))
        .collect();
    let total = known.len().max(local_paths.len()).max(target_paths.len());
    let count = deleted.len();
    if count > 0
        && (local_paths.is_empty()
            || target_paths.is_empty()
            || count > crate::mirror::MASS_DELETE_ABSOLUTE
            || (count >= crate::mirror::MASS_DELETE_MINIMUM
                && count.saturating_mul(100)
                    > total.saturating_mul(crate::mirror::MASS_DELETE_PERCENT)))
    {
        return Err(EngineError::DeletionConfirmation);
    }
    Ok(())
}

/// Validate all paths before any writes and establish target directories.
/// Kept separate so recovery can run in its original position before file application.
pub fn prepare(
    request: &MaterializeRequest<'_>,
    check: &dyn Fn() -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let MaterializeRequest {
        root,
        private,
        target,
        mapping,
        ..
    } = request;
    super::namespace::components(mapping).map_err(EngineError::Merge)?;
    if !root.is_absolute()
        || !private.is_absolute()
        || root.starts_with(private)
        || private.starts_with(root)
    {
        return Err(EngineError::UnsafePath((*root).into()));
    }
    if !std::fs::symlink_metadata(private)
        .map_err(|_| EngineError::UnsafePath((*private).into()))?
        .is_dir()
    {
        return Err(EngineError::UnsafePath((*private).into()));
    }
    for entry in &target.entries {
        let relative = entry.projection_path.iter().collect::<PathBuf>();
        super::paths::validate_relative(&relative)
            .map_err(|_| EngineError::UnsafePath(relative))?;
    }
    for entry in &target.entries {
        check()?;
        if matches!(
            entry.value,
            RevisionValue::Live {
                node: Node::Dir,
                ..
            }
        ) {
            ensure_directory(
                root,
                private,
                &entry.projection_path.iter().collect::<PathBuf>(),
            )?;
        }
    }
    Ok(())
}

/// Apply a prepared projection using the existing safe-apply journal contract.
/// Caller runs `prepare`, then any pending recovery, before this function.
/// Returns verified local state only; never acknowledges a baseline or publishes.
pub async fn materialize<B: BlockReader>(
    request: &MaterializeRequest<'_>,
    blocks: &B,
    journal: &mut (
             dyn FnMut(
        &super::apply_types::ApplyIntent,
    ) -> Result<(), super::apply_types::ApplyError>
                 + Send
         ),
    cancel: Arc<AtomicBool>,
    check: &(dyn Fn() -> Result<(), EngineError> + Send + Sync),
) -> Result<Materialized, MaterializeError<B::Error>> {
    let MaterializeRequest {
        root,
        private,
        run_id: operation_id,
        source_scan,
        current_scan: scan_report,
        base_projection,
        target: projection,
        local_chunks,
        ..
    } = request;
    let known_paths: BTreeMap<PathBuf, _> = base_projection
        .entries
        .iter()
        .filter(|entry| matches!(entry.value, RevisionValue::Live { .. }))
        .map(|entry| (entry.projection_path.iter().collect(), entry))
        .collect();
    let mut applied = 0;
    for entry in &projection.entries {
        check()?;
        let RevisionValue::Live {
            node: Node::File(file),
            ..
        } = &entry.value
        else {
            continue;
        };
        let rel = entry.projection_path.iter().collect::<PathBuf>();
        let target = root.join(&rel);
        let scanned = scan_report
            .files
            .iter()
            .find(|scan| Path::new(&scan.path) == rel);
        if scanned
            .is_some_and(|scan| scan.whole_file_hash == file.digest && scan.size == file.length)
        {
            continue;
        }
        let scanned = source_scan
            .files
            .iter()
            .find(|scan| Path::new(&scan.path) == rel);
        let action = if scanned.is_some() {
            super::apply_types::ApplyAction::Replace
        } else {
            super::apply_types::ApplyAction::Create
        };
        let mut tmp = tempfile::NamedTempFile::new_in(private)
            .map_err(|_| EngineError::Changed(target.clone()))?;
        for block in &file.blocks {
            check()?;
            let len =
                usize::try_from(block.len).map_err(|_| EngineError::Changed(target.clone()))?;
            if len > super::CDC_MAX_SIZE {
                return Err(EngineError::Unsupported.into());
            }
            let reused =
                if let Some((path, offset)) = local_chunks.get(&(block.id.clone(), block.len)) {
                    use std::io::{Seek, SeekFrom};
                    let local = (|| -> Result<_, EngineError> {
                        let mut source = open_nofollow(root, path)?;
                        source
                            .seek(SeekFrom::Start(*offset))
                            .map_err(EngineError::Io)?;
                        let mut bytes = zeroize::Zeroizing::new(vec![0_u8; len]);
                        source.read_exact(&mut bytes).map_err(EngineError::Io)?;
                        Ok(bytes)
                    })();
                    match local {
                        Ok(bytes) => match blocks.verify_local(&block.id, &bytes) {
                            Ok(true) => Some(bytes),
                            // Local cache failures are misses, as in the CAS engine.
                            Ok(false) | Err(_) => None,
                        },
                        Err(_) => None,
                    }
                } else {
                    None
                };
            let bytes = match reused {
                Some(bytes) => bytes,
                None => blocks
                    .read_block(&block.id, len)
                    .await
                    .map_err(MaterializeError::Block)?,
            };
            std::io::Write::write_all(&mut tmp, &bytes)
                .map_err(|_| EngineError::Changed(target.clone()))?;
        }
        tmp.as_file_mut()
            .sync_all()
            .map_err(|_| EngineError::Changed(target.clone()))?;
        let operation = hex::encode(Sha256::digest(format!("{operation_id}:{}", rel.display())));
        super::apply::apply_bytes(
            root,
            private,
            &operation,
            &rel,
            action,
            tmp.reopen()
                .map_err(|_| EngineError::Changed(target.clone()))?,
            scanned.map(|scan| super::apply_types::FileIdentity {
                dev: scan.device,
                ino: scan.inode,
            }),
            scanned.map(|scan| scan.whole_file_hash.as_str()),
            scanned.map(|scan| scan.size),
            &file.digest,
            file.length,
            journal,
        )
        .map_err(EngineError::Apply)?;
        applied += 1;
    }
    let target_paths = live_paths(projection);
    for (relative, old) in &known_paths {
        if target_paths.contains(relative)
            || !matches!(
                old.value,
                RevisionValue::Live {
                    node: Node::File(_),
                    ..
                }
            )
        {
            continue;
        }
        if !scan_report
            .files
            .iter()
            .any(|scan| Path::new(&scan.path) == relative)
        {
            continue;
        }
        if let Some(scan) = source_scan
            .files
            .iter()
            .find(|scan| Path::new(&scan.path) == relative)
        {
            check()?;
            let operation = hex::encode(Sha256::digest(format!(
                "{operation_id}:delete:{}",
                relative.display()
            )));
            super::apply::apply_bytes(
                root,
                private,
                &operation,
                relative,
                super::apply_types::ApplyAction::Delete,
                std::io::empty(),
                Some(super::apply_types::FileIdentity {
                    dev: scan.device,
                    ino: scan.inode,
                }),
                Some(&scan.whole_file_hash),
                Some(scan.size),
                "",
                0,
                journal,
            )
            .map_err(EngineError::Apply)?;
        }
    }
    let mut deleted_directories = known_paths
        .iter()
        .filter(|(relative, entry)| {
            !target_paths.contains(*relative)
                && matches!(
                    entry.value,
                    RevisionValue::Live {
                        node: Node::Dir,
                        ..
                    }
                )
        })
        .map(|(relative, _)| relative.clone())
        .collect::<Vec<_>>();
    deleted_directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for relative in deleted_directories {
        if !scan_report
            .directories
            .iter()
            .any(|path| Path::new(path) == relative)
        {
            continue;
        }
        check()?;
        let directory = open_root(&root.join(&relative))?
            .dir_metadata()
            .map_err(EngineError::Io)?;
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            let identity = super::apply_types::FileIdentity {
                dev: directory.dev(),
                ino: directory.ino(),
            };
            let operation = hex::encode(Sha256::digest(format!(
                "{operation_id}:directory:{}",
                relative.display()
            )));
            super::apply::apply_bytes(
                root,
                private,
                &operation,
                &relative,
                super::apply_types::ApplyAction::DeleteDirectory,
                std::io::empty(),
                Some(identity),
                None,
                None,
                "",
                0,
                journal,
            )
            .map_err(EngineError::Apply)?;
        }
        #[cfg(not(unix))]
        {
            drop(directory);
            return Err(EngineError::Unsupported.into());
        }
    }
    let verify_root = root.to_path_buf();
    let verified = tokio::task::spawn_blocking(move || {
        scan::scan(
            &verify_root,
            &ScanOptions {
                cancel,
                ..Default::default()
            },
        )
    })
    .await
    .map_err(EngineError::Join)?
    .map_err(EngineError::Scan)?;
    verify_projection(projection, &verified, request.final_verify)?;
    check()?;
    Ok(Materialized {
        applied_files: applied,
        verified_scan: verified,
    })
}

/// Pure path plan shared by materialization and strict verification.
pub fn live_paths(projection: &Projection) -> BTreeSet<PathBuf> {
    projection
        .entries
        .iter()
        .filter(|entry| matches!(entry.value, RevisionValue::Live { .. }))
        .map(|entry| entry.projection_path.iter().collect())
        .collect()
}

pub fn verify_projection(
    projection: &Projection,
    verified: &super::scan_types::ScanReport,
    mode: FinalVerifyMode,
) -> Result<(), EngineError> {
    for entry in &projection.entries {
        if let RevisionValue::Live {
            node: Node::File(file),
            ..
        } = &entry.value
        {
            let relative = entry.projection_path.iter().collect::<PathBuf>();
            if !verified.files.iter().any(|observed| {
                Path::new(&observed.path) == relative
                    && observed.whole_file_hash == file.digest
                    && observed.size == file.length
            }) {
                return Err(EngineError::Changed(relative));
            }
        }
    }
    if mode == FinalVerifyMode::Strict {
        if !verified.complete || !verified.ignored_paths.is_empty() {
            return Err(EngineError::Scan(super::scan_types::ScanError::RootChanged));
        }
        let mut directories = BTreeSet::new();
        let mut files = BTreeSet::new();
        for entry in &projection.entries {
            let relative = entry.projection_path.iter().collect::<PathBuf>();
            match &entry.value {
                RevisionValue::Live {
                    node: Node::Dir, ..
                } => {
                    directories.insert(relative.clone());
                }
                RevisionValue::Live {
                    node: Node::File(_),
                    ..
                } => {
                    files.insert(relative.clone());
                }
                _ => continue,
            }
            for ancestor in relative
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                directories.insert(ancestor.to_owned());
            }
        }
        let actual_files = verified
            .files
            .iter()
            .map(|file| PathBuf::from(&file.path))
            .collect::<BTreeSet<_>>();
        let actual_dirs = verified
            .directories
            .iter()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>();
        if files != actual_files || directories != actual_dirs {
            return Err(EngineError::Changed(PathBuf::new()));
        }
    }
    Ok(())
}
