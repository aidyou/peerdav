#[allow(dead_code)]
mod append_common;

use append_common::Remote;
use peerdav_lib::sync::append_repository::AppendRepository;
use peerdav_lib::sync::append_types::{
    AppendError, AppendMode, BlockBinding, Chunker, Commit, Descriptor, Envelope, ObjectRef, Tree,
};
use peerdav_lib::sync::merge;
use peerdav_lib::sync::merge_types::{
    BlockRef, ChangeId, EntityId, FileNode, LocalChange, Node, RevisionValue,
};
use sha2::Digest;

fn descriptor(mode: AppendMode) -> Descriptor {
    Descriptor {
        version: 2,
        repo_id: "00112233445566778899aabbccddeeff".into(),
        mode,
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

fn repository(mode: AppendMode) -> (Remote, AppendRepository) {
    let remote = Remote::new(1);
    let (anchor, _) = AppendRepository::create_anchor(descriptor(mode), b"password").unwrap();
    let repo =
        AppendRepository::new(std::sync::Arc::new(remote.store()), anchor, b"password").unwrap();
    (remote, repo)
}

fn sample_tree(repo_id: &str, binding: BlockBinding) -> Tree {
    let root = merge::revision(LocalChange {
        entity: EntityId("root".into()),
        change_id: ChangeId("root-change".into()),
        parents: vec![],
        value: RevisionValue::Live {
            parent_dir: EntityId("root".into()),
            name: "root".into(),
            node: Node::Dir,
        },
    })
    .unwrap();
    let revision = merge::revision(LocalChange {
        entity: EntityId("file".into()),
        change_id: ChangeId("change".into()),
        parents: vec![],
        value: RevisionValue::Live {
            parent_dir: EntityId("root".into()),
            name: "file".into(),
            node: Node::File(FileNode {
                blocks: vec![BlockRef {
                    id: binding.block_id.clone(),
                    len: binding.len,
                }],
                length: binding.len,
                digest: "0".repeat(64),
            }),
        },
    })
    .unwrap();
    Tree {
        version: 2,
        repo_id: repo_id.into(),
        directory: "root".into(),
        entries: vec![root, revision],
        children: vec![],
        bindings: vec![binding],
    }
}

#[test]
fn tree_and_commit_roundtrip_canonicalize_and_preserve_history() {
    for mode in [AppendMode::Encrypted, AppendMode::AuthenticatedPlain] {
        let (_remote, repo) = repository(mode.clone());
        let (_key, block) = repo.seal_block(b"tree payload").unwrap();
        let env: Envelope = Envelope::from_wire(&block).unwrap();
        let binding = BlockBinding {
            block_id: env.logical_id,
            len: env.plaintext_len,
            object: ObjectRef {
                kind: "block".into(),
                digest: hex::encode(sha2::Sha256::digest(&block)),
            },
        };
        let tree = sample_tree(&descriptor(mode.clone()).repo_id, binding);
        let (tree_ref, tree_wire) = repo.seal_tree(&tree).unwrap();
        assert_eq!(tree_ref.kind, "tree");
        let decoded: Tree = repo.open_tree(&tree_wire, &tree_ref).unwrap();
        assert_eq!(decoded.entries.len(), 2);
        assert!(decoded.entries[0].revision_id <= decoded.entries[1].revision_id);

        let commit = Commit {
            version: 2,
            repo_id: descriptor(mode.clone()).repo_id,
            operation_id: "11223344556677889900aabbccddeeff".into(),
            parents: vec![
                peerdav_lib::sync::append_types::CommitRef {
                    operation_id: "00112233445566778899aabbccddeeff".into(),
                    digest: "a".repeat(64),
                },
                peerdav_lib::sync::append_types::CommitRef {
                    operation_id: "ffeeddccbbaa99887766554433221100".into(),
                    digest: "b".repeat(64),
                },
            ],
            root: tree_ref,
        };
        let (commit_ref, commit_wire) = repo.seal_commit(&commit).unwrap();
        let decoded: Commit = repo
            .open_commit(
                &commit_wire,
                &ObjectRef {
                    kind: "commit".into(),
                    digest: commit_ref.digest.clone(),
                },
            )
            .unwrap();
        assert_eq!(decoded.parents.len(), 2);
        assert_eq!(decoded.parents[0].digest, "a".repeat(64));
        assert_eq!(decoded.parents[1].digest, "b".repeat(64));
        assert_eq!(decoded.operation_id, "11223344556677889900aabbccddeeff");
    }
}

#[test]
fn metadata_rejects_missing_binding_wrong_binding_and_len_mutation() {
    let (_remote, repo) = repository(AppendMode::AuthenticatedPlain);
    let (_key, block) = repo.seal_block(b"bound").unwrap();
    let env: Envelope = Envelope::from_wire(&block).unwrap();
    let good = BlockBinding {
        block_id: env.logical_id.clone(),
        len: env.plaintext_len,
        object: ObjectRef {
            kind: "block".into(),
            digest: hex::encode(sha2::Sha256::digest(&block)),
        },
    };
    let mut missing = sample_tree(
        &descriptor(AppendMode::AuthenticatedPlain).repo_id,
        good.clone(),
    );
    missing.bindings.clear();
    assert!(repo.seal_tree(&missing).is_err());
    let mut wrong = good.clone();
    wrong.block_id = "00".repeat(32);
    assert!(repo.open_block_with_ref(&block, Some(&wrong)).is_err());
    let mut changed = Envelope::from_wire(&block).unwrap();
    changed.plaintext_len = env.plaintext_len + 1;
    let changed = changed.to_wire().unwrap();
    assert!(repo.open_block(&changed).is_err());
}

#[test]
fn metadata_rejects_cross_repository_mode_and_digest_or_len_tampering() {
    let (_remote, repo) = repository(AppendMode::Encrypted);
    let (_other_remote, other) = repository(AppendMode::AuthenticatedPlain);
    let (_key, block) = repo.seal_block(b"cross repo").unwrap();
    assert!(other.open_block(&block).is_err());
    let env: Envelope = Envelope::from_wire(&block).unwrap();
    let binding = BlockBinding {
        block_id: env.logical_id,
        len: env.plaintext_len,
        object: ObjectRef {
            kind: "block".into(),
            digest: hex::encode(sha2::Sha256::digest(&block)),
        },
    };
    let (tree_ref, tree_wire) = repo
        .seal_tree(&sample_tree(
            &descriptor(AppendMode::Encrypted).repo_id,
            binding,
        ))
        .unwrap();
    let mut bad_ref = tree_ref.clone();
    bad_ref.digest = "00".repeat(32);
    assert!(
        repo.open_metadata::<Tree>(&tree_wire, &bad_ref, "tree")
            .is_err()
    );
    let mut bad_wire = Envelope::from_wire(&tree_wire).unwrap();
    bad_wire.plaintext_len = 1;
    assert!(
        repo.open_metadata::<Tree>(&bad_wire.to_wire().unwrap(), &tree_ref, "tree")
            .is_err()
    );
}

#[tokio::test]
async fn prepare_block_seals_without_put() {
    let (remote, repo) = repository(AppendMode::AuthenticatedPlain);
    let prepared = repo.prepare_block(b"prepared").await.unwrap();
    assert!(prepared.key().contains("/blocks/"));
    assert!(!prepared.bytes().is_empty());
    assert!(remote.requests().iter().all(|r| r.operation != "put"));
}

#[test]
fn anchor_roundtrip_and_wrong_password() {
    for mode in [AppendMode::Encrypted, AppendMode::AuthenticatedPlain] {
        let (anchor, _) = AppendRepository::create_anchor(descriptor(mode), b"password").unwrap();
        assert!(AppendRepository::unlock(&anchor, b"password").is_ok());
        assert!(matches!(
            AppendRepository::unlock(&anchor, b"wrong"),
            Err(AppendError::Authentication)
        ));
        let mut bad = anchor.clone();
        bad.wrapped_master.kdf.memory_kib = 1;
        assert!(matches!(
            AppendRepository::unlock(&bad, b"password"),
            Err(AppendError::Input(_))
        ));
    }
}

#[tokio::test]
async fn blocks_roundtrip_binding_and_wire_mutations() {
    for mode in [AppendMode::Encrypted, AppendMode::AuthenticatedPlain] {
        let (anchor, _) = AppendRepository::create_anchor(descriptor(mode), b"password").unwrap();
        let repo = AppendRepository::new(
            std::sync::Arc::new(Remote::new(1).store()),
            anchor.clone(),
            b"password",
        )
        .unwrap();
        let (key, bytes) = repo.seal_block(b"small block").unwrap();
        let env: Envelope = Envelope::from_wire(&bytes).unwrap();
        let binding = BlockBinding {
            block_id: env.logical_id.clone(),
            len: env.plaintext_len,
            object: ObjectRef {
                kind: "block".into(),
                digest: hex::encode(sha2::Sha256::digest(&bytes)),
            },
        };
        assert!(key.contains("/blocks/"));
        assert_eq!(
            repo.open_block_with_ref(&bytes, Some(&binding))
                .unwrap()
                .as_slice(),
            b"small block"
        );
        assert!(repo.open_block(&bytes).is_ok());
        let mut changed = Envelope::from_wire(&bytes).unwrap();
        changed.payload[0] ^= 1;
        let changed = changed.to_wire().unwrap();
        assert!(repo.open_block(&changed).is_err());
        let mut bad_binding = binding.clone();
        bad_binding.len += 1;
        assert!(
            repo.open_block_with_ref(&bytes, Some(&bad_binding))
                .is_err()
        );
        let mut bad_nonce = Envelope::from_wire(&bytes).unwrap();
        bad_nonce.nonce = Some("00".into());
        assert!(
            repo.open_block(&bad_nonce.to_wire().unwrap())
                .is_err()
        );
    }
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
use peerdav_lib::sync::append_types::{TrustBinding, TrustSource};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Default)]
struct AnchorStore {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    gets: Arc<AtomicUsize>,
    puts: Arc<AtomicUsize>,
    deletes: Arc<AtomicUsize>,
    fail_list_after: Option<usize>,
}
impl AnchorStore {
    fn insert(&self, key: String, bytes: Vec<u8>) {
        self.objects.lock().unwrap().insert(key, bytes);
    }
    fn counts(&self) -> (usize, usize, usize) {
        (
            self.gets.load(Ordering::SeqCst),
            self.puts.load(Ordering::SeqCst),
            self.deletes.load(Ordering::SeqCst),
        )
    }
    fn assert_read_only(&self) {
        let (_, puts, deletes) = self.counts();
        assert_eq!((puts, deletes), (0, 0));
    }
    fn failing_after(&self, n: usize) -> Self {
        Self {
            fail_list_after: Some(n),
            ..self.clone()
        }
    }
}
fn metadata(key: &str, bytes: &[u8]) -> ObjectMetadata {
    ObjectMetadata {
        key: key.into(),
        size: bytes.len() as u64,
        etag: None,
    }
}
impl ObjectStore for AnchorStore {
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::default()
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        let entries: Vec<_> = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| metadata(k, v))
            .collect();
        let fail = self.fail_list_after;
        Box::pin(stream::iter(entries.into_iter().enumerate().map(
            move |(i, item)| {
                if fail == Some(i) {
                    Err(CloudError::Storage {
                        source: object_store::Error::Generic {
                            store: "anchor-test",
                            source: Box::new(std::io::Error::other("list interrupted")),
                        },
                    })
                } else {
                    Ok(item)
                }
            },
        )))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .map(|v| metadata(key, v))
                .ok_or(CloudError::NotFound)
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        _range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let value = self.objects.lock().unwrap().get(key).cloned();
        Box::pin(async move {
            value
                .map(|v| Box::pin(stream::iter(vec![Ok(Bytes::from(v))])) as ObjectStream)
                .ok_or(CloudError::NotFound)
        })
    }
    fn put<'a>(
        &'a self,
        _key: &'a str,
        _body: ObjectStream,
        _condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(CloudError::Unsupported) })
    }
    fn delete<'a>(&'a self, _key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(CloudError::Unsupported) })
    }
    fn list_page<'a>(
        &'a self,
        _prefix: &'a str,
        _token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }
}

fn anchor_binding(mode: AppendMode, repo: &str, password: &[u8]) -> (TrustBinding, Vec<u8>) {
    let mut d = descriptor(mode);
    d.repo_id = repo.into();
    let (prepared, _) = AppendRepository::prepare_anchor(d, password).unwrap();
    let binding = TrustBinding {
        key: prepared.key().into(),
        repo_id: repo.into(),
        fingerprint: prepared.key().rsplit('-').next().unwrap().into(),
        mode: descriptor(AppendMode::Encrypted).mode,
        source: TrustSource::Unconfirmed,
    };
    (binding, prepared.bytes().to_vec())
}

#[tokio::test]
async fn anchor_discovery_keeps_multiple_roots_without_auto_selection() {
    let store = AnchorStore::default();
    let (mut a, bytes_a) = anchor_binding(
        AppendMode::Encrypted,
        "00112233445566778899aabbccddeeff",
        b"password",
    );
    let (b, bytes_b) = anchor_binding(
        AppendMode::Encrypted,
        "ffeeddccbbaa99887766554433221100",
        b"password",
    );
    a.mode = AppendMode::Encrypted;
    store.insert(a.key.clone(), bytes_a);
    store.insert(b.key.clone(), bytes_b);
    let candidates = AppendRepository::discover_anchors(&store).await.unwrap();
    assert_eq!(candidates.len(), 2);
    assert_ne!(candidates[0].repo_id, candidates[1].repo_id);
    store.assert_read_only();
}

#[tokio::test]
async fn unconfirmed_selection_is_rejected_without_extra_get() {
    let store = AnchorStore::default();
    let (binding, bytes) = anchor_binding(
        AppendMode::Encrypted,
        &descriptor(AppendMode::Encrypted).repo_id,
        b"password",
    );
    store.insert(binding.key.clone(), bytes);
    let before = store.counts().0;
    assert!(matches!(
        AppendRepository::open_selected(Arc::new(store.clone()), binding, b"password").await,
        Err(AppendError::TrustRejected)
    ));
    assert_eq!(store.counts().0, before);
    store.assert_read_only();
}

#[tokio::test]
async fn trusted_selection_rejects_wrong_fingerprint() {
    let store = AnchorStore::default();
    let (mut binding, bytes) = anchor_binding(
        AppendMode::Encrypted,
        &descriptor(AppendMode::Encrypted).repo_id,
        b"password",
    );
    binding.source = TrustSource::TrustedFingerprint;
    binding.fingerprint = "00".repeat(32);
    store.insert(binding.key.clone(), bytes);
    assert!(matches!(
        AppendRepository::open_selected(Arc::new(store.clone()), binding, b"password").await,
        Err(AppendError::FingerprintMismatch)
    ));
    store.assert_read_only();
}

#[tokio::test]
async fn trusted_selection_missing_anchor_preserves_pending_source() {
    let store = AnchorStore::default();
    let (mut binding, _) = anchor_binding(
        AppendMode::Encrypted,
        &descriptor(AppendMode::Encrypted).repo_id,
        b"password",
    );
    binding.source = TrustSource::TrustedFingerprint;
    let result =
        AppendRepository::open_selected(Arc::new(store.clone()), binding, b"password").await;
    assert!(matches!(
        result,
        Err(AppendError::PendingDependency(CloudError::NotFound))
    ));
    store.assert_read_only();
}

#[tokio::test]
async fn trusted_and_accepted_tofu_bindings_open_successfully() {
    for source in [TrustSource::TrustedFingerprint, TrustSource::AcceptedTofu] {
        let store = AnchorStore::default();
        let (mut binding, bytes) = anchor_binding(
            AppendMode::AuthenticatedPlain,
            &descriptor(AppendMode::AuthenticatedPlain).repo_id,
            b"password",
        );
        binding.source = source;
        binding.mode = AppendMode::AuthenticatedPlain;
        store.insert(binding.key.clone(), bytes);
        assert!(
            AppendRepository::open_selected(Arc::new(store.clone()), binding, b"password")
                .await
                .is_ok()
        );
        store.assert_read_only();
    }
}

#[tokio::test]
async fn wrong_password_does_not_write_or_delete() {
    let store = AnchorStore::default();
    let (mut binding, bytes) = anchor_binding(
        AppendMode::Encrypted,
        &descriptor(AppendMode::Encrypted).repo_id,
        b"password",
    );
    binding.source = TrustSource::TrustedFingerprint;
    store.insert(binding.key.clone(), bytes);
    assert!(matches!(
        AppendRepository::open_selected(Arc::new(store.clone()), binding, b"wrong").await,
        Err(AppendError::Authentication)
    ));
    store.assert_read_only();
}

#[tokio::test]
async fn interrupted_anchor_listing_does_not_return_complete_candidates() {
    let base = AnchorStore::default();
    let (a, bytes_a) = anchor_binding(
        AppendMode::Encrypted,
        &descriptor(AppendMode::Encrypted).repo_id,
        b"password",
    );
    let (b, bytes_b) = anchor_binding(
        AppendMode::Encrypted,
        "ffeeddccbbaa99887766554433221100",
        b"password",
    );
    base.insert(a.key, bytes_a);
    base.insert(b.key, bytes_b);
    let store = base.failing_after(1);
    assert!(matches!(
        AppendRepository::discover_anchors(&store).await,
        Err(AppendError::Storage(_))
    ));
    store.assert_read_only();
}

#[tokio::test]
async fn encrypted_same_block_has_same_id_and_distinct_locator() {
    let (anchor, _) =
        AppendRepository::create_anchor(descriptor(AppendMode::Encrypted), b"password").unwrap();
    let repo = AppendRepository::new(
        std::sync::Arc::new(Remote::new(1).store()),
        anchor,
        b"password",
    )
    .unwrap();
    let (key_a, wire_a) = repo.seal_block(b"same").unwrap();
    let (key_b, wire_b) = repo.seal_block(b"same").unwrap();
    let a: Envelope = Envelope::from_wire(&wire_a).unwrap();
    let b: Envelope = Envelope::from_wire(&wire_b).unwrap();
    assert_eq!(a.logical_id, b.logical_id);
    assert_ne!(key_a, key_b);
    assert_ne!(wire_a, wire_b);

    let binding = |wire: &[u8], env: &Envelope| BlockBinding {
        block_id: env.logical_id.clone(),
        len: env.plaintext_len,
        object: ObjectRef {
            kind: "block".into(),
            digest: hex::encode(sha2::Sha256::digest(wire)),
        },
    };
    let tree_a = sample_tree(repo.repo_id(), binding(&wire_a, &a));
    let tree_b = sample_tree(repo.repo_id(), binding(&wire_b, &b));
    // Physical random ciphertext locators never enter v1 logical revisions.
    assert_eq!(tree_a.entries, tree_b.entries);
    assert_ne!(tree_a.bindings, tree_b.bindings);
    let (ref_a, bytes_a) = repo.seal_tree(&tree_a).unwrap();
    let (ref_b, bytes_b) = repo.seal_tree(&tree_b).unwrap();
    assert_eq!(
        repo.open_tree(&bytes_a, &ref_a).unwrap().entries,
        repo.open_tree(&bytes_b, &ref_b).unwrap().entries
    );
}

#[tokio::test]
async fn plain_same_block_has_same_logical_id_key_and_wire() {
    let (remote, repo) = repository(AppendMode::AuthenticatedPlain);
    let a = repo.prepare_block(b"same").await.unwrap();
    let b = repo.prepare_block(b"same").await.unwrap();
    let env_a: Envelope = Envelope::from_wire(a.bytes()).unwrap();
    let env_b: Envelope = Envelope::from_wire(b.bytes()).unwrap();
    assert_eq!(env_a.logical_id, env_b.logical_id);
    assert_eq!(a.key(), b.key());
    assert_eq!(a.bytes(), b.bytes());
    assert_eq!(repo.open_block(a.bytes()).unwrap().as_slice(), b"same");
    assert!(remote.requests().is_empty());
}

#[test]
fn tree_accepts_multiple_authenticated_locators_and_repeated_file_references() {
    let (remote, repo) = repository(AppendMode::Encrypted);
    let mut bindings = Vec::new();
    for _ in 0..2 {
        let (_, wire) = repo.seal_block(b"shared").unwrap();
        let env: Envelope = Envelope::from_wire(&wire).unwrap();
        let binding = BlockBinding {
            block_id: env.logical_id,
            len: env.plaintext_len,
            object: ObjectRef {
                kind: "block".into(),
                digest: hex::encode(sha2::Sha256::digest(&wire)),
            },
        };
        assert_eq!(
            repo.open_block_with_ref(&wire, Some(&binding))
                .unwrap()
                .as_slice(),
            b"shared"
        );
        bindings.push(binding);
    }
    assert_eq!(bindings[0].block_id, bindings[1].block_id);
    assert_eq!(bindings[0].len, bindings[1].len);
    assert_ne!(bindings[0].object, bindings[1].object);
    let mut tree = sample_tree(repo.repo_id(), bindings[0].clone());
    // Two files, each referencing the same block twice, share both locators.
    for file in ["duplicate-a", "duplicate-b"] {
        tree.entries.push(
            merge::revision(LocalChange {
                entity: EntityId(file.into()),
                change_id: ChangeId(file.into()),
                parents: vec![],
                value: RevisionValue::Live {
                    parent_dir: EntityId("root".into()),
                    name: file.into(),
                    node: Node::File(FileNode {
                        blocks: vec![
                            BlockRef {
                                id: bindings[0].block_id.clone(),
                                len: bindings[0].len
                            };
                            2
                        ],
                        length: bindings[0].len * 2,
                        digest: "0".repeat(64),
                    }),
                },
            })
            .unwrap(),
        );
    }
    tree.bindings = vec![
        bindings[1].clone(),
        bindings[0].clone(),
        bindings[0].clone(),
    ];
    let (reference, wire) = repo.seal_tree(&tree).unwrap();
    let decoded = repo.open_tree(&wire, &reference).unwrap();
    assert_eq!(decoded.entries.len(), 4);
    assert_eq!(decoded.bindings.len(), 2);
    assert!(decoded.bindings[0].object.digest < decoded.bindings[1].object.digest);
    assert!(remote.requests().is_empty());
}

#[test]
fn unchanged_tree_reuses_authenticated_reference_and_frozen_metadata_bytes() {
    for mode in [AppendMode::Encrypted, AppendMode::AuthenticatedPlain] {
        let (remote, repo) = repository(mode.clone());
        let (_, block) = repo.seal_block(b"unchanged").unwrap();
        let env: Envelope = Envelope::from_wire(&block).unwrap();
        let binding = BlockBinding {
            block_id: env.logical_id,
            len: env.plaintext_len,
            object: ObjectRef {
                kind: "block".into(),
                digest: hex::encode(sha2::Sha256::digest(&block)),
            },
        };
        let tree = sample_tree(repo.repo_id(), binding);
        let (reference, wire) = repo.seal_tree(&tree).unwrap();
        let authenticated = repo.open_tree(&wire, &reference).unwrap();
        let frozen_ref = reference.clone();
        let frozen_wire = wire.clone();
        // Reuse the already authenticated reference in two later commits;
        // do not re-encrypt an unchanged subtree just to obtain another root.
        for operation in ["11".repeat(16), "22".repeat(16)] {
            let commit = Commit {
                version: 2,
                repo_id: repo.repo_id().into(),
                operation_id: operation,
                parents: vec![],
                root: frozen_ref.clone(),
            };
            let (commit_ref, commit_wire) = repo.seal_commit(&commit).unwrap();
            let opened = repo
                .open_commit(
                    &commit_wire,
                    &ObjectRef {
                        kind: "commit".into(),
                        digest: commit_ref.digest,
                    },
                )
                .unwrap();
            assert_eq!(opened.root, reference);
            assert_eq!(
                repo.open_tree(&frozen_wire, &opened.root).unwrap(),
                authenticated
            );
        }
        assert_eq!(frozen_wire, wire);
        let (resealed_ref, resealed_wire) = repo.seal_tree(&authenticated).unwrap();
        match mode {
            AppendMode::AuthenticatedPlain => {
                assert_eq!(resealed_ref, reference);
                assert_eq!(resealed_wire, wire);
            }
            AppendMode::Encrypted => {
                assert_ne!(resealed_ref, reference);
                assert_ne!(resealed_wire, wire);
                assert_eq!(
                    repo.open_tree(&resealed_wire, &resealed_ref).unwrap(),
                    authenticated
                );
            }
        }
        assert!(remote.requests().is_empty());
    }
}
