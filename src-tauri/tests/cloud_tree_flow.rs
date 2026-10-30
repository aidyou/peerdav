use bytes::Bytes;
use futures_util::{StreamExt, future::BoxFuture, stream};
use peerdav_lib::cloud::{
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, StorageCapabilities,
    },
};
use peerdav_lib::sync::{
    merge::{LocalChange, revision},
    merge_types::{
        BlockRef, ChangeId, EntityId, FileNode, MergeSet, Node, Revision, RevisionId, RevisionValue,
    },
    publisher_types::PublishControl,
    repository::Repository,
    repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode},
    tree::{load_tree, write_tree},
    tree_types::{DirectoryRefDto, TreeDto, TreeKind, TreeReuse},
};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Default)]
struct Fake {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    puts: AtomicU64,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn count(&self) -> u64 {
        self.puts.load(Ordering::SeqCst)
    }
}
impl ObjectStore for Fake {
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
                    etag: None,
                })
            })
            .collect::<Vec<_>>();
        Box::pin(stream::iter(v))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .map(|b| ObjectMetadata {
                    key: key.into(),
                    size: b.len() as u64,
                    etag: None,
                })
                .ok_or(CloudError::NotFound)
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        _: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
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
            if matches!(condition, PutCondition::CreateOnly) && o.contains_key(key) {
                return Err(CloudError::Precondition);
            }
            o.insert(key.into(), b.clone());
            self.puts.fetch_add(1, Ordering::SeqCst);
            Ok(ObjectMetadata {
                key: key.into(),
                size: b.len() as u64,
                etag: None,
            })
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        })
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
fn control() -> PublishControl {
    PublishControl {
        cancel: Arc::new(AtomicBool::new(false)),
        epoch: Arc::new(AtomicU64::new(0)),
        expected_epoch: 0,
    }
}
fn change(entity: &str, id: &str, parents: Vec<RevisionId>, value: RevisionValue) -> Revision {
    revision(LocalChange {
        entity: EntityId(entity.into()),
        change_id: ChangeId(id.into()),
        parents,
        value,
    })
    .unwrap()
}
fn dir(entity: &str, parent: &str, name: &str) -> Revision {
    change(
        entity,
        name,
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId(parent.into()),
            name: name.into(),
            node: Node::Dir,
        },
    )
}
fn file(entity: &str, parent: &str, name: &str) -> Revision {
    change(
        entity,
        name,
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId(parent.into()),
            name: name.into(),
            node: Node::File(FileNode {
                blocks: vec![BlockRef {
                    id: "00".repeat(32),
                    len: 1024,
                }],
                length: 1024,
                digest: "11".repeat(32),
            }),
        },
    )
}
fn set(revisions: Vec<Revision>) -> MergeSet {
    MergeSet {
        revisions: revisions
            .into_iter()
            .map(|r| (r.revision_id.clone(), r))
            .collect(),
    }
}
async fn repo(mode: RepositoryMode) -> (Arc<Fake>, Repository) {
    let store = Fake::new();
    let repo = Repository::create(
        store.clone(),
        descriptor(mode.clone()),
        if mode == RepositoryMode::Encrypted {
            b"pass"
        } else {
            b""
        },
    )
    .await
    .unwrap();
    (store, repo)
}

#[tokio::test]
async fn empty_root_roundtrip() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let err = write_tree(
        &repo,
        &MergeSet {
            revisions: BTreeMap::new(),
        },
        &mut TreeReuse::default(),
        &control(),
    )
    .await;
    assert!(
        err.is_ok(),
        "empty snapshot should have a representable root: {err:?}"
    );
    let root = err.unwrap();
    let loaded = load_tree(&repo, &root, &control()).await.unwrap();
    assert!(loaded.snapshot.revisions.is_empty());
}

#[tokio::test]
async fn nested_empty_dirs_and_file_histories_roundtrip() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let a = dir("a", "root", "a");
    let b = dir("b", "a", "b");
    let f = file("f", "b", "f");
    let tomb = change("gone", "delete", vec![], RevisionValue::Tombstone);
    let loaded = write_tree(
        &repo,
        &set(vec![a.clone(), b.clone(), f.clone(), tomb.clone()]),
        &mut TreeReuse::default(),
        &control(),
    )
    .await
    .unwrap();
    let snapshot = load_tree(&repo, &loaded, &control())
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snapshot.revisions.len(), 4);
    assert!(matches!(
        snapshot.revisions[&tomb.revision_id].value,
        RevisionValue::Tombstone
    ));
}

#[tokio::test]
async fn encrypted_unchanged_tree_reuses_every_object() {
    let (store, repo) = repo(RepositoryMode::Encrypted).await;
    let snapshot = set(vec![dir("a", "root", "a"), file("f", "root", "f")]);
    let mut reuse = TreeReuse::default();
    let first = write_tree(&repo, &snapshot, &mut reuse, &control())
        .await
        .unwrap();
    let mut loaded_first = load_tree(&repo, &first, &control()).await.unwrap();
    let puts = store.count();
    let second = write_tree(&repo, &snapshot, &mut loaded_first.reuse, &control())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(store.count(), puts);
    assert!(repo.get_metadata("tree", &first).await.is_ok());
}

#[tokio::test]
async fn changing_one_file_does_not_rewrite_unaffected_child() {
    let (store, repo) = repo(RepositoryMode::Encrypted).await;
    let a = dir("a", "root", "a");
    let b = dir("b", "root", "b");
    let fa = file("fa", "a", "fa");
    let fb = file("fb", "b", "fb");
    let mut reuse = TreeReuse::default();
    let first = write_tree(
        &repo,
        &set(vec![a.clone(), b.clone(), fa, fb.clone()]),
        &mut reuse,
        &control(),
    )
    .await
    .unwrap();
    let before = store.count();
    let changed = file("fa2", "a", "fa2");
    let second = write_tree(&repo, &set(vec![a, b, changed, fb]), &mut reuse, &control())
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        store.count() - before,
        2,
        "only changed directory and root are uploaded"
    );
    let first_dto: TreeDto = peerdav_lib::sync::repository_types::decode_metadata(
        &repo.get_metadata("tree", &first).await.unwrap(),
    )
    .unwrap();
    let second_dto: TreeDto = peerdav_lib::sync::repository_types::decode_metadata(
        &repo.get_metadata("tree", &second).await.unwrap(),
    )
    .unwrap();
    let children = |dto: TreeDto| match dto.tree {
        TreeKind::Directory { children, .. } => children,
        _ => panic!("root must be a directory"),
    };
    let first_children = children(first_dto);
    let second_children = children(second_dto);
    assert_eq!(
        first_children
            .iter()
            .find(|child| child.dir.0 == "b")
            .unwrap()
            .object,
        second_children
            .iter()
            .find(|child| child.dir.0 == "b")
            .unwrap()
            .object,
        "unchanged encrypted sibling must retain ciphertext object ID"
    );
    let loaded = load_tree(&repo, &second, &control()).await.unwrap();
    assert!(
        loaded
            .snapshot
            .revisions
            .values()
            .any(|r| r.entity.0 == "fb")
    );
}

#[tokio::test]
async fn old_flat_v1_is_rejected() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let id = repo
        .put_metadata("tree", br#"{"version":1,"snapshot":{"revisions":{}}}"#)
        .await
        .unwrap();
    assert!(load_tree(&repo, &id, &control()).await.is_err());
}

#[tokio::test]
async fn missing_and_duplicate_children_are_rejected() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let missing = TreeDto {
        version: 1,
        tree: TreeKind::Directory {
            directory: EntityId("root".into()),
            entries: vec![],
            children: vec![DirectoryRefDto {
                dir: EntityId("child".into()),
                object: "00".repeat(32),
            }],
            recovery: None,
        },
    };
    let id = repo
        .put_metadata("tree", &serde_json::to_vec(&missing).unwrap())
        .await
        .unwrap();
    assert!(load_tree(&repo, &id, &control()).await.is_err());
    let dup = TreeDto {
        version: 1,
        tree: TreeKind::Directory {
            directory: EntityId("root".into()),
            recovery: None,
            entries: vec![],
            children: vec![
                DirectoryRefDto {
                    dir: EntityId("x".into()),
                    object: "11".repeat(32),
                },
                DirectoryRefDto {
                    dir: EntityId("x".into()),
                    object: "22".repeat(32),
                },
            ],
        },
    };
    let id = repo
        .put_metadata("tree", &serde_json::to_vec(&dup).unwrap())
        .await
        .unwrap();
    assert!(load_tree(&repo, &id, &control()).await.is_err());
}

#[tokio::test]
async fn historical_parent_cycles_roundtrip_via_typed_recovery() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let a = change(
        "a",
        "a",
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId("b".into()),
            name: "a".into(),
            node: Node::Dir,
        },
    );
    let b = change(
        "b",
        "b",
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId("a".into()),
            name: "b".into(),
            node: Node::Dir,
        },
    );
    let snapshot = set(vec![a, b]);
    let root = write_tree(&repo, &snapshot, &mut TreeReuse::default(), &control())
        .await
        .unwrap();
    assert_eq!(
        load_tree(&repo, &root, &control()).await.unwrap().snapshot,
        snapshot
    );
    let dto: TreeDto = peerdav_lib::sync::repository_types::decode_metadata(
        &repo.get_metadata("tree", &root).await.unwrap(),
    )
    .unwrap();
    assert!(matches!(
        dto.tree,
        TreeKind::Directory {
            recovery: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn moved_entity_and_tombstone_keep_complete_causal_history() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let initial = file("f", "a", "private-name-marker");
    let moved = change(
        "f",
        "move",
        vec![initial.revision_id.clone()],
        RevisionValue::Live {
            parent_dir: EntityId("b".into()),
            name: "renamed".into(),
            node: Node::File(FileNode {
                blocks: vec![],
                length: 0,
                digest: "00".repeat(32),
            }),
        },
    );
    let deleted = change(
        "f",
        "delete",
        vec![moved.revision_id.clone()],
        RevisionValue::Tombstone,
    );
    let snapshot = set(vec![
        dir("a", "root", "a"),
        dir("b", "root", "b"),
        initial,
        moved,
        deleted,
    ]);
    let root = write_tree(&repo, &snapshot, &mut TreeReuse::default(), &control())
        .await
        .unwrap();
    assert_eq!(
        load_tree(&repo, &root, &control()).await.unwrap().snapshot,
        snapshot
    );
}

#[tokio::test]
async fn cancelled_tree_write_and_read_fail_closed() {
    let (store, repo) = repo(RepositoryMode::Plain).await;
    let snapshot = set(vec![dir("a", "root", "a")]);
    let root = write_tree(&repo, &snapshot, &mut TreeReuse::default(), &control())
        .await
        .unwrap();
    let cancelled = control();
    cancelled.cancel.store(true, Ordering::SeqCst);
    let puts = store.count();
    assert!(matches!(
        write_tree(&repo, &snapshot, &mut TreeReuse::default(), &cancelled).await,
        Err(peerdav_lib::sync::tree_types::TreeError::Cancelled)
    ));
    assert!(matches!(
        load_tree(&repo, &root, &cancelled).await,
        Err(peerdav_lib::sync::tree_types::TreeError::Cancelled)
    ));
    assert_eq!(store.count(), puts);
}

#[tokio::test]
async fn malformed_child_identity_and_unknown_version_are_rejected() {
    let (_, repo) = repo(RepositoryMode::Plain).await;
    let child = TreeDto {
        version: 1,
        tree: TreeKind::Directory {
            directory: EntityId("wrong".into()),
            entries: vec![],
            children: vec![],
            recovery: None,
        },
    };
    let child_id = repo
        .put_metadata("tree", &serde_json::to_vec(&child).unwrap())
        .await
        .unwrap();
    let root = TreeDto {
        version: 1,
        tree: TreeKind::Directory {
            directory: EntityId("root".into()),
            entries: vec![],
            children: vec![DirectoryRefDto {
                dir: EntityId("expected".into()),
                object: child_id,
            }],
            recovery: None,
        },
    };
    let root_id = repo
        .put_metadata("tree", &serde_json::to_vec(&root).unwrap())
        .await
        .unwrap();
    assert!(load_tree(&repo, &root_id, &control()).await.is_err());
    let unknown = TreeDto {
        version: 2,
        tree: TreeKind::Directory {
            directory: EntityId("root".into()),
            entries: vec![],
            children: vec![],
            recovery: None,
        },
    };
    let id = repo
        .put_metadata("tree", &serde_json::to_vec(&unknown).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        load_tree(&repo, &id, &control()).await,
        Err(peerdav_lib::sync::tree_types::TreeError::Unsupported)
    ));
}

#[tokio::test]
async fn oversized_single_directory_is_rejected_before_any_tree_put() {
    let (store, repo) = repo(RepositoryMode::Plain).await;
    let revisions = (0..400)
        .map(|n| {
            let entity = format!("file-{n}");
            change(
                &entity,
                &entity,
                vec![],
                RevisionValue::Live {
                    parent_dir: EntityId("root".into()),
                    name: entity.clone(),
                    node: Node::File(FileNode {
                        blocks: (0..1024)
                            .map(|block| BlockRef {
                                id: format!("{block:064x}"),
                                len: 1,
                            })
                            .collect(),
                        length: 1024,
                        digest: "11".repeat(32),
                    }),
                },
            )
        })
        .collect();
    let puts = store.count();
    assert!(matches!(
        write_tree(
            &repo,
            &set(revisions),
            &mut TreeReuse::default(),
            &control()
        )
        .await,
        Err(peerdav_lib::sync::tree_types::TreeError::ResourceLimit(_))
    ));
    assert_eq!(store.count(), puts);
}

#[tokio::test]
async fn aggregate_above_32mib_roundtrips_across_real_directory_manifests() {
    let (store, repo) = repo(RepositoryMode::Plain).await;
    let mut revisions = Vec::new();
    for d in 0..40 {
        let directory = format!("directory-{d:02}");
        revisions.push(dir(&directory, "root", &directory));
        for n in 0..12 {
            let entity = format!("file-{d:02}-{n:02}");
            let blocks = (0..1024)
                .map(|block| BlockRef {
                    id: format!("{block:064x}"),
                    len: 1,
                })
                .collect::<Vec<_>>();
            revisions.push(change(
                &entity,
                &entity,
                vec![],
                RevisionValue::Live {
                    parent_dir: EntityId(directory.clone()),
                    name: entity.clone(),
                    node: Node::File(FileNode {
                        blocks,
                        length: 1024,
                        digest: "11".repeat(32),
                    }),
                },
            ));
        }
    }
    let snapshot = set(revisions);
    assert!(serde_json::to_vec(&snapshot).unwrap().len() > 32 * 1024 * 1024);
    let root = write_tree(&repo, &snapshot, &mut TreeReuse::default(), &control())
        .await
        .unwrap();
    let objects = store.objects.lock().unwrap();
    let trees = objects
        .iter()
        .filter(|(key, _)| key.starts_with("trees/"))
        .collect::<Vec<_>>();
    assert_eq!(trees.len(), 41, "one root and forty meaningful directories");
    assert!(
        trees
            .iter()
            .all(|(_, bytes)| bytes.len() <= 32 * 1024 * 1024)
    );
    assert!(trees.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > 32 * 1024 * 1024);
    drop(objects);
    let loaded = load_tree(&repo, &root, &control()).await.unwrap();
    assert_eq!(loaded.snapshot, snapshot);
}
