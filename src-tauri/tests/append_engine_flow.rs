#[allow(dead_code)]
mod append_common;
use append_common::Remote;
use futures_util::{StreamExt, future::BoxFuture};
use peerdav_lib::cloud::{traits::ObjectStore, types::*};
struct Paged(append_common::FakeStore, bool, Option<Arc<AtomicBool>>);
impl ObjectStore for Paged {
    fn capabilities(&self) -> StorageCapabilities {
        self.0.capabilities()
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        self.0.list(prefix)
    }
    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move {
            assert!(token.is_none());
            let objects = self
                .0
                .list(prefix)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ObjectPage {
                objects,
                next_token: None,
            })
        })
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.0.stat(key)
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<std::ops::Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        self.0.get(key, range)
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        assert!(self.1, "pull must never PUT");
        Box::pin(async move {
            let result = self.0.put(key, body, condition).await;
            if key.contains("/commits/") {
                if let Some(cancel) = &self.2 {
                    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            result
        })
    }
    fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async { panic!("pull must never DELETE") })
    }
}
fn store(remote: &Remote) -> Arc<Paged> {
    Arc::new(Paged(remote.store(), false, None))
}
use peerdav_lib::sync::{
    append_engine::{pull, run},
    append_engine_types::{AppendEngineError, AppendPullRequest},
    append_index::AppendIndex,
    append_repository::AppendRepository,
    append_types::*,
    merge,
    merge_types::*,
};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

struct Device {
    root: tempfile::TempDir,
    private: tempfile::TempDir,
    db: tempfile::TempDir,
}
impl Device {
    fn new() -> Self {
        let device = Self {
            root: tempfile::tempdir().unwrap(),
            private: tempfile::tempdir().unwrap(),
            db: tempfile::tempdir().unwrap(),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                device.private.path(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        device
    }
    fn index(&self, binding: &TrustBinding) -> AppendIndex {
        index(&self.db.path().join("index"), binding)
    }
    fn request<'a>(&'a self, run: &'a str) -> AppendPullRequest<'a> {
        AppendPullRequest {
            root: self.root.path(),
            private: self.private.path(),
            mapping: "docs",
            run_id: run,
        }
    }
}
fn index(path: &Path, binding: &TrustBinding) -> AppendIndex {
    AppendIndex::open(
        path,
        zeroize::Zeroizing::new([7; 32]),
        binding.repo_id.clone(),
    )
    .unwrap()
}
async fn fixture() -> (Remote, AppendRepository, TrustBinding, String) {
    let remote = Remote::new(1);
    let descriptor = Descriptor {
        version: 2,
        repo_id: "00112233445566778899aabbccddeeff".into(),
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
    let (anchor, mut binding) = AppendRepository::prepare_anchor(descriptor, b"p").unwrap();
    binding.source = TrustSource::TrustedFingerprint;
    remote.put_object(anchor.key(), anchor.bytes());
    let (repo, _) = AppendRepository::open_selected(store(&remote), binding.clone(), b"p")
        .await
        .unwrap();
    let (block_key, block) = repo.seal_block(b"hello").unwrap();
    remote.put_object(&block_key, &block);
    let directory = merge::revision(LocalChange {
        entity: EntityId("docs".into()),
        change_id: ChangeId("directory".into()),
        parents: vec![],
        value: RevisionValue::Live {
            parent_dir: EntityId("root".into()),
            name: "docs".into(),
            node: Node::Dir,
        },
    })
    .unwrap();
    let file = merge::revision(LocalChange {
        entity: EntityId("file".into()),
        change_id: ChangeId("file".into()),
        parents: vec![],
        value: RevisionValue::Live {
            parent_dir: EntityId("docs".into()),
            name: "hello.txt".into(),
            node: Node::File(FileNode {
                blocks: vec![BlockRef {
                    id: repo.block_id(b"hello").unwrap(),
                    len: 5,
                }],
                length: 5,
                digest: hex::encode(Sha256::digest(b"hello")),
            }),
        },
    })
    .unwrap();
    let mut tree = Tree {
        version: 2,
        repo_id: repo.repo_id().into(),
        directory: "root".into(),
        entries: vec![directory, file],
        children: vec![],
        bindings: vec![BlockBinding {
            block_id: repo.block_id(b"hello").unwrap(),
            len: 5,
            object: ObjectRef {
                kind: "block".into(),
                digest: block_key.rsplit('/').next().unwrap().into(),
            },
        }],
    };
    peerdav_lib::sync::append_tree::canonicalize_tree(&mut tree).unwrap();
    let (root, wire) = repo.seal_tree(&tree).unwrap();
    remote.put_object(
        &format!("v2/{}/trees/{}", repo.repo_id(), root.digest),
        &wire,
    );
    let (reference, wire) = repo
        .seal_commit(&Commit {
            version: 2,
            repo_id: repo.repo_id().into(),
            operation_id: "11".repeat(16),
            parents: vec![],
            root,
        })
        .unwrap();
    remote.put_object(
        &format!(
            "v2/{}/commits/{}-{}",
            repo.repo_id(),
            reference.operation_id,
            reference.digest
        ),
        &wire,
    );
    (remote, repo, binding, block_key)
}

async fn sync(
    remote: &Remote,
    binding: &TrustBinding,
    device: &Device,
    index: &mut AppendIndex,
    id: &str,
) {
    run(
        Arc::new(Paged(remote.store(), true, None)),
        binding,
        b"p",
        index,
        &device.request(id),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
}

struct BudgetProbe {
    store: Paged,
    path: std::path::PathBuf,
    binding: TrustBinding,
    operation: String,
    before: DiscoveryBudget,
    gets: std::sync::atomic::AtomicUsize,
    block_visits: std::sync::atomic::AtomicUsize,
    interrupt: bool,
    freeze: bool,
}
impl ObjectStore for BudgetProbe {
    fn capabilities(&self) -> StorageCapabilities {
        self.store.capabilities()
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        self.store.list(prefix)
    }
    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        self.store.list_page(prefix, token)
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.store.stat(key)
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<std::ops::Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            if key.contains("/blocks/") {
                let durable = index(&self.path, &self.binding)
                    .discovery_budget(&self.operation)
                    .unwrap();
                let declared = self.store.stat(key).await?.size as usize;
                if self.freeze {
                    // First visit is discovery, second is freeze; do not infer
                    // stage from byte totals now that both use precise lengths.
                    if self
                        .block_visits
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        == 0
                    {
                        return self.store.get(key, range).await;
                    }
                    assert!(durable.requests > self.before.requests);
                    assert!(durable.bytes_read >= self.before.bytes_read + declared);
                } else {
                    assert_eq!(durable.requests, self.before.requests + 4);
                    let anchor_len = self.store.stat(&self.binding.key).await?.size as usize;
                    assert_eq!(
                        durable.bytes_read,
                        self.before.bytes_read + anchor_len + declared
                    );
                }
                self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.interrupt {
                    return std::future::pending().await;
                }
                return Err(CloudError::NotFound);
            }
            self.store.get(key, range).await
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.store.put(key, body, condition)
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        self.store.delete(key)
    }
}

#[tokio::test]
async fn apply_budget_is_durable_before_get_and_survives_dropped_future() {
    use futures_util::FutureExt;
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut idx = device.index(&binding);
    // Stage the epoch without issuing its block request.
    std::fs::remove_dir(device.private.path()).unwrap();
    assert!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut idx,
            &device.request("budget"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .is_err()
    );
    std::fs::create_dir(device.private.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            device.private.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    // A failed durable reservation must refuse the GET, not perform I/O and
    // discover the journal failure afterward.
    let connection = rusqlite::Connection::open(device.db.path().join("index")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_budget BEFORE UPDATE ON append_records WHEN NEW.kind='discovery-budget' BEGIN SELECT RAISE(FAIL,'budget reservation fault'); END;").unwrap();
    let probe = Arc::new(BudgetProbe {
        store: Paged(remote.store(), false, None),
        path: device.db.path().join("index"),
        binding: binding.clone(),
        operation: "budget".into(),
        before: idx.discovery_budget("budget").unwrap(),
        gets: std::sync::atomic::AtomicUsize::new(0),
        block_visits: std::sync::atomic::AtomicUsize::new(0),
        interrupt: false,
        freeze: false,
    });
    let error = pull(
        probe.clone(),
        &binding,
        b"p",
        &mut idx,
        &device.request("fault"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .err()
    .unwrap();
    let mut source: &dyn std::error::Error = &error;
    let mut found_fault = source.to_string().contains("budget reservation fault");
    while let Some(next) = source.source() {
        source = next;
        found_fault |= source.to_string().contains("budget reservation fault");
    }
    assert!(
        found_fault,
        "original SQLite error must be retained: {error:?}"
    );
    assert_eq!(probe.gets.load(std::sync::atomic::Ordering::SeqCst), 0);
    connection
        .execute_batch("DROP TRIGGER fail_budget")
        .unwrap();
    for interrupt in [false, true] {
        let before = idx.discovery_budget("budget").unwrap();
        let probe = Arc::new(BudgetProbe {
            store: Paged(remote.store(), false, None),
            path: device.db.path().join("index"),
            binding: binding.clone(),
            operation: "budget".into(),
            before: before.clone(),
            gets: std::sync::atomic::AtomicUsize::new(0),
            block_visits: std::sync::atomic::AtomicUsize::new(0),
            interrupt,
            freeze: false,
        });
        let result = pull(
            probe.clone(),
            &binding,
            b"p",
            &mut idx,
            &device.request("new-id-must-not-reset-budget"),
            Arc::new(AtomicBool::new(false)),
        )
        .now_or_never();
        if interrupt {
            assert!(
                result.is_none(),
                "drop while GET is pending, bypassing cleanup"
            );
        } else {
            let error = result.unwrap().err().unwrap();
            let mut source: &dyn std::error::Error = &error;
            while let Some(next) = source.source() {
                source = next;
            }
            assert_eq!(source.to_string(), CloudError::NotFound.to_string());
        }
        assert_eq!(probe.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(idx);
        idx = device.index(&binding);
        let durable = idx.discovery_budget("budget").unwrap();
        assert_eq!(durable.requests, before.requests + 4);
        let anchor_len = store(&remote).stat(&binding.key).await.unwrap().size as usize;
        let block_len = remote
            .requests()
            .iter()
            .find(|r| r.operation == "get" && r.key.contains("/blocks/"))
            .unwrap()
            .bytes
            .len();
        assert_eq!(
            durable.bytes_read,
            before.bytes_read + anchor_len + block_len
        );
        assert!(idx.active_epoch().unwrap().is_none());
    }
    pull(
        store(&remote),
        &binding,
        b"p",
        &mut idx,
        &device.request("complete"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert_eq!(idx.active_epoch().unwrap().unwrap().id, "budget");
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
}

#[tokio::test]
async fn freeze_budget_is_durable_before_get_and_survives_reopen() {
    use futures_util::FutureExt;
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut idx = device.index(&binding);
    sync(&remote, &binding, &device, &mut idx, "baseline").await;
    // A new file forces publication while retaining the authenticated old block,
    // so its first block GET belongs to freeze, before an epoch can be staged.
    std::fs::write(device.root.path().join("new.txt"), b"local change").unwrap();
    for interrupt in [false, true, false] {
        let before = idx.discovery_budget("freeze-budget").unwrap();
        let probe = Arc::new(BudgetProbe {
            store: Paged(remote.store(), true, None),
            path: device.db.path().join("index"),
            binding: binding.clone(),
            operation: "freeze-budget".into(),
            before: before.clone(),
            gets: std::sync::atomic::AtomicUsize::new(0),
            block_visits: std::sync::atomic::AtomicUsize::new(0),
            interrupt,
            freeze: true,
        });
        let result = run(
            probe.clone(),
            &binding,
            b"p",
            &mut idx,
            &device.request(if before.requests == 0 {
                "freeze-budget"
            } else {
                "new-outer-attempt"
            }),
            Arc::new(AtomicBool::new(false)),
        )
        .now_or_never();
        if interrupt {
            assert!(result.is_none(), "drop pending freeze GET without cleanup");
        } else {
            let error = result.unwrap().err().unwrap();
            let mut source: &dyn std::error::Error = &error;
            while let Some(next) = source.source() {
                source = next;
            }
            assert_eq!(source.to_string(), CloudError::NotFound.to_string());
        }
        assert_eq!(probe.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(idx);
        idx = device.index(&binding);
        let after = idx.discovery_budget("freeze-budget").unwrap();
        assert!(after.requests > before.requests);
        let block_len = remote
            .requests()
            .iter()
            .find(|r| r.operation == "get" && r.key.contains("/blocks/"))
            .unwrap()
            .bytes
            .len();
        assert!(after.bytes_read >= before.bytes_read + block_len);
        assert_eq!(idx.active_epoch().unwrap().unwrap().id, "baseline");
    }
    // Successful retry uses only declared sizes, without a second stream charge.
    let before = idx.discovery_budget("freeze-budget").unwrap();
    let request_count = remote.requests().len();
    sync(&remote, &binding, &device, &mut idx, "freeze-budget").await;
    let after = idx.discovery_budget("freeze-budget").unwrap();
    let received: usize = remote.requests()[request_count..]
        .iter()
        .filter(|r| r.operation == "get")
        .map(|r| r.bytes.len())
        .sum();
    // Publisher's existence-check GET uses its publication identity, not the
    // active discovery identity. The retained block is read once by that path.
    let block_len = remote
        .requests()
        .iter()
        .find(|r| r.operation == "get" && r.key.contains("/blocks/"))
        .unwrap()
        .bytes
        .len();
    assert_eq!(after.bytes_read - before.bytes_read, received - block_len);
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
}

#[tokio::test]
async fn completed_apply_archive_survives_reopen_and_does_not_replay() {
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    sync(&remote, &binding, &device, &mut index, "archive-one").await;
    let ids = index.recovery_record_ids("", 10).unwrap();
    assert_eq!(ids.len(), 1);
    let record = index.recovery_record(&ids[0]).unwrap().unwrap();
    assert_eq!(
        record.phase,
        peerdav_lib::sync::apply_types::ApplyPhase::Installed
    );
    drop(index);
    let mut index = device.index(&binding);
    assert_eq!(index.recovery_record_ids("", 10).unwrap(), ids);
    assert_eq!(
        serde_json::to_value(index.recovery_record(&ids[0]).unwrap().unwrap()).unwrap(),
        serde_json::to_value(record).unwrap()
    );
    sync(&remote, &binding, &device, &mut index, "archive-two").await;
    assert_eq!(index.recovery_record_ids("", 10).unwrap(), ids);
}

#[tokio::test]
async fn same_epoch_records_are_unique_and_originals_remain_queryable() {
    let (remote, _, binding, _) = fixture().await;
    let a = Device::new();
    let b = Device::new();
    let mut ai = a.index(&binding);
    let mut bi = b.index(&binding);
    sync(&remote, &binding, &a, &mut ai, "a-initial").await;
    sync(&remote, &binding, &b, &mut bi, "b-initial").await;
    std::fs::write(a.root.path().join("hello.txt"), b"changed").unwrap();
    std::fs::write(a.root.path().join("second.txt"), b"second").unwrap();
    sync(&remote, &binding, &a, &mut ai, "a-change").await;
    sync(&remote, &binding, &b, &mut bi, "b-change").await;
    drop(bi);
    let bi = b.index(&binding);
    let ids = bi.recovery_record_ids("", 100).unwrap();
    let changed = ids
        .iter()
        .filter(|id| id.starts_with("b-change:"))
        .collect::<Vec<_>>();
    assert_eq!(changed.len(), 2);
    assert_ne!(changed[0], changed[1]);
    let old = changed
        .iter()
        .map(|id| bi.recovery_record(id).unwrap().unwrap())
        .find(|record| record.relative_target == Path::new("hello.txt"))
        .unwrap();
    assert_eq!(std::fs::read(old.recovery_path).unwrap(), b"hello");
    assert!(bi.recovery_record_ids("", 0).is_err());
    assert_eq!(
        bi.recovery_record_ids(&ids[0], 1).unwrap(),
        vec![ids[1].clone()]
    );
}

#[tokio::test]
async fn unknown_b_can_add_without_inferred_remote_delete() {
    let (remote, repo, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    std::fs::write(device.root.path().join("precious"), b"local").unwrap();
    sync(&remote, &binding, &device, &mut index, "unknown-add").await;
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
    assert_eq!(
        std::fs::read(device.root.path().join("precious")).unwrap(),
        b"local"
    );
    let closure = peerdav_lib::sync::append_discovery::discover(
        store(&remote),
        &binding,
        b"p",
        &mut index,
        "inspect",
    )
    .await
    .unwrap();
    let entries = closure
        .trees()
        .values()
        .flat_map(|tree| &tree.entries)
        .collect::<Vec<_>>();
    assert!(
        entries
            .iter()
            .all(|r| !matches!(r.value, RevisionValue::Tombstone))
    );
    let local = entries
        .iter()
        .find(|r| matches!(&r.value, RevisionValue::Live { name, .. } if name == "precious"))
        .unwrap();
    assert!(local.parents.is_empty());
    assert_eq!(repo.repo_id(), binding.repo_id);
    let puts = remote
        .requests()
        .iter()
        .filter(|r| r.operation == "put")
        .count();
    sync(&remote, &binding, &device, &mut index, "steady").await;
    assert_eq!(
        remote
            .requests()
            .iter()
            .filter(|r| r.operation == "put")
            .count(),
        puts
    );
}

#[tokio::test]
async fn concurrent_local_revision_parents_only_b_not_r() {
    let (remote, _, binding, _) = fixture().await;
    let a = Device::new();
    let b = Device::new();
    let mut ia = a.index(&binding);
    let mut ib = b.index(&binding);
    sync(&remote, &binding, &a, &mut ia, "base-a").await;
    sync(&remote, &binding, &b, &mut ib, "base-b").await;
    std::fs::write(a.root.path().join("hello.txt"), b"remote edit").unwrap();
    sync(&remote, &binding, &a, &mut ia, "edit-a").await;
    std::fs::write(b.root.path().join("hello.txt"), b"local edit").unwrap();
    sync(&remote, &binding, &b, &mut ib, "edit-b").await;
    let closure = peerdav_lib::sync::append_discovery::discover(
        store(&remote),
        &binding,
        b"p",
        &mut ib,
        "inspect",
    )
    .await
    .unwrap();
    let revisions: RevisionMap = closure
        .trees()
        .values()
        .flat_map(|tree| tree.entries.iter().cloned())
        .map(|r| (r.revision_id.clone(), r))
        .collect();
    let digest = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
    let revision = |hash: String| {
        revisions
            .values()
            .find(|r| {
                matches!(&r.value,
        RevisionValue::Live { node: Node::File(file), .. } if file.digest == hash)
            })
            .unwrap()
    };
    let base = revision(digest(b"hello"));
    let remote_revision = revision(digest(b"remote edit"));
    let local = revision(digest(b"local edit"));
    assert_eq!(local.parents, vec![base.revision_id.clone()]);
    assert_eq!(remote_revision.parents, vec![base.revision_id.clone()]);
    assert!(!local.parents.contains(&remote_revision.revision_id));
    let projection = merge::project(&revisions).unwrap();
    assert_eq!(
        projection
            .entries
            .iter()
            .filter(|e| e.entity == local.entity)
            .count(),
        2
    );
    sync(&remote, &binding, &a, &mut ia, "converge").await;
    let files = |device: &Device| {
        let mut out = std::fs::read_dir(device.root.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        out.sort();
        out
    };
    assert_eq!(files(&a), files(&b));
}

#[tokio::test]
async fn run_hidden_sibling_tips_converge_without_merge_publication() {
    let (remote, _, binding, _) = fixture().await;
    let a = Device::new();
    let b = Device::new();
    let mut ia = a.index(&binding);
    let mut ib = b.index(&binding);
    sync(&remote, &binding, &a, &mut ia, "fork-base-a").await;
    sync(&remote, &binding, &b, &mut ib, "fork-base-b").await;
    let base = ia.active_epoch().unwrap().unwrap().frontier;
    assert_eq!(ib.active_epoch().unwrap().unwrap().frontier, base);
    assert_eq!(base.tips.len(), 1);
    // Both discovery and dependency GET see only B while the siblings publish.
    let base_keys = remote
        .requests()
        .into_iter()
        .filter(|r| r.operation == "get")
        .map(|r| r.key)
        .collect::<Vec<_>>();
    remote.set_visible_keys(Some(base_keys.clone()));
    remote.set_get_visible_keys(Some(base_keys));
    for (device, index, bytes, id) in [
        (&a, &mut ia, b"sibling a".as_slice(), "fork-publish-a"),
        (&b, &mut ib, b"sibling b".as_slice(), "fork-publish-b"),
    ] {
        std::fs::write(device.root.path().join("hello.txt"), bytes).unwrap();
        sync(&remote, &binding, device, index, id).await;
    }
    let a_tip = ia.active_epoch().unwrap().unwrap().frontier.tips;
    let b_tip = ib.active_epoch().unwrap().unwrap().frontier.tips;
    assert_eq!(a_tip.len(), 1);
    assert_eq!(b_tip.len(), 1);
    assert_ne!(a_tip, b_tip);
    remote.set_visible_keys(None);
    remote.set_get_visible_keys(None);
    let closure = peerdav_lib::sync::append_discovery::discover(
        store(&remote),
        &binding,
        b"p",
        &mut ia,
        "fork-inspect",
    )
    .await
    .unwrap();
    assert_eq!(closure.commits().len(), 3);
    let mut tips = closure
        .commits()
        .values()
        .filter(|commit| {
            !closure.commits().values().any(|child| {
                child
                    .parents
                    .iter()
                    .any(|parent| parent.operation_id == commit.operation_id)
            })
        })
        .map(|commit| commit.operation_id.clone())
        .collect::<Vec<_>>();
    tips.sort();
    let mut expected_tips = vec![a_tip[0].operation_id.clone(), b_tip[0].operation_id.clone()];
    expected_tips.sort();
    assert_eq!(
        tips, expected_tips,
        "remote history must really have two tips"
    );
    for commit in closure
        .commits()
        .values()
        .filter(|commit| !commit.parents.is_empty())
    {
        assert_eq!(
            commit.parents, base.tips,
            "sibling commit must parent B, not R"
        );
    }
    let revisions: RevisionMap = closure
        .trees()
        .values()
        .flat_map(|tree| tree.entries.iter().cloned())
        .map(|revision| (revision.revision_id.clone(), revision))
        .collect();
    let file_revision = |bytes: &[u8]| {
        revisions
            .values()
            .find(|revision| {
                matches!(&revision.value,
            RevisionValue::Live { node: Node::File(file), .. }
            if file.digest == hex::encode(Sha256::digest(bytes)))
            })
            .unwrap()
    };
    let original = file_revision(b"hello");
    let left = file_revision(b"sibling a");
    let right = file_revision(b"sibling b");
    assert_eq!(left.entity, original.entity);
    assert_eq!(right.entity, original.entity);
    assert_eq!(left.parents, vec![original.revision_id.clone()]);
    assert_eq!(right.parents, vec![original.revision_id.clone()]);
    assert!(!right.parents.contains(&left.revision_id));
    let projection = merge::project(&revisions).unwrap();
    let expected_files = projection
        .entries
        .iter()
        .filter_map(|entry| match &entry.value {
            RevisionValue::Live {
                node: Node::File(file),
                ..
            } => {
                let mut path = entry.projection_path.clone();
                assert_eq!(path.remove(0), "docs");
                Some((path.join("/"), file.digest.clone()))
            }
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(expected_files.len(), 2);
    assert!(expected_files.contains_key("hello.txt"));
    let requests_before = remote.requests().len();
    // Third publication-capable run has no local delta: it must only pull both tips.
    sync(&remote, &binding, &a, &mut ia, "fork-converge-a").await;
    sync(&remote, &binding, &b, &mut ib, "fork-converge-b").await;
    for (device, index) in [(&a, &mut ia), (&b, &mut ib)] {
        let actual = std::fs::read_dir(device.root.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    hex::encode(Sha256::digest(std::fs::read(entry.path()).unwrap())),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(actual, expected_files);
        let mut frontier = index
            .active_epoch()
            .unwrap()
            .unwrap()
            .frontier
            .tips
            .into_iter()
            .map(|tip| tip.operation_id)
            .collect::<Vec<_>>();
        frontier.sort();
        assert_eq!(frontier, expected_tips);
        sync(&remote, &binding, device, index, "fork-steady").await;
    }
    let requests = remote.requests();
    assert!(
        requests[requests_before..]
            .iter()
            .any(|r| r.operation == "get")
    );
    assert!(
        requests[requests_before..]
            .iter()
            .all(|r| r.operation != "put" && r.operation != "delete")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.operation == "put" && r.key.contains("/commits/"))
            .count(),
        2
    );
}

#[tokio::test]
async fn run_symlink_root_never_infers_delete_or_advances_known_baseline() {
    use std::os::unix::fs::symlink;

    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    sync(&remote, &binding, &device, &mut index, "guard-base").await;
    let baseline = index.active_epoch().unwrap().unwrap();
    assert_eq!(baseline.sequence, 1);
    assert_eq!(baseline.source_scan.files.len(), 1);
    assert_eq!(baseline.source_scan.files[0].path, "hello.txt");
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
    let saved_root = device.db.path().join("saved-root");
    let empty_root = tempfile::tempdir().unwrap();
    std::fs::rename(device.root.path(), &saved_root).unwrap();
    symlink(empty_root.path(), device.root.path()).unwrap();
    assert!(
        std::fs::symlink_metadata(device.root.path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!device.root.path().join("hello.txt").exists());
    let requests_before = remote.requests().len();
    let result = run(
        Arc::new(Paged(remote.store(), true, None)),
        &binding,
        b"p",
        &mut index,
        &device.request("guard-symlink"),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    // run opens a no-follow root capability before scan_complete. A symlink
    // therefore fails at this earlier real boundary, not as Scan::RootChanged.
    assert!(
        matches!(result,
            Err(AppendEngineError::Local(peerdav_lib::sync::engine_types::EngineError::Io(ref error)))
            if error.raw_os_error().is_some_and(|code| code == rustix::io::Errno::NOTDIR.raw_os_error() || code == rustix::io::Errno::LOOP.raw_os_error())
        ),
        "unexpected symlink-root result: {:?}",
        result.err()
    );
    assert_eq!(index.active_epoch().unwrap().unwrap(), baseline);
    let requests = remote.requests();
    assert!(
        requests[requests_before..]
            .iter()
            .all(|r| r.operation != "put" && r.operation != "delete")
    );
    drop(index);
    let mut index = device.index(&binding);
    assert_eq!(index.active_epoch().unwrap().unwrap(), baseline);
    std::fs::remove_file(device.root.path()).unwrap();
    std::fs::rename(&saved_root, device.root.path()).unwrap();
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
    // Reach the scanner too: the known file is absent and a Unix socket
    // cannot be scanned completely. The real API returns UnsafeEntry, not
    // Incomplete/RootChanged, and run exposes no ignore configuration.
    let saved_file = device.db.path().join("saved-hello");
    std::fs::rename(device.root.path().join("hello.txt"), &saved_file).unwrap();
    let socket_path = device.root.path().join("unscannable.sock");
    let socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let result = run(
        Arc::new(Paged(remote.store(), true, None)),
        &binding,
        b"p",
        &mut index,
        &device.request("guard-incomplete-scan"),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    assert!(
        matches!(result,
            Err(AppendEngineError::Scan(peerdav_lib::sync::scan_types::ScanError::UnsafeEntry(ref path)))
            if path == "unscannable.sock"
        ),
        "unexpected incomplete-scan result: {:?}",
        result.err()
    );
    assert_eq!(index.active_epoch().unwrap().unwrap(), baseline);
    assert!(
        remote.requests()[requests_before..]
            .iter()
            .all(|r| r.operation != "put" && r.operation != "delete")
    );
    drop(socket);
    std::fs::remove_file(socket_path).unwrap();
    std::fs::rename(saved_file, device.root.path().join("hello.txt")).unwrap();
    drop(index);
    let mut index = device.index(&binding);
    assert_eq!(index.active_epoch().unwrap().unwrap(), baseline);
    sync(&remote, &binding, &device, &mut index, "guard-reopened").await;
    assert_eq!(index.active_epoch().unwrap().unwrap(), baseline);
    assert!(
        remote.requests()[requests_before..]
            .iter()
            .all(|r| r.operation != "put" && r.operation != "delete")
    );
}

#[tokio::test]
async fn published_cancel_still_applies_and_acknowledges_in_one_epoch() {
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    std::fs::write(device.root.path().join("local"), b"added").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    run(
        Arc::new(Paged(remote.store(), true, Some(cancel.clone()))),
        &binding,
        b"p",
        &mut index,
        &device.request("cancel-after-commit"),
        cancel.clone(),
    )
    .await
    .unwrap();
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
    let epoch = index.active_epoch().unwrap().unwrap();
    let publication = index
        .frozen_operation(epoch.publication.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(publication.state, OperationState::Applied);
    assert_eq!(
        epoch.frontier.tips[0].operation_id,
        publication.operation_id
    );
}

#[tokio::test]
async fn published_unknown_retry_keeps_operation_and_bytes_without_rescan() {
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    // Reuse existing content so the first PUT is the new tree, then force the
    // commit response loss using a store wrapper below.
    std::fs::write(device.root.path().join("local"), b"hello").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    struct LoseCommit(Paged);
    impl ObjectStore for LoseCommit {
        fn capabilities(&self) -> StorageCapabilities {
            self.0.capabilities()
        }
        fn list<'a>(&'a self, p: &'a str) -> ObjectListing<'a> {
            self.0.list(p)
        }
        fn list_page<'a>(
            &'a self,
            p: &'a str,
            t: Option<&'a str>,
        ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
            self.0.list_page(p, t)
        }
        fn stat<'a>(&'a self, k: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            self.0.stat(k)
        }
        fn get<'a>(
            &'a self,
            k: &'a str,
            r: Option<std::ops::Range<u64>>,
        ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
            self.0.get(k, r)
        }
        fn delete<'a>(&'a self, k: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
            self.0.delete(k)
        }
        fn put<'a>(
            &'a self,
            k: &'a str,
            b: ObjectStream,
            c: PutCondition,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                let result = self.0.put(k, b, c).await?;
                if k.contains("/commits/") {
                    return Err(CloudError::NotFound);
                }
                Ok(result)
            })
        }
    }
    assert!(
        run(
            Arc::new(LoseCommit(Paged(remote.store(), true, None))),
            &binding,
            b"p",
            &mut index,
            &device.request("lost-response"),
            cancel
        )
        .await
        .is_err()
    );
    assert!(index.active_epoch().unwrap().is_none());
    let commit = remote
        .requests()
        .into_iter()
        .find(|r| r.operation == "put" && r.key.contains("/commits/"))
        .unwrap();
    let operation = commit
        .key
        .rsplit('/')
        .next()
        .unwrap()
        .split('-')
        .next()
        .unwrap();
    assert_eq!(
        index.frozen_operation(operation).unwrap().unwrap().state,
        OperationState::PublishedUnknown
    );
    drop(index);
    let mut index = device.index(&binding);
    sync(
        &remote,
        &binding,
        &device,
        &mut index,
        "different-request-id",
    )
    .await;
    assert_eq!(index.active_epoch().unwrap().unwrap().id, "lost-response");
    assert_eq!(
        index.frozen_operation(operation).unwrap().unwrap().state,
        OperationState::Applied
    );
    let puts = remote
        .requests()
        .into_iter()
        .filter(|r| r.operation == "put" && r.key.contains("/commits/"))
        .collect::<Vec<_>>();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].bytes, commit.bytes);
}

#[tokio::test]
async fn prepared_journal_fault_resumes_but_tampering_never_advances_b() {
    let (remote, _, binding, _) = fixture().await;
    for tamper in [false, true] {
        let device = Device::new();
        let mut index = device.index(&binding);
        let db = rusqlite::Connection::open(device.db.path().join("index")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_preserved BEFORE UPDATE ON append_records WHEN NEW.kind='engine-apply' BEGIN SELECT RAISE(FAIL,'preserved fault'); END;").unwrap();
        assert!(
            pull(
                store(&remote),
                &binding,
                b"p",
                &mut index,
                &device.request("prepared-fault"),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err()
        );
        assert!(index.active_epoch().unwrap().is_none());
        let ids = index.recovery_record_ids("", 10).unwrap();
        assert_eq!(ids.len(), 1);
        let intent = index.recovery_record(&ids[0]).unwrap().unwrap();
        assert_eq!(
            intent.phase,
            peerdav_lib::sync::apply_types::ApplyPhase::Prepared
        );
        if tamper {
            std::fs::write(&intent.staging_path, b"tamper").unwrap();
        }
        db.execute_batch("DROP TRIGGER fail_preserved").unwrap();
        drop(index);
        let mut index = device.index(&binding);
        let result = pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("retry"),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        if tamper {
            assert!(matches!(result, Err(AppendEngineError::RecoveryPaused)));
            assert!(index.active_epoch().unwrap().is_none());
            assert!(intent.staging_path.exists());
        } else {
            assert!(result.unwrap().resumed);
            assert!(index.active_epoch().unwrap().is_some());
        }
    }
}

#[tokio::test]
async fn installed_journal_fault_reopens_without_early_b() {
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    let connection = rusqlite::Connection::open(device.db.path().join("index")).unwrap();
    connection.execute_batch("CREATE TABLE fault_counter(n INTEGER); INSERT INTO fault_counter VALUES(0);
        CREATE TRIGGER count_preserved AFTER UPDATE ON append_records WHEN NEW.kind='engine-apply'
        BEGIN UPDATE fault_counter SET n=n+1; END;
        CREATE TRIGGER fail_installed BEFORE UPDATE ON append_records WHEN NEW.kind='engine-apply' AND (SELECT n FROM fault_counter)=1
        BEGIN SELECT RAISE(FAIL,'installed journal fault'); END;").unwrap();
    assert!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("install-fault"),
            Arc::new(AtomicBool::new(false))
        )
        .await
        .is_err()
    );
    assert_eq!(
        connection
            .query_row("SELECT n FROM fault_counter", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // Installation happened; only its durable journal update failed.
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
    assert!(index.active_epoch().unwrap().is_none());
    drop(index);
    connection
        .execute_batch(
            "DROP TRIGGER fail_installed; DROP TRIGGER count_preserved; DROP TABLE fault_counter;",
        )
        .unwrap();
    drop(connection);
    let mut index = device.index(&binding);
    let report = pull(
        store(&remote),
        &binding,
        b"p",
        &mut index,
        &device.request("reopened"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert!(report.resumed);
    assert_eq!(index.active_epoch().unwrap().unwrap().id, "install-fault");
}

async fn assert_three_device_edit_delete_rename_delivery_orders(orders: &[[usize; 3]]) {
    for &order in orders {
        let (remote, _, binding, _) = fixture().await;
        let devices = [Device::new(), Device::new(), Device::new()];
        let mut indices = devices
            .iter()
            .map(|d| d.index(&binding))
            .collect::<Vec<_>>();
        for i in 0..3 {
            sync(&remote, &binding, &devices[i], &mut indices[i], "base").await;
        }
        std::fs::write(devices[0].root.path().join("hello.txt"), b"edited").unwrap();
        std::fs::remove_file(devices[1].root.path().join("hello.txt")).unwrap();
        std::fs::rename(
            devices[2].root.path().join("hello.txt"),
            devices[2].root.path().join("renamed.txt"),
        )
        .unwrap();
        for i in order {
            sync(&remote, &binding, &devices[i], &mut indices[i], "publish").await;
        }
        for i in 0..3 {
            sync(&remote, &binding, &devices[i], &mut indices[i], "converge").await;
        }
        let snapshot = |device: &Device| {
            let mut files = std::fs::read_dir(device.root.path())
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect::<Vec<_>>();
            files.sort();
            files
        };
        let expected = snapshot(&devices[0]);
        assert_eq!(snapshot(&devices[1]), expected, "order {order:?}");
        assert_eq!(snapshot(&devices[2]), expected, "order {order:?}");
        assert!(expected.iter().any(|(_, bytes)| bytes == b"edited"));
        assert!(
            expected
                .iter()
                .any(|(name, bytes)| name == "renamed.txt" && bytes == b"hello")
        );
        let closure = peerdav_lib::sync::append_discovery::discover(
            store(&remote),
            &binding,
            b"p",
            &mut indices[0],
            "inspect",
        )
        .await
        .unwrap();
        let revisions: RevisionMap = closure
            .trees()
            .values()
            .flat_map(|tree| tree.entries.iter().cloned())
            .map(|r| (r.revision_id.clone(), r))
            .collect();
        let original = revisions.values().find(|r| matches!(&r.value, RevisionValue::Live { name, node: Node::File(file), .. } if name == "hello.txt" && file.digest == hex::encode(Sha256::digest(b"hello")))).unwrap();
        let descendants = revisions
            .values()
            .filter(|r| r.entity == original.entity && !r.parents.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(descendants.len(), 3);
        assert!(
            descendants
                .iter()
                .all(|r| r.parents == vec![original.revision_id.clone()])
        );
        assert!(
            descendants
                .iter()
                .any(|r| matches!(r.value, RevisionValue::Tombstone))
        );
        let puts = remote
            .requests()
            .iter()
            .filter(|r| r.operation == "put")
            .count();
        for i in 0..3 {
            sync(&remote, &binding, &devices[i], &mut indices[i], "steady").await;
        }
        assert_eq!(
            remote
                .requests()
                .iter()
                .filter(|r| r.operation == "put")
                .count(),
            puts
        );
    }
}

#[tokio::test]
async fn three_device_edit_delete_rename_delivery_orders_012_021_converge() {
    assert_three_device_edit_delete_rename_delivery_orders(&[[0, 1, 2], [0, 2, 1]]).await;
}

#[tokio::test]
async fn three_device_edit_delete_rename_delivery_orders_102_120_converge() {
    assert_three_device_edit_delete_rename_delivery_orders(&[[1, 0, 2], [1, 2, 0]]).await;
}

#[tokio::test]
async fn three_device_edit_delete_rename_delivery_orders_201_210_converge() {
    assert_three_device_edit_delete_rename_delivery_orders(&[[2, 0, 1], [2, 1, 0]]).await;
}

#[tokio::test]
async fn three_independent_devices_pull_and_steady_reopen_zero_put() {
    let (remote, _, binding, _) = fixture().await;
    for _ in 0..3 {
        let device = Device::new();
        let mut index = device.index(&binding);
        let report = pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("first"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(report.local_epoch, 1);
        assert_eq!(report.frontier.tips.len(), 1);
        assert_eq!(report.frontier.tips[0].digest.len(), 64);
        assert_eq!(
            std::fs::read(device.root.path().join("hello.txt")).unwrap(),
            b"hello"
        );
        drop(index);
        let mut index = device.index(&binding);
        let report = pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("second"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(report.local_epoch, 1);
        assert_eq!(report.applied_files, 0);
    }
    assert!(
        remote
            .requests()
            .iter()
            .all(|request| request.operation != "put")
    );
}

#[tokio::test]
async fn unknown_baseline_and_known_local_edits_are_retained_not_deleted() {
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    std::fs::write(device.root.path().join("precious"), b"local").unwrap();
    assert!(matches!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("unknown"),
            Arc::new(AtomicBool::new(false))
        )
        .await,
        Err(AppendEngineError::LocalChangesUnsupported)
    ));
    assert!(index.active_epoch().unwrap().is_none());
    assert_eq!(
        std::fs::read(device.root.path().join("precious")).unwrap(),
        b"local"
    );
    std::fs::remove_file(device.root.path().join("precious")).unwrap();
    pull(
        store(&remote),
        &binding,
        b"p",
        &mut index,
        &device.request("initial"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    std::fs::write(device.root.path().join("hello.txt"), b"edited").unwrap();
    assert!(matches!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("edited"),
            Arc::new(AtomicBool::new(false))
        )
        .await,
        Err(AppendEngineError::LocalChangesUnsupported)
    ));
    assert_eq!(index.active_epoch().unwrap().unwrap().sequence, 1);
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"edited"
    );
    assert!(
        remote
            .requests()
            .iter()
            .all(|request| request.operation != "put")
    );
}

#[tokio::test]
async fn apply_preparation_failure_reopens_without_early_baseline_and_converges() {
    let (remote, _, binding, _) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    // Fail shared materialize preparation after durable epoch staging.
    std::fs::remove_dir(device.private.path()).unwrap();
    assert!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("failed"),
            Arc::new(AtomicBool::new(false))
        )
        .await
        .is_err()
    );
    assert!(index.active_epoch().unwrap().is_none());
    drop(index);
    std::fs::create_dir(device.private.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            device.private.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let mut index = device.index(&binding);
    let report = pull(
        store(&remote),
        &binding,
        b"p",
        &mut index,
        &device.request("retry"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert!(report.resumed);
    assert_eq!(index.active_epoch().unwrap().unwrap().id, "failed");
    assert_eq!(
        std::fs::read(device.root.path().join("hello.txt")).unwrap(),
        b"hello"
    );
}

#[tokio::test]
async fn missing_dependency_cancel_and_root_rebinding_never_advance_baseline() {
    let (remote, _, binding, block_key) = fixture().await;
    let device = Device::new();
    let mut index = device.index(&binding);
    let result = pull(
        store(&remote),
        &binding,
        b"p",
        &mut index,
        &device.request("cancel"),
        Arc::new(AtomicBool::new(true)),
    )
    .await;
    assert!(matches!(result, Err(AppendEngineError::Cancelled)));
    assert!(index.active_epoch().unwrap().is_none());
    let other = Device::new();
    pull(
        store(&remote),
        &binding,
        b"p",
        &mut other.index(&binding),
        &other.request("collect"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    let visible = remote
        .requests()
        .into_iter()
        .filter(|request| request.operation == "get" && request.key != block_key)
        .map(|request| request.key)
        .collect();
    remote.set_get_visible_keys(Some(visible));
    assert!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &device.request("missing"),
            Arc::new(AtomicBool::new(false))
        )
        .await
        .is_err()
    );
    assert!(index.active_epoch().unwrap().is_none());
    remote.set_get_visible_keys(None);
    pull(
        store(&remote),
        &binding,
        b"p",
        &mut index,
        &device.request("ok"),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    let replacement = Device::new();
    assert!(matches!(
        pull(
            store(&remote),
            &binding,
            b"p",
            &mut index,
            &replacement.request("rootchange"),
            Arc::new(AtomicBool::new(false))
        )
        .await,
        Err(AppendEngineError::BindingChanged)
    ));
    assert_eq!(index.active_epoch().unwrap().unwrap().sequence, 1);
}
