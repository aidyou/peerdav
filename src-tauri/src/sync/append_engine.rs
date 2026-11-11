//! Append B/L/R synchronization, sharing the scan, merge and materialize paths.
use super::{
    append_discovery,
    append_engine_types::*,
    append_index::AppendIndex,
    append_repository::AppendRepository,
    append_types::*,
    apply_types::FileIdentity,
    engine_types::EngineError,
    materialize,
    materialize_types::*,
    merge,
    merge_types::{Node, Projection, RevisionMap, RevisionValue},
    scan,
    scan_types::{ScanOptions, ScanReport},
    traits::BlockReader,
};
use crate::cloud::traits::ObjectStore;
use futures_util::future::BoxFuture;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

impl<S: ObjectStore + ?Sized> BlockReader for PullBlocks<'_, S> {
    type Error = AppendError;
    fn verify_local(&self, id: &str, bytes: &[u8]) -> Result<bool, AppendError> {
        Ok(self.repository.block_id(bytes)? == id)
    }
    fn read_block<'a>(
        &'a self,
        id: &'a str,
        expected_len: usize,
    ) -> BoxFuture<'a, Result<zeroize::Zeroizing<Vec<u8>>, AppendError>> {
        Box::pin(async move {
            let binding = self
                .bindings
                .get(&(id.to_owned(), expected_len as u64))
                .ok_or(AppendError::Invalid("missing authenticated block binding"))?;
            let key = format!(
                "{APPEND_ROOT}/{}/blocks/{}",
                self.repository.repo_id(),
                binding.object.digest
            );
            let wire = append_discovery::read_bounded(self.store, &key, |_, bytes| {
                reserve_block_read(self.index, self.operation, bytes)
            })
            .await?;
            let plain = self.repository.open_block_with_ref(&wire, Some(binding))?;
            if plain.len() != expected_len || !self.verify_local(id, &plain)? {
                return Err(AppendError::Authentication);
            }
            Ok(plain)
        })
    }
}

fn reserve_block_read(
    shared: &Mutex<&mut AppendIndex>,
    operation: &str,
    limit: usize,
) -> Result<(), AppendError> {
    let mut index = shared
        .lock()
        .map_err(|_| AppendError::Index(super::index_types::IndexError::invalid_record()))?;
    let mut budget = index.discovery_budget(operation)?;
    if budget.requests >= MAX_DISCOVERY_REQUESTS {
        return Err(AppendError::ResourceLimit("block requests"));
    }
    if limit > MAX_DISCOVERY_READ_BYTES.saturating_sub(budget.bytes_read) {
        return Err(AppendError::ResourceLimit("block read bytes"));
    }
    budget.requests += 1;
    budget.bytes_read += limit;
    index.save_discovery_budget(operation, &budget)?;
    Ok(())
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[tokio::test]
    async fn freeze_insufficient_budget_performs_no_get() -> Result<(), Box<dyn std::error::Error>>
    {
        use super::super::merge_types::*;
        use crate::cloud::types::*;
        use sha2::Digest;
        struct NoGet;
        impl ObjectStore for NoGet {
            fn capabilities(&self) -> StorageCapabilities {
                panic!("no capabilities")
            }
            fn list<'a>(&'a self, _: &'a str) -> ObjectListing<'a> {
                panic!("no LIST")
            }
            fn stat<'a>(
                &'a self,
                key: &'a str,
            ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
                Box::pin(async move {
                    Ok(ObjectMetadata {
                        key: key.into(),
                        size: 5,
                        etag: None,
                    })
                })
            }
            fn get<'a>(
                &'a self,
                _: &'a str,
                _: Option<std::ops::Range<u64>>,
            ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
                panic!("budget must reject before GET")
            }
            fn put<'a>(
                &'a self,
                _: &'a str,
                _: ObjectStream,
                _: PutCondition,
            ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
                panic!("no PUT")
            }
            fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
                panic!("no DELETE")
            }
        }
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("index");
        let repo_id = "00112233445566778899aabbccddeeff";
        let descriptor = Descriptor {
            version: APPEND_VERSION,
            repo_id: repo_id.into(),
            mode: AppendMode::Encrypted,
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
        let (anchor, _) = AppendRepository::create_anchor(descriptor, b"p")?;
        let repository = AppendRepository::new(Arc::new(NoGet), anchor, b"p")?;
        let (key, _) = repository.seal_block(b"hello")?;
        let block_id = repository.block_id(b"hello")?;
        let revision = merge::revision(LocalChange {
            entity: EntityId("remote".into()),
            change_id: ChangeId("remote".into()),
            parents: vec![],
            value: RevisionValue::Live {
                parent_dir: EntityId("root".into()),
                name: "remote.txt".into(),
                node: Node::File(FileNode {
                    blocks: vec![BlockRef {
                        id: block_id.clone(),
                        len: 5,
                    }],
                    length: 5,
                    digest: hex::encode(sha2::Sha256::digest(b"hello")),
                }),
            },
        })?;
        for (requests, bytes_read) in [(MAX_DISCOVERY_REQUESTS, 0), (0, MAX_DISCOVERY_READ_BYTES)] {
            let path = path.with_extension(requests.to_string());
            let mut index =
                AppendIndex::open(&path, zeroize::Zeroizing::new([7; 32]), repo_id.into())?;
            let before = DiscoveryBudget {
                requests,
                bytes_read,
                ..DiscoveryBudget::default()
            };
            index.save_discovery_budget("freeze", &before)?;
            let mut history = RevisionMap::from([(revision.revision_id.clone(), revision.clone())]);
            let mut bindings = Bindings::from([(
                (block_id.clone(), 5),
                BlockBinding {
                    block_id: block_id.clone(),
                    len: 5,
                    object: ObjectRef {
                        kind: "block".into(),
                        digest: key.rsplit('/').next().ok_or("block key")?.into(),
                    },
                },
            )]);
            let cancel = Arc::new(AtomicBool::new(false));
            let scan = scan_complete(directory.path(), cancel.clone())?;
            let result = prepare_local(
                &NoGet,
                &repository,
                &mut index,
                &AppendPullRequest {
                    root: directory.path(),
                    private: directory.path(),
                    mapping: "docs",
                    run_id: "freeze",
                },
                &ScanReport {
                    files: vec![],
                    directories: vec![],
                    ..scan
                },
                &RevisionMap::new(),
                &Projection { entries: vec![] },
                &mut history,
                &mut bindings,
                &mut TaggedFrontier {
                    repo_id: repo_id.into(),
                    anchor_fingerprint: String::new(),
                    tips: vec![],
                },
                &cancel,
                &[],
            )
            .await;
            assert!(
                matches!(
                    result,
                    Err(AppendEngineError::Append(AppendError::ResourceLimit(_)))
                ),
                "{result:?}"
            );
            drop(index);
            let index = AppendIndex::open(&path, zeroize::Zeroizing::new([7; 32]), repo_id.into())?;
            let after = index.discovery_budget("freeze")?;
            assert_eq!(after.requests, before.requests + usize::from(requests == 0));
            assert_eq!(after.bytes_read, before.bytes_read);
        }
        Ok(())
    }

    #[test]
    fn reservation_boundaries_are_cumulative_and_atomic() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let mut index = AppendIndex::open(
            &directory.path().join("index"),
            zeroize::Zeroizing::new([7; 32]),
            "budget-test".into(),
        )?;
        let limit = 2 * (MAX_BLOCK_SIZE + 16) + 4096;
        for (operation, requests, bytes) in [
            ("requests", MAX_DISCOVERY_REQUESTS, 0),
            ("bytes", 0, MAX_DISCOVERY_READ_BYTES - limit + 1),
        ] {
            let before = DiscoveryBudget {
                requests,
                bytes_read: bytes,
                ..DiscoveryBudget::default()
            };
            index.save_discovery_budget(operation, &before)?;
            assert!(matches!(
                reserve_block_read(&Mutex::new(&mut index), operation, limit),
                Err(AppendError::ResourceLimit(_))
            ));
            let after = index.discovery_budget(operation)?;
            assert_eq!(after.requests, before.requests);
            assert_eq!(after.bytes_read, before.bytes_read);
        }
        index.save_discovery_budget(
            "exact",
            &DiscoveryBudget {
                requests: MAX_DISCOVERY_REQUESTS - 1,
                bytes_read: MAX_DISCOVERY_READ_BYTES - limit,
                ..DiscoveryBudget::default()
            },
        )?;
        reserve_block_read(&Mutex::new(&mut index), "exact", limit)?;
        let exact = index.discovery_budget("exact")?;
        assert_eq!(exact.requests, MAX_DISCOVERY_REQUESTS);
        assert_eq!(exact.bytes_read, MAX_DISCOVERY_READ_BYTES);
        assert!(reserve_block_read(&Mutex::new(&mut index), "exact", limit).is_err());
        Ok(())
    }
}

fn scan_complete(
    root: &std::path::Path,
    cancel: Arc<AtomicBool>,
) -> Result<ScanReport, AppendEngineError> {
    let result = scan::scan(
        root,
        &ScanOptions {
            cancel,
            ..ScanOptions::default()
        },
    )?;
    if !result.complete || !result.ignored_paths.is_empty() {
        return Err(super::scan_types::ScanError::RootChanged.into());
    }
    Ok(result)
}

fn matches_projection(scan: &ScanReport, projection: &Projection) -> bool {
    let files: BTreeMap<_, _> = projection
        .entries
        .iter()
        .filter_map(|entry| match &entry.value {
            RevisionValue::Live {
                node: Node::File(file),
                ..
            } => Some((entry.projection_path.join("/"), (&file.digest, file.length))),
            _ => None,
        })
        .collect();
    let directories: BTreeSet<_> = projection
        .entries
        .iter()
        .filter_map(|entry| {
            matches!(
                entry.value,
                RevisionValue::Live {
                    node: Node::Dir,
                    ..
                }
            )
            .then(|| entry.projection_path.join("/"))
        })
        .collect();
    scan.files.len() == files.len()
        && scan.files.iter().all(|file| {
            files.get(&file.path).is_some_and(|(digest, length)| {
                **digest == file.whole_file_hash && *length == file.size
            })
        })
        && scan
            .directories
            .iter()
            .filter(|path| !path.is_empty())
            .cloned()
            .collect::<BTreeSet<_>>()
            == directories
}

fn selected_path(path: &str, selection: &[String]) -> bool {
    selection.is_empty() || selection.iter().any(|selected| path == selected || path.strip_prefix(selected).is_some_and(|suffix| suffix.starts_with('/')))
}

fn selected_projection(full: &Projection, selection: &[String]) -> Projection {
    Projection { entries: full.entries.iter().filter(|entry| selected_path(&entry.logical_path.join("/"), selection) || selected_path(&entry.projection_path.join("/"), selection)).cloned().collect() }
}

fn selected_scan(full: &ScanReport, selection: &[String]) -> ScanReport {
    let mut scan = full.clone();
    scan.files.retain(|file| selected_path(&file.path, selection));
    scan.directories.retain(|path| selected_path(path, selection) || selection.iter().any(|selected| selected.starts_with(&format!("{path}/"))));
    scan
}

fn projection(revisions: &RevisionMap, mapping: &str) -> Result<Projection, AppendEngineError> {
    Ok(super::namespace::project_scoped(
        &merge::project(revisions)?,
        mapping,
    )?)
}

type Bindings = BTreeMap<(String, u64), BlockBinding>;

fn local_revision(
    entity: super::merge_types::EntityId,
    value: RevisionValue,
    operation: &str,
    frontier: &BTreeMap<super::merge_types::EntityId, Vec<super::merge_types::RevisionId>>,
    history: &mut RevisionMap,
) -> Result<(), AppendEngineError> {
    use super::merge_types::{Baseline, ChangeId};
    if history.len() >= super::merge_types::MAX_REVISIONS {
        return Err(AppendError::ResourceLimit("revisions").into());
    }
    let baseline = frontier
        .get(&entity)
        .map_or(Baseline::Unknown, |parents| Baseline::Known {
            frontier: parents.clone(),
        });
    let revision = merge::revision(merge::prepare_local_change(
        entity,
        ChangeId(operation.into()),
        baseline,
        value,
        history,
        true,
    )?)?;
    history.insert(revision.revision_id.clone(), revision);
    Ok(())
}

fn cache_object(
    index: &mut AppendIndex,
    operation: &str,
    count: &mut usize,
    object: PreparedObject,
) -> Result<(), AppendEngineError> {
    if *count >= MAX_PUBLISH_OBJECTS {
        return Err(AppendError::ResourceLimit("publish objects").into());
    }
    index.reserve(
        &format!("engine-staging:{operation}"),
        object.bytes().len() as u64,
        MAX_PUBLISH_STAGING,
    )?;
    index.cache_engine_object(operation, *count, &object)?;
    *count += 1;
    Ok(())
}

async fn prepare_local<S: ObjectStore + ?Sized>(
    store: &S,
    repository: &AppendRepository,
    index: &mut AppendIndex,
    request: &AppendPullRequest<'_>,
    scan: &ScanReport,
    base: &RevisionMap,
    base_projection: &Projection,
    history: &mut RevisionMap,
    bindings: &mut Bindings,
    remote: &mut TaggedFrontier,
    cancel: &Arc<AtomicBool>,
    selection: &[String],
) -> Result<String, AppendEngineError> {
    use super::{merge_types::*, namespace};
    use sha2::{Digest, Sha256};
    use std::{io::Read, path::Path};
    // This operation identity is generated once and persisted before remote PUT.
    let mut random = [0u8; 16];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut random)
        .map_err(|_| AppendError::Input("operation randomness"))?;
    let operation = hex::encode(random);
    let frontier = merge::frontier(base)?;
    let known: BTreeMap<_, _> = base_projection
        .entries
        .iter()
        .map(|entry| (entry.projection_path.join("/"), entry))
        .collect();
    let full = merge::project(history)?;
    let mut directories: BTreeMap<Vec<String>, EntityId> = full
        .entries
        .iter()
        .filter_map(|entry| {
            matches!(
                entry.value,
                RevisionValue::Live {
                    node: Node::Dir,
                    ..
                }
            )
            .then(|| (entry.logical_path.clone(), entry.entity.clone()))
        })
        .collect();
    drop(full);
    let prefix = namespace::components(request.mapping)?;
    let mut required: BTreeSet<Vec<String>> =
        (1..=prefix.len()).map(|n| prefix[..n].to_vec()).collect();
    for directory in scan.directories.iter().filter(|path| !path.is_empty()) {
        let mut path = prefix.clone();
        path.extend(directory.split('/').map(str::to_owned));
        required.insert(path);
    }
    let mut observed = BTreeSet::new();
    for path in required {
        let relative = path.get(prefix.len()..).map(|parts| parts.join("/"));
        let old = relative.as_ref().and_then(|path| known.get(path)).copied();
        let entity = old
            .map(|entry| entry.entity.clone())
            .or_else(|| directories.get(&path).cloned())
            .unwrap_or_else(|| namespace::entity(&path));
        observed.insert(entity.clone());
        let parent = if path.len() == 1 {
            EntityId("root".into())
        } else {
            directories
                .get(&path[..path.len() - 1])
                .cloned()
                .ok_or(AppendError::Invalid("directory parent"))?
        };
        if !directories.contains_key(&path) {
            local_revision(
                entity.clone(),
                RevisionValue::Live {
                    parent_dir: parent,
                    name: path
                        .last()
                        .ok_or(AppendError::Input("directory path"))?
                        .clone(),
                    node: Node::Dir,
                },
                &operation,
                &frontier,
                history,
            )?;
        }
        directories.insert(path, entity);
    }
    let mut count = 0usize;
    let mut cached = BTreeSet::new();
    for file in &scan.files {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppendEngineError::Cancelled);
        }
        let old = known.get(&file.path).copied();
        let mut logical = prefix.clone();
        logical.extend(file.path.split('/').map(str::to_owned));
        let entity = old.map_or_else(|| namespace::entity(&logical), |entry| entry.entity.clone());
        observed.insert(entity.clone());
        let same = old.is_some_and(|entry| {
            matches!(&entry.value,
            RevisionValue::Live { node: Node::File(node), .. }
                if node.digest == file.whole_file_hash && node.length == file.size)
        });
        if same {
            continue;
        }
        let mut reader = materialize::open_nofollow(request.root, Path::new(&file.path))?;
        let mut whole = Sha256::new();
        let mut refs = Vec::with_capacity(file.blocks.len());
        for chunk in &file.blocks {
            if cancel.load(Ordering::SeqCst) {
                return Err(AppendEngineError::Cancelled);
            }
            if chunk.length == 0 || chunk.length > MAX_BLOCK_SIZE as u64 {
                return Err(AppendError::ResourceLimit("local block").into());
            }
            let mut bytes = zeroize::Zeroizing::new(vec![0; chunk.length as usize]);
            reader.read_exact(&mut bytes)?;
            if hex::encode(Sha256::digest(&bytes)) != chunk.signature {
                return Err(EngineError::Changed(request.root.join(&file.path)).into());
            }
            whole.update(&bytes);
            let id = repository.block_id(&bytes)?;
            if !bindings.contains_key(&(id.clone(), chunk.length)) {
                let object = repository.prepare_block(&bytes).await?;
                let binding = BlockBinding {
                    block_id: id.clone(),
                    len: chunk.length,
                    object: ObjectRef {
                        kind: "block".into(),
                        digest: object
                            .key()
                            .rsplit('/')
                            .next()
                            .ok_or(AppendError::Invalid("block locator"))?
                            .into(),
                    },
                };
                cached.insert(binding.object.digest.clone());
                cache_object(index, &operation, &mut count, object)?;
                bindings.insert((id.clone(), chunk.length), binding);
            }
            refs.push(BlockRef {
                id,
                len: chunk.length,
            });
        }
        let mut tail = [0u8; 1];
        if reader.read(&mut tail)? != 0 || hex::encode(whole.finalize()) != file.whole_file_hash {
            return Err(EngineError::Changed(request.root.join(&file.path)).into());
        }
        let name = logical.pop().ok_or(AppendError::Input("file name"))?;
        let parent_dir = directories
            .get(&logical)
            .cloned()
            .ok_or(AppendError::Invalid("file parent"))?;
        local_revision(
            entity,
            RevisionValue::Live {
                parent_dir,
                name,
                node: Node::File(FileNode {
                    blocks: refs,
                    length: file.size,
                    digest: file.whole_file_hash.clone(),
                }),
            },
            &operation,
            &frontier,
            history,
        )?;
    }
    for entry in &base_projection.entries {
        if matches!(entry.value, RevisionValue::Live { .. }) && !observed.contains(&entry.entity) {
            local_revision(
                entry.entity.clone(),
                RevisionValue::Tombstone,
                &operation,
                &frontier,
                history,
            )?;
        }
    }
    merge::validate(history)?;
    // Exact GET of authenticated locators, never reseal an old encrypted block.
    // One wire object is live at a time; staging is encrypted SQLite, not a Vec of closure bytes.
    let mut used = BTreeSet::new();
    for revision in history.values() {
        if let RevisionValue::Live {
            node: Node::File(file),
            ..
        } = &revision.value
        {
            used.extend(
                file.blocks
                    .iter()
                    .map(|block| (block.id.clone(), block.len)),
            );
        }
    }
    bindings.retain(|key, _| used.contains(key));
    for binding in bindings.values() {
        if !cached.insert(binding.object.digest.clone()) {
            continue;
        }
        let key = format!(
            "{APPEND_ROOT}/{}/blocks/{}",
            repository.repo_id(),
            binding.object.digest
        );
        // Freeze and apply share the active run's nonrefundable durable meter.
        let bytes = append_discovery::read_bounded(store, &key, |_, bytes| {
            reserve_block_read(&Mutex::new(&mut *index), request.run_id, bytes)
        })
        .await?;
        repository.open_block_with_ref(&bytes, Some(binding))?;
        cache_object(
            index,
            &operation,
            &mut count,
            PreparedObject::new(key, bytes),
        )?;
    }
    let tree = Tree {
        version: APPEND_VERSION,
        repo_id: repository.repo_id().into(),
        directory: "root".into(),
        entries: history.values().cloned().collect(),
        children: vec![],
        bindings: bindings.values().cloned().collect(),
    };
    let (root, wire) = repository.seal_tree_owned(tree)?;
    cache_object(
        index,
        &operation,
        &mut count,
        PreparedObject::new(
            format!(
                "{APPEND_ROOT}/{}/trees/{}",
                repository.repo_id(),
                root.digest
            ),
            wire,
        ),
    )?;
    let parents = remote
        .tips
        .iter()
        .map(|tip| {
            format!(
                "{APPEND_ROOT}/{}/commits/{}-{}",
                repository.repo_id(),
                tip.operation_id,
                tip.digest
            )
        })
        .collect::<Vec<_>>();
    let (tip, wire) = repository.seal_commit(&Commit {
        version: APPEND_VERSION,
        repo_id: repository.repo_id().into(),
        operation_id: operation.clone(),
        parents: remote.tips.clone(),
        root: root.clone(),
    })?;
    cache_object(
        index,
        &operation,
        &mut count,
        PreparedObject::new(
            format!(
                "{APPEND_ROOT}/{}/commits/{}-{}",
                repository.repo_id(),
                tip.operation_id,
                tip.digest
            ),
            wire,
        ),
    )?;
    if selected_scan(&scan_complete(request.root, cancel.clone())?, selection) != *scan {
        return Err(super::scan_types::ScanError::RootChanged.into());
    }
    super::append_publisher::AppendPublisher::prepare_cached(
        index, repository, &operation, &parents, root, count,
    )?;
    remote.tips = vec![tip];
    Ok(operation)
}

fn check_binding(
    epoch: &LocalEpoch,
    request: &AppendPullRequest<'_>,
    binding: &TrustBinding,
    identity: &FileIdentity,
) -> Result<(), AppendEngineError> {
    if epoch.root != request.root
        || epoch.private != request.private
        || epoch.mapping != request.mapping
        || epoch.root_identity != *identity
        || epoch.frontier.repo_id != binding.repo_id
        || epoch.frontier.anchor_fingerprint != binding.fingerprint
    {
        return Err(AppendEngineError::BindingChanged);
    }
    Ok(())
}

/// Pull authenticated remote state into an unchanged confirmed local baseline.
/// An unknown baseline is accepted only for a completely empty local root.
/// Pending apply is resumed before new discovery, with its original source scan.
pub async fn pull<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    index: &mut AppendIndex,
    request: &AppendPullRequest<'_>,
    cancel: Arc<AtomicBool>,
) -> Result<AppendEngineReport, AppendEngineError> {
    pull_or_run(
        store, binding, password, index, request, cancel, false, false, &[],
    )
    .await
}

/// Synchronize local changes and remote history with independent apply cancellation.
pub async fn run<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    index: &mut AppendIndex,
    request: &AppendPullRequest<'_>,
    cancel: Arc<AtomicBool>,
) -> Result<AppendEngineReport, AppendEngineError> {
    run_with_deletion_policy(store, binding, password, index, request, cancel, false).await
}

pub async fn run_with_deletion_policy<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    index: &mut AppendIndex,
    request: &AppendPullRequest<'_>,
    cancel: Arc<AtomicBool>,
    automatic: bool,
) -> Result<AppendEngineReport, AppendEngineError> {
    pull_or_run(
        store, binding, password, index, request, cancel, true, automatic, &[],
    )
    .await
}

pub async fn run_selected<S: ObjectStore + ?Sized>(
    store: Arc<S>, binding: &TrustBinding, password: &[u8], index: &mut AppendIndex,
    request: &AppendPullRequest<'_>, cancel: Arc<AtomicBool>, selection: &[String],
) -> Result<AppendEngineReport, AppendEngineError> {
    if selection.is_empty() || selection.len() > 1000 { return Err(AppendError::Input("selection").into()); }
    for path in selection { super::namespace::components(path)?; }
    pull_or_run(store, binding, password, index, request, cancel, true, false, selection).await
}

async fn pull_or_run<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    index: &mut AppendIndex,
    request: &AppendPullRequest<'_>,
    mut cancel: Arc<AtomicBool>,
    publish: bool,
    automatic: bool,
    selection: &[String],
) -> Result<AppendEngineReport, AppendEngineError> {
    if cancel.load(Ordering::SeqCst) && index.pending_epoch()?.is_none() {
        return Err(AppendEngineError::Cancelled);
    }
    if request.run_id.is_empty()
        || request.run_id.len() > 128
        || !request.run_id.is_ascii()
        || request.run_id.contains(':')
    {
        return Err(AppendError::Input("local run identity").into());
    }
    // Capability opening rejects symlink roots before scan or recovery.
    let root = materialize::open_root(request.root)?;
    #[cfg(unix)]
    let identity = {
        use cap_std::fs::MetadataExt;
        let metadata = root.dir_metadata()?;
        FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    };
    #[cfg(not(unix))]
    let identity = {
        return Err(EngineError::Unsupported.into());
    };
    let active = index.active_epoch()?;
    if let Some(epoch) = &active {
        check_binding(epoch, request, binding, &identity)?;
    }
    let base = match &active {
        Some(epoch) => index.epoch_revisions(epoch)?,
        None => RevisionMap::new(),
    };
    let base_projection = projection(&base, request.mapping)?;
    let pending = index.pending_epoch()?;
    let resumed_selection = if selection.is_empty() { pending.as_ref().map(|epoch| epoch.selection.clone()) } else { None };
    let selection = resumed_selection.as_deref().unwrap_or(selection);
    if pending.as_ref().is_some_and(|epoch| epoch.selection != selection) {
        return Err(AppendEngineError::RecoveryPaused);
    }
    if automatic && pending.is_some() {
        return Err(AppendEngineError::RecoveryPaused);
    }
    let operation = match &pending {
        Some(epoch) => epoch.id.clone(),
        None => index.begin_run("engine", request.run_id)?,
    };
    let request = &AppendPullRequest {
        root: request.root,
        private: request.private,
        mapping: request.mapping,
        run_id: &operation,
    };
    if pending
        .as_ref()
        .is_some_and(|epoch| epoch.publication.is_some())
    {
        // Resume the frozen operation before scanning; a retry never replaces bytes.
        let epoch = pending
            .as_ref()
            .ok_or(AppendError::Invalid("pending epoch"))?;
        check_binding(epoch, request, binding, &identity)?;
        let id = epoch
            .publication
            .as_deref()
            .ok_or(AppendError::Invalid("publication"))?;
        super::append_publisher::AppendPublisher::resume(index, store.as_ref(), id, &cancel)
            .await?;
        cancel = Arc::new(AtomicBool::new(false));
    }
    let resumed = pending.is_some();
    let mut current_scan = scan_complete(request.root, cancel.clone())?;
    let mut epoch = if let Some(epoch) = pending {
        check_binding(&epoch, request, binding, &identity)?;
        epoch
    } else {
        if !publish && !matches_projection(&current_scan, &base_projection) {
            return Err(AppendEngineError::LocalChangesUnsupported);
        }
        let closure =
            append_discovery::discover(store.clone(), binding, password, index, request.run_id)
                .await?;
        if cancel.load(Ordering::SeqCst) {
            return Err(AppendEngineError::Cancelled);
        }
        let mut revisions = RevisionMap::new();
        let mut bindings = BTreeMap::new();
        let mut revision_bytes = 0usize;
        for tree in closure.trees().values() {
            for revision in &tree.entries {
                if revisions.contains_key(&revision.revision_id) {
                    if revisions.get(&revision.revision_id) != Some(revision) {
                        return Err(AppendError::Invalid("revision body mismatch").into());
                    }
                    continue;
                }
                if revisions.len() >= super::merge_types::MAX_REVISIONS {
                    return Err(AppendError::ResourceLimit("revisions").into());
                }
                revision_bytes = revision_bytes
                    .checked_add(
                        serde_json::to_vec(revision)
                            .map_err(AppendError::from)?
                            .len(),
                    )
                    .ok_or(AppendError::ResourceLimit("revision bytes"))?;
                if revision_bytes > MAX_DISCOVERY_ALLOCATION {
                    return Err(AppendError::ResourceLimit("revision bytes").into());
                }
                if let Some(old) = revisions.insert(revision.revision_id.clone(), revision.clone())
                {
                    if old != *revision {
                        return Err(AppendError::Invalid("revision body mismatch").into());
                    }
                }
            }
            for block in &tree.bindings {
                if bindings.len() >= MAX_DISCOVERY_NODES {
                    return Err(AppendError::ResourceLimit("bindings").into());
                }
                bindings
                    .entry((block.block_id.clone(), block.len))
                    .or_insert_with(|| block.clone());
            }
        }
        merge::validate(&revisions)?;
        let parents: BTreeSet<_> = closure
            .commits()
            .values()
            .flat_map(|commit| {
                commit
                    .parents
                    .iter()
                    .map(|parent| parent.operation_id.clone())
            })
            .collect();
        let tips = closure
            .commits()
            .iter()
            .filter(|(_, commit)| !parents.contains(&commit.operation_id))
            .map(|(key, commit)| {
                let digest = key
                    .rsplit('-')
                    .next()
                    .ok_or(AppendError::Invalid("commit locator"))?;
                Ok(CommitRef {
                    operation_id: commit.operation_id.clone(),
                    digest: digest.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, AppendError>>()?;
        let mut frontier = TaggedFrontier {
            repo_id: binding.repo_id.clone(),
            anchor_fingerprint: binding.fingerprint.clone(),
            tips,
        };
        // Retain B history even when the frozen listing does not contain its tips.
        for (id, revision) in &base {
            if let Some(old) = revisions.get(id) {
                if old != revision {
                    return Err(AppendError::Invalid("revision body mismatch").into());
                }
            } else {
                if revisions.len() >= super::merge_types::MAX_REVISIONS {
                    return Err(AppendError::ResourceLimit("revisions").into());
                }
                revisions.insert(id.clone(), revision.clone());
            }
        }
        if let Some(epoch) = &active {
            for ordinal in 0..epoch.binding_count {
                let binding = index.epoch_binding(epoch, ordinal)?;
                bindings
                    .entry((binding.block_id.clone(), binding.len))
                    .or_insert(binding);
            }
        }
        merge::validate(&revisions)?;
        if automatic {
            materialize::check_automatic_deletions(
                &base_projection,
                &current_scan,
                &projection(&revisions, request.mapping)?,
            )?;
        }
        let mut publication = None;
        let publish_scan = selected_scan(&current_scan, selection);
        let publish_base = selected_projection(&base_projection, selection);
        if publish && !matches_projection(&publish_scan, &publish_base) {
            let repository = append_discovery::open_budgeted(
                store.clone(),
                binding,
                password,
                index,
                request.run_id,
            )
            .await?;
            publication = Some(
                prepare_local(
                    store.as_ref(),
                    &repository,
                    index,
                    request,
                    &publish_scan,
                    &base,
                    &publish_base,
                    &mut revisions,
                    &mut bindings,
                    &mut frontier,
                    &cancel,
                    selection,
                )
                .await?,
            );
        }
        if let Some(active) = &active {
            if active.frontier == frontier && base == revisions {
                index.complete_run("engine", request.run_id)?;
                return Ok(AppendEngineReport {
                    frontier,
                    local_epoch: active.sequence,
                    applied_files: 0,
                    resumed: false,
                });
            }
        }
        let sequence = active.as_ref().map_or(Ok(1), |epoch| {
            epoch
                .sequence
                .checked_add(1)
                .ok_or(AppendError::ResourceLimit("local epoch"))
        })?;
        if !selection.is_empty() {
            let full = projection(&revisions, request.mapping)?;
            let chosen = full.entries.iter().chain(base_projection.entries.iter()).filter(|entry| selected_path(&entry.logical_path.join("/"), selection) || selected_path(&entry.projection_path.join("/"), selection)).map(|entry| entry.entity.clone()).collect::<BTreeSet<_>>();
            let ancestor_paths = selection.iter().flat_map(|path| {
                let parts = path.split('/').collect::<Vec<_>>();
                (1..parts.len()).map(move |n| parts[..n].join("/"))
            }).collect::<BTreeSet<_>>();
            let ancestors = full.entries.iter().filter(|entry| ancestor_paths.contains(&entry.logical_path.join("/"))).map(|entry| entry.entity.clone()).collect::<BTreeSet<_>>();
            let prefix = super::namespace::components(request.mapping)?;
            let anchors = (1..=prefix.len()).map(|n| super::namespace::entity(&prefix[..n])).collect::<BTreeSet<_>>();
            let mut retained = chosen.union(&ancestors).cloned().chain(anchors).chain(base.values().map(|revision| revision.entity.clone())).collect::<BTreeSet<_>>();
            loop {
                let parents = revisions.values().filter(|revision| retained.contains(&revision.entity)).filter_map(|revision| match &revision.value { RevisionValue::Live { parent_dir, .. } if parent_dir.0 != "root" => Some(parent_dir.clone()), _ => None }).collect::<Vec<_>>();
                let before = retained.len();
                retained.extend(parents);
                if retained.len() == before { break; }
            }
            let known_entities = base.values().map(|revision| revision.entity.clone()).collect::<BTreeSet<_>>();
            revisions.retain(|id, revision| base.contains_key(id) || retained.contains(&revision.entity) && !known_entities.contains(&revision.entity) || chosen.contains(&revision.entity) || ancestors.contains(&revision.entity));
            merge::validate(&revisions)?;
        }
        let epoch = LocalEpoch {
            selection: selection.to_vec(),
            id: request.run_id.to_owned(),
            sequence,
            frontier,
            root: request.root.to_owned(),
            private: request.private.to_owned(),
            mapping: request.mapping.to_owned(),
            root_identity: identity,
            source_scan: current_scan.clone(),
            revision_count: revisions.len(),
            binding_count: bindings.len(),
            publication,
        };
        index.stage_epoch(
            &epoch,
            &revisions,
            &bindings.into_values().collect::<Vec<_>>(),
        )?;
        epoch
    };
    if let Some(id) = &epoch.publication {
        super::append_publisher::AppendPublisher::resume(index, store.as_ref(), id, &cancel)
            .await?;
        cancel = Arc::new(AtomicBool::new(false));
    }
    let repository =
        append_discovery::open_budgeted(store.clone(), binding, password, index, &epoch.id).await?;
    let target = projection(&index.epoch_revisions(&epoch)?, request.mapping)?;
    let mut apply_target = selected_projection(&target, selection);
    let apply_base = selected_projection(&base_projection, selection);
    if !selection.is_empty() {
        // Unselected local paths participate only in strict verification, never
        // in remote history or publication. Existing bytes must remain intact.
        use super::merge_types::{EntityId, RevisionId, ProjectionEntry, FileNode};
        for file in current_scan.files.iter().filter(|file| !selected_path(&file.path, selection)) {
            apply_target.entries.push(ProjectionEntry { entity: EntityId(format!("local:{}", file.path)), version: RevisionId("local-only".into()), logical_path: file.path.split('/').map(str::to_owned).collect(), projection_path: file.path.split('/').map(str::to_owned).collect(), value: RevisionValue::Live { parent_dir: EntityId("root".into()), name: file.path.clone(), node: Node::File(FileNode { blocks: vec![], length: file.size, digest: file.whole_file_hash.clone() }) }, conflict: false });
        }
        for path in current_scan.directories.iter().filter(|path| !path.is_empty() && !selected_path(path, selection)) {
            apply_target.entries.push(ProjectionEntry { entity: EntityId(format!("local:{path}")), version: RevisionId("local-only".into()), logical_path: path.split('/').map(str::to_owned).collect(), projection_path: path.split('/').map(str::to_owned).collect(), value: RevisionValue::Live { parent_dir: EntityId("root".into()), name: path.clone(), node: Node::Dir }, conflict: false });
        }
    }
    let records = index.engine_apply_records(&epoch.id)?;
    if !records.is_empty() {
        let summary = super::recovery::recover_append_records(
            request.root,
            request.private,
            records,
            &mut |_record_id, intent| {
                index
                    .record_engine_apply(&epoch.id, intent)
                    .map_err(super::recovery_types::RecoveryError::from)
            },
        )?;
        if summary.needs_user_confirmation {
            return Err(AppendEngineError::RecoveryPaused);
        }
        current_scan = scan_complete(request.root, cancel.clone())?;
    }
    let mut bindings = BTreeMap::new();
    for ordinal in 0..epoch.binding_count {
        let binding = index.epoch_binding(&epoch, ordinal)?;
        bindings.insert((binding.block_id.clone(), binding.len), binding);
    }
    let apply_index = Mutex::new(&mut *index);
    let reader = PullBlocks {
        repository: &repository,
        store: store.as_ref(),
        bindings,
        index: &apply_index,
        operation: &epoch.id,
    };
    let request = MaterializeRequest {
        root: request.root,
        private: request.private,
        mapping: request.mapping,
        run_id: &epoch.id,
        source_scan: &epoch.source_scan,
        current_scan: &current_scan,
        base_projection: &apply_base,
        target: &apply_target,
        local_chunks: &LocalChunks::new(),
        final_verify: FinalVerifyMode::Strict,
    };
    let check = || {
        if cancel.load(Ordering::SeqCst) {
            Err(EngineError::Cancelled)
        } else {
            Ok(())
        }
    };
    materialize::prepare(&request, &check)?;
    let mut journal = |intent: &super::apply_types::ApplyIntent| {
        apply_index
            .lock()
            .map_err(|_| super::apply_types::ApplyError::Journal)?
            .record_engine_apply(&epoch.id, intent)
            .map_err(|_| super::apply_types::ApplyError::Journal)
    };
    let materialized =
        materialize::materialize(&request, &reader, &mut journal, cancel.clone(), &check).await?;
    epoch.source_scan = materialized.verified_scan;
    apply_index
        .lock()
        .map_err(|_| super::index_types::IndexError::invalid_record())?
        .acknowledge_epoch(&epoch)?;
    Ok(AppendEngineReport {
        frontier: epoch.frontier,
        local_epoch: epoch.sequence,
        applied_files: materialized.applied_files,
        resumed,
    })
}
