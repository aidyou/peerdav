#[allow(dead_code)]
mod append_common;
use append_common::Remote;
use peerdav_lib::sync::{
    append_index::AppendIndex, append_publisher::AppendPublisher,
    append_repository::AppendRepository, append_types::*,
};
use std::sync::{Arc, atomic::AtomicBool};

pub(crate) fn fixture() -> (
    tempfile::TempDir,
    AppendIndex,
    Remote,
    AppendRepository,
    Vec<PreparedObject>,
    ObjectRef,
) {
    let dir = tempfile::tempdir().unwrap();
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
    let (anchor, _) = AppendRepository::create_anchor(descriptor, b"password").unwrap();
    let repo = AppendRepository::new(Arc::new(remote.store()), anchor, b"password").unwrap();
    let index = AppendIndex::open(
        &dir.path().join("index"),
        zeroize::Zeroizing::new([7; 32]),
        repo.repo_id().into(),
    )
    .unwrap();
    let (block_key, block) = repo.seal_block(b"publication payload").unwrap();
    let tree = Tree {
        version: 2,
        repo_id: repo.repo_id().into(),
        directory: "root".into(),
        entries: vec![],
        children: vec![],
        bindings: vec![{
            let envelope: Envelope = serde_json::from_slice(&block).unwrap();
            BlockBinding {
                block_id: envelope.logical_id,
                len: envelope.plaintext_len,
                object: ObjectRef {
                    kind: "block".into(),
                    digest: block_key.rsplit('/').next().unwrap().into(),
                },
            }
        }],
    };
    let (root, tree_bytes) = repo.seal_tree(&tree).unwrap();
    let id = "11".repeat(16);
    let (reference, commit) = repo
        .seal_commit(&Commit {
            version: 2,
            repo_id: repo.repo_id().into(),
            operation_id: id.clone(),
            parents: vec![],
            root: root.clone(),
        })
        .unwrap();
    let objects = vec![
        PreparedObject::new(block_key, block),
        PreparedObject::new(
            format!("v2/{}/trees/{}", repo.repo_id(), root.digest),
            tree_bytes,
        ),
        PreparedObject::new(
            format!("v2/{}/commits/{}-{}", repo.repo_id(), id, reference.digest),
            commit,
        ),
    ];
    (dir, index, remote, repo, objects, root)
}
#[tokio::test]
async fn durable_prepare_order_any_only_and_steady_state() {
    let (_dir, mut index, remote, repo, objects, root) = fixture();
    let id = "11".repeat(16);
    assert!(remote.requests().is_empty());
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), &objects).unwrap();
    assert!(remote.requests().is_empty());
    assert_eq!(
        index.frozen_operation(&id).unwrap().unwrap().state,
        OperationState::Prepared
    );
    let cancel = AtomicBool::new(false);
    assert_eq!(
        AppendPublisher::resume(&mut index, &remote.store(), &id, &cancel)
            .await
            .unwrap(),
        Some(OperationState::Published)
    );
    let budget = index.discovery_budget(&id).unwrap();
    assert_eq!(budget.requests, objects.len() * 2);
    assert_eq!(
        budget.bytes_read,
        objects.iter().map(|o| o.bytes().len()).sum::<usize>()
    );
    let requests = remote.requests();
    let puts: Vec<_> = requests.iter().filter(|r| r.operation == "put").collect();
    assert_eq!(puts.len(), objects.len());
    for (put, frozen) in puts.iter().zip(&objects) {
        assert_eq!(put.key, frozen.key());
        assert_eq!(put.bytes, frozen.bytes());
        assert!(matches!(
            put.condition,
            Some(peerdav_lib::cloud::types::PutCondition::Any)
        ));
    }
    assert!(
        requests
            .iter()
            .all(|r| matches!(r.operation, "get" | "put"))
    );
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
    AppendPublisher::resume(&mut index, &remote.store(), &id, &cancel)
        .await
        .unwrap();
    assert_eq!(remote.requests().len(), requests.len());
}
#[tokio::test]
async fn no_changes_and_cancel_do_not_publish() {
    let (_dir, mut index, remote, repo, objects, root) = fixture();
    let id = "11".repeat(16);
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), &[]).unwrap();
    assert!(
        AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(false))
            .await
            .unwrap()
            .is_none()
    );
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
    assert!(matches!(
        AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(true)).await,
        Err(PublishError::Cancelled)
    ));
    assert!(remote.requests().is_empty());
    assert_eq!(
        index.frozen_operation(&id).unwrap().unwrap().state,
        OperationState::Prepared
    );
}
#[test]
fn authenticated_missing_dependencies_and_tight_limits_fail_before_freeze() {
    let (_dir, mut index, remote, repo, objects, root) = fixture();
    let id = "11".repeat(16);
    for closure in [&objects[1..], &objects[2..]] {
        assert!(
            AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), closure).is_err()
        );
        assert!(index.frozen_operation(&id).unwrap().is_none());
    }
    let total: u64 = objects.iter().map(|o| o.bytes().len() as u64).sum();
    for limits in [
        PublishLimits {
            max_objects: 2,
            max_staging: total,
        },
        PublishLimits {
            max_objects: 3,
            max_staging: total - 1,
        },
    ] {
        assert!(
            AppendPublisher::prepare_with_limits(
                &mut index,
                &repo,
                &id,
                &[],
                root.clone(),
                &objects,
                &limits
            )
            .is_err()
        );
        assert!(index.frozen_operation(&id).unwrap().is_none());
    }
    assert!(remote.requests().is_empty());
    AppendPublisher::prepare_with_limits(
        &mut index,
        &repo,
        &id,
        &[],
        root,
        &objects,
        &PublishLimits {
            max_objects: 3,
            max_staging: total,
        },
    )
    .unwrap();
}

#[test]
fn authenticated_binding_identity_and_length_mismatch_fail_before_freeze() {
    for bad_length in [false, true] {
        let (_dir, mut index, remote, repo, objects, old_root) = fixture();
        let mut tree = repo.open_tree(objects[1].bytes(), &old_root).unwrap();
        if bad_length {
            tree.bindings[0].len += 1;
        } else {
            tree.bindings[0].block_id = "ab".repeat(32);
        }
        let (root, bytes) = repo.seal_tree(&tree).unwrap();
        let id = "11".repeat(16);
        let (reference, commit) = repo
            .seal_commit(&Commit {
                version: 2,
                repo_id: repo.repo_id().into(),
                operation_id: id.clone(),
                parents: vec![],
                root: root.clone(),
            })
            .unwrap();
        let closure = vec![
            objects[0].clone(),
            PreparedObject::new(
                format!("v2/{}/trees/{}", repo.repo_id(), root.digest),
                bytes,
            ),
            PreparedObject::new(
                format!("v2/{}/commits/{}-{}", repo.repo_id(), id, reference.digest),
                commit,
            ),
        ];
        assert!(AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &closure).is_err());
        assert!(index.frozen_operation(&id).unwrap().is_none());
        assert!(remote.requests().is_empty());
    }
}

#[test]
fn missing_child_duplicate_keys_unreachable_objects_and_intent_are_rejected() {
    let (_dir, mut index, remote, repo, objects, original_root) = fixture();
    let id = "11".repeat(16);
    let mut tree = repo.open_tree(objects[1].bytes(), &original_root).unwrap();
    tree.children.push(DirectoryRef {
        directory: "missing".into(),
        object: ObjectRef {
            kind: "tree".into(),
            digest: "ab".repeat(32),
        },
    });
    let (root, wire) = repo.seal_tree(&tree).unwrap();
    let (reference, commit) = repo
        .seal_commit(&Commit {
            version: 2,
            repo_id: repo.repo_id().into(),
            operation_id: id.clone(),
            parents: vec![],
            root: root.clone(),
        })
        .unwrap();
    let closure = vec![
        objects[0].clone(),
        PreparedObject::new(format!("v2/{}/trees/{}", repo.repo_id(), root.digest), wire),
        PreparedObject::new(
            format!("v2/{}/commits/{}-{}", repo.repo_id(), id, reference.digest),
            commit,
        ),
    ];
    assert!(AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &closure).is_err());
    let mut duplicate = objects.clone();
    duplicate.insert(
        0,
        PreparedObject::new(objects[0].key().into(), b"different bytes".to_vec()),
    );
    assert!(
        AppendPublisher::prepare(
            &mut index,
            &repo,
            &id,
            &[],
            original_root.clone(),
            &duplicate
        )
        .is_err()
    );
    let mut unreachable = objects.clone();
    let (key, wire) = repo.seal_block(b"unreachable").unwrap();
    unreachable.insert(0, PreparedObject::new(key, wire));
    assert!(
        AppendPublisher::prepare(
            &mut index,
            &repo,
            &id,
            &[],
            original_root.clone(),
            &unreachable
        )
        .is_err()
    );
    assert!(
        AppendPublisher::prepare(
            &mut index,
            &repo,
            &id,
            &["unapproved parent".into()],
            original_root.clone(),
            &objects
        )
        .is_err()
    );
    assert!(
        AppendPublisher::prepare(
            &mut index,
            &repo,
            &id,
            &[],
            ObjectRef {
                kind: "tree".into(),
                digest: "ab".repeat(32)
            },
            &objects
        )
        .is_err()
    );
    let mut foreign = objects.clone();
    foreign[0] = PreparedObject::new(
        objects[0]
            .key()
            .replacen(repo.repo_id(), &"ff".repeat(16), 1),
        objects[0].bytes().to_vec(),
    );
    assert!(
        AppendPublisher::prepare(&mut index, &repo, &id, &[], original_root, &foreign).is_err()
    );
    assert!(index.frozen_operation(&id).unwrap().is_none());
    assert!(remote.requests().is_empty());
}

#[test]
fn same_operation_different_bytes_and_wrong_keys_rejected() {
    let (_dir, mut index, remote, repo, mut objects, root) = fixture();
    let id = "11".repeat(16);
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), &objects).unwrap();
    let (key, bytes) = repo.seal_block(b"changed").unwrap();
    objects[0] = PreparedObject::new(key, bytes);
    assert!(AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), &objects).is_err());
    objects[0] = PreparedObject::new("wrong-key".into(), objects[0].bytes().to_vec());
    assert!(AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).is_err());
    assert!(remote.requests().is_empty());
}
