use bytes::Bytes;
use futures_util::{StreamExt, future::BoxFuture, stream};
use peerdav_lib::cloud::{
    traits::ObjectStore,
    types::{
        CloudError, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, StorageCapabilities,
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Vec<u8>>,
    requests: Vec<Request>,
    page_size: usize,
    visible: Option<Vec<String>>,
    get_visible: Option<Vec<String>>,
    drop_next_put: bool,
    fail_list_page: Option<usize>,
    duplicate_list_page: Option<usize>,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub operation: &'static str,
    pub key: String,
    pub bytes: Vec<u8>,
    pub condition: Option<PutCondition>,
}
#[derive(Clone, Default)]
pub struct Remote(Arc<Mutex<State>>);
impl Remote {
    pub fn new(page_size: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            page_size: page_size.max(1),
            ..State::default()
        })))
    }
    pub fn reopen(&self) -> Self {
        self.clone()
    }
    pub fn put_object(&self, key: &str, bytes: &[u8]) {
        self.0
            .lock()
            .unwrap()
            .objects
            .insert(key.into(), bytes.into());
    }
    pub fn set_visible_keys(&self, keys: Option<Vec<String>>) {
        self.0.lock().unwrap().visible = keys;
    }
    pub fn drop_next_put_response(&self) {
        self.0.lock().unwrap().drop_next_put = true;
    }
    pub fn set_get_visible_keys(&self, keys: Option<Vec<String>>) {
        self.0.lock().unwrap().get_visible = keys;
    }
    pub fn fail_list_page(&self, page: usize) {
        self.0.lock().unwrap().fail_list_page = Some(page);
    }
    pub fn duplicate_list_page(&self, page: usize) {
        self.0.lock().unwrap().duplicate_list_page = Some(page);
    }
    pub fn requests(&self) -> Vec<Request> {
        self.0.lock().unwrap().requests.clone()
    }
    pub fn bytes_for(&self, operation: &'static str) -> Vec<Vec<u8>> {
        self.requests()
            .into_iter()
            .filter(|r| r.operation == operation)
            .map(|r| r.bytes)
            .collect()
    }
    pub fn store(&self) -> FakeStore {
        FakeStore(self.clone())
    }
}
#[derive(Clone)]
pub struct FakeStore(Remote);
fn storage(message: String) -> CloudError {
    CloudError::Storage {
        source: object_store::Error::Generic {
            store: "append-fake",
            source: Box::new(std::io::Error::other(message)),
        },
    }
}
impl ObjectStore for FakeStore {
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities {
            create_only: true,
            conditional_replace: true,
            range_read: true,
            conditional_writes_verified: true,
        }
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        let (keys, size) = {
            let s = self.0.0.lock().unwrap();
            let mut k: Vec<_> = s
                .objects
                .keys()
                .filter(|x| x.starts_with(prefix))
                .cloned()
                .collect();
            if let Some(v) = &s.visible {
                k.retain(|x| v.contains(x));
            }
            (k, s.page_size)
        };
        let remote = self.0.clone();
        let prefix = prefix.to_owned();
        Box::pin(stream::unfold(
            (keys, size, 0usize, Vec::new(), remote, prefix),
            |(keys, size, index, mut pending, remote, prefix)| async move {
                loop {
                    if let Some(item) = pending.pop() {
                        return Some((item, (keys, size, index, pending, remote, prefix)));
                    }
                    let start = index * size;
                    if start >= keys.len() {
                        if index == 0 {
                            remote.0.lock().unwrap().requests.push(Request {
                                operation: "list",
                                key: format!("{prefix}#page=1"),
                                bytes: Vec::new(),
                                condition: None,
                            });
                        }
                        return None;
                    }
                    let page = index + 1;
                    let chunk = keys[start..(start + size).min(keys.len())].to_vec();
                    let (fail, dup) = {
                        let mut s = remote.0.lock().unwrap();
                        s.requests.push(Request {
                            operation: "list",
                            key: format!("{prefix}#page={page}"),
                            bytes: Vec::new(),
                            condition: None,
                        });
                        (
                            s.fail_list_page == Some(page),
                            s.duplicate_list_page == Some(page),
                        )
                    };
                    if fail {
                        pending.push(Err(storage(format!("list page {page} failed"))));
                        return Some((
                            pending.pop().unwrap(),
                            (keys.clone(), size, keys.len(), pending, remote, prefix),
                        ));
                    }
                    let mut vals = chunk
                        .iter()
                        .map(|key| {
                            let s = remote.0.lock().unwrap();
                            Ok(meta(key, &s.objects[key]))
                        })
                        .collect::<Vec<_>>();
                    if dup {
                        remote.0.lock().unwrap().requests.push(Request {
                            operation: "list",
                            key: format!("{prefix}#page={page}:repeat"),
                            bytes: Vec::new(),
                            condition: None,
                        });
                        vals.extend(chunk.iter().map(|key| {
                            let s = remote.0.lock().unwrap();
                            Ok(meta(key, &s.objects[key]))
                        }));
                    }
                    vals.reverse();
                    pending = vals;
                    // Continue only to consume this fetched page; no next request occurs until all items are polled.
                    return Some((
                        pending.pop().unwrap(),
                        (keys, size, index + 1, pending, remote, prefix),
                    ));
                }
            },
        ))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let mut s = self.0.0.lock().unwrap();
            let r = s
                .objects
                .get(key)
                .map(|b| meta(key, b))
                .ok_or(CloudError::NotFound);
            s.requests.push(Request {
                operation: "stat",
                key: key.into(),
                bytes: Vec::new(),
                condition: None,
            });
            r
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            let mut s = self.0.0.lock().unwrap();
            let allowed = s
                .get_visible
                .as_ref()
                .is_none_or(|v| v.iter().any(|x| x == key));
            let Some(b) = s.objects.get(key).cloned() else {
                s.requests.push(Request {
                    operation: "get",
                    key: key.into(),
                    bytes: Vec::new(),
                    condition: None,
                });
                return Err(CloudError::NotFound);
            };
            if !allowed {
                s.requests.push(Request {
                    operation: "get",
                    key: key.into(),
                    bytes: Vec::new(),
                    condition: None,
                });
                return Err(CloudError::NotFound);
            }
            let bytes = range
                .map(|r| b[r.start as usize..r.end.min(b.len() as u64) as usize].to_vec())
                .unwrap_or(b);
            s.requests.push(Request {
                operation: "get",
                key: key.into(),
                bytes: bytes.clone(),
                condition: None,
            });
            Ok(Box::pin(stream::once(async move { Ok(Bytes::from(bytes)) })) as ObjectStream)
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            while let Some(p) = body.next().await {
                bytes.extend_from_slice(&p?);
            }
            let mut s = self.0.0.lock().unwrap();
            s.requests.push(Request {
                operation: "put",
                key: key.into(),
                bytes: bytes.clone(),
                condition: Some(condition.clone()),
            });
            let old = s.objects.get(key);
            if matches!(condition, PutCondition::CreateOnly) && old.is_some()
                || matches!(&condition, PutCondition::MatchEtag(e) if old.map(|b| etag(b)) != Some(e.clone()))
            {
                return Err(CloudError::Precondition);
            }
            s.objects.insert(key.into(), bytes.clone());
            if s.drop_next_put {
                s.drop_next_put = false;
                return Err(storage("PUT response was lost after remote write".into()));
            }
            Ok(meta(key, &bytes))
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            let mut s = self.0.0.lock().unwrap();
            s.requests.push(Request {
                operation: "delete",
                key: key.into(),
                bytes: Vec::new(),
                condition: None,
            });
            s.objects.remove(key);
            Ok(())
        })
    }
}
fn etag(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    hex::encode(h.finalize())
}
fn meta(key: &str, b: &[u8]) -> ObjectMetadata {
    ObjectMetadata {
        key: key.into(),
        size: b.len() as u64,
        etag: Some(etag(b)),
    }
}
