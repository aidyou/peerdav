#[allow(dead_code)]
mod append_common;
use append_common::Remote;
use peerdav_lib::sync::append_repository::AppendRepository;
use std::sync::Arc;

mod publish {
    use super::*;
    pub fn fixture() -> (
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
        let (key, bytes) = repo.seal_block(b"publication payload").unwrap();
        let envelope: Envelope = Envelope::from_wire(&bytes).unwrap();
        let tree = Tree {
            version: 2,
            repo_id: repo.repo_id().into(),
            directory: "root".into(),
            entries: vec![],
            children: vec![],
            bindings: vec![BlockBinding {
                block_id: envelope.logical_id,
                len: envelope.plaintext_len,
                object: ObjectRef {
                    kind: "block".into(),
                    digest: key.rsplit('/').next().unwrap().into(),
                },
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
            PreparedObject::new(key, bytes),
            PreparedObject::new(
                format!("{APPEND_ROOT}/{}/trees/{}", repo.repo_id(), root.digest),
                tree_bytes,
            ),
            PreparedObject::new(
                format!("{APPEND_ROOT}/{}/commits/{}-{}", repo.repo_id(), id, reference.digest),
                commit,
            ),
        ];
        (dir, index, remote, repo, objects, root)
    }
}
use futures_util::{StreamExt, future::BoxFuture};
use peerdav_lib::cloud::{
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, StorageCapabilities,
    },
};
use peerdav_lib::sync::{
    append_index::AppendIndex, append_publisher::AppendPublisher, append_types::*,
};
use std::sync::atomic::{AtomicBool, Ordering};

struct ScriptedStore {
    inner: append_common::FakeStore,
    target: String,
    pending: bool,
    probe_get: Option<(std::path::PathBuf, String, String, usize)>,
    fired: AtomicBool,
    attempts: std::sync::Mutex<Vec<Vec<u8>>>,
}
impl ObjectStore for ScriptedStore {
    fn capabilities(&self) -> StorageCapabilities {
        self.inner.capabilities()
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        self.inner.list(prefix)
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.inner.stat(key)
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<std::ops::Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            if let Some((path, repo, id, len)) = &self.probe_get {
                // This independent connection runs inside GET, before any response
                // exists: both ledger metrics must already be durably committed.
                let observer =
                    AppendIndex::open(path, zeroize::Zeroizing::new([7; 32]), repo.clone())
                        .unwrap();
                let budget = observer.discovery_budget(id).unwrap();
                assert_eq!(budget.bytes_read, budget.requests * len);
                assert_eq!(
                    observer.frozen_operation(id).unwrap().unwrap().requests as usize,
                    budget.requests
                );
                let db = rusqlite::Connection::open(path).unwrap();
                for (metric, expected) in
                    [("read", budget.bytes_read), ("requests", budget.requests)]
                {
                    let name = format!("discover:{}:{id}:{metric}", id.len());
                    let (used, reserved): (usize, usize) = db
                        .query_row(
                            "SELECT used,reserved FROM append_ledger WHERE name=?1",
                            [name],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .unwrap();
                    assert_eq!((used, reserved), (expected, expected));
                }
                self.fired.store(true, Ordering::SeqCst);
                if self.pending {
                    return std::future::pending().await;
                }
                return Err(CloudError::Unsupported);
            }
            self.inner.get(key, range).await
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        self.inner.delete(key)
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            while let Some(part) = body.next().await {
                bytes.extend_from_slice(&part?);
            }
            self.attempts.lock().unwrap().push(bytes.clone());
            if key == self.target && !self.fired.swap(true, Ordering::SeqCst) {
                if self.pending {
                    return std::future::pending().await;
                }
                return Err(CloudError::Unsupported);
            }
            self.inner
                .put(
                    key,
                    Box::pin(futures_util::stream::once(async move {
                        Ok(bytes::Bytes::from(bytes))
                    })),
                    condition,
                )
                .await
        })
    }
}

#[tokio::test]
async fn before_write_failure_and_pending_commit_drop_reopen_identical_bytes() {
    for pending in [false, true] {
        let (dir, mut index, remote, repo, objects, root) = publish::fixture();
        let id = "11".repeat(16);
        AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
        let frozen = index.frozen_operation(&id).unwrap().unwrap();
        let store = ScriptedStore {
            inner: remote.store(),
            target: objects[2].key().into(),
            pending,
            probe_get: None,
            fired: AtomicBool::new(false),
            attempts: std::sync::Mutex::new(vec![]),
        };
        let cancel = AtomicBool::new(false);
        if pending {
            let mut future = Box::pin(AppendPublisher::resume(&mut index, &store, &id, &cancel));
            assert!(matches!(
                futures_util::poll!(future.as_mut()),
                std::task::Poll::Pending
            ));
            assert!(store.fired.load(Ordering::SeqCst));
            drop(future);
        } else {
            assert!(matches!(
                AppendPublisher::resume(&mut index, &store, &id, &cancel).await,
                Err(PublishError::Cloud(CloudError::Unsupported))
            ));
        }
        let interrupted = index.frozen_operation(&id).unwrap().unwrap();
        assert_eq!(interrupted.state, OperationState::PublishedUnknown);
        assert_eq!(interrupted.next_object, 2);
        assert_eq!(interrupted.objects, frozen.objects);
        assert_eq!(interrupted.parents_frontier, frozen.parents_frontier);
        assert!(
            remote
                .requests()
                .iter()
                .all(|r| r.operation != "put" || r.key != objects[2].key())
        );
        drop(index);
        let mut index = AppendIndex::open(
            &dir.path().join("index"),
            zeroize::Zeroizing::new([7; 32]),
            repo.repo_id().into(),
        )
        .unwrap();
        assert_eq!(
            AppendPublisher::resume(&mut index, &store, &id, &cancel)
                .await
                .unwrap(),
            Some(OperationState::Published)
        );
        let attempts = store.attempts.lock().unwrap();
        assert_eq!(attempts.len(), 4);
        assert_eq!(attempts[2], objects[2].bytes());
        assert_eq!(attempts[3], objects[2].bytes());
        assert_eq!(index.frozen_operation(&id).unwrap().unwrap().requests, 8);
    }
}

#[tokio::test]
async fn lost_response_at_each_object_reopens_exact_bytes() {
    for fail_at in 0..3 {
        let (dir, mut index, remote, repo, objects, root) = publish::fixture();
        let id = "11".repeat(16);
        AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
        for object in &objects[..fail_at] {
            remote.put_object(object.key(), object.bytes());
        }
        remote.drop_next_put_response();
        assert!(
            AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(false))
                .await
                .is_err()
        );
        let state = index.frozen_operation(&id).unwrap().unwrap();
        assert_eq!(state.next_object, fail_at);
        if fail_at == 2 {
            assert_eq!(state.state, OperationState::PublishedUnknown);
        }
        drop(index);
        let mut index = AppendIndex::open(
            &dir.path().join("index"),
            zeroize::Zeroizing::new([7; 32]),
            repo.repo_id().into(),
        )
        .unwrap();
        assert!(matches!(
            AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(true)).await,
            Err(PublishError::Cancelled)
        ));
        assert_eq!(
            AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(false))
                .await
                .unwrap(),
            Some(OperationState::Published)
        );
        assert_eq!(
            index.discovery_budget(&id).unwrap().bytes_read,
            objects.iter().map(|o| o.bytes().len()).sum::<usize>() + objects[fail_at].bytes().len()
        );
        let requests = remote.requests();
        let puts: Vec<_> = requests.iter().filter(|r| r.operation == "put").collect();
        assert_eq!(puts.len(), 3 - fail_at);
        for put in puts {
            let object = objects.iter().find(|o| o.key() == put.key).unwrap();
            assert_eq!(put.bytes, object.bytes());
        }
        assert!(
            requests
                .iter()
                .any(|r| r.operation == "get" && r.key == objects[fail_at].key())
        );
    }
}
#[tokio::test]
async fn remote_wrong_bytes_are_rejected_without_overwrite() {
    let (_dir, mut index, remote, repo, objects, root) = publish::fixture();
    let id = "11".repeat(16);
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
    for wrong in [
        b"wrong bytes".to_vec(),
        objects[0].bytes()[..objects[0].bytes().len() - 1].to_vec(),
        [objects[0].bytes(), b"extra"].concat(),
        {
            let mut bytes = objects[0].bytes().to_vec();
            bytes[0] ^= 1;
            bytes
        },
    ] {
        remote.put_object(objects[0].key(), &wrong);
        assert!(matches!(
            AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(false))
                .await,
            Err(PublishError::Invalid(_))
        ));
        assert!(remote.requests().iter().all(|r| r.operation == "get"));
        assert_eq!(index.frozen_operation(&id).unwrap().unwrap().next_object, 0);
    }
    assert_eq!(
        index.discovery_budget(&id).unwrap().bytes_read,
        4 * objects[0].bytes().len()
    );
}
#[tokio::test]
async fn get_failure_and_dropped_future_keep_precommitted_read_on_reopen() {
    for pending in [false, true] {
        let (dir, mut index, remote, repo, objects, root) = publish::fixture();
        let id = "11".repeat(16);
        AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), &objects).unwrap();
        let frozen = index.frozen_operation(&id).unwrap().unwrap();
        let store = ScriptedStore {
            inner: remote.store(),
            target: String::new(),
            pending,
            probe_get: Some((
                dir.path().join("index"),
                repo.repo_id().into(),
                id.clone(),
                objects[0].bytes().len(),
            )),
            fired: AtomicBool::new(false),
            attempts: std::sync::Mutex::new(vec![]),
        };
        let cancel = AtomicBool::new(false);
        for attempt in 1..=2 {
            if pending {
                let mut future =
                    Box::pin(AppendPublisher::resume(&mut index, &store, &id, &cancel));
                assert!(matches!(
                    futures_util::poll!(future.as_mut()),
                    std::task::Poll::Pending
                ));
                drop(future);
            } else {
                assert!(matches!(
                    AppendPublisher::resume(&mut index, &store, &id, &cancel).await,
                    Err(PublishError::Cloud(CloudError::Unsupported))
                ));
            }
            assert!(store.fired.load(Ordering::SeqCst));
            drop(index);
            index = AppendIndex::open(
                &dir.path().join("index"),
                zeroize::Zeroizing::new([7; 32]),
                repo.repo_id().into(),
            )
            .unwrap();
            // Preparing the identical closure again cannot reset retry budgets.
            AppendPublisher::prepare(&mut index, &repo, &id, &[], root.clone(), &objects).unwrap();
            let budget = index.discovery_budget(&id).unwrap();
            assert_eq!(budget.requests, attempt);
            assert_eq!(budget.bytes_read, attempt * objects[0].bytes().len());
            let current = index.frozen_operation(&id).unwrap().unwrap();
            assert_eq!(current.next_object, 0);
            assert_eq!(current.objects, frozen.objects);
            assert_eq!(current.parents_frontier, frozen.parents_frontier);
            assert_eq!(current.root, frozen.root);
        }
        assert!(remote.requests().is_empty());
        assert!(store.attempts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn reservation_commit_failure_dispatches_no_get() {
    let (dir, mut index, remote, repo, objects, root) = publish::fixture();
    let id = "11".repeat(16);
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
    let db = rusqlite::Connection::open(dir.path().join("index")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_reservation BEFORE INSERT ON append_ledger BEGIN SELECT RAISE(ABORT, 'reservation unavailable'); END;").unwrap();
    assert!(
        AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(false))
            .await
            .is_err()
    );
    assert!(remote.requests().is_empty());
    assert_eq!(
        serde_json::to_value(index.discovery_budget(&id).unwrap()).unwrap(),
        serde_json::to_value(DiscoveryBudget::default()).unwrap()
    );
    assert_eq!(index.frozen_operation(&id).unwrap().unwrap().requests, 0);
}

#[tokio::test]
async fn missing_staging_fails_closed_before_requests() {
    let (dir, mut index, remote, repo, objects, root) = publish::fixture();
    let id = "11".repeat(16);
    AppendPublisher::prepare(&mut index, &repo, &id, &[], root, &objects).unwrap();
    let db = rusqlite::Connection::open(dir.path().join("index")).unwrap();
    db.execute("DELETE FROM append_records WHERE kind='publish-chunk'", [])
        .unwrap();
    assert!(
        AppendPublisher::resume(&mut index, &remote.store(), &id, &AtomicBool::new(false))
            .await
            .is_err()
    );
    assert!(remote.requests().is_empty());
    assert_eq!(
        index.frozen_operation(&id).unwrap().unwrap().state,
        OperationState::Prepared
    );
}
