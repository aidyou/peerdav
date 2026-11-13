use peerdav_lib::sync::{
    append_discovery::discover,
    append_index::AppendIndex,
    append_types::{DiscoveryState, TrustBinding, TrustSource},
};
use zeroize::Zeroizing;

async fn discovery_fixture() -> (Arc<Fake>, AppendRepository, TrustBinding) {
    let f = Arc::new(Fake::default());
    let (object, mut binding) =
        AppendRepository::prepare_anchor(descriptor("00112233445566778899aabbccddeeff"), b"p")
            .unwrap();
    binding.source = TrustSource::TrustedFingerprint;
    f.put(object.key().into(), object.bytes().to_vec());
    let (repo, _) = AppendRepository::open_selected(f.clone(), binding.clone(), b"p")
        .await
        .unwrap();
    (f, repo, binding)
}
fn discovery_index(path: &std::path::Path, binding: &TrustBinding) -> AppendIndex {
    AppendIndex::open(path, Zeroizing::new([7; 32]), binding.repo_id.clone()).unwrap()
}
fn commit_key(r: &AppendRepository, c: &CommitRef) -> String {
    format!("_peerdav_encrypt/{}/commits/{}-{}", r.repo_id(), c.operation_id, c.digest)
}

#[tokio::test]
async fn linking_reads_metadata_only_and_never_records_full_validation() {
    use peerdav_lib::sync::append_discovery::{discover_metadata, discover_roots};
    use peerdav_lib::sync::append_types::{BlockBinding, APPEND_ROOT};
    let (f, r, binding) = discovery_fixture().await;
    let mut tree = empty_tree(&r);
    tree.bindings.push(BlockBinding {
        block_id: "ab".repeat(32),
        len: 1024,
        object: ObjectRef { kind: "block".into(), digest: "cd".repeat(32) },
    });
    let tree_ref = put_tree(&f, &r, &tree);
    let reference = put_commit(&f, &r, &commit(&r, "00000000000000000000000000000001", vec![], tree_ref.clone()));
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    assert!(discover_roots(f.clone(), &binding, b"wrong", &mut index, "wrong").await.is_err());
    let before = f.gets.load(std::sync::atomic::Ordering::SeqCst);
    assert!(discover_roots(f.clone(), &binding, b"p", &mut index, "link").await.unwrap().is_empty());
    assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst) - before, 3);
    let record = index.records().unwrap().into_iter().find(|record| record.key == commit_key(&r, &reference)).unwrap();
    assert_eq!(record.state, DiscoveryState::Observed);
    let before = f.gets.load(std::sync::atomic::Ordering::SeqCst);
    let metadata = discover_metadata(f.clone(), &binding, b"p", &mut index, "preview-metadata").await.unwrap();
    assert_eq!(metadata.trees().len(), 1);
    assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst) - before, 3);
    assert!(index.records().unwrap().iter().all(|record| record.state == DiscoveryState::Observed));
    // Retries retain cumulative reservations even for metadata-only discovery.
    let budget = index.discovery_budget("preview-metadata").unwrap();
    discover_metadata(f.clone(), &binding, b"p", &mut index, "preview-metadata").await.unwrap();
    assert_eq!(index.discovery_budget("preview-metadata").unwrap().requests, budget.requests * 2);
    assert_eq!(index.discovery_budget("preview-metadata").unwrap().allocation_bytes, budget.allocation_bytes * 2);
    // The same authenticated metadata is not enough to apply files: full sync
    // still requires the missing block, and all original budgets remain active.
    assert!(matches!(discover(f.clone(), &binding, b"p", &mut index, "sync").await,
        Err(AppendError::PendingDependency(CloudError::NotFound))));
    f.objects.lock().unwrap().remove(&format!("{APPEND_ROOT}/{}/trees/{}", r.repo_id(), tree_ref.digest));
    assert!(matches!(discover_roots(f, &binding, b"p", &mut index, "missing-tree").await,
        Err(AppendError::PendingDependency(CloudError::NotFound))));
}

#[tokio::test]
async fn durable_reservations_precede_every_discovery_get_and_failure_issues_zero_gets() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = discovery_index(&path, &binding);
    *f.reservation_probe.lock().unwrap() = Some((path.clone(), binding.clone(), "probe".into(), 0));
    discover(f.clone(), &binding, b"p", &mut index, "probe")
        .await
        .unwrap();
    let before = index.discovery_budget("probe").unwrap();
    assert_eq!(before.pages, 1);
    assert_eq!(before.requests, 7); // three HEAD + GET pairs and LIST
    assert_eq!(
        before.bytes_read,
        f.objects
            .lock()
            .unwrap()
            .values()
            .map(Vec::len)
            .sum::<usize>()
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_budget BEFORE UPDATE ON append_records WHEN NEW.kind='discovery-budget' BEGIN SELECT RAISE(FAIL,'discovery reservation fault'); END;").unwrap();
    let gets = f.gets.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        discover(f.clone(), &binding, b"p", &mut index, "probe")
            .await
            .is_err()
    );
    assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst), gets);
    assert_eq!(
        index.discovery_budget("probe").unwrap().bytes_read,
        before.bytes_read
    );
    connection
        .execute_batch("DROP TRIGGER fail_budget")
        .unwrap();
    *f.get_error.lock().unwrap() = true;
    assert!(
        discover(f.clone(), &binding, b"p", &mut index, "probe")
            .await
            .is_err()
    );
    drop(index);
    let index = discovery_index(&path, &binding);
    assert_eq!(
        index.discovery_budget("probe").unwrap().bytes_read,
        before.bytes_read + f.objects.lock().unwrap()[&binding.key].len()
    );
    assert_eq!(index.discovery_budget("probe").unwrap().pages, 1);
}

#[tokio::test]
async fn stat_changes_fail_closed_and_failed_attempts_survive_restart() {
    let (f, _, binding) = discovery_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = discovery_index(&path, &binding);
    let anchor_len = f.objects.lock().unwrap()[&binding.key].len();

    // An unavailable HEAD consumes a request, but must never issue a GET.
    *f.stat_error.lock().unwrap() = true;
    assert!(
        discover(f.clone(), &binding, b"p", &mut index, "changing")
            .await
            .is_err()
    );
    assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst), 1); // fixture open only
    assert_eq!(index.discovery_budget("changing").unwrap().requests, 1);
    drop(index);
    let mut index = discovery_index(&path, &binding);
    *f.stat_error.lock().unwrap() = false;
    *f.declared_delta.lock().unwrap() = -1;
    assert!(matches!(
        discover(f.clone(), &binding, b"p", &mut index, "changing").await,
        Err(AppendError::ResourceLimit("declared object length"))
    ));
    assert_eq!(index.discovery_budget("changing").unwrap().requests, 3);
    assert_eq!(
        index.discovery_budget("changing").unwrap().bytes_read,
        anchor_len - 1
    );
    drop(index);
    let mut index = discovery_index(&path, &binding);
    // Shrinking between HEAD and GET is safe; keep the full declared charge.
    *f.declared_delta.lock().unwrap() = 1;
    discover(f.clone(), &binding, b"p", &mut index, "changing")
        .await
        .unwrap();
    let budget = index.discovery_budget("changing").unwrap();
    assert_eq!(budget.requests, 6); // HEAD + GET + empty LIST
    assert_eq!(budget.bytes_read, 2 * anchor_len);
}

#[tokio::test]
async fn get_reservation_failure_and_stat_failures_issue_zero_gets() {
    let (f, _, binding) = discovery_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = discovery_index(&path, &binding);
    let connection = rusqlite::Connection::open(&path).unwrap();
    // HEAD reservation inserts the row; GET reservation updates it and fails.
    connection.execute_batch("CREATE TRIGGER fail_get_budget BEFORE UPDATE ON append_records WHEN NEW.kind='discovery-budget' BEGIN SELECT RAISE(FAIL,'GET reservation fault'); END;").unwrap();
    let gets = f.gets.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        discover(f.clone(), &binding, b"p", &mut index, "atomic")
            .await
            .is_err()
    );
    assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst), gets);
    assert_eq!(index.discovery_budget("atomic").unwrap().requests, 1);
    assert_eq!(index.discovery_budget("atomic").unwrap().bytes_read, 0);
    connection
        .execute_batch("DROP TRIGGER fail_get_budget")
        .unwrap();
    // Further stat failures retain their charges after a fresh open.
    for _ in 0..2 {
        *f.stat_error.lock().unwrap() = true;
        assert!(
            discover(f.clone(), &binding, b"p", &mut index, "atomic")
                .await
                .is_err()
        );
    }
    drop(index);
    let index = discovery_index(&path, &binding);
    assert_eq!(index.discovery_budget("atomic").unwrap().requests, 3);
    assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst), gets);
}

#[tokio::test]
async fn coordinator_reopens_and_gets_known_omitted_by_list_with_cumulative_budget() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree),
    );
    let key = commit_key(&r, &root);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = discovery_index(&path, &binding);
    assert_eq!(
        discover(f.clone(), &binding, b"p", &mut index, "op")
            .await
            .unwrap()
            .commits()
            .len(),
        1
    );
    let before = index.discovery_budget("op").unwrap();
    assert_eq!(
        index
            .verification(&key)
            .unwrap()
            .unwrap()
            .anchor_fingerprint,
        binding.fingerprint
    );
    drop(index);
    f.page(
        None,
        ObjectPage {
            objects: vec![],
            next_token: None,
        },
    );
    let mut index = discovery_index(&path, &binding);
    assert_eq!(
        discover(f.clone(), &binding, b"p", &mut index, "op")
            .await
            .unwrap()
            .commits()
            .len(),
        1
    );
    let after = index.discovery_budget("op").unwrap();
    assert!(after.requests > before.requests && after.pages == before.pages + 1);
    assert!(
        after.bytes_read > before.bytes_read && after.allocation_bytes > before.allocation_bytes
    );
    f.objects.lock().unwrap().remove(&key);
    assert!(matches!(
        discover(f, &binding, b"p", &mut index, "op").await,
        Err(AppendError::PendingDependency(_))
    ));
    assert_eq!(
        index.load(&key).unwrap().unwrap().state,
        DiscoveryState::Validated
    );
}

#[tokio::test]
async fn coordinator_missing_parent_is_pending_then_recovers() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let parent = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree.clone()),
    );
    let parent_key = commit_key(&r, &parent);
    let bytes = f.objects.lock().unwrap().remove(&parent_key).unwrap();
    let child = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000002", vec![parent], tree),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    assert!(matches!(
        discover(f.clone(), &binding, b"p", &mut index, "op").await,
        Err(AppendError::PendingDependency(_))
    ));
    assert_eq!(
        index.load(&commit_key(&r, &child)).unwrap().unwrap().state,
        DiscoveryState::Pending
    );
    f.put(parent_key, bytes);
    assert_eq!(
        discover(f, &binding, b"p", &mut index, "op")
            .await
            .unwrap()
            .commits()
            .len(),
        2
    );
}

#[tokio::test]
async fn coordinator_page_failure_preserves_known_and_never_completes() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree),
    );
    let key = commit_key(&r, &root);
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    discover(f.clone(), &binding, b"p", &mut index, "op")
        .await
        .unwrap();
    f.page(
        None,
        ObjectPage {
            objects: vec![],
            next_token: Some("next".into()),
        },
    );
    f.fail_page(Some("next".into()));
    assert!(matches!(
        discover(f, &binding, b"p", &mut index, "op").await,
        Err(AppendError::IncompleteListing(_))
    ));
    assert_eq!(
        index.load(&key).unwrap().unwrap().state,
        DiscoveryState::Validated
    );
}

#[tokio::test]
async fn coordinator_different_anchor_never_reuses_validated_marker() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree),
    );
    let key = commit_key(&r, &root);
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    discover(f.clone(), &binding, b"p", &mut index, "op")
        .await
        .unwrap();
    let old = index.verification(&key).unwrap();
    let (object, mut other) =
        AppendRepository::prepare_anchor(descriptor(r.repo_id()), b"p").unwrap();
    other.source = TrustSource::TrustedFingerprint;
    f.put(object.key().into(), object.bytes().to_vec());
    assert!(discover(f, &other, b"p", &mut index, "op2").await.is_err());
    assert_eq!(index.verification(&key).unwrap(), old);
    let mut mismatched = binding.clone();
    mismatched.repo_id = "ffffffffffffffffffffffffffffffff".into();
    assert!(
        discover(
            Arc::new(Fake::default()),
            &mismatched,
            b"p",
            &mut index,
            "op3"
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn coordinator_rejects_history_missing_from_any_parent() {
    let (f, r, binding) = discovery_fixture().await;
    let empty = put_tree(&f, &r, &empty_tree(&r));
    let left = put_commit(
        &f,
        &r,
        &commit(
            &r,
            "00000000000000000000000000000001",
            vec![],
            empty.clone(),
        ),
    );
    let mut full = empty_tree(&r);
    full.entries.push(revision());
    let full = put_tree(&f, &r, &full);
    let right = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000002", vec![], full),
    );
    put_commit(
        &f,
        &r,
        &commit(
            &r,
            "00000000000000000000000000000003",
            vec![left, right],
            empty,
        ),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    assert!(matches!(
        discover(f, &binding, b"p", &mut index, "op").await,
        Err(AppendError::Invalid("commit history"))
    ));
}

#[tokio::test]
async fn coordinator_changed_bytes_at_same_key_preserves_validated_record_and_context() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree.clone()),
    );
    let key = commit_key(&r, &root);
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    discover(f.clone(), &binding, b"p", &mut index, "op")
        .await
        .unwrap();
    let old_record = index.load(&key).unwrap();
    let old_context = index.verification(&key).unwrap();
    let (_, different) = r
        .seal_commit(&commit(
            &r,
            "00000000000000000000000000000002",
            vec![],
            tree,
        ))
        .unwrap();
    f.put(key.clone(), different);
    assert!(discover(f, &binding, b"p", &mut index, "op").await.is_err());
    assert_eq!(index.load(&key).unwrap(), old_record);
    assert_eq!(index.verification(&key).unwrap(), old_context);
}

use bytes::Bytes;
use futures_util::{future::BoxFuture, stream};
use peerdav_lib::cloud::{
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectPage, ObjectStream, PutCondition,
        StorageCapabilities,
    },
};
use peerdav_lib::sync::{
    append_discovery::{DiscoveryBudget, list_candidates, validate_closure},
    append_repository::AppendRepository,
    append_types::{
        AppendError, AppendMode, Chunker, Commit, CommitRef, Descriptor, DiscoveryLimits,
        ObjectRef, Tree,
    },
};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{Arc, Mutex},
};

use peerdav_lib::sync::{
    append_discovery::validate_closure_with_limits,
    merge,
    merge_types::{ChangeId, EntityId, LocalChange, Revision, RevisionValue},
};

fn revision() -> Revision {
    merge::revision(LocalChange {
        entity: EntityId("entity".into()),
        change_id: ChangeId("change".into()),
        parents: vec![],
        value: RevisionValue::Tombstone,
    })
    .unwrap()
}

#[derive(Clone, Default)]
struct Fake {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    pages: Arc<Mutex<BTreeMap<Option<String>, ObjectPage>>>,
    page_errors: Arc<Mutex<BTreeMap<Option<String>, bool>>>,
    get_error: Arc<Mutex<bool>>,
    stat_error: Arc<Mutex<bool>>,
    declared_delta: Arc<Mutex<i64>>,
    reservation_probe: Arc<Mutex<Option<(std::path::PathBuf, TrustBinding, String, usize)>>>,
    gets: Arc<std::sync::atomic::AtomicUsize>,
}
impl Fake {
    fn put(&self, key: String, bytes: Vec<u8>) {
        self.objects.lock().unwrap().insert(key, bytes);
    }
    fn page(&self, token: Option<String>, page: ObjectPage) {
        self.pages.lock().unwrap().insert(token, page);
    }
    fn fail_page(&self, token: Option<String>) {
        self.page_errors.lock().unwrap().insert(token, true);
    }
}
fn meta(key: &str, bytes: &[u8]) -> ObjectMetadata {
    ObjectMetadata {
        key: key.into(),
        size: bytes.len() as u64,
        etag: None,
    }
}
impl ObjectStore for Fake {
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::default()
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        let xs = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| Ok(meta(k, v)))
            .collect::<Vec<_>>();
        Box::pin(stream::iter(xs))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            if *self.stat_error.lock().unwrap() {
                return Err(CloudError::Unsupported);
            }
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .map(|v| {
                    let mut m = meta(key, v);
                    m.size = m
                        .size
                        .saturating_add_signed(*self.declared_delta.lock().unwrap());
                    m
                })
                .ok_or(CloudError::NotFound)
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        _: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some((path, binding, operation, total)) =
            &mut *self.reservation_probe.lock().unwrap()
        {
            *total += self.objects.lock().unwrap().get(key).unwrap().len();
            let durable = discovery_index(path, binding)
                .discovery_budget(operation)
                .unwrap();
            assert_eq!(
                durable.bytes_read, *total,
                "reservation must be visible before GET {key}"
            );
        }
        let e = *self.get_error.lock().unwrap();
        let v = self.objects.lock().unwrap().get(key).cloned();
        Box::pin(async move {
            if e {
                return Err(CloudError::Unsupported);
            }
            v.map(|x| Box::pin(stream::iter(vec![Ok(Bytes::from(x))])) as ObjectStream)
                .ok_or(CloudError::NotFound)
        })
    }
    fn put<'a>(
        &'a self,
        _: &'a str,
        _: ObjectStream,
        _: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }
    fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }
    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        let t = token.map(str::to_owned);
        let failed = self
            .page_errors
            .lock()
            .unwrap()
            .get(&t)
            .copied()
            .unwrap_or(false);
        let custom = self.pages.lock().unwrap().get(&t).cloned();
        let mut os = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| meta(k, v))
            .collect::<Vec<_>>();
        os.sort_by(|a, b| a.key.cmp(&b.key));
        Box::pin(async move {
            if failed {
                return Err(CloudError::Unsupported);
            }
            Ok(custom.unwrap_or(ObjectPage {
                objects: os,
                next_token: None,
            }))
        })
    }
}
fn descriptor(id: &str) -> Descriptor {
    Descriptor {
        version: 2,
        repo_id: id.into(),
        mode: AppendMode::AuthenticatedPlain,
        chunker: Chunker {
            name: "fastcdc".into(),
            version: "5.0.0".into(),
            normalization: "level1".into(),
            level: 1,
            min: 1_048_576,
            average: 4_194_304,
            max: 8_388_608,
            seed: 0,
        },
    }
}
async fn repo() -> (Arc<Fake>, AppendRepository) {
    let f = Arc::new(Fake::default());
    let (a, _) =
        AppendRepository::create_anchor(descriptor("00112233445566778899aabbccddeeff"), b"p")
            .unwrap();
    let r = AppendRepository::new(f.clone(), a, b"p").unwrap();
    (f, r)
}
fn empty_tree(r: &AppendRepository) -> Tree {
    Tree {
        version: 2,
        repo_id: r.repo_id().into(),
        directory: "root".into(),
        entries: vec![],
        children: vec![],
        bindings: vec![],
    }
}
fn put_commit(f: &Fake, r: &AppendRepository, c: &Commit) -> CommitRef {
    let (cr, cb) = r.seal_commit(c).unwrap();
    f.put(
        format!(
            "_peerdav_encrypt/{}/commits/{}-{}",
            r.repo_id(),
            cr.operation_id,
            cr.digest
        ),
        cb,
    );
    cr
}
fn put_tree(f: &Fake, r: &AppendRepository, t: &Tree) -> ObjectRef {
    let (tr, tb) = r.seal_tree(t).unwrap();
    f.put(format!("_peerdav_encrypt/{}/trees/{}", r.repo_id(), tr.digest), tb);
    tr
}
fn commit(r: &AppendRepository, id: &str, parents: Vec<CommitRef>, root: ObjectRef) -> Commit {
    Commit {
        version: 2,
        repo_id: r.repo_id().into(),
        operation_id: id.into(),
        parents,
        root,
    }
}

#[tokio::test]
async fn authenticated_diamond_closure_succeeds() {
    let (f, r) = repo().await;
    let p = put_tree(&f, &r, &empty_tree(&r));
    let pc = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], p.clone()),
    );
    let l = put_commit(
        &f,
        &r,
        &commit(
            &r,
            "00000000000000000000000000000002",
            vec![pc.clone()],
            p.clone(),
        ),
    );
    let rr = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000003", vec![pc], p.clone()),
    );
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000004", vec![l, rr], p),
    );
    let c = validate_closure(
        f,
        r.repo_id(),
        &r,
        vec![root],
        &mut DiscoveryBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(c.commits().len(), 4);
}
#[tokio::test]
async fn child_snapshot_missing_parent_revision_is_rejected() {
    let (f, r) = repo().await;
    let mut parent_tree = empty_tree(&r);
    parent_tree.entries.push(revision());
    let t = put_tree(&f, &r, &parent_tree);
    let parent = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], t),
    );
    let empty = put_tree(&f, &r, &empty_tree(&r));
    let child = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000002", vec![parent], empty),
    );
    assert!(matches!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![child],
            &mut DiscoveryBudget::default()
        )
        .await,
        Err(AppendError::Invalid("commit history"))
    ));
}
#[tokio::test]
async fn second_page_error_is_propagated() {
    let f = Fake::default();
    f.page(
        None,
        ObjectPage {
            objects: vec![],
            next_token: Some("next".into()),
        },
    );
    f.fail_page(Some("next".into()));
    let mut b = DiscoveryBudget::default();
    assert!(matches!(
        list_candidates(&f, "r", &mut b).await,
        Err(peerdav_lib::sync::append_types::AppendError::Storage(
            CloudError::Unsupported
        ))
    ));
}
#[tokio::test]
async fn empty_page_token_continues_successfully() {
    let f = Fake::default();
    f.page(
        None,
        ObjectPage {
            objects: vec![],
            next_token: Some("next".into()),
        },
    );
    f.page(
        Some("next".into()),
        ObjectPage {
            objects: vec![meta("_peerdav_encrypt/r/commits/a", &[])],
            next_token: None,
        },
    );
    let mut b = DiscoveryBudget::default();
    assert_eq!(list_candidates(&f, "r", &mut b).await.unwrap().len(), 1);
}
#[tokio::test]
async fn listing_token_cycle_is_rejected() {
    let f = Fake::default();
    f.page(
        None,
        ObjectPage {
            objects: vec![],
            next_token: Some("a".into()),
        },
    );
    f.page(
        Some("a".into()),
        ObjectPage {
            objects: vec![],
            next_token: Some("a".into()),
        },
    );
    let mut b = DiscoveryBudget::default();
    assert!(matches!(
        list_candidates(&f, "r", &mut b).await,
        Err(peerdav_lib::sync::append_types::AppendError::InvalidGraph(
            "listing token cycle"
        ))
    ));
}
#[tokio::test]
async fn reference_operation_mismatch_is_rejected() {
    let (f, r) = repo().await;
    let t = put_tree(&f, &r, &empty_tree(&r));
    let c = commit(&r, "00000000000000000000000000000001", vec![], t);
    let cr = put_commit(&f, &r, &c);
    let bad = CommitRef {
        operation_id: "00000000000000000000000000000002".into(),
        digest: cr.digest.clone(),
    };
    f.put(
        format!(
            "_peerdav_encrypt/{}/commits/{}-{}",
            r.repo_id(),
            bad.operation_id,
            bad.digest
        ),
        {
            let (_, bytes) = r.seal_commit(&c).unwrap();
            bytes
        },
    );
    assert!(matches!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![bad],
            &mut DiscoveryBudget::default()
        )
        .await,
        Err(peerdav_lib::sync::append_types::AppendError::Invalid(
            "commit operation reference"
        ))
    ));
}
#[tokio::test]
async fn request_page_and_readbytes_boundaries_are_enforced() {
    let f = Fake::default();
    let mut b = DiscoveryBudget {
        requests: 20_000,
        ..Default::default()
    };
    assert!(list_candidates(&f, "r", &mut b).await.is_err());
    let (f, r) = repo().await;
    f.get_error.lock().unwrap().clone_from(&true);
    let root = CommitRef {
        operation_id: "00000000000000000000000000000001".into(),
        digest: "a".repeat(64),
    };
    assert!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![root],
            &mut DiscoveryBudget::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn empty_roots_is_valid_empty_closure() {
    let (f, r) = repo().await;
    let c = validate_closure(f, r.repo_id(), &r, vec![], &mut DiscoveryBudget::default())
        .await
        .unwrap();
    assert!(c.commits().is_empty());
}
#[tokio::test]
async fn missing_commit_is_pending() {
    let (f, r) = repo().await;
    let root = CommitRef {
        operation_id: "00112233445566778899aabbccddeeff".into(),
        digest: "a".repeat(64),
    };
    assert!(matches!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![root],
            &mut DiscoveryBudget::default()
        )
        .await,
        Err(peerdav_lib::sync::append_types::AppendError::PendingDependency(_))
    ));
}
#[tokio::test]
async fn repository_binding_rejects_caller_repo() {
    let (f, r) = repo().await;
    assert!(matches!(
        validate_closure(
            f,
            "ffeeddccbbaa99887766554433221100",
            &r,
            vec![],
            &mut DiscoveryBudget::default()
        )
        .await,
        Err(peerdav_lib::sync::append_types::AppendError::Invalid(
            "repository binding"
        ))
    ));
}
#[tokio::test]
async fn candidate_listing_uses_page_api() {
    let f = Fake::default();
    f.put("_peerdav_encrypt/r/commits/a".into(), vec![1]);
    let mut b = DiscoveryBudget::default();
    assert_eq!(list_candidates(&f, "r", &mut b).await.unwrap().len(), 1);
    assert_eq!(b.pages, 1);
}

#[tokio::test]
async fn listing_failure_is_not_complete() {
    let f = Fake::default();
    f.fail_page(None);
    let mut b = DiscoveryBudget::default();
    assert!(list_candidates(&f, "r", &mut b).await.is_err());
}
#[tokio::test]
async fn budget_rejects_exhausted_requests() {
    let f = Fake::default();
    let mut b = DiscoveryBudget {
        requests: 20_000,
        ..Default::default()
    };
    assert!(list_candidates(&f, "r", &mut b).await.is_err());
}

#[tokio::test]
async fn page_and_request_exact_limit_and_one_over() {
    let f = Fake::default();
    for (pages, requests, expected) in [(999, 19_999, true), (1000, 0, false), (0, 20_000, false)] {
        let mut b = DiscoveryBudget {
            pages,
            requests,
            ..Default::default()
        };
        assert_eq!(list_candidates(&f, "r", &mut b).await.is_ok(), expected);
        assert_eq!(b.requests, requests + usize::from(expected));
    }
    f.fail_page(None);
    let mut b = DiscoveryBudget::default();
    assert!(list_candidates(&f, "r", &mut b).await.is_err());
    assert_eq!((b.pages, b.requests), (1, 1));
}

#[tokio::test]
async fn real_second_page_and_duplicate_raw_entries_are_charged() {
    let f = Fake::default();
    let duplicate = meta("_peerdav_encrypt/r/commits/a", &[]);
    f.page(
        None,
        ObjectPage {
            objects: vec![duplicate.clone(); 50_000],
            next_token: Some("two".into()),
        },
    );
    f.page(
        Some("two".into()),
        ObjectPage {
            objects: vec![duplicate.clone(); 50_000],
            next_token: None,
        },
    );
    let mut b = DiscoveryBudget::default();
    assert_eq!(
        list_candidates(&f, "r", &mut b).await.unwrap().len(),
        100_000
    );
    assert_eq!((b.pages, b.requests, b.raw_entries), (2, 2, 100_000));
    f.page(
        Some("two".into()),
        ObjectPage {
            objects: vec![duplicate; 50_001],
            next_token: None,
        },
    );
    let mut b = DiscoveryBudget::default();
    assert!(matches!(
        list_candidates(&f, "r", &mut b).await,
        Err(AppendError::ResourceLimit("candidates"))
    ));
    assert_eq!(b.raw_entries, 50_000); // rejected page is checked before extending
}

#[tokio::test]
async fn read_limit_exact_and_one_over_use_real_get_bytes() {
    let (f, r) = repo().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree),
    );
    let mut measured = DiscoveryBudget::default();
    validate_closure(
        f.clone(),
        r.repo_id(),
        &r,
        vec![root.clone()],
        &mut measured,
    )
    .await
    .unwrap();
    for extra in [0, 1] {
        let mut b = DiscoveryBudget {
            bytes_read: 2 * 1024 * 1024 * 1024 - measured.bytes_read + extra,
            ..Default::default()
        };
        let result = validate_closure(f.clone(), r.repo_id(), &r, vec![root.clone()], &mut b).await;
        if extra == 0 {
            assert!(result.is_ok());
            assert_eq!(b.bytes_read, 2 * 1024 * 1024 * 1024);
        } else {
            assert!(matches!(
                result,
                Err(AppendError::ResourceLimit("read bytes"))
            ));
        }
    }
}

#[tokio::test]
async fn tree_plaintext_and_metered_allocation_exact_and_one_over() {
    let (f, r) = repo().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], tree),
    );
    let mut measured = DiscoveryBudget::default();
    validate_closure(
        f.clone(),
        r.repo_id(),
        &r,
        vec![root.clone()],
        &mut measured,
    )
    .await
    .unwrap();
    assert!(measured.tree_plaintext > 0 && measured.allocation_bytes > 0);
    for allocation in [false, true] {
        for extra in [0, 1] {
            let mut limits = DiscoveryLimits::default();
            if allocation {
                limits.max_allocation = measured.allocation_bytes - extra;
            } else {
                limits.append.max_tree_total = measured.tree_plaintext - extra;
            }
            let mut b = DiscoveryBudget::default();
            let result = validate_closure_with_limits(
                f.clone(),
                r.repo_id(),
                &r,
                vec![root.clone()],
                &mut b,
                &limits,
            )
            .await;
            if extra == 0 {
                assert!(result.is_ok());
            } else if allocation {
                assert!(matches!(
                    result,
                    Err(AppendError::ResourceLimit("allocation bytes"))
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(AppendError::ResourceLimit("tree plaintext"))
                ));
            }
        }
    }
    // Test default hard bounds using ledger precharge, not a 512/256 MiB RSS allocation.
    for allocation in [false, true] {
        for extra in [0, 1] {
            let mut b = DiscoveryBudget::default();
            if allocation {
                b.allocation_bytes = 256 * 1024 * 1024 - measured.allocation_bytes + extra;
            } else {
                b.tree_plaintext = 512 * 1024 * 1024 - measured.tree_plaintext + extra;
            }
            assert_eq!(
                validate_closure(f.clone(), r.repo_id(), &r, vec![root.clone()], &mut b)
                    .await
                    .is_ok(),
                extra == 0
            );
        }
    }
}

#[tokio::test]
async fn duplicate_candidates_are_not_a_65_tip_frontier() {
    let (f, r) = repo().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let root = put_commit(&f, &r, &commit(&r, &format!("{:032x}", 1), vec![], tree));
    let mut b = DiscoveryBudget::default();
    let closure = validate_closure(f, r.repo_id(), &r, vec![root; 65], &mut b)
        .await
        .unwrap();
    assert_eq!(closure.commits().len(), 1);
    assert_eq!(b.requests, 4); // two HEAD + GET pairs, not 65 roots
}

#[tokio::test]
async fn coordinator_authenticates_100_linear_known_commits_with_one_maximal_tip() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let mut parents = vec![];
    let mut references = vec![];
    // Reverse key order forces a deep DFS as well as >64 known candidates.
    for n in (1..=100).rev() {
        let reference = put_commit(
            &f,
            &r,
            &commit(&r, &format!("{n:032x}"), parents, tree.clone()),
        );
        references.push(reference.clone());
        parents = vec![reference];
    }
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    let closure = discover(f.clone(), &binding, b"p", &mut index, "linear")
        .await
        .unwrap();
    assert_eq!(closure.commits().len(), 100);
    let parent_digests: std::collections::BTreeSet<_> = closure
        .commits()
        .values()
        .flat_map(|commit| commit.parents.iter().map(|parent| &parent.digest))
        .collect();
    assert_eq!(
        closure
            .commits()
            .keys()
            .filter(|digest| !parent_digests.contains(digest))
            .count(),
        1
    );
    for reference in &references {
        let key = commit_key(&r, reference);
        assert_eq!(
            index.load(&key).unwrap().unwrap().state,
            DiscoveryState::Validated
        );
        assert!(index.verification(&key).unwrap().is_some());
    }
    // Reauthenticate every persisted known entry even when listing omits all.
    f.page(
        None,
        ObjectPage {
            objects: vec![],
            next_token: None,
        },
    );
    let before = index.discovery_budget("linear").unwrap();
    assert_eq!(
        discover(f, &binding, b"p", &mut index, "linear")
            .await
            .unwrap()
            .commits()
            .len(),
        100
    );
    assert_eq!(
        index.discovery_budget("linear").unwrap().requests - before.requests,
        403 // 201 HEAD + GET pairs and one real LIST page
    );
}

#[tokio::test]
async fn coordinator_rejects_65_authenticated_concurrent_tips_without_eviction_or_publication() {
    let (f, r, binding) = discovery_fixture().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let mut references = vec![];
    for n in 1..=65 {
        references.push(put_commit(
            &f,
            &r,
            &commit(&r, &format!("{n:032x}"), vec![], tree.clone()),
        ));
    }
    let original_objects = f.objects.lock().unwrap().clone();
    let dir = tempfile::tempdir().unwrap();
    let mut index = discovery_index(&dir.path().join("index.db"), &binding);
    assert!(matches!(
        discover(f.clone(), &binding, b"p", &mut index, "concurrent").await,
        Err(AppendError::ResourceLimit("frontier"))
    ));
    assert_eq!(index.discovery_budget("concurrent").unwrap().requests, 263);
    for reference in references {
        let key = commit_key(&r, &reference);
        assert!(index.load(&key).unwrap().is_some());
        assert!(index.verification(&key).unwrap().is_none());
    }
    assert!(index.load_operation("concurrent").unwrap().is_none());
    assert_eq!(*f.objects.lock().unwrap(), original_objects);
}

#[tokio::test]
async fn candidate_hard_limit_remains_100000_before_get() {
    use peerdav_lib::sync::append_types::MAX_DISCOVERY_NODES;
    assert_eq!(MAX_DISCOVERY_NODES, 100_000);
    let (f, r) = repo().await;
    let root = CommitRef {
        operation_id: "0".repeat(32),
        digest: "a".repeat(64),
    };
    let mut budget = DiscoveryBudget::default();
    assert!(matches!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![root; MAX_DISCOVERY_NODES + 1],
            &mut budget
        )
        .await,
        Err(AppendError::ResourceLimit("roots"))
    ));
    assert_eq!(budget.requests, 0);
}

#[tokio::test]
async fn pending_dfs_work_above_64_is_not_a_frontier_limit() {
    let (f, r) = repo().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let mut parents = vec![];
    for n in 1..=64 {
        let mut ancestors = vec![];
        for id in [2 * n - 1, 2 * n] {
            ancestors.push(put_commit(
                &f,
                &r,
                &commit(&r, &format!("{id:032x}"), vec![], tree.clone()),
            ));
        }
        parents.push(put_commit(
            &f,
            &r,
            &commit(&r, &format!("{:032x}", n + 128), ancestors, tree.clone()),
        ));
    }
    parents.sort_by(|a, b| (&a.operation_id, &a.digest).cmp(&(&b.operation_id, &b.digest)));
    // The first parent's dependency is scheduled while 63 siblings are pending.
    let tip = put_commit(
        &f,
        &r,
        &commit(&r, &format!("{:032x}", 193), parents.clone(), tree),
    );
    let closure = validate_closure(
        f.clone(),
        r.repo_id(),
        &r,
        vec![tip],
        &mut DiscoveryBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(closure.commits().len(), 193);
    // Exactly 64 authenticated maximal tips remains legal.
    let closure = validate_closure(f, r.repo_id(), &r, parents, &mut DiscoveryBudget::default())
        .await
        .unwrap();
    assert_eq!(closure.commits().len(), 192);
}

#[tokio::test]
async fn small_depth_limit_rejects_real_parent_chain() {
    let (f, r) = repo().await;
    let tree = put_tree(&f, &r, &empty_tree(&r));
    let mut parents = vec![];
    for n in 1..=4 {
        parents = vec![put_commit(
            &f,
            &r,
            &commit(&r, &format!("{n:032x}"), parents, tree.clone()),
        )];
    }
    let mut limits = DiscoveryLimits::default();
    limits.append.max_depth = 3;
    validate_closure_with_limits(
        f.clone(),
        r.repo_id(),
        &r,
        parents.clone(),
        &mut DiscoveryBudget::default(),
        &limits,
    )
    .await
    .unwrap();
    limits.append.max_depth = 2;
    assert!(matches!(
        validate_closure_with_limits(
            f,
            r.repo_id(),
            &r,
            parents,
            &mut DiscoveryBudget::default(),
            &limits
        )
        .await,
        Err(AppendError::InvalidGraph("depth"))
    ));
}

#[tokio::test]
async fn maximum_legal_blocks_and_shared_history_fit_discovery_budget() {
    use peerdav_lib::sync::append_types::{BlockBinding, MAX_BLOCK_SIZE, MAX_DISCOVERY_ALLOCATION};
    use sha2::{Digest, Sha256};
    for mode in [AppendMode::Encrypted, AppendMode::AuthenticatedPlain] {
        let f = Arc::new(Fake::default());
        let mut desc = descriptor("00112233445566778899aabbccddeeff");
        desc.mode = mode;
        let (anchor, _) = AppendRepository::create_anchor(desc, b"p").unwrap();
        let r = AppendRepository::new(f.clone(), anchor, b"p").unwrap();
        let (key, bytes) = r.seal_block(&vec![7; MAX_BLOCK_SIZE]).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));
        let block_id = r.block_id(&vec![7; MAX_BLOCK_SIZE]).unwrap();
        f.put(key, bytes);
        let mut tree = empty_tree(&r);
        tree.bindings.push(BlockBinding {
            block_id,
            len: MAX_BLOCK_SIZE as u64,
            object: ObjectRef { kind: "block".into(), digest },
        });
        let tree_ref = put_tree(&f, &r, &tree);
        let parent = put_commit(&f, &r, &commit(&r, "00000000000000000000000000000001", vec![], tree_ref.clone()));
        let child = put_commit(&f, &r, &commit(&r, "00000000000000000000000000000002", vec![parent], tree_ref));
        let before = f.gets.load(std::sync::atomic::Ordering::SeqCst);
        let mut budget = DiscoveryBudget::default();
        let result = validate_closure(f.clone(), r.repo_id(), &r, vec![child], &mut budget).await;
        assert!(result.is_ok(), "legal block failed: {result:?}");
        // Each commit/tree is authenticated; the shared file block only once.
        assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst) - before, 5);
        assert!(budget.allocation_bytes < MAX_DISCOVERY_ALLOCATION);
    }
}

#[tokio::test]
async fn block_workspace_boundary_and_reauthentication_remain_enforced() {
    use peerdav_lib::sync::append_types::BlockBinding;
    use sha2::{Digest, Sha256};
    let (f, r) = repo().await;
    let (key, bytes) = r.seal_block(&vec![9; 65536]).unwrap();
    let workspace = bytes.len() * 8 + 4096;
    let mut tree = empty_tree(&r);
    tree.bindings.push(BlockBinding {
        block_id: r.block_id(&vec![9; 65536]).unwrap(),
        len: 65536,
        object: ObjectRef { kind: "block".into(), digest: hex::encode(Sha256::digest(&bytes)) },
    });
    f.put(key.clone(), bytes.clone());
    let tree_ref = put_tree(&f, &r, &tree);
    let root = put_commit(&f, &r, &commit(&r, "00000000000000000000000000000001", vec![], tree_ref));
    for extra in [0, 1] {
        let limits = DiscoveryLimits { max_allocation: workspace - extra, ..Default::default() };
        let mut budget = DiscoveryBudget::default();
        let before = f.gets.load(std::sync::atomic::Ordering::SeqCst);
        let result = validate_closure_with_limits(f.clone(), r.repo_id(), &r, vec![root.clone()], &mut budget, &limits).await;
        if extra == 0 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(AppendError::ResourceLimit("block workspace"))));
            assert_eq!(f.gets.load(std::sync::atomic::Ordering::SeqCst) - before, 2);
        }
    }
    // Successful evidence is invocation-local: corrupting the object afterwards
    // must never be hidden by a persistent or cross-run validation cache.
    let mut corrupt = bytes;
    corrupt[0] ^= 1;
    f.put(key, corrupt);
    assert!(validate_closure(f, r.repo_id(), &r, vec![root], &mut DiscoveryBudget::default()).await.is_err());
}

#[tokio::test]
async fn shared_block_digest_with_conflicting_binding_is_rejected() {
    use peerdav_lib::sync::append_types::BlockBinding;
    use sha2::{Digest, Sha256};
    let (f, r) = repo().await;
    let (key, bytes) = r.seal_block(b"shared").unwrap();
    let mut tree = empty_tree(&r);
    tree.bindings.push(BlockBinding {
        block_id: r.block_id(b"shared").unwrap(),
        len: 6,
        object: ObjectRef { kind: "block".into(), digest: hex::encode(Sha256::digest(&bytes)) },
    });
    f.put(key, bytes);
    let root_tree = put_tree(&f, &r, &tree);
    let parent = put_commit(&f, &r, &commit(&r, "00000000000000000000000000000001", vec![], root_tree));
    tree.bindings[0].len = 5;
    let conflict = put_tree(&f, &r, &tree);
    let child = put_commit(&f, &r, &commit(&r, "00000000000000000000000000000002", vec![parent.clone()], conflict));
    assert!(matches!(validate_closure(f, r.repo_id(), &r, vec![parent, child], &mut DiscoveryBudget::default()).await,
        Err(AppendError::Invalid("binding mismatch"))));
}

#[tokio::test]
async fn missing_tree_and_block_preserve_pending_source_and_failed_get_charge() {
    use peerdav_lib::sync::append_types::{BlockBinding, DirectoryRef};
    let (f, r) = repo().await;
    let missing = ObjectRef {
        kind: "tree".into(),
        digest: "a".repeat(64),
    };
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], missing),
    );
    let mut b = DiscoveryBudget::default();
    assert!(matches!(
        validate_closure(f.clone(), r.repo_id(), &r, vec![root], &mut b).await,
        Err(AppendError::PendingDependency(CloudError::NotFound))
    ));
    assert_eq!(b.requests, 3); // commit HEAD + GET, missing tree HEAD

    let (_, bytes) = r.seal_block(b"block").unwrap();
    let envelope: peerdav_lib::sync::append_types::Envelope =
        peerdav_lib::sync::append_types::Envelope::from_wire(&bytes).unwrap();
    let block_id = envelope.logical_id;
    use sha2::Digest;
    let digest = hex::encode(sha2::Sha256::digest(&bytes));
    let mut child = empty_tree(&r);
    child.directory = "child".into();
    child.bindings.push(BlockBinding {
        block_id,
        len: 5,
        object: ObjectRef {
            kind: "block".into(),
            digest: digest.clone(),
        },
    });
    let child_ref = put_tree(&f, &r, &child);
    let mut tree = empty_tree(&r);
    tree.children.push(DirectoryRef {
        directory: "child".into(),
        object: child_ref,
    });
    let tree_ref = put_tree(&f, &r, &tree);
    let root = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000002", vec![], tree_ref),
    );
    let mut b = DiscoveryBudget::default();
    assert!(matches!(
        validate_closure(f.clone(), r.repo_id(), &r, vec![root.clone()], &mut b).await,
        Err(AppendError::PendingDependency(CloudError::NotFound))
    ));
    assert_eq!(b.requests, 7); // three HEAD + GET pairs, missing block HEAD
    f.put(format!("_peerdav_encrypt/{}/blocks/{digest}", r.repo_id()), bytes);
    let closure = validate_closure(
        f,
        r.repo_id(),
        &r,
        vec![root],
        &mut DiscoveryBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(closure.trees().len(), 2);
}

#[tokio::test]
async fn every_child_checks_parent_history_even_after_parent_completed() {
    let (f, r) = repo().await;
    let mut parent_tree = empty_tree(&r);
    parent_tree.entries.push(revision());
    let full = put_tree(&f, &r, &parent_tree);
    let empty = put_tree(&f, &r, &empty_tree(&r));
    let parent = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], full.clone()),
    );
    let good = put_commit(
        &f,
        &r,
        &commit(
            &r,
            "00000000000000000000000000000002",
            vec![parent.clone()],
            full,
        ),
    );
    let bad = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000003", vec![parent], empty),
    );
    assert!(matches!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![good, bad],
            &mut DiscoveryBudget::default()
        )
        .await,
        Err(AppendError::Invalid("commit history"))
    ));
}

#[tokio::test]
async fn same_revision_id_different_content_is_rejected() {
    let (f, r) = repo().await;
    let mut tree = empty_tree(&r);
    tree.entries.push(revision());
    let good = put_tree(&f, &r, &tree);
    let parent = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000001", vec![], good.clone()),
    );
    let key = format!("_peerdav_encrypt/{}/trees/{}", r.repo_id(), good.digest);
    let bytes = f.objects.lock().unwrap().get(&key).cloned().unwrap();
    let mut envelope = peerdav_lib::sync::append_types::Envelope::from_wire(&bytes).unwrap();
    tree.entries[0].change_id = ChangeId("altered".into());
    envelope.payload = serde_json::to_vec(&tree).unwrap();
    envelope.plaintext_len = envelope.payload.len() as u64;
    let bad_bytes = envelope.to_wire().unwrap();
    use sha2::Digest;
    let bad = ObjectRef {
        kind: "tree".into(),
        digest: hex::encode(sha2::Sha256::digest(&bad_bytes)),
    };
    f.put(
        format!("_peerdav_encrypt/{}/trees/{}", r.repo_id(), bad.digest),
        bad_bytes,
    );
    let child = put_commit(
        &f,
        &r,
        &commit(&r, "00000000000000000000000000000002", vec![parent], bad),
    );
    // The changed body must not pass closure validation, even with a matching wire digest.
    assert!(
        validate_closure(
            f,
            r.repo_id(),
            &r,
            vec![child],
            &mut DiscoveryBudget::default()
        )
        .await
        .is_err()
    );
}
