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
    repository::Repository,
    repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode},
};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Fake {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    puts: Mutex<usize>,
    attempts: std::sync::atomic::AtomicUsize,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn count(&self) -> usize {
        *self.puts.lock().unwrap()
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
            let o = self.objects.lock().unwrap();
            o.get(key)
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
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut b = Vec::new();
            while let Some(x) = body.next().await {
                b.extend_from_slice(&x?);
            }
            let mut o = self.objects.lock().unwrap();
            if matches!(condition, PutCondition::CreateOnly) && o.contains_key(key) {
                return Err(CloudError::Precondition);
            }
            *self.puts.lock().unwrap() += 1;
            let size = b.len() as u64;
            o.insert(key.into(), b);
            Ok(ObjectMetadata {
                key: key.into(),
                size,
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

#[tokio::test]
async fn encrypted_repeat_uses_one_put_and_hides_plaintext_metadata() {
    let store = Fake::new();
    let repo = Repository::create(
        store.clone(),
        descriptor(RepositoryMode::Encrypted),
        b"pass",
    )
    .await
    .unwrap();
    let before = store.count();
    let block = b"same-block";
    let id = repo.put_block(block).await.unwrap();
    let attempts = store.attempts.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(repo.put_block(block).await.unwrap(), id);
    assert_eq!(
        store.attempts.load(std::sync::atomic::Ordering::SeqCst),
        attempts
    );
    assert_eq!(store.count(), before + 1);
    let marker = b"sensitive-file-name-marker";
    let mid = repo.put_metadata("tree", marker).await.unwrap();
    assert!(
        store
            .objects
            .lock()
            .unwrap()
            .keys()
            .any(|k| k == &format!("trees/{mid}"))
    );
    assert!(
        !store
            .objects
            .lock()
            .unwrap()
            .values()
            .any(|v| v.windows(marker.len()).any(|w| w == marker))
    );
    assert_eq!(
        repo.get_block(&id, block.len()).await.unwrap().as_slice(),
        block
    );
    assert_eq!(
        repo.get_metadata("tree", &mid).await.unwrap().as_slice(),
        marker
    );
    let joined = Repository::join(store.clone(), b"pass")
        .await
        .expect("second device join");
    assert_eq!(
        joined
            .get_block(&id, block.len())
            .await
            .expect("second device content")
            .as_slice(),
        block
    );
    let writes = store.count();
    assert!(Repository::join(store.clone(), b"wrong").await.is_err());
    assert_eq!(store.count(), writes);
}

#[tokio::test]
async fn plain_round_trip_and_unknown_objects_are_rejected() {
    let store = Fake::new();
    let repo = Repository::create(store.clone(), descriptor(RepositoryMode::Plain), b"")
        .await
        .unwrap();
    let id = repo.put_metadata("commit", b"commit").await.unwrap();
    assert_eq!(
        repo.get_metadata("commit", &id).await.unwrap().as_slice(),
        b"commit"
    );
    store
        .objects
        .lock()
        .unwrap()
        .insert("unknown/object".into(), vec![1]);
    assert!(
        Repository::create(store, descriptor(RepositoryMode::Plain), b"")
            .await
            .is_err()
    );
}
