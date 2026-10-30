use bytes::Bytes;
use futures_util::{StreamExt, future::BoxFuture, stream};
use peerdav_lib::cloud::{
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, StorageCapabilities,
    },
};
use peerdav_lib::sync::{
    engine,
    index::SyncIndex,
    publisher_types::PublishControl,
    repository::Repository,
    repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ops::Range,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tempfile::{TempDir, tempdir};

#[derive(Default)]
struct Store {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    block_puts: AtomicU64,
    block_get_bytes: AtomicU64,
    /// Optional cancellation hook invoked after a non-initial HEAD replacement.
    on_head: Mutex<Option<Arc<AtomicBool>>>,
}
impl Store {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn generation(&self) -> u64 {
        let bytes = self
            .objects
            .lock()
            .unwrap()
            .get("control/HEAD")
            .cloned()
            .unwrap();
        peerdav_lib::sync::repository_types::decode_head(&bytes)
            .unwrap()
            .generation
    }
    fn block_puts(&self) -> u64 {
        self.block_puts.load(Ordering::SeqCst)
    }
    fn has_prefix(&self, prefix: &str) -> bool {
        self.objects
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.starts_with(prefix))
    }
}
impl ObjectStore for Store {
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities {
            create_only: true,
            conditional_replace: true,
            range_read: true,
            conditional_writes_verified: true,
        }
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        let values = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| {
                Ok(ObjectMetadata {
                    key: key.clone(),
                    size: value.len() as u64,
                    etag: Some(hex::encode(Sha256::digest(value))),
                })
            })
            .collect::<Vec<_>>();
        Box::pin(stream::iter(values))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let objects = self.objects.lock().unwrap();
            objects
                .get(key)
                .map(|value| metadata(key, value))
                .ok_or(CloudError::NotFound)
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        _: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            let value = self
                .objects
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or(CloudError::NotFound)?;
            if key.starts_with("blocks/") {
                self.block_get_bytes
                    .fetch_add(value.len() as u64, Ordering::SeqCst);
            }
            Ok(stream::once(async move { Ok(Bytes::from(value)) }).boxed())
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let mut value = Vec::new();
            while let Some(part) = body.next().await {
                value.extend_from_slice(&part?);
            }
            let mut objects = self.objects.lock().unwrap();
            match condition {
                PutCondition::CreateOnly if objects.contains_key(key) => {
                    return Err(CloudError::Precondition);
                }
                PutCondition::MatchEtag(expected) => {
                    let current = objects.get(key).ok_or(CloudError::NotFound)?;
                    if hex::encode(Sha256::digest(current)) != expected {
                        return Err(CloudError::Precondition);
                    }
                }
                _ => {}
            }
            if key.starts_with("blocks/") {
                self.block_puts.fetch_add(1, Ordering::SeqCst);
            }
            let result = metadata(key, &value);
            objects.insert(key.to_owned(), value);
            drop(objects);
            if key == "control/HEAD" {
                let hook = self.on_head.lock().unwrap().clone();
                if let Some(cancel) = hook {
                    // The first HEAD is repository creation; only cancel after a
                    // subsequent publish has durably advanced the generation.
                    if self.generation() > 1 {
                        cancel.store(true, Ordering::Release);
                    }
                }
            }
            Ok(result)
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        })
    }
}
fn metadata(key: &str, value: &[u8]) -> ObjectMetadata {
    ObjectMetadata {
        key: key.to_owned(),
        size: value.len() as u64,
        etag: Some(hex::encode(Sha256::digest(value))),
    }
}
fn descriptor(mode: RepositoryMode) -> RepoDescriptor {
    RepoDescriptor {
        version: 1,
        repo_id: "00112233445566778899aabbccddeeff".into(),
        mode,
        chunker: ChunkerProfile {
            name: "fastcdc".into(),
            version: "5.0.0".into(),
            normalization: "v2020".into(),
            level: 1,
            min: 1_048_576,
            average: 4_194_304,
            max: 8_388_608,
            seed: 0,
        },
    }
}
async fn repository(store: Arc<Store>, mode: RepositoryMode, password: &[u8]) -> Arc<Repository> {
    match Repository::create(store.clone(), descriptor(mode), password).await {
        Ok(repository) => Arc::new(repository),
        Err(_) => Arc::new(Repository::join(store, password).await.unwrap()),
    }
}
struct Device {
    _root: TempDir,
    _private: TempDir,
    _index_dir: TempDir,
    index: SyncIndex,
}
fn device(name: &str) -> Device {
    let root = tempdir().unwrap();
    let private = tempdir().unwrap();
    let index_dir = tempdir().unwrap();
    let index_path = index_dir.path().join(format!("{name}.db"));
    let index = SyncIndex::open(
        &index_path,
        zeroize::Zeroizing::new([name.as_bytes()[0]; 32]),
        "repo".into(),
        format!("local-{name}"),
    )
    .unwrap();
    Device {
        _root: root,
        _private: private,
        _index_dir: index_dir,
        index,
    }
}
fn control() -> PublishControl {
    PublishControl {
        cancel: Arc::new(AtomicBool::new(false)),
        epoch: Arc::new(AtomicU64::new(1)),
        expected_epoch: 1,
    }
}
fn op(n: u8) -> String {
    format!("{n:032x}")
}
async fn run(
    store: Arc<Store>,
    device: &mut Device,
    mode: RepositoryMode,
    password: &[u8],
    id: &str,
) -> Result<
    peerdav_lib::sync::engine_types::EngineReport,
    peerdav_lib::sync::engine_types::EngineError,
> {
    run_with_mapping(store, device, mode, password, id, "shared").await
}
async fn run_with_mapping(
    store: Arc<Store>,
    device: &mut Device,
    mode: RepositoryMode,
    password: &[u8],
    id: &str,
    mapping: &str,
) -> Result<
    peerdav_lib::sync::engine_types::EngineReport,
    peerdav_lib::sync::engine_types::EngineError,
> {
    let repository = repository(store, mode, password).await;
    engine::run(
        repository,
        &mut device.index,
        device._root.path(),
        device._private.path(),
        mapping,
        id,
        1,
        &control(),
    )
    .await
}
async fn run_with_control(
    store: Arc<Store>,
    device: &mut Device,
    id: &str,
    control: &PublishControl,
) -> Result<
    peerdav_lib::sync::engine_types::EngineReport,
    peerdav_lib::sync::engine_types::EngineError,
> {
    let repository = repository(store, RepositoryMode::Plain, b"").await;
    engine::run(
        repository,
        &mut device.index,
        device._root.path(),
        device._private.path(),
        "shared",
        id,
        1,
        control,
    )
    .await
}

fn write_relative(root: &Path, relative: &str, value: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, value).unwrap();
}
fn read_relative(root: &Path, relative: &str) -> Vec<u8> {
    std::fs::read(root.join(relative)).unwrap()
}
fn write_file(root: &Path, value: &[u8]) {
    write_relative(root, "note.txt", value);
}
fn read_file(root: &Path) -> Vec<u8> {
    read_relative(root, "note.txt")
}
fn regular_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read(path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[tokio::test]
async fn empty_second_device_materializes_first_file() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"from-a");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(1))
        .await
        .unwrap();
    run(store, &mut b, RepositoryMode::Plain, b"", &op(2))
        .await
        .unwrap();
    assert_eq!(read_file(b._root.path()), b"from-a");
}

#[tokio::test]
async fn edit_on_second_device_syncs_back_without_conflict_double() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"old");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(3))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(4))
        .await
        .unwrap();
    write_file(b._root.path(), b"new-from-b");
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(5))
        .await
        .unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(6))
        .await
        .unwrap();
    assert_eq!(read_file(a._root.path()), b"new-from-b");
    assert!(!store.has_prefix("conflicts/"));
    let blocks = store.block_puts();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(7))
        .await
        .unwrap();
    assert_eq!(store.block_puts(), blocks);
}

#[tokio::test]
async fn unknown_empty_device_does_not_remove_remote() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"keep-me");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(8))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(9))
        .await
        .unwrap();
    assert!(store.has_prefix("commits/"));
    assert_eq!(read_file(b._root.path()), b"keep-me");
}

#[tokio::test]
async fn missing_root_errors_without_advancing_generation() {
    let store = Store::new();
    let mut a = device("a");
    write_file(a._root.path(), b"present");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(10))
        .await
        .unwrap();
    let generation = store.generation();
    std::fs::remove_dir_all(a._root.path()).unwrap();
    let error = run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(11))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::engine_types::EngineError::RootMissing(_)
    ));
    assert_eq!(store.generation(), generation);
}

#[tokio::test]
async fn known_delete_is_materialized_as_local_delete_with_recovery_content() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"recoverable");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(12))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(13))
        .await
        .unwrap();
    std::fs::remove_file(a._root.path().join("note.txt")).unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(14))
        .await
        .unwrap();
    run(store, &mut b, RepositoryMode::Plain, b"", &op(15))
        .await
        .unwrap();
    assert!(
        !b._root.path().join("note.txt").exists(),
        "remote deletion must remove the materialized target"
    );
    let records = b
        .index
        .apply_records::<peerdav_lib::sync::apply_types::ApplyIntent>()
        .unwrap();
    let deletion = records
        .iter()
        .find(|(_, intent)| intent.action == peerdav_lib::sync::apply_types::ApplyAction::Delete)
        .expect("durable deletion intent");
    assert_eq!(
        std::fs::read(&deletion.1.recovery_path).unwrap(),
        b"recoverable"
    );
}

#[tokio::test]
async fn same_repo_different_logical_folders_are_isolated() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"note-a");
    write_file(b._root.path(), b"note-b");
    run_with_mapping(
        store.clone(),
        &mut a,
        RepositoryMode::Plain,
        b"",
        &op(20),
        "documents",
    )
    .await
    .unwrap();
    run_with_mapping(
        store.clone(),
        &mut b,
        RepositoryMode::Plain,
        b"",
        &op(21),
        "pictures",
    )
    .await
    .unwrap();
    assert_eq!(read_file(a._root.path()), b"note-a");
    assert_eq!(read_file(b._root.path()), b"note-b");
    std::fs::remove_file(a._root.path().join("note.txt")).unwrap();
    run_with_mapping(
        store.clone(),
        &mut a,
        RepositoryMode::Plain,
        b"",
        &op(22),
        "documents",
    )
    .await
    .unwrap();
    run_with_mapping(
        store.clone(),
        &mut b,
        RepositoryMode::Plain,
        b"",
        &op(23),
        "pictures",
    )
    .await
    .unwrap();
    assert!(b._root.path().join("note.txt").exists());
    let mut third = device("c");
    run_with_mapping(
        store,
        &mut third,
        RepositoryMode::Plain,
        b"",
        &op(24),
        "documents",
    )
    .await
    .unwrap();
    assert!(!third._root.path().join("note.txt").exists());
}

#[tokio::test]
async fn nested_empty_directory_and_file_materialize_without_shared_wrapper() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    std::fs::create_dir_all(a._root.path().join("dir1/empty")).unwrap();
    write_relative(a._root.path(), "dir1/file", b"nested");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(25))
        .await
        .unwrap();
    run(store, &mut b, RepositoryMode::Plain, b"", &op(26))
        .await
        .unwrap();
    assert_eq!(read_relative(b._root.path(), "dir1/file"), b"nested");
    assert!(b._root.path().join("dir1/empty").is_dir());
    assert!(!b._root.path().join("shared").exists());
}

#[tokio::test]
async fn concurrent_edits_converge_to_both_files_and_are_idempotent() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_relative(a._root.path(), "baseline", b"base");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(27))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(28))
        .await
        .unwrap();
    write_relative(a._root.path(), "one", b"from-a");
    write_relative(b._root.path(), "two", b"from-b");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(29))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(30))
        .await
        .unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(31))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(32))
        .await
        .unwrap();
    let expected = regular_files(a._root.path());
    assert_eq!(expected, regular_files(b._root.path()));
    assert!(
        expected
            .iter()
            .any(|(path, value)| path == "one" && value == b"from-a")
    );
    assert!(
        expected
            .iter()
            .any(|(path, value)| path == "two" && value == b"from-b")
    );
    let count = expected.len();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(33))
        .await
        .unwrap();
    run(store, &mut b, RepositoryMode::Plain, b"", &op(34))
        .await
        .unwrap();
    assert_eq!(regular_files(a._root.path()).len(), count);
    assert_eq!(regular_files(b._root.path()).len(), count);
}

#[tokio::test]
async fn replacing_root_after_first_sync_returns_changed_error() {
    let store = Store::new();
    let mut a = device("a");
    write_file(a._root.path(), b"before");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(35))
        .await
        .unwrap();
    let old_root = std::mem::replace(&mut a._root, tempdir().unwrap());
    let replacement = old_root.path().to_path_buf();
    std::fs::rename(&replacement, replacement.with_extension("moved")).unwrap();
    let error = run(store, &mut a, RepositoryMode::Plain, b"", &op(36))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::engine_types::EngineError::Changed(_)
    ));
}

#[tokio::test]
async fn delete_vs_edit_keeps_edit_and_does_not_leave_original_note() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"original");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(40))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(41))
        .await
        .unwrap();
    std::fs::remove_file(a._root.path().join("note.txt")).unwrap();
    write_file(b._root.path(), b"new-value");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(42))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(43))
        .await
        .unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(44))
        .await
        .unwrap();
    for root in [a._root.path(), b._root.path()] {
        assert!(
            !root.join("note.txt").exists(),
            "delete wins the original path; concurrent edit must be a conflict copy"
        );
        let contents = regular_files(root)
            .into_iter()
            .map(|(_, content)| content)
            .collect::<Vec<_>>();
        assert!(
            contents
                .iter()
                .any(|content| content.as_slice() == b"new-value"),
            "concurrent edited content must remain reachable"
        );
    }
    let count = regular_files(a._root.path()).len();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(45))
        .await
        .unwrap();
    run(store, &mut b, RepositoryMode::Plain, b"", &op(46))
        .await
        .unwrap();
    assert_eq!(regular_files(a._root.path()).len(), count);
    assert_eq!(regular_files(b._root.path()).len(), count);
}

#[tokio::test]
async fn cancellation_after_head_put_keeps_materialized_baseline_pending() {
    let store = Store::new();
    let mut a = device("a");
    write_file(a._root.path(), b"before");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(47))
        .await
        .unwrap();
    let before: peerdav_lib::sync::merge_types::MergeSet =
        a.index.load("engine", "materialized").unwrap().unwrap();
    write_file(a._root.path(), b"after");
    let cancel = Arc::new(AtomicBool::new(false));
    *store.on_head.lock().unwrap() = Some(cancel.clone());
    let control = PublishControl {
        cancel,
        epoch: Arc::new(AtomicU64::new(1)),
        expected_epoch: 1,
    };
    let generation = store.generation();
    let error = run_with_control(store.clone(), &mut a, &op(48), &control)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::engine_types::EngineError::Cancelled
    ));
    assert_eq!(store.generation(), generation + 1);
    let after: peerdav_lib::sync::merge_types::MergeSet =
        a.index.load("engine", "materialized").unwrap().unwrap();
    assert_eq!(before, after);
    let journal: peerdav_lib::sync::index_types::RunJournal =
        a.index.load("run", &op(48)).unwrap().unwrap();
    assert_eq!(
        journal.phase,
        peerdav_lib::sync::index_types::RunPhase::PublishedPendingApply
    );
    assert_ne!(
        journal.phase,
        peerdav_lib::sync::index_types::RunPhase::Completed
    );
    *store.on_head.lock().unwrap() = None;
    let published_generation = store.generation();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(53))
        .await
        .unwrap();
    assert_eq!(
        store.generation(),
        published_generation,
        "restart must recover the same operation, not publish another user version"
    );
    let journal: peerdav_lib::sync::index_types::RunJournal =
        a.index.load("run", &op(48)).unwrap().unwrap();
    assert_eq!(
        journal.phase,
        peerdav_lib::sync::index_types::RunPhase::Completed
    );
    assert_eq!(read_file(a._root.path()), b"after");
    let mut b = device("b");
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(54))
        .await
        .unwrap();
    assert_eq!(read_file(b._root.path()), b"after");
}

#[tokio::test]
async fn cancelled_remote_delete_keeps_later_local_edit_and_pending_apply() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    let mut c = device("c");
    write_file(a._root.path(), b"baseline-old");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(70))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(71))
        .await
        .unwrap();
    run(store.clone(), &mut c, RepositoryMode::Plain, b"", &op(72))
        .await
        .unwrap();
    std::fs::remove_file(a._root.path().join("note.txt")).unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(73))
        .await
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    *store.on_head.lock().unwrap() = Some(cancel.clone());
    let generation = store.generation();
    let control = PublishControl {
        cancel,
        epoch: Arc::new(AtomicU64::new(1)),
        expected_epoch: 1,
    };
    let error = run_with_control(store.clone(), &mut b, &op(74), &control)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::engine_types::EngineError::Cancelled
    ));
    assert_eq!(store.generation(), generation + 1);
    assert_eq!(read_file(b._root.path()), b"baseline-old");
    write_file(b._root.path(), b"b-new-content");
    *store.on_head.lock().unwrap() = None;
    let error = run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(74))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::engine_types::EngineError::Apply(_)
    ));
    assert_eq!(read_file(b._root.path()), b"b-new-content");
    let journal: peerdav_lib::sync::index_types::RunJournal =
        b.index.load("run", &op(74)).unwrap().unwrap();
    assert_eq!(
        journal.phase,
        peerdav_lib::sync::index_types::RunPhase::PublishedPendingApply
    );
    assert!(!a._root.path().join("note.txt").exists());
    run(store.clone(), &mut c, RepositoryMode::Plain, b"", &op(75))
        .await
        .unwrap();
    assert!(!c._root.path().join("note.txt").exists());
}

#[tokio::test]
async fn cancelled_head_resume_preserves_published_snapshot_against_local_change() {
    let store = Store::new();
    let mut a = device("a");
    write_file(a._root.path(), b"before");
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(76))
        .await
        .unwrap();
    write_file(a._root.path(), b"after");
    let cancel = Arc::new(AtomicBool::new(false));
    *store.on_head.lock().unwrap() = Some(cancel.clone());
    let generation = store.generation();
    let control = PublishControl {
        cancel,
        epoch: Arc::new(AtomicU64::new(1)),
        expected_epoch: 1,
    };
    assert!(matches!(
        run_with_control(store.clone(), &mut a, &op(77), &control).await,
        Err(peerdav_lib::sync::engine_types::EngineError::Cancelled)
    ));
    assert_eq!(store.generation(), generation + 1);
    write_file(a._root.path(), b"third");
    *store.on_head.lock().unwrap() = None;
    let error = run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(77))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::engine_types::EngineError::Apply(_)
    ));
    assert_eq!(read_file(a._root.path()), b"third");
    assert_eq!(store.generation(), generation + 1);
    let journal: peerdav_lib::sync::index_types::RunJournal =
        a.index.load("run", &op(77)).unwrap().unwrap();
    assert_eq!(
        journal.phase,
        peerdav_lib::sync::index_types::RunPhase::PublishedPendingApply
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn removing_empty_directory_preserves_recovery_and_converges() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    std::fs::create_dir_all(a._root.path().join("empty")).unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(49))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(50))
        .await
        .unwrap();
    std::fs::remove_dir(a._root.path().join("empty")).unwrap();
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(51))
        .await
        .unwrap();
    run(store, &mut b, RepositoryMode::Plain, b"", &op(52))
        .await
        .unwrap();
    assert!(!b._root.path().join("empty").exists());
}

#[tokio::test]
async fn local_plaintext_chunks_reduce_apply_download_after_middle_insertion() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    let mut state = 0x6a09e667f3bcc909_u64;
    let original = (0..64 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect::<Vec<_>>();
    write_file(a._root.path(), &original);
    run(store.clone(), &mut a, RepositoryMode::Plain, b"", &op(90))
        .await
        .unwrap();
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(91))
        .await
        .unwrap();
    let mut edited = original.clone();
    edited.splice(original.len() / 2..original.len() / 2, vec![0x41; 4096]);
    write_file(b._root.path(), &edited);
    run(store.clone(), &mut b, RepositoryMode::Plain, b"", &op(92))
        .await
        .unwrap();
    let repository = repository(store.clone(), RepositoryMode::Plain, b"").await;
    let progress = |phase| {
        if phase == peerdav_lib::sync::manager_types::SyncPhase::Applying {
            store.block_get_bytes.store(0, Ordering::SeqCst);
        }
    };
    engine::run_with_progress(
        repository,
        &mut a.index,
        a._root.path(),
        a._private.path(),
        "shared",
        &op(93),
        1,
        &control(),
        &progress,
    )
    .await
    .unwrap();
    let fetched = store.block_get_bytes.load(Ordering::SeqCst);
    assert!(
        fetched < original.len() as u64 / 4,
        "apply downloaded {fetched} bytes rather than reusing at least 75% of local content"
    );
    assert_eq!(read_file(a._root.path()), edited);
}

#[tokio::test]
async fn encrypted_devices_join_same_store_with_same_password() {
    let store = Store::new();
    let mut a = device("a");
    let mut b = device("b");
    write_file(a._root.path(), b"encrypted-content");
    run(
        store.clone(),
        &mut a,
        RepositoryMode::Encrypted,
        b"shared-password",
        &op(16),
    )
    .await
    .unwrap();
    run(
        store,
        &mut b,
        RepositoryMode::Encrypted,
        b"shared-password",
        &op(17),
    )
    .await
    .unwrap();
    assert_eq!(read_file(b._root.path()), b"encrypted-content");
}
