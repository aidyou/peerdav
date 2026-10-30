use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::engine_types::{EngineError, EngineReport};
use super::{
    index::SyncIndex,
    merge,
    merge_types::{
        Baseline, BlockRef, ChangeId, EntityId, FileNode, MergeSet, Node, RevisionValue,
    },
    publisher,
    publisher_types::PublishControl,
    repository::Repository,
    scan,
    scan_types::ScanOptions,
};

fn change(operation: &str, path: &str, digest: &str) -> ChangeId {
    ChangeId(hex::encode(Sha256::digest(
        format!("peerdav-s3/change/v1/{operation}/{path}/{digest}").as_bytes(),
    )))
}
fn namespace_path(mapping: &str, path: &str) -> Result<Vec<String>, EngineError> {
    let mut result = super::namespace::components(mapping).map_err(EngineError::Merge)?;
    if !path.is_empty() {
        result.extend(path.split('/').map(str::to_owned));
    }
    Ok(result)
}
fn namespace_entity(mapping: &str, path: &str) -> Result<EntityId, EngineError> {
    Ok(super::namespace::entity(&namespace_path(mapping, path)?))
}
fn parent(mapping: &str, path: &str) -> Result<EntityId, EngineError> {
    let p = Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or("");
    if p.is_empty() {
        namespace_entity(mapping, "")
    } else {
        namespace_entity(mapping, p)
    }
}
fn check(control: &PublishControl) -> Result<(), EngineError> {
    if control.allowed() {
        Ok(())
    } else {
        Err(EngineError::Cancelled)
    }
}
fn open_root(root: &Path) -> Result<cap_std::fs::Dir, EngineError> {
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
fn open_nofollow(root: &Path, relative: &Path) -> Result<std::fs::File, EngineError> {
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
    let root = std::fs::File::from(
        rustix::fs::openat(
            &root_cap,
            ".",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| EngineError::Io(e.into()))?,
    );
    let private = std::fs::File::from(
        rustix::fs::openat(
            &private_cap,
            ".",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| EngineError::Io(e.into()))?,
    );
    let relative = relative.to_owned();
    super::sandbox::run_confined(root, private, move |mut dir, _| {
        let outcome = (|| -> Result<(), EngineError> {
            for part in relative.components() {
                let std::path::Component::Normal(name) = part else {
                    return Err(EngineError::UnsafePath(relative.clone()));
                };
                match rustix::fs::openat(
                    &dir,
                    name,
                    rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::NOFOLLOW,
                    rustix::fs::Mode::empty(),
                ) {
                    Ok(fd) => dir = std::fs::File::from(fd),
                    Err(rustix::io::Errno::NOENT) => {
                        rustix::fs::mkdirat(&dir, name, rustix::fs::Mode::from_raw_mode(0o700))
                            .map_err(|e| EngineError::Io(e.into()))?;
                        dir.sync_all().map_err(EngineError::Io)?;
                        dir = std::fs::File::from(
                            rustix::fs::openat(
                                &dir,
                                name,
                                rustix::fs::OFlags::DIRECTORY
                                    | rustix::fs::OFlags::RDONLY
                                    | rustix::fs::OFlags::NOFOLLOW,
                                rustix::fs::Mode::empty(),
                            )
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

async fn load_remote(
    repository: &Repository,
    commit_id: &str,
    control: &PublishControl,
) -> Result<MergeSet, EngineError> {
    match super::tree::load_commit_snapshot(repository, commit_id, control).await {
        Ok(loaded) => Ok(loaded.snapshot),
        Err(super::tree_types::TreeError::Cancelled) => Err(EngineError::Cancelled),
        Err(error) => Err(EngineError::Tree(error)),
    }
}

/// Execute one complete synchronization run, including remote materialization.
pub async fn run(
    repository: Arc<Repository>,
    index: &mut SyncIndex,
    root: &Path,
    private: &Path,
    mapping: &str,
    operation_id: &str,
    config_revision: u64,
    control: &PublishControl,
) -> Result<EngineReport, EngineError> {
    run_with_progress(
        repository,
        index,
        root,
        private,
        mapping,
        operation_id,
        config_revision,
        control,
        &|_| {},
    )
    .await
}

pub async fn run_with_progress(
    repository: Arc<Repository>,
    index: &mut SyncIndex,
    root: &Path,
    private: &Path,
    mapping: &str,
    operation_id: &str,
    config_revision: u64,
    control: &PublishControl,
    progress: &(dyn Fn(super::manager_types::SyncPhase) + Send + Sync),
) -> Result<EngineReport, EngineError> {
    progress(super::manager_types::SyncPhase::Scanning);
    if !root.is_absolute()
        || !private.is_absolute()
        || root == private
        || root.starts_with(private)
        || private.starts_with(root)
    {
        return Err(EngineError::UnsafePath(root.to_path_buf()));
    }
    let meta = match std::fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(mut binding) = index.load_binding()? {
                let _ =
                    super::root_state::inspect_binding(&mut binding).map_err(EngineError::Root)?;
                index.record_binding(&binding)?;
            }
            return Err(EngineError::RootMissing(root.to_path_buf()));
        }
        Err(error) => return Err(EngineError::Io(error)),
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(EngineError::UnsafePath(root.to_path_buf()));
    }
    check(control)?;
    // A published-but-not-materialized run is resumed under its original
    // operation id. Publisher deduplication then returns the existing commit
    // instead of creating a second user change after a restart.
    let mut pending: Option<super::engine_types::PendingRun> = index
        .load::<Option<super::engine_types::PendingRun>>("engine", "pending")?
        .flatten();
    if let Some(active) = index
        .load::<Option<super::engine_types::PendingRun>>("engine", "active_intent")?
        .flatten()
    {
        if pending.is_none() {
            pending = Some(active);
        }
    }
    let operation_id = pending
        .as_ref()
        .map(|run| run.operation_id.as_str())
        .unwrap_or(operation_id);
    let mut binding = match index.load_binding()? {
        Some(binding) => binding,
        None => super::root_state::bind_initial(root).map_err(EngineError::Root)?,
    };
    if binding.root_path != root
        || !matches!(
            super::root_state::inspect_binding(&mut binding).map_err(EngineError::Root)?,
            super::root_types::RootStatus::Ready
        )
    {
        index.record_binding(&binding)?;
        return Err(EngineError::Changed(root.into()));
    }
    index.record_binding(&binding)?;
    let scan_root = root.to_path_buf();
    let cancel = Arc::clone(&control.cancel);
    let scan_report = tokio::task::spawn_blocking(move || {
        scan::scan(
            &scan_root,
            &ScanOptions {
                cancel,
                ..Default::default()
            },
        )
    })
    .await
    .map_err(EngineError::Join)??;
    if !scan_report.complete {
        return Err(EngineError::Scan(super::scan_types::ScanError::RootChanged));
    }
    check(control)?;
    let (head, _) = repository.read_head_revision().await?;
    let remote = match head.commit.as_deref() {
        Some(c) => load_remote(&repository, c, control).await?,
        None => MergeSet {
            revisions: BTreeMap::new(),
        },
    };
    let base: MergeSet = index
        .load("engine", "materialized")?
        .unwrap_or_else(|| MergeSet {
            revisions: BTreeMap::new(),
        });
    let base_projection =
        super::namespace::project_scoped(&merge::project(&base.revisions)?, mapping)
            .map_err(EngineError::Merge)?;
    let known_paths: BTreeMap<PathBuf, _> = base_projection
        .entries
        .iter()
        .filter(|entry| matches!(entry.value, RevisionValue::Live { .. }))
        .map(|entry| (entry.projection_path.iter().collect(), entry))
        .collect();
    progress(super::manager_types::SyncPhase::Uploading);
    let history = merge::join(&base, &remote)?.revisions;
    let mut local = base.revisions.clone();
    let mut uploaded = 0;
    let mut local_chunks = BTreeMap::<(String, u64), (PathBuf, u64)>::new();
    let mut observed = std::collections::BTreeSet::new();
    let namespace_components = super::namespace::components(mapping).map_err(EngineError::Merge)?;
    for (index, name) in namespace_components.iter().enumerate() {
        let logical = namespace_components[..=index].join("/");
        let id = super::namespace::entity(&namespace_components[..=index]);
        let parent_id = if index == 0 {
            EntityId("root".into())
        } else {
            super::namespace::entity(&namespace_components[..index])
        };
        let value = RevisionValue::Live {
            parent_dir: parent_id,
            name: name.clone(),
            node: Node::Dir,
        };
        let baseline = Baseline::Unknown;
        let revision = merge::revision(merge::prepare_local_change(
            id,
            change("namespace", &logical, "directory"),
            baseline,
            value,
            &history,
            true,
        )?)?;
        local.insert(revision.revision_id.clone(), revision);
    }
    for directory in scan_report
        .directories
        .iter()
        .filter(|directory| !directory.is_empty())
    {
        let rel = Path::new(directory);
        let old = known_paths.get(rel);
        let id = old.map_or_else(
            || namespace_entity(mapping, directory),
            |entry| Ok(entry.entity.clone()),
        )?;
        observed.insert(id.clone());
        if old.is_some_and(|entry| {
            matches!(
                entry.value,
                RevisionValue::Live {
                    node: Node::Dir,
                    ..
                }
            )
        }) {
            continue;
        }
        let name = rel
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| EngineError::UnsafePath(rel.into()))?
            .to_owned();
        let value = RevisionValue::Live {
            parent_dir: parent(mapping, directory)?,
            name,
            node: Node::Dir,
        };
        let frontier = merge::frontier(&base.revisions)?.remove(&id);
        let baseline = frontier.map_or(Baseline::Unknown, |frontier| Baseline::Known { frontier });
        let revision = merge::revision(merge::prepare_local_change(
            id,
            change("namespace", directory, "directory"),
            baseline,
            value,
            &history,
            true,
        )?)?;
        local.insert(revision.revision_id.clone(), revision);
    }
    for file in &scan_report.files {
        check(control)?;
        let path = root.join(&file.path);
        let mut reader = open_nofollow(root, Path::new(&file.path));
        let mut whole = Sha256::new();
        let mut refs = Vec::with_capacity(file.blocks.len());
        let reader = reader
            .as_mut()
            .map_err(|_| EngineError::Changed(path.clone()))?;
        for chunk in &file.blocks {
            let len =
                usize::try_from(chunk.length).map_err(|_| EngineError::Changed(path.clone()))?;
            let mut data = zeroize::Zeroizing::new(vec![0_u8; len]);
            reader
                .read_exact(&mut data)
                .map_err(|_| EngineError::Changed(path.clone()))?;
            whole.update(&data);
            let id = repository.put_block(&data).await?;
            uploaded += 1;
            refs.push(BlockRef {
                id,
                len: chunk.length,
            });
        }
        let digest = hex::encode(whole.finalize());
        if digest != file.whole_file_hash {
            return Err(EngineError::Changed(path));
        }
        for (reference, chunk) in refs.iter().zip(&file.blocks) {
            local_chunks
                .entry((reference.id.clone(), reference.len))
                .or_insert_with(|| (PathBuf::from(&file.path), chunk.offset));
        }
        let old = known_paths.get(Path::new(&file.path));
        let ent = old.map_or_else(
            || namespace_entity(mapping, &file.path),
            |entry| Ok(entry.entity.clone()),
        )?;
        observed.insert(ent.clone());
        let value = RevisionValue::Live {
            parent_dir: parent(mapping, &file.path)?,
            name: Path::new(&file.path)
                .file_name()
                .and_then(|x| x.to_str())
                .ok_or_else(|| EngineError::UnsafePath(file.path.clone().into()))?
                .to_owned(),
            node: Node::File(FileNode {
                blocks: refs,
                length: file.size,
                digest: digest.clone(),
            }),
        };
        let same = old.is_some_and(|entry| matches!(&entry.value, RevisionValue::Live { node: Node::File(node), .. } if node.digest == digest && node.length == file.size));
        if !same {
            let baseline = merge::frontier(&base.revisions)?
                .remove(&ent)
                .map_or(Baseline::Unknown, |frontier| Baseline::Known { frontier });
            let r = merge::revision(merge::prepare_local_change(
                ent,
                change(operation_id, &file.path, &digest),
                baseline,
                value,
                &history,
                true,
            )?)?;
            local.insert(r.revision_id.clone(), r);
        }
    }
    for entry in base_projection
        .entries
        .iter()
        .filter(|entry| matches!(entry.value, RevisionValue::Live { .. }))
    {
        if !observed.contains(&entry.entity) {
            let frontier = merge::frontier(&base.revisions)?
                .remove(&entry.entity)
                .ok_or(EngineError::Unsupported)?;
            let change = merge::prepare_local_change(
                entry.entity.clone(),
                change(operation_id, &entry.entity.0, "deleted"),
                Baseline::Known { frontier },
                RevisionValue::Tombstone,
                &history,
                true,
            )?;
            let revision = merge::revision(change)?;
            local.insert(revision.revision_id.clone(), revision);
        }
    }
    let candidate = match &pending {
        Some(pending) => pending.snapshot.clone(),
        None => merge::join(&remote, &MergeSet { revisions: local })?,
    };
    index.save(
        "engine",
        "active_intent",
        &Some(super::engine_types::PendingRun {
            operation_id: operation_id.into(),
            config_revision,
            published_commit: None,
            generation: None,
            snapshot: candidate.clone(),
            source_scan: pending.as_ref().map_or_else(
                || scan_report.clone(),
                |pending| pending.source_scan.clone(),
            ),
        }),
    )?;
    index.save("engine", "active_operation", &Some(operation_id.to_owned()))?;
    progress(super::manager_types::SyncPhase::Publishing);
    let published = publisher::publish(
        repository.clone(),
        index,
        operation_id,
        config_revision,
        candidate,
        &[],
        control,
    )
    .await?;
    let final_snapshot = load_remote(&repository, &published.commit, control).await?;
    index.save(
        "engine",
        "pending",
        &Some(super::engine_types::PendingRun {
            operation_id: operation_id.to_owned(),
            config_revision,
            published_commit: Some(published.commit.clone()),
            generation: Some(published.generation),
            snapshot: final_snapshot.clone(),
            source_scan: pending.as_ref().map_or_else(
                || scan_report.clone(),
                |pending| pending.source_scan.clone(),
            ),
        }),
    )?;
    let projection =
        super::namespace::project_scoped(&merge::project(&final_snapshot.revisions)?, mapping)
            .map_err(EngineError::Merge)?;
    // Private staging must already have been established and validated by the caller.
    if !std::fs::symlink_metadata(private)
        .map_err(|_| EngineError::UnsafePath(private.into()))?
        .is_dir()
    {
        return Err(EngineError::UnsafePath(private.into()));
    }
    progress(super::manager_types::SyncPhase::Applying);
    let mut applied = 0;
    for entry in &projection.entries {
        let relative = entry.projection_path.iter().collect::<PathBuf>();
        super::paths::validate_relative(&relative)
            .map_err(|_| EngineError::UnsafePath(relative.clone()))?;
    }
    for entry in &projection.entries {
        check(control)?;
        let rel = entry.projection_path.iter().collect::<PathBuf>();
        if matches!(
            entry.value,
            RevisionValue::Live {
                node: Node::Dir,
                ..
            }
        ) {
            ensure_directory(root, private, &rel)?;
        }
    }
    if pending.is_some() {
        let summary = super::recovery::recover_pending(index, root, private)
            .map_err(EngineError::Recovery)?;
        if summary.needs_user_confirmation {
            return Err(EngineError::PendingRecovery);
        }
    }
    let source_scan = pending
        .as_ref()
        .map_or(&scan_report, |pending| &pending.source_scan);
    for entry in &projection.entries {
        check(control)?;
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
            check(control)?;
            let len =
                usize::try_from(block.len).map_err(|_| EngineError::Changed(target.clone()))?;
            if len > super::repository_types::MAX_BLOCK_SIZE {
                return Err(EngineError::Unsupported);
            }
            let reused =
                local_chunks
                    .get(&(block.id.clone(), block.len))
                    .and_then(|(path, offset)| {
                        use std::io::{Seek, SeekFrom};
                        let mut source = open_nofollow(root, path).ok()?;
                        source.seek(SeekFrom::Start(*offset)).ok()?;
                        let mut bytes = zeroize::Zeroizing::new(vec![0_u8; len]);
                        source.read_exact(&mut bytes).ok()?;
                        match repository.verify_local_block(&block.id, &bytes) {
                            Ok(true) => Some(bytes),
                            _ => None,
                        }
                    });
            let bytes = match reused {
                Some(bytes) => bytes,
                None => repository.get_block(&block.id, len).await?,
            };
            std::io::Write::write_all(&mut tmp, &bytes)
                .map_err(|_| EngineError::Changed(target.clone()))?;
        }
        tmp.as_file_mut()
            .sync_all()
            .map_err(|_| EngineError::Changed(target.clone()))?;
        let apply_operation =
            hex::encode(Sha256::digest(format!("{operation_id}:{}", rel.display())));
        let mut journal = |intent: &super::apply_types::ApplyIntent| {
            index
                .record_apply(&apply_operation, intent)
                .map_err(|_| super::apply_types::ApplyError::Journal)
        };
        super::apply::apply_bytes(
            root,
            private,
            &apply_operation,
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
            &mut journal,
        )?;
        applied += 1;
    }
    let target_paths: std::collections::BTreeSet<PathBuf> = projection
        .entries
        .iter()
        .filter(|entry| matches!(entry.value, RevisionValue::Live { .. }))
        .map(|entry| entry.projection_path.iter().collect())
        .collect();
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
            check(control)?;
            let apply_operation = hex::encode(Sha256::digest(format!(
                "{operation_id}:delete:{}",
                relative.display()
            )));
            let mut journal = |intent: &super::apply_types::ApplyIntent| {
                index
                    .record_apply(&apply_operation, intent)
                    .map_err(|_| super::apply_types::ApplyError::Journal)
            };
            super::apply::apply_bytes(
                root,
                private,
                &apply_operation,
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
                &mut journal,
            )?;
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
        check(control)?;
        let directory = open_root(&root.join(&relative))?
            .dir_metadata()
            .map_err(EngineError::Io)?;
        #[cfg(unix)]
        let identity = {
            use cap_std::fs::MetadataExt;
            super::apply_types::FileIdentity {
                dev: directory.dev(),
                ino: directory.ino(),
            }
        };
        #[cfg(not(unix))]
        {
            let _ = directory;
            return Err(EngineError::Unsupported);
        }
        #[cfg(unix)]
        {
            let operation = hex::encode(Sha256::digest(format!(
                "{operation_id}:directory:{}",
                relative.display()
            )));
            let mut journal = |intent: &super::apply_types::ApplyIntent| {
                index
                    .record_apply(&operation, intent)
                    .map_err(|_| super::apply_types::ApplyError::Journal)
            };
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
                &mut journal,
            )?;
        }
    }
    // Re-read the materialized view before advancing the baseline. Changes made
    // while network/apply work was in flight must not be certified as applied.
    let verify_root = root.to_owned();
    let cancel = Arc::clone(&control.cancel);
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
    .map_err(EngineError::Join)??;
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
    check(control)?;
    let baseline_entries = projection
        .entries
        .iter()
        .filter_map(|entry| {
            let RevisionValue::Live {
                node: Node::File(_),
                ..
            } = &entry.value
            else {
                return None;
            };
            let relative = entry.projection_path.iter().collect::<PathBuf>();
            Some((
                relative.to_string_lossy().into_owned(),
                super::index_types::BaselineRecord::Materialized {
                    generation: published.generation,
                    commit: published.commit.clone(),
                    snapshot: published.commit.clone(),
                },
            ))
        })
        .collect::<Vec<_>>();
    let journal = super::index_types::RunJournal {
        operation_id: operation_id.into(),
        config_revision,
        phase: super::index_types::RunPhase::Completed,
        published_commit: Some(published.commit.clone()),
    };
    index.confirm_engine(operation_id, &journal, &final_snapshot, &baseline_entries)?;
    Ok(EngineReport {
        phase: super::index_types::RunPhase::Completed,
        scan: scan_report,
        commit: Some(published.commit),
        generation: Some(published.generation),
        uploaded_blocks: uploaded,
        applied_files: applied,
        conflicts: projection
            .entries
            .iter()
            .filter(|entry| entry.conflict)
            .count() as u64,
        pending_recovery: false,
    })
}
