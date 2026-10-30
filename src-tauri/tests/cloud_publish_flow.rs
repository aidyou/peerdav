use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::{StreamExt, stream};
use peerdav_lib::cloud::{
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, StorageCapabilities,
    },
};
use peerdav_lib::sync::{
    index::SyncIndex,
    merge::revision,
    merge_types::{
        Baseline, BlockRef, ChangeId, EntityId, FileNode, MergeSet, Node, RevisionValue,
    },
    publisher::publish,
    publisher_types::{PublishBlock, PublishControl},
    repository::Repository,
    repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    io,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tempfile::tempdir;

#[derive(Default)]
struct Store {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    puts: AtomicU64,
    cas: AtomicU64,
    lost_once: AtomicBool,
    tree_put_fail_after: AtomicU64,
    tree_lost_once: AtomicBool,
    get_failure: AtomicBool,
    cancel_on_tree: Mutex<Option<Arc<AtomicBool>>>,
}
impl Store {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn head(&self) -> Vec<u8> {
        self.objects
            .lock()
            .unwrap()
            .get("control/HEAD")
            .cloned()
            .unwrap()
    }
    fn head_generation(&self) -> u64 {
        peerdav_lib::sync::repository_types::decode_head(&self.head())
            .unwrap()
            .generation
    }
    fn metadata_count(&self, prefix: &str) -> u64 {
        self.puts.load(Ordering::SeqCst);
        self.objects
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .count() as u64
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
        let v = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, b)| {
                Ok(ObjectMetadata {
                    key: k.clone(),
                    size: b.len() as u64,
                    etag: Some(hex::encode(Sha256::digest(b))),
                })
            })
            .collect::<Vec<_>>();
        Box::pin(stream::iter(v))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let o = self.objects.lock().unwrap();
            let b = o.get(key).ok_or(CloudError::NotFound)?;
            Ok(meta(key, b))
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        _: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            if key.starts_with("blocks/") && self.get_failure.swap(false, Ordering::SeqCst) {
                return Err(CloudError::Storage {
                    source: object_store::Error::Generic {
                        store: "fixture",
                        source: Box::new(io::Error::new(io::ErrorKind::Other, "wire read failed")),
                    },
                });
            }
            let b = self
                .objects
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or(CloudError::NotFound)?;
            Ok(stream::once(async move { Ok(Bytes::from(b)) }).boxed())
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let mut b = Vec::new();
            while let Some(x) = body.next().await {
                b.extend_from_slice(&x?);
            }
            let mut o = self.objects.lock().unwrap();
            match condition {
                PutCondition::CreateOnly if o.contains_key(key) => {
                    return Err(CloudError::Precondition);
                }
                PutCondition::MatchEtag(expected) => {
                    let got = o.get(key).ok_or(CloudError::NotFound)?;
                    if hex::encode(Sha256::digest(got)) != expected {
                        return Err(CloudError::Precondition);
                    }
                }
                _ => {}
            }
            self.puts.fetch_add(1, Ordering::SeqCst);
            if key.starts_with("trees/") {
                let tree_put = self.tree_put_fail_after.load(Ordering::SeqCst);
                if tree_put > 0
                    && o.keys().filter(|k| k.starts_with("trees/")).count() as u64 >= tree_put
                {
                    return Err(CloudError::Storage {
                        source: object_store::Error::Generic {
                            store: "fixture",
                            source: Box::new(io::Error::new(
                                io::ErrorKind::Other,
                                "tree put failed",
                            )),
                        },
                    });
                }
            }
            let m = meta(key, &b);
            o.insert(key.to_owned(), b);
            if key.starts_with("trees/") {
                if self.tree_lost_once.swap(false, Ordering::SeqCst) {
                    return Err(CloudError::Storage {
                        source: object_store::Error::Generic {
                            store: "fixture",
                            source: Box::new(io::Error::new(
                                io::ErrorKind::Other,
                                "tree response lost",
                            )),
                        },
                    });
                }
                if let Some(cancel) = self.cancel_on_tree.lock().unwrap().as_ref() {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            if key == "control/HEAD" {
                self.cas.fetch_add(1, Ordering::SeqCst);
                if self.lost_once.swap(false, Ordering::SeqCst) {
                    return Err(CloudError::Storage {
                        source: object_store::Error::Generic {
                            store: "fixture",
                            source: Box::new(io::Error::new(io::ErrorKind::Other, "lost response")),
                        },
                    });
                }
            }
            Ok(m)
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        })
    }
}
fn meta(key: &str, b: &[u8]) -> ObjectMetadata {
    ObjectMetadata {
        key: key.into(),
        size: b.len() as u64,
        etag: Some(hex::encode(Sha256::digest(b))),
    }
}
fn descriptor() -> RepoDescriptor {
    RepoDescriptor {
        version: 1,
        repo_id: "00112233445566778899aabbccddeeff".into(),
        mode: RepositoryMode::Plain,
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
async fn repo(store: Arc<Store>) -> Arc<Repository> {
    match Repository::create(store.clone(), descriptor(), b"").await {
        Ok(r) => Arc::new(r),
        Err(_) => Arc::new(Repository::join(store, b"").await.unwrap()),
    }
}
fn index() -> (tempfile::TempDir, SyncIndex) {
    let d = tempdir().unwrap();
    let p = d.path().join("index.db");
    let i = SyncIndex::open(
        &p,
        zeroize::Zeroizing::new([7u8; 32]),
        "repo".into(),
        "map".into(),
    )
    .unwrap();
    (d, i)
}
fn control() -> PublishControl {
    PublishControl {
        cancel: Arc::new(AtomicBool::new(false)),
        epoch: Arc::new(AtomicU64::new(4)),
        expected_epoch: 4,
    }
}
fn snapshot(entity: &str, name: &str, block: &str, block_id: String) -> MergeSet {
    let e = EntityId(entity.into());
    let c = peerdav_lib::sync::merge::prepare_local_change(
        e,
        ChangeId(format!("change-{name}")),
        Baseline::Unknown,
        RevisionValue::Live {
            parent_dir: EntityId("root".into()),
            name: name.into(),
            node: Node::File(FileNode {
                blocks: vec![BlockRef {
                    id: block_id,
                    len: block.len() as u64,
                }],
                length: block.len() as u64,
                digest: hex::encode(Sha256::digest(block.as_bytes())),
            }),
        },
        &BTreeMap::new(),
        true,
    )
    .unwrap();
    let r = revision(c).unwrap();
    MergeSet {
        revisions: BTreeMap::from([(r.revision_id.clone(), r)]),
    }
}
async fn run(
    store: Arc<Store>,
    entity: &str,
    name: &str,
    body: &str,
    op: &str,
    idx: &mut SyncIndex,
    control: &PublishControl,
) -> peerdav_lib::sync::publisher_types::PublishResult {
    let r = repo(store.clone()).await;
    let id = r.put_block(body.as_bytes()).await.unwrap();
    publish(
        r,
        idx,
        op,
        9,
        snapshot(entity, name, body, id),
        &[PublishBlock {
            content: body.as_bytes().to_vec(),
        }],
        control,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn empty_valid_snapshot_publishes_generation_one_and_journals() {
    let s = Store::new();
    let r = repo(s.clone()).await;
    let (_d, mut i) = index();
    let out = publish(
        r,
        &mut i,
        "11111111111111111111111111111111",
        3,
        MergeSet {
            revisions: BTreeMap::new(),
        },
        &[],
        &control(),
    )
    .await
    .unwrap();
    assert_eq!(out.generation, 1);
    let j: peerdav_lib::sync::index_types::RunJournal = i
        .load("run", "11111111111111111111111111111111")
        .unwrap()
        .unwrap();
    assert!(matches!(
        j.phase,
        peerdav_lib::sync::index_types::RunPhase::PublishedPendingApply
    ));
    assert_eq!(s.head_generation(), 1);
}

#[tokio::test]
async fn cancellation_does_not_advance_head() {
    let s = Store::new();
    let r = repo(s.clone()).await;
    let (_d, mut i) = index();
    let c = control();
    c.cancel.store(true, Ordering::Release);
    assert!(
        publish(
            r,
            &mut i,
            "22222222222222222222222222222222",
            1,
            MergeSet {
                revisions: BTreeMap::new()
            },
            &[],
            &c
        )
        .await
        .is_err()
    );
    assert_eq!(s.head_generation(), 0);
}

#[tokio::test]
async fn two_publishers_merge_disjoint_entities() {
    let s = Store::new();
    let (_d1, mut i1) = index();
    let (_d2, mut i2) = index();
    let c1 = control();
    let c2 = control();
    let a = run(
        s.clone(),
        "entity-a",
        "a",
        "alpha",
        "33333333333333333333333333333333",
        &mut i1,
        &c1,
    );
    let b = run(
        s.clone(),
        "entity-b",
        "b",
        "bravo",
        "44444444444444444444444444444444",
        &mut i2,
        &c2,
    );
    let (_a, _b) = tokio::join!(a, b);
    assert_eq!(s.head_generation(), 2);
    assert_eq!(s.metadata_count("commits/"), 2);
}

#[tokio::test]
async fn lost_cas_response_recovers_by_operation_id_without_double_advance() {
    let s = Store::new();
    let (_d, mut i) = index();
    let _ = repo(s.clone()).await;
    s.lost_once.store(true, Ordering::SeqCst);
    let out = run(
        s.clone(),
        "entity",
        "x",
        "payload",
        "55555555555555555555555555555555",
        &mut i,
        &control(),
    )
    .await;
    assert_eq!(out.generation, 1);
    assert_eq!(s.head_generation(), 1);
    assert_eq!(s.cas.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn missing_block_fails_before_head_advance() {
    let s = Store::new();
    let r = repo(s.clone()).await;
    let (_d, mut i) = index();
    let bad = snapshot(
        "entity",
        "x",
        "missing",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
    );
    let e = publish(
        r,
        &mut i,
        "66666666666666666666666666666666",
        1,
        bad,
        &[],
        &control(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        e,
        peerdav_lib::sync::publisher_types::PubError::MissingDependency
    ));
    assert_eq!(s.head_generation(), 0);
}

#[tokio::test]
async fn repeated_block_upload_is_not_put_again() {
    let s = Store::new();
    let r = repo(s.clone()).await;
    let before = s.metadata_count("blocks/");
    let _ = r.put_block(b"repeat").await.unwrap();
    let after = s.metadata_count("blocks/");
    let _ = r.put_block(b"repeat").await.unwrap();
    assert_eq!(s.metadata_count("blocks/"), after);
    assert_eq!(after, before + 1);
}

#[tokio::test]
async fn replay_operation_id_is_idempotent() {
    let s = Store::new();
    let (_d, mut i) = index();
    let out1 = run(
        s.clone(),
        "entity",
        "x",
        "payload",
        "77777777777777777777777777777777",
        &mut i,
        &control(),
    )
    .await;
    let commits = s.metadata_count("commits/");
    let out2 = run(
        s.clone(),
        "entity",
        "x",
        "payload",
        "77777777777777777777777777777777",
        &mut i,
        &control(),
    )
    .await;
    assert_eq!(out1.commit, out2.commit);
    assert_eq!(out2.generation, 1);
    assert_eq!(s.head_generation(), 1);
    assert_eq!(s.metadata_count("commits/"), commits);
}

#[tokio::test]
async fn different_empty_operation_publishes_new_commit_and_rollback_is_rejected() {
    let s = Store::new();
    let (_d, mut i) = index();
    let empty = MergeSet {
        revisions: BTreeMap::new(),
    };
    let first = publish(
        repo(s.clone()).await,
        &mut i,
        "88888888888888888888888888888888",
        1,
        empty.clone(),
        &[],
        &control(),
    )
    .await
    .unwrap();
    let old_head = s.head();
    let second = publish(
        repo(s.clone()).await,
        &mut i,
        "99999999999999999999999999999999",
        1,
        empty,
        &[],
        &control(),
    )
    .await
    .unwrap();
    assert_eq!(first.generation, 1);
    assert_eq!(second.generation, 2);
    s.objects
        .lock()
        .unwrap()
        .insert("control/HEAD".into(), old_head);
    let puts_before = s.puts.load(Ordering::SeqCst);
    let result = publish(
        repo(s.clone()).await,
        &mut i,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        1,
        MergeSet {
            revisions: BTreeMap::new(),
        },
        &[],
        &control(),
    )
    .await;
    assert!(matches!(
        result,
        Err(peerdav_lib::sync::publisher_types::PubError::Rollback)
    ));
    assert_eq!(s.head_generation(), 1);
    assert_eq!(s.puts.load(Ordering::SeqCst), puts_before);
}

#[tokio::test]
async fn replay_of_older_operation_recovers_without_advancing_head() {
    let s = Store::new();
    let (_d, mut i) = index();
    let first = run(
        s.clone(),
        "entity-a",
        "a",
        "alpha",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        &mut i,
        &control(),
    )
    .await;
    let second = run(
        s.clone(),
        "entity-b",
        "b",
        "bravo",
        "cccccccccccccccccccccccccccccccc",
        &mut i,
        &control(),
    )
    .await;
    let puts_before = s.puts.load(Ordering::SeqCst);
    let replay = run(
        s.clone(),
        "entity-a",
        "a",
        "alpha",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        &mut i,
        &control(),
    )
    .await;
    assert_eq!(first.generation, 1);
    assert_eq!(second.generation, 2);
    assert_eq!(replay.commit, second.commit);
    assert_eq!(replay.generation, 2);
    assert!(replay.recovered_operation_found);
    assert_eq!(s.head_generation(), 2);
    assert_eq!(s.puts.load(Ordering::SeqCst), puts_before);
}

#[tokio::test]
async fn cancellation_during_child_manifest_upload_does_not_publish_head() {
    let s = Store::new();
    let repository = repo(s.clone()).await;
    let (_d, mut index) = index();
    let control = control();
    *s.cancel_on_tree.lock().unwrap() = Some(control.cancel.clone());
    let directory = revision(peerdav_lib::sync::merge_types::LocalChange {
        entity: EntityId("directory".into()),
        change_id: ChangeId("create-directory".into()),
        parents: vec![],
        value: RevisionValue::Live {
            parent_dir: EntityId("root".into()),
            name: "folder".into(),
            node: Node::Dir,
        },
    })
    .unwrap();
    let snapshot = MergeSet {
        revisions: BTreeMap::from([(directory.revision_id.clone(), directory)]),
    };
    let error = publish(
        repository,
        &mut index,
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        1,
        snapshot,
        &[],
        &control,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::publisher_types::PubError::Cancelled
    ));
    assert_eq!(s.head_generation(), 0);
    let engine: peerdav_lib::sync::engine_types::EngineError = error.into();
    assert!(matches!(
        engine,
        peerdav_lib::sync::engine_types::EngineError::Cancelled
    ));
}

#[tokio::test]
async fn tree_put_failure_preserves_head_and_journal_then_retry_succeeds() {
    let s = Store::new();
    let (_d, mut i) = index();
    let first = run(
        s.clone(),
        "entity-a",
        "a",
        "alpha",
        "10101010101010101010101010101010",
        &mut i,
        &control(),
    )
    .await;
    assert_eq!(first.generation, 1);
    let old_head = s.head();
    s.tree_put_fail_after.store(1, Ordering::SeqCst);
    let r = repo(s.clone()).await;
    let id = r.put_block(b"bravo").await.unwrap();
    let err = publish(
        r,
        &mut i,
        "20202020202020202020202020202020",
        9,
        snapshot("entity-b", "b", "bravo", id),
        &[],
        &control(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        peerdav_lib::sync::publisher_types::PubError::Tree(_)
    ));
    assert_eq!(s.head(), old_head);
    let journal: peerdav_lib::sync::index_types::RunJournal = i
        .load("run", "20202020202020202020202020202020")
        .unwrap()
        .unwrap();
    assert!(matches!(
        journal.phase,
        peerdav_lib::sync::index_types::RunPhase::Publishing
    ));
    s.tree_put_fail_after.store(0, Ordering::SeqCst);
    let retry = run(
        s.clone(),
        "entity-b",
        "b",
        "bravo",
        "20202020202020202020202020202020",
        &mut i,
        &control(),
    )
    .await;
    assert_eq!(retry.generation, 2);
    assert_eq!(s.head_generation(), 2);
}

#[tokio::test]
async fn tree_response_loss_is_recoverable_without_head_change() {
    let s = Store::new();
    let (_d, mut i) = index();
    let r = repo(s.clone()).await;
    let id = r.put_block(b"payload").await.unwrap();
    let old_head = s.head();
    s.tree_lost_once.store(true, Ordering::SeqCst);
    let first = publish(
        r.clone(),
        &mut i,
        "30303030303030303030303030303030",
        9,
        snapshot("entity", "x", "payload", id.clone()),
        &[],
        &control(),
    )
    .await;
    assert!(first.is_err());
    assert_eq!(s.head(), old_head);
    let result = publish(
        r,
        &mut i,
        "30303030303030303030303030303030",
        9,
        snapshot("entity", "x", "payload", id),
        &[],
        &control(),
    )
    .await
    .unwrap();
    assert_eq!(result.generation, 1);
    assert_eq!(s.head_generation(), 1);
}
#[tokio::test]
async fn corrupt_referenced_block_returns_dependency_with_source_chain() {
    let s = Store::new();
    let r = repo(s.clone()).await;
    let (_d, mut i) = index();
    let block_id = r.put_block(b"payload").await.unwrap();
    s.objects
        .lock()
        .unwrap()
        .insert(format!("blocks/{block_id}"), b"corrupt-wire-data".to_vec());
    s.get_failure.store(true, Ordering::SeqCst);
    let result = publish(
        r,
        &mut i,
        "dddddddddddddddddddddddddddddddd",
        1,
        snapshot("entity", "x", "payload", block_id),
        &[],
        &control(),
    )
    .await;
    let error = result.unwrap_err();
    assert!(matches!(
        error,
        peerdav_lib::sync::publisher_types::PubError::Dependency(_)
    ));
    assert!(error.source().is_some());
    assert_eq!(s.head_generation(), 0);
}
