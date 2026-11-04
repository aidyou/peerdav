use bytes::Bytes;
use futures_util::{StreamExt, stream};
use peerdav_lib::{
    cloud::{
        ObjectGuard, ObjectService, ObjectServiceError, PhysicalRoot, ProtectedIdentity,
        ProtectedMapping, StorageIdentityAssociation,
        traits::ObjectStore,
        types::{
            CloudError, ObjectListing, ObjectMetadata, ObjectPage, ObjectStoreScope, ObjectStream,
            PutCondition, StorageCapabilities,
        },
    },
    sync::repository_types::{ChunkerProfile, Head, RepoDescriptor, RepositoryMode, encode_head},
};
use std::{
    ops::Range,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tempfile::tempdir;

#[derive(Default)]
struct Calls {
    list: usize,
    upload: usize,
    put: usize,
    stat: usize,
    get: usize,
    deletes: Vec<String>,
    cancelled: bool,
    delete_fail: Option<String>,
    upload_keys: Vec<String>,
    stat_keys: Vec<String>,
    list_prefixes: Vec<String>,
    complete_unknown: bool,
}
struct FakeStore {
    scope: Option<ObjectStoreScope>,
    pages: Mutex<Vec<ObjectPage>>,
    objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    calls: Arc<Mutex<Calls>>,
}
impl FakeStore {
    fn new(pages: Vec<ObjectPage>) -> (Arc<Self>, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        (
            Arc::new(Self {
                scope: Some(scope("")),
                pages: Mutex::new(pages),
                objects: Mutex::new(std::collections::HashMap::new()),
                calls: calls.clone(),
            }),
            calls,
        )
    }
    fn meta(key: &str) -> ObjectMetadata {
        ObjectMetadata {
            key: key.into(),
            size: if key == "control/HEAD" {
                Self::head_bytes().len() as u64
            } else {
                1
            },
            etag: Some(format!("etag-{key}")),
        }
    }
    fn head_bytes() -> Vec<u8> {
        encode_head(&Head {
            descriptor: RepoDescriptor {
                version: 1,
                repo_id: "repo".into(),
                mode: RepositoryMode::Plain,
                chunker: ChunkerProfile {
                    name: "fixed".into(),
                    version: "1".into(),
                    normalization: "none".into(),
                    level: 0,
                    min: 1,
                    average: 1,
                    max: 1,
                    seed: 0,
                },
            },
            generation: 0,
            commit: None,
            wrapped_master: None,
            auth: "auth".into(),
        })
        .unwrap()
    }
}
impl ObjectStore for FakeStore {
    fn scope(&self) -> Option<ObjectStoreScope> {
        self.scope.clone()
    }
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::default()
    }
    fn list<'a>(&'a self, _prefix: &'a str) -> ObjectListing<'a> {
        stream::empty().boxed()
    }
    fn stat<'a>(
        &'a self,
        key: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().stat += 1;
            self.calls.lock().unwrap().stat_keys.push(key.into());
            let found = self
                .pages
                .lock()
                .unwrap()
                .iter()
                .flat_map(|p| p.objects.iter())
                .find(|m| m.key == key)
                .cloned();
            found.ok_or(CloudError::NotFound)
        })
    }
    fn get<'a>(
        &'a self,
        _key: &'a str,
        _range: Option<Range<u64>>,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().get += 1;
            let bytes = self
                .objects
                .lock()
                .unwrap()
                .get(_key)
                .cloned()
                .unwrap_or_else(|| {
                    if _key == "control/HEAD" {
                        Self::head_bytes()
                    } else {
                        b"x".to_vec()
                    }
                });
            Ok(stream::once(async move { Ok(Bytes::from(bytes)) }).boxed())
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        mut body: ObjectStream,
        _condition: PutCondition,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().put += 1;
            while body.next().await.is_some() {}
            Ok(Self::meta(key))
        })
    }
    fn delete<'a>(
        &'a self,
        key: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            let mut calls = self.calls.lock().unwrap();
            if calls
                .delete_fail
                .as_deref()
                .is_some_and(|failed| failed == key)
            {
                return Err(CloudError::Precondition);
            }
            calls.deletes.push(key.into());
            Ok(())
        })
    }
    fn list_page<'a>(
        &'a self,
        _prefix: &'a str,
        token: Option<&'a str>,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move {
            let pages = self.pages.lock().unwrap();
            self.calls.lock().unwrap().list += 1;
            self.calls
                .lock()
                .unwrap()
                .list_prefixes
                .push(_prefix.into());
            let index = token.and_then(|x| x.parse::<usize>().ok()).unwrap_or(0);
            pages.get(index).cloned().ok_or(CloudError::NotFound)
        })
    }
    fn upload_stream_with_control<'a>(
        &'a self,
        _key: &'a str,
        mut body: ObjectStream,
        cancel: Arc<AtomicBool>,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().upload += 1;
            self.calls.lock().unwrap().upload_keys.push(_key.into());
            if cancel.load(Ordering::SeqCst) {
                self.calls.lock().unwrap().cancelled = true;
                return Err(CloudError::UploadCancelled);
            }
            while body.next().await.is_some() {}
            if self.calls.lock().unwrap().complete_unknown {
                return Err(CloudError::CompleteUnknown {
                    source: object_store::Error::Generic {
                        store: "object-service-fixture",
                        source: Box::new(std::io::Error::other("completion response lost")),
                    },
                });
            }
            Ok(Self::meta("multipart"))
        })
    }
}

#[derive(Clone)]
enum DownloadMode {
    Good,
    StreamError,
    CancelAfterFirst,
    ChangedEtag,
    ChangedSize,
    CancelAtEnd(Arc<AtomicBool>),
    CommitCollision(std::path::PathBuf),
    FileCollision(std::path::PathBuf),
    CleanupFailure(std::path::PathBuf),
}

struct DownloadStore {
    calls: Arc<Mutex<DownloadCalls>>,
    bytes: Vec<u8>,
    declared_size: u64,
    mode: DownloadMode,
}
#[derive(Default)]
struct DownloadCalls {
    stat: usize,
    get: usize,
    range: Option<Range<u64>>,
}
impl DownloadStore {
    fn new(
        bytes: &[u8],
        declared_size: u64,
        mode: DownloadMode,
    ) -> (Arc<Self>, Arc<Mutex<DownloadCalls>>) {
        let calls = Arc::new(Mutex::new(DownloadCalls::default()));
        (
            Arc::new(Self {
                calls: calls.clone(),
                bytes: bytes.to_vec(),
                declared_size,
                mode,
            }),
            calls,
        )
    }
}
impl ObjectStore for DownloadStore {
    fn scope(&self) -> Option<ObjectStoreScope> {
        Some(scope("download"))
    }
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::default()
    }
    fn list<'a>(&'a self, _prefix: &'a str) -> ObjectListing<'a> {
        stream::once(async { Err(CloudError::Unsupported) }).boxed()
    }
    fn stat<'a>(
        &'a self,
        key: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let mut calls = self.calls.lock().unwrap();
            calls.stat += 1;
            Ok(ObjectMetadata {
                key: key.into(),
                size: self.declared_size
                    + u64::from(matches!(self.mode, DownloadMode::ChangedSize) && calls.stat > 1),
                etag: Some(
                    if matches!(self.mode, DownloadMode::ChangedEtag) && calls.stat > 1 {
                        "changed"
                    } else {
                        "original"
                    }
                    .into(),
                ),
            })
        })
    }
    fn get<'a>(
        &'a self,
        _key: &'a str,
        range: Option<Range<u64>>,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            let mut calls = self.calls.lock().unwrap();
            calls.get += 1;
            calls.range = range;
            drop(calls);
            let first = Bytes::from(self.bytes.clone());
            let mode = self.mode.clone();
            let stream = match mode {
                DownloadMode::Good | DownloadMode::ChangedEtag | DownloadMode::ChangedSize => {
                    stream::once(async move { Ok(first) }).boxed()
                }
                DownloadMode::CancelAtEnd(cancel) => {
                    let content = if first.is_empty() {
                        stream::empty().boxed()
                    } else {
                        stream::once(async move { Ok(first) }).boxed()
                    };
                    content
                        .chain(stream::poll_fn(move |_| {
                            cancel.store(true, Ordering::SeqCst);
                            std::task::Poll::Ready(None)
                        }))
                        .boxed()
                }
                DownloadMode::CommitCollision(path) => stream::once(async move {
                    std::fs::create_dir(path).unwrap();
                    Ok(first)
                })
                .boxed(),
                DownloadMode::FileCollision(path) => stream::once(async move {
                    std::fs::write(path, b"racing writer").unwrap();
                    Ok(first)
                })
                .boxed(),
                DownloadMode::CleanupFailure(root) => stream::once(async move {
                    let staging = std::fs::read_dir(root)
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .find(|path| {
                            path.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".peerdav-")
                        })
                        .unwrap();
                    std::fs::remove_file(&staging).unwrap();
                    std::fs::create_dir(&staging).unwrap();
                    Err(CloudError::Unsupported)
                })
                .boxed(),
                DownloadMode::StreamError => {
                    stream::iter(vec![Ok(first), Err(CloudError::Unsupported)]).boxed()
                }
                DownloadMode::CancelAfterFirst => {
                    stream::iter(vec![Ok(first), Ok(Bytes::from_static(b"late"))]).boxed()
                }
            };
            Ok(stream)
        })
    }
    fn put<'a>(
        &'a self,
        _key: &'a str,
        _body: ObjectStream,
        _condition: PutCondition,
    ) -> futures_util::future::BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }
    fn delete<'a>(
        &'a self,
        _key: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }
}

fn assert_no_staging(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".peerdav-")
        );
    }
}

#[test]
fn selected_directory_symlink_is_rejected_including_trailing_separator() {
    let parent = tempdir().unwrap();
    let real = parent.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = parent.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(
        PhysicalRoot::open(&link),
        Err(ObjectServiceError::InvalidLocalCapability)
    ));
    assert!(matches!(
        PhysicalRoot::open(format!("{}/", link.display())),
        Err(ObjectServiceError::InvalidLocalCapability)
    ));
}

#[tokio::test]
async fn download_changed_source_etag_preserves_target_and_removes_staging() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("out"), b"old").unwrap();
    let (store, calls) = DownloadStore::new(b"new", 3, DownloadMode::ChangedEtag);
    let err = download_service(store, d.path())
        .download_to("remote/object", "out", None, true, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ObjectServiceError::Cloud(CloudError::Precondition)
    ));
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
    assert_eq!(calls.lock().unwrap().stat, 2);
    assert_eq!(calls.lock().unwrap().get, 1);
    assert_no_staging(d.path());
}

#[tokio::test]
async fn download_changed_source_size_preserves_target_and_removes_staging() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("out"), b"old").unwrap();
    let (store, _) = DownloadStore::new(b"new", 3, DownloadMode::ChangedSize);
    let err = download_service(store, d.path())
        .download_to("remote/object", "out", None, true, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ObjectServiceError::Cloud(CloudError::Precondition)
    ));
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
    assert_no_staging(d.path());
}

#[tokio::test]
async fn download_cancel_at_stream_end_preserves_target_for_empty_and_nonempty_content() {
    for bytes in [b"".as_slice(), b"new".as_slice()] {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("out"), b"old").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (store, _) = DownloadStore::new(
            bytes,
            bytes.len() as u64,
            DownloadMode::CancelAtEnd(cancel.clone()),
        );
        let err = download_service(store, d.path())
            .download_to("remote/object", "out", Some(cancel), true, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ObjectServiceError::Cancelled));
        assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
        assert_no_staging(d.path());
    }
}

#[tokio::test]
async fn download_commit_failure_cleans_staging_and_preserves_destination_directory() {
    for overwrite in [false, true] {
        let d = tempdir().unwrap();
        let target = d.path().join("out");
        let (store, _) =
            DownloadStore::new(b"new", 3, DownloadMode::CommitCollision(target.clone()));
        let err = download_service(store, d.path())
            .download_to("remote/object", "out", None, overwrite, None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ObjectServiceError::Io { .. } | ObjectServiceError::DestinationExists
        ));
        assert!(target.is_dir());
        assert_no_staging(d.path());
    }
}

#[tokio::test]
async fn download_racing_file_after_preflight_never_overwrites_without_confirmation() {
    let d = tempdir().unwrap();
    let target = d.path().join("out");
    let root = PhysicalRoot::open(d.path()).unwrap();
    assert!(!root.relative_target_exists("out").unwrap());
    let (store, _) = DownloadStore::new(b"new", 3, DownloadMode::FileCollision(target.clone()));
    assert!(matches!(
        download_service(store, d.path())
            .download_to("remote/object", "out", None, false, None)
            .await,
        Err(ObjectServiceError::DestinationExists)
    ));
    assert_eq!(std::fs::read(target).unwrap(), b"racing writer");
    assert_no_staging(d.path());
}

#[tokio::test]
async fn download_ordinary_overwrite_requires_explicit_confirmation() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("out"), b"old").unwrap();
    let (store, _) = DownloadStore::new(b"new", 3, DownloadMode::Good);
    let svc = download_service(store, d.path());
    assert!(matches!(
        svc.download_to("remote/object", "out", None, false, None).await,
        Err(ObjectServiceError::DestinationExists)
    ));
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
    svc.download_to("remote/object", "out", None, true, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"new");
    assert_no_staging(d.path());
}

#[tokio::test]
async fn plan_rejects_out_of_prefix_control_head_and_sibling() {
    for key in ["control/HEAD", "root-other/a"] {
        let (store, calls) = FakeStore::new(vec![page(&["root/a"], None)]);
        let svc = service(store.clone(), "");
        svc.discover("root").await.unwrap();
        *store.pages.lock().unwrap() = vec![page(&[key], None)];
        assert!(matches!(
            svc.delete_plan("root", 4_000_000_000).await,
            Err(ObjectServiceError::StaleDeletePlan)
        ));
        assert!(calls.lock().unwrap().deletes.is_empty());
    }
}

#[tokio::test]
async fn empty_root_browse_and_confirmed_delete_work_for_ordinary_objects() {
    let (store, calls) = FakeStore::new(vec![page(&["folder/a", "folder/b"], None)]);
    let svc = service(store, "");
    svc.discover("").await.unwrap();
    let listed = svc.browse("", None).await.unwrap();
    assert_eq!(listed.prefix, "");
    assert_eq!(listed.objects.len(), 2);
    // 根目录禁止整体删除：必须按目录逐个删除条目。
    assert!(matches!(
        svc.delete_plan("", 4_000_000_000).await,
        Err(ObjectServiceError::RootDeleteForbidden)
    ));
    let plan = svc.delete_plan("folder/", 4_000_000_000).await.unwrap();
    assert_eq!(svc.execute_delete(&plan, true).await.unwrap().len(), 2);
    assert_eq!(calls.lock().unwrap().deletes, vec!["folder/a", "folder/b"]);
}

#[tokio::test]
async fn folder_prefix_with_trailing_separator_browses_like_the_store_does() {
    // The UI navigates to folder prefixes such as "probes/"; the service must
    // trim the trailing separator exactly like the store, not reject the key.
    let (store, _calls) = FakeStore::new(vec![page(&["probes/abc", "probes/def"], None)]);
    let svc = service(store, "");
    let listed = svc.browse("probes/", None).await.unwrap();
    assert_eq!(listed.objects.len(), 2);
}

#[tokio::test]
async fn root_sync_marker_blocks_empty_root_delete() {
    let (store, calls) = FakeStore::new(vec![page(&["control/HEAD", "a"], None)]);
    let svc = service(store, "");
    svc.discover("").await.unwrap();
    assert_eq!(svc.browse("", None).await.unwrap().objects.len(), 2);
    // 根目录整体删除被安全规则拒绝，不会进入保护检查与列举。
    assert!(matches!(
        svc.delete_plan("", 4_000_000_000).await,
        Err(ObjectServiceError::RootDeleteForbidden)
    ));
    assert!(calls.lock().unwrap().deletes.is_empty());
}

#[tokio::test]
async fn multipage_delete_plan_accepts_complete_final_page() {
    let (store, _) = FakeStore::new(vec![page(&["root/a"], Some("1")), page(&["root/b"], None)]);
    let svc = service(store, "");
    svc.discover("root").await.unwrap();
    let plan = svc.delete_plan("root", 4_000_000_000).await.unwrap();
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(svc.execute_delete(&plan, true).await.unwrap().len(), 2);
}

#[tokio::test]
async fn page_budget_is_checked_before_another_store_request() {
    let pages = (0..1001)
        .map(|index| page(&[], Some(&(index + 1).to_string())))
        .collect();
    let (store, calls) = FakeStore::new(pages);
    let svc = service(store, "");
    let mut next = None;
    for _ in 0..1000 {
        next = svc.browse("", next.as_deref()).await.unwrap().next;
    }
    assert!(matches!(
        svc.browse("", next.as_deref()).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    // Omitting the continuation cannot reset an incomplete operation.
    assert!(matches!(
        svc.browse("", None).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(calls.lock().unwrap().list, 1000);
}

#[tokio::test]
async fn independent_completed_operations_do_not_exhaust_the_connection() {
    let (store, calls) = FakeStore::new(vec![page(&[], None)]);
    let svc = service(store, "");
    for _ in 0..1001 {
        svc.discover("").await.unwrap();
        assert!(svc.browse("", None).await.unwrap().complete);
        assert!(
            svc.delete_plan("a", 4_000_000_000)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }
    assert_eq!(calls.lock().unwrap().list, 3003);
}

#[tokio::test]
async fn complete_thousand_page_traversals_have_independent_budgets() {
    let pages = (0..1000)
        .map(|index| {
            if index == 999 {
                page(&[], None)
            } else {
                page(&[], Some(&(index + 1).to_string()))
            }
        })
        .collect();
    let (store, calls) = FakeStore::new(pages);
    let svc = service(store, "");
    for _ in 0..2 {
        svc.discover("").await.unwrap();
        assert!(
            svc.delete_plan("a", 4_000_000_000)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }
    assert_eq!(calls.lock().unwrap().list, 4000);
}

#[tokio::test]
async fn single_traversals_reject_100001_raw_entries_without_deduplication() {
    // Direct in-memory metadata; no producer or second copy of an existing store.
    let oversized = ObjectPage {
        objects: (0..100_001).map(|_| FakeStore::meta("a")).collect(),
        next_token: None,
    };
    let (store, calls) = FakeStore::new(vec![page(&[], None)]);
    let svc = service(store.clone(), "");
    svc.discover("").await.unwrap();
    *store.pages.lock().unwrap() = vec![oversized];
    assert!(matches!(
        svc.discover("").await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert!(matches!(
        svc.delete_plan("a", 4_000_000_000).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert!(matches!(
        svc.browse("", None).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert!(calls.lock().unwrap().deletes.is_empty());
}

#[tokio::test]
async fn exact_entry_limit_is_per_traversal_and_remaining_budget_is_explicit() {
    use peerdav_lib::cloud::object_service::BudgetLedger;
    let (store, _) = FakeStore::new(vec![ObjectPage {
        objects: (0..100_000).map(|_| FakeStore::meta("a")).collect(),
        next_token: None,
    }]);
    let svc = service(store, "");
    for _ in 0..2 {
        svc.discover("").await.unwrap();
        let plan = svc.delete_plan("a", 4_000_000_000).await.unwrap();
        assert_eq!(plan.entries.len(), 100_000);
        assert_eq!(svc.delete_plan_snapshot(&plan.id).unwrap().len(), 100_000);
    }
    let mut ledger = BudgetLedger::default();
    assert!(
        svc.browse_with_budget("", None, &mut ledger)
            .await
            .unwrap()
            .complete
    );
    assert_eq!(ledger.remaining_requests(), 999);
    assert_eq!(ledger.remaining_entries(), 0);
    assert!(matches!(
        svc.browse_with_budget("", None, &mut ledger).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
}

#[tokio::test]
async fn delete_plan_rejects_thousandth_nonterminal_page_without_poisoning_discovery() {
    let (store, calls) = FakeStore::new(vec![page(&[], None)]);
    let svc = service(store.clone(), "");
    svc.discover("").await.unwrap();
    *store.pages.lock().unwrap() = (0..1000)
        .map(|index| page(&[], Some(&(index + 1).to_string())))
        .collect();
    assert!(matches!(
        svc.delete_plan("a", 4_000_000_000).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(calls.lock().unwrap().list, 1001);
    *store.pages.lock().unwrap() = vec![page(&[], None)];
    svc.discover("").await.unwrap();
    assert!(
        svc.delete_plan("a", 4_000_000_000)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(calls.lock().unwrap().deletes.is_empty());
}

#[tokio::test]
async fn browse_chain_counts_raw_entries_across_pages() {
    let (store, calls) = FakeStore::new(vec![
        ObjectPage {
            objects: (0..60_000).map(|_| FakeStore::meta("a")).collect(),
            next_token: Some("1".into()),
        },
        ObjectPage {
            objects: (0..40_001).map(|_| FakeStore::meta("a")).collect(),
            next_token: None,
        },
    ]);
    let svc = service(store, "");
    let first = svc.browse("", None).await.unwrap();
    assert!(matches!(
        svc.browse("", first.next.as_deref()).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert!(matches!(
        svc.browse("", first.next.as_deref()).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(calls.lock().unwrap().list, 2);
}

#[tokio::test]
async fn retained_operation_ledger_charges_retries_and_reports_remaining_budget() {
    use peerdav_lib::cloud::object_service::BudgetLedger;
    let (store, calls) = FakeStore::new(Vec::new());
    let svc = service(store, "");
    let mut ledger = BudgetLedger::default();
    assert_eq!(ledger.remaining_requests(), 1000);
    assert_eq!(ledger.remaining_entries(), 100_000);
    for _ in 0..1000 {
        assert!(matches!(
            svc.browse_with_budget("", None, &mut ledger).await,
            Err(ObjectServiceError::Cloud(CloudError::NotFound))
        ));
    }
    assert!(matches!(
        svc.browse_with_budget("", None, &mut ledger).await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(ledger.remaining_requests(), 0);
    assert_eq!(ledger.remaining_entries(), 100_000);
    assert_eq!(calls.lock().unwrap().list, 1000);
}

#[tokio::test]
async fn issued_snapshot_is_exact_immutable_relative_and_has_no_remote_reads() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[83; 32]).unwrap();
    let view = identity("physical/data", "browse");
    guard
        .register_storage_identity(&view, StorageIdentityAssociation::New)
        .unwrap();
    let (mut store, calls) = FakeStore::new(vec![ObjectPage {
        objects: vec![
            ObjectMetadata {
                key: "folder/file".into(),
                size: 42,
                etag: Some("listed".into()),
            },
            ObjectMetadata {
                key: "folder/file".into(),
                size: 43,
                etag: None,
            },
        ],
        next_token: None,
    }]);
    Arc::get_mut(&mut store).unwrap().scope = Some(scope("physical/data"));
    let svc = ObjectService::try_new(store.clone(), guard, view, "rev").unwrap();
    svc.discover("").await.unwrap();
    assert!(matches!(
        svc.delete_plan_snapshot("forged"),
        Err(ObjectServiceError::StaleDeletePlan)
    ));
    let plan = svc.delete_plan("folder", 4_000_000_000).await.unwrap();
    let reads = {
        let calls = calls.lock().unwrap();
        (calls.list, calls.stat, calls.get)
    };
    store.pages.lock().unwrap()[0].objects[0].size = 999;
    let snapshot = svc.delete_plan_snapshot(&plan.id).unwrap();
    assert_eq!(
        snapshot,
        vec![
            ("folder/file".into(), 42, Some("listed".into())),
            ("folder/file".into(), 43, None)
        ]
    );
    assert_eq!(snapshot.len(), plan.entries.len());
    assert_eq!(svc.delete_plan_snapshot(&plan.id).unwrap(), snapshot);
    let calls = calls.lock().unwrap();
    assert_eq!((calls.list, calls.stat, calls.get), reads);
    assert_eq!(
        calls.list_prefixes.last().map(String::as_str),
        Some("folder")
    );
}

#[tokio::test]
async fn issued_snapshot_rejects_oversized_escaped_metadata_before_output_copy() {
    let (store, calls) = FakeStore::new(vec![ObjectPage {
        objects: vec![ObjectMetadata {
            key: "file".into(),
            size: 1,
            etag: Some("\u{1}".repeat(6 * 1024 * 1024)),
        }],
        next_token: None,
    }]);
    let svc = service(store, "");
    svc.discover("").await.unwrap();
    let plan = svc.delete_plan("file", 4_000_000_000).await.unwrap();
    assert!(matches!(
        svc.delete_plan_snapshot(&plan.id),
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(calls.lock().unwrap().stat, 0);
}

#[tokio::test]
async fn protection_query_uses_physical_data_prefix_without_granting_trust() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[81; 32]).unwrap();
    guard
        .register(ProtectedMapping {
            identity: identity("data/repo", "sync"),
            repository_id: "repository".into(),
        })
        .unwrap();
    let (mut store, calls) = FakeStore::new(vec![page(&[], None)]);
    Arc::get_mut(&mut store).unwrap().scope = Some(scope("data"));
    let svc =
        ObjectService::try_new(store, guard.clone(), identity("data", "browse"), "rev").unwrap();
    let epoch = guard.registry_epoch().unwrap();
    assert!(svc.is_protected("repo/child").unwrap());
    assert!(!svc.is_protected("repo-other").unwrap());
    assert!(!svc.is_protected("ordinary").unwrap());
    assert_eq!(guard.registry_epoch().unwrap(), epoch);
    assert_eq!(guard.mappings().unwrap().len(), 1);
    assert!(
        guard
            .begin_mutation(&identity("data/ordinary", "browse"))
            .is_err()
    );
    let calls = calls.lock().unwrap();
    assert_eq!(
        (calls.list, calls.get, calls.stat, calls.upload, calls.put),
        (0, 0, 0, 0, 0)
    );
    assert!(calls.deletes.is_empty());
}

#[test]
fn protection_query_reports_ancestors_and_validates_path_segments() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[82; 32]).unwrap();
    guard
        .register(ProtectedMapping {
            identity: identity("data/parent/repo", "sync"),
            repository_id: "repository".into(),
        })
        .unwrap();
    let (mut store, _) = FakeStore::new(vec![]);
    Arc::get_mut(&mut store).unwrap().scope = Some(scope("data"));
    let svc = ObjectService::try_new(store, guard, identity("data", "browse"), "rev").unwrap();
    assert!(svc.is_protected("").unwrap());
    assert!(svc.is_protected("parent").unwrap());
    assert!(!svc.is_protected("parent-other").unwrap());
    for invalid in [
        "/parent",
        "parent/",
        "parent//repo",
        "parent/../repo",
        "parent/./repo",
        "parent\\repo",
        "a:b",
        "a\0b",
    ] {
        assert!(matches!(
            svc.is_protected(invalid),
            Err(ObjectServiceError::InvalidKey)
        ));
    }
}

#[tokio::test]
async fn delete_without_etag_rejects_size_change_but_accepts_unchanged_size() {
    let mut metadata = FakeStore::meta("a");
    metadata.etag = None;
    let (store, calls) = FakeStore::new(vec![ObjectPage {
        objects: vec![metadata],
        next_token: None,
    }]);
    let svc = service(store.clone(), "");
    svc.discover("").await.unwrap();
    let stale = svc.delete_plan("a", 4_000_000_000).await.unwrap();
    store.pages.lock().unwrap()[0].objects[0].size = 2;
    assert!(matches!(
        svc.execute_delete(&stale, true).await,
        Err(ObjectServiceError::StaleDeletePlan)
    ));
    assert!(calls.lock().unwrap().deletes.is_empty());
    let current = svc.delete_plan("a", 4_000_000_000).await.unwrap();
    assert_eq!(svc.execute_delete(&current, true).await.unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap().deletes, vec!["a"]);
}

#[tokio::test]
async fn cleanup_failure_preserves_primary_and_cleanup_errors() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("out"), b"old").unwrap();
    let (store, _) = DownloadStore::new(
        b"new",
        3,
        DownloadMode::CleanupFailure(d.path().to_path_buf()),
    );
    let error = download_service(store, d.path())
        .download_to("remote/object", "out", None, true, None)
        .await
        .unwrap_err();
    match error {
        ObjectServiceError::Cleanup { original, source } => {
            assert!(matches!(
                *original,
                ObjectServiceError::Cloud(CloudError::Unsupported)
            ));
            assert!(source.raw_os_error().is_some());
        }
        other => panic!("expected both errors, got {other:?}"),
    }
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
}

#[tokio::test]
async fn upload_ordinary_overwrite_requires_matching_explicit_etag() {
    let (store, calls) = FakeStore::new(vec![page(&["root/a"], None)]);
    let svc = service(store, "");
    svc.discover("root").await.unwrap();
    for confirmation in [None, Some("wrong".to_owned())] {
        assert!(matches!(
            svc.upload_stream_with_control(
                "root/a",
                body(),
                Arc::new(AtomicBool::new(false)),
                confirmation
            )
            .await,
            Err(ObjectServiceError::Cloud(CloudError::Precondition))
        ));
    }
    assert_eq!(calls.lock().unwrap().upload, 0);
    svc.upload_stream_with_control(
        "root/a",
        body(),
        Arc::new(AtomicBool::new(false)),
        Some("etag-root/a".into()),
    )
    .await
    .unwrap();
    assert_eq!(calls.lock().unwrap().upload, 1);
}

fn download_service(store: Arc<dyn ObjectStore>, root: &Path) -> ObjectService {
    let guard = ObjectGuard::open(root.join("registry.db"), &[8u8; 32]).unwrap();
    ObjectService::try_new(store, guard, identity("download", "alias"), "rev")
        .unwrap()
        .with_physical_root(PhysicalRoot::open(root).unwrap())
}
fn scope(prefix: &str) -> ObjectStoreScope {
    ObjectStoreScope {
        endpoint: "https://example.test".into(),
        bucket: "bucket".into(),
        prefix: prefix.into(),
    }
}
fn identity(prefix: &str, alias: &str) -> ProtectedIdentity {
    ProtectedIdentity {
        endpoint: "https://example.test:443/".into(),
        bucket: "bucket".into(),
        prefix: prefix.into(),
        alias: alias.into(),
    }
}
fn service(store: Arc<dyn ObjectStore>, _relative_prefix: &str) -> ObjectService {
    let d = tempdir().unwrap();
    let db = d.path().join("registry.db");
    // Keep the temporary directory alive for the service's lifetime via a leaked fixture path.
    let _ = Box::leak(Box::new(d));
    let guard = ObjectGuard::open(db, &[9u8; 32]).unwrap();
    let scope = store.scope().unwrap();
    let identity = ProtectedIdentity {
        endpoint: scope.endpoint,
        bucket: scope.bucket,
        prefix: scope.prefix,
        alias: "alias".into(),
    };
    guard
        .register_storage_identity(&identity, StorageIdentityAssociation::New)
        .unwrap();
    ObjectService::try_new(store, guard, identity, "rev-1").unwrap()
}
fn page(keys: &[&str], next: Option<&str>) -> ObjectPage {
    ObjectPage {
        objects: keys.iter().map(|k| FakeStore::meta(k)).collect(),
        next_token: next.map(str::to_owned),
    }
}
fn body() -> ObjectStream {
    stream::once(async { Ok(Bytes::from_static(b"payload")) }).boxed()
}

fn append_marker(mode: peerdav_lib::sync::append_types::AppendMode) -> (String, Vec<u8>) {
    use peerdav_lib::sync::{
        append_repository::AppendRepository,
        append_types::{Chunker, Descriptor},
    };
    let (prepared, binding) = AppendRepository::prepare_anchor(
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
        },
        b"fixture-password",
    )
    .unwrap();
    assert_eq!(
        binding.source,
        peerdav_lib::sync::append_types::TrustSource::Unconfirmed
    );
    (prepared.key().into(), prepared.bytes().to_vec())
}

fn install_marker(store: &FakeStore, key: &str, bytes: Vec<u8>) {
    let size = bytes.len() as u64;
    store.objects.lock().unwrap().insert(key.into(), bytes);
    for object in store
        .pages
        .lock()
        .unwrap()
        .iter_mut()
        .flat_map(|p| &mut p.objects)
    {
        if object.key == key {
            object.size = size;
        }
    }
}

fn discovery_fixture(store: Arc<FakeStore>) -> (ObjectService, ObjectGuard, tempfile::TempDir) {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[73; 32]).unwrap();
    guard
        .register_storage_identity(&identity("", "explicit"), StorageIdentityAssociation::New)
        .unwrap();
    let service =
        ObjectService::try_new(store, guard.clone(), identity("", "browser"), "revision").unwrap();
    (service, guard, directory)
}

#[tokio::test]
async fn ensure_identity_reuses_exact_scope_and_discovers_relative_root() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[91; 32]).unwrap();
    let scope = identity("scope", "browse");
    let physical_id = guard.ensure_storage_identity(&scope).unwrap();
    // The same endpoint and bucket always resolve to the same local identity,
    // regardless of the prefix or display alias.
    assert_eq!(
        guard
            .ensure_storage_identity(&identity("scope/sub", "other"))
            .unwrap(),
        physical_id
    );
    let mut other = identity("scope", "browse");
    other.bucket = "other-bucket".into();
    let other_id = guard.ensure_storage_identity(&other).unwrap();
    assert_ne!(other_id, physical_id);
    assert!(!guard.has_completed_discovery(&scope).unwrap());
    let (mut store, _calls) = FakeStore::new(vec![page(&[], None)]);
    Arc::get_mut(&mut store).unwrap().scope = Some(ObjectStoreScope {
        endpoint: "https://example.test:443/".into(),
        bucket: "bucket".into(),
        prefix: "scope".into(),
    });
    let service = ObjectService::try_new(store, guard.clone(), scope.clone(), "revision").unwrap();
    // `discover` takes a prefix relative to the connection scope.
    service.discover("").await.unwrap();
    assert!(guard.has_completed_discovery(&scope).unwrap());
    assert!(
        guard
            .has_completed_discovery(&identity("scope/sub", "child"))
            .unwrap()
    );
    assert!(!guard.has_completed_discovery(&other).unwrap());
}

async fn assert_marker_protection(mode: peerdav_lib::sync::append_types::AppendMode) {
    let (relative, bytes) = append_marker(mode);
    for root in ["", "nested/repository"] {
        let key = if root.is_empty() {
            relative.clone()
        } else {
            format!("{root}/{relative}")
        };
        let (store, calls) = FakeStore::new(vec![page(&[&key], None)]);
        install_marker(&store, &key, bytes.clone());
        let (svc, guard, directory) = discovery_fixture(store);
        svc.discover("").await.unwrap();
        assert!(
            guard
                .is_protected(&identity(root, "different-alias"))
                .unwrap()
        );
        let target = if root.is_empty() {
            "file".into()
        } else {
            format!("{root}/file")
        };
        assert!(matches!(
            svc.upload_stream_with_control(&target, body(), Arc::new(AtomicBool::new(false)), None)
                .await,
            Err(ObjectServiceError::Guard(_))
        ));
        assert_eq!(calls.lock().unwrap().stat, 0);
        assert_eq!(calls.lock().unwrap().upload, 0);
        let mappings = guard.mappings().unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].identity.prefix, root);
        drop(svc);
        drop(guard);
        let reopened = ObjectGuard::open(directory.path().join("guard.db"), &[73; 32]).unwrap();
        assert!(
            reopened
                .is_protected(&identity(&target, "reopened"))
                .unwrap()
        );
    }
}

#[tokio::test]
async fn encrypted_v2_anchor_protects_each_discovered_root() {
    assert_marker_protection(peerdav_lib::sync::append_types::AppendMode::Encrypted).await;
}

#[tokio::test]
async fn authenticated_plain_v2_anchor_protects_each_discovered_root() {
    assert_marker_protection(peerdav_lib::sync::append_types::AppendMode::AuthenticatedPlain).await;
}

async fn assert_invalid_marker_still_protects(case: &str) {
    use peerdav_lib::sync::append_types::APPEND_ANCHORS;
    use sha2::{Digest, Sha256};
    let (mut relative, mut bytes) =
        append_marker(peerdav_lib::sync::append_types::AppendMode::Encrypted);
    match case {
        "version" => {
            let mut anchor: peerdav_lib::sync::append_types::Anchor =
                serde_json::from_slice(&bytes).unwrap();
            anchor.descriptor.version = 99;
            bytes = serde_json::to_vec(&anchor).unwrap();
            relative = format!(
                "{APPEND_ANCHORS}{}-{}",
                anchor.descriptor.repo_id,
                hex::encode(Sha256::digest(&bytes))
            );
        }
        "repo" => {
            relative = relative.replacen(
                "00112233445566778899aabbccddeeff",
                "ffeeddccbbaa99887766554433221100",
                1,
            )
        }
        "digest" => {
            relative = format!(
                "{APPEND_ANCHORS}00112233445566778899aabbccddeeff-{}",
                "0".repeat(64)
            )
        }
        "noncanonical" => {
            let anchor: peerdav_lib::sync::append_types::Anchor =
                serde_json::from_slice(&bytes).unwrap();
            bytes = serde_json::to_vec_pretty(&anchor).unwrap();
            relative = format!(
                "{APPEND_ANCHORS}{}-{}",
                anchor.descriptor.repo_id,
                hex::encode(Sha256::digest(&bytes))
            );
        }
        "listed-size" | "stream-size" => bytes.resize(64 * 1024 + 1, b' '),
        _ => panic!("unknown fixture case"),
    }
    let key = format!("repo/{relative}");
    let (store, calls) = FakeStore::new(vec![page(&[&key], None)]);
    install_marker(&store, &key, bytes);
    if case == "stream-size" {
        store.pages.lock().unwrap()[0].objects[0].size = 1;
    }
    let (svc, guard, _directory) = discovery_fixture(store);
    assert!(matches!(
        svc.discover("").await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert!(guard.is_protected(&identity("repo/file", "probe")).unwrap());
    assert_eq!(guard.mappings().unwrap().len(), 1);
    assert!(
        guard
            .begin_mutation(&identity("ordinary/file", "probe"))
            .is_err()
    );
    assert!(
        svc.upload_stream_with_control("repo/file", body(), Arc::new(AtomicBool::new(false)), None)
            .await
            .is_err()
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.get, usize::from(case != "listed-size"));
    assert_eq!((calls.stat, calls.upload), (0, 0));
}

#[tokio::test]
async fn unknown_anchor_version_preregisters_protection() {
    assert_invalid_marker_still_protects("version").await;
}
#[tokio::test]
async fn wrong_anchor_repo_id_preregisters_protection() {
    assert_invalid_marker_still_protects("repo").await;
}
#[tokio::test]
async fn wrong_anchor_digest_preregisters_protection() {
    assert_invalid_marker_still_protects("digest").await;
}
#[tokio::test]
async fn noncanonical_anchor_preregisters_protection() {
    assert_invalid_marker_still_protects("noncanonical").await;
}
#[tokio::test]
async fn oversized_marker_metadata_preregisters_protection_without_get() {
    assert_invalid_marker_still_protects("listed-size").await;
}
#[tokio::test]
async fn oversized_marker_actual_bytes_preregister_protection() {
    assert_invalid_marker_still_protects("stream-size").await;
}

#[tokio::test]
async fn second_page_append_marker_protects_root() {
    let (relative, bytes) =
        append_marker(peerdav_lib::sync::append_types::AppendMode::AuthenticatedPlain);
    let key = format!("repo/{relative}");
    let (store, calls) = FakeStore::new(vec![
        page(&["ordinary/file"], Some("1")),
        page(&[&key], None),
    ]);
    install_marker(&store, &key, bytes);
    let (svc, guard, _directory) = discovery_fixture(store);
    svc.discover("").await.unwrap();
    assert_eq!(calls.lock().unwrap().list, 2);
    assert!(guard.is_protected(&identity("repo/file", "probe")).unwrap());
    assert!(
        guard
            .begin_mutation(&identity("ordinary/file", "probe"))
            .is_ok()
    );
    assert!(svc.delete_plan("repo", 4_000_000_000).await.is_err());
    assert!(calls.lock().unwrap().deletes.is_empty());
}

#[tokio::test]
async fn internal_discovery_token_cycle_never_authorizes_mutation() {
    let (store, calls) = FakeStore::new(vec![
        page(&[], Some("1")),
        page(&[], Some("2")),
        page(&[], Some("1")),
    ]);
    let (svc, guard, _directory) = discovery_fixture(store);
    assert!(matches!(
        svc.discover("").await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(calls.lock().unwrap().list, 3);
    assert!(
        guard
            .begin_mutation(&identity("ordinary/file", "probe"))
            .is_err()
    );
    assert!(
        svc.upload_stream_with_control(
            "ordinary/file",
            body(),
            Arc::new(AtomicBool::new(false)),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(calls.lock().unwrap().stat, 0);
}

#[tokio::test]
async fn thousandth_nonterminal_discovery_page_never_authorizes_mutation() {
    let pages = (0..1000)
        .map(|index| page(&[], Some(&(index + 1).to_string())))
        .collect();
    let (store, calls) = FakeStore::new(pages);
    let (svc, guard, _directory) = discovery_fixture(store);
    assert!(matches!(
        svc.discover("").await,
        Err(ObjectServiceError::DiscoveryIncomplete)
    ));
    assert_eq!(calls.lock().unwrap().list, 1000);
    assert!(
        guard
            .begin_mutation(&identity("ordinary/file", "probe"))
            .is_err()
    );
    assert!(
        svc.upload_stream_with_control(
            "ordinary/file",
            body(),
            Arc::new(AtomicBool::new(false)),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(calls.lock().unwrap().stat, 0);
}

#[tokio::test]
async fn discovery_active_lease_conflict_is_propagated_without_marker_read() {
    let (store, calls) = FakeStore::new(vec![page(&["control/HEAD"], None)]);
    let (svc, guard, _directory) = discovery_fixture(store);
    guard
        .mark_discovery_complete(&identity("", "explicit"))
        .unwrap();
    let lease = guard
        .begin_mutation(&identity("ordinary/file", "lease"))
        .unwrap();
    assert!(matches!(
        svc.discover("").await,
        Err(ObjectServiceError::Guard(
            peerdav_lib::cloud::ObjectGuardError::Conflict
        ))
    ));
    assert!(!guard.is_protected(&identity("", "probe")).unwrap());
    assert_eq!(calls.lock().unwrap().get, 0);
    drop(lease);
    svc.discover("").await.unwrap();
    assert!(guard.is_protected(&identity("", "probe")).unwrap());
}

#[tokio::test]
async fn complete_unknown_propagates_original_source_and_releases_service_lease() {
    let (store, calls) = FakeStore::new(vec![page(&[], None)]);
    let (svc, guard, _directory) = discovery_fixture(store);
    svc.discover("").await.unwrap();
    calls.lock().unwrap().complete_unknown = true;
    let error = svc
        .upload_stream_with_control(
            "ordinary/file",
            body(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap_err();
    match error {
        ObjectServiceError::Cloud(CloudError::CompleteUnknown { source }) => {
            assert!(source.to_string().contains("completion response lost"));
        }
        other => panic!("unexpected service error: {other:?}"),
    }
    let lease = guard
        .begin_mutation(&identity("ordinary/file", "probe"))
        .unwrap();
    drop(lease);
    calls.lock().unwrap().complete_unknown = false;
    svc.upload_stream_with_control(
        "ordinary/file",
        body(),
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(calls.lock().unwrap().upload, 2);
}

fn scoped_fake(prefix: &str) -> (Arc<FakeStore>, Arc<Mutex<Calls>>) {
    let (mut store, calls) = FakeStore::new(vec![page(&[], None)]);
    Arc::get_mut(&mut store).unwrap().scope = Some(scope(prefix));
    (store, calls)
}

#[test]
fn mismatched_endpoint_bucket_or_prefix_is_rejected_without_store_or_guard_changes() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[27; 32]).unwrap();
    for field in ["endpoint", "bucket", "prefix"] {
        let (store, calls) = scoped_fake("data");
        let mut claimed = identity("data", "alias-is-not-authority");
        match field {
            "endpoint" => claimed.endpoint = "https://other.test".into(),
            "bucket" => claimed.bucket = "other-bucket".into(),
            _ => claimed.prefix = "other-prefix".into(),
        }
        assert!(matches!(
            ObjectService::try_new(store, guard.clone(), claimed, "rev"),
            Err(ObjectServiceError::StoreScopeMismatch)
        ));
        let calls = calls.lock().unwrap();
        assert_eq!(
            (calls.list, calls.stat, calls.get, calls.put, calls.upload),
            (0, 0, 0, 0, 0)
        );
        assert!(calls.deletes.is_empty());
        assert!(guard.mappings().unwrap().is_empty());
        assert!(guard.begin_mutation(&identity("data/a", "probe")).is_err());
    }
}

#[test]
fn unknown_store_scope_is_rejected_without_operations() {
    let (mut store, calls) = scoped_fake("data");
    Arc::get_mut(&mut store).unwrap().scope = None;
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[28; 32]).unwrap();
    assert!(matches!(
        ObjectService::try_new(store, guard.clone(), identity("data", "alias"), "rev"),
        Err(ObjectServiceError::UnknownStoreScope)
    ));
    let calls = calls.lock().unwrap();
    assert_eq!(
        (calls.list, calls.stat, calls.get, calls.put, calls.upload),
        (0, 0, 0, 0, 0)
    );
    assert!(calls.deletes.is_empty());
    assert!(guard.mappings().unwrap().is_empty());
}

#[test]
fn physical_scope_normalizes_default_ports_prefix_and_host_but_preserves_other_ports() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[29; 32]).unwrap();
    for (actual, claimed, accepted) in [
        ("https://EXAMPLE.test:443/", "https://example.test", true),
        ("http://example.test:80", "http://example.test/", true),
        (
            "https://example.test:8443",
            "https://example.test:8443/",
            true,
        ),
        ("https://example.test:8443", "https://example.test", false),
        (
            "https://example.test:8443",
            "https://example.test:9443",
            false,
        ),
        ("https://example.test", "http://example.test", false),
    ] {
        let (mut store, _) = scoped_fake("data/");
        Arc::get_mut(&mut store)
            .unwrap()
            .scope
            .as_mut()
            .unwrap()
            .endpoint = actual.into();
        let mut id = identity("data", "different-alias");
        id.endpoint = claimed.into();
        let result = ObjectService::try_new(store, guard.clone(), id, "rev");
        assert_eq!(result.is_ok(), accepted, "{actual} vs {claimed}");
        if !accepted {
            assert!(matches!(
                result,
                Err(ObjectServiceError::StoreScopeMismatch)
            ));
        }
    }
}

#[tokio::test]
async fn data_scope_uses_relative_store_keys_and_exactly_one_physical_guard_prefix() {
    let (store, calls) = scoped_fake("data/");
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[30; 32]).unwrap();
    guard
        .register_storage_identity(
            &identity("data", "browser"),
            StorageIdentityAssociation::New,
        )
        .unwrap();
    let svc = ObjectService::try_new(
        store.clone(),
        guard.clone(),
        identity("data", "browser"),
        "rev",
    )
    .unwrap();
    svc.discover("").await.unwrap();
    let lease = guard
        .begin_mutation(&identity("data/a", "other-connection"))
        .unwrap();
    assert!(matches!(
        svc.upload_stream_with_control("a", body(), Arc::new(AtomicBool::new(false)), None)
            .await,
        Err(ObjectServiceError::Guard(_))
    ));
    assert_eq!(calls.lock().unwrap().stat, 0);
    assert_eq!(calls.lock().unwrap().upload, 0);
    drop(lease);
    let doubled = guard
        .begin_mutation(&identity("data/data/a", "other"))
        .unwrap();
    svc.upload_stream_with_control("a", body(), Arc::new(AtomicBool::new(false)), None)
        .await
        .unwrap();
    drop(doubled);
    *store.pages.lock().unwrap() = vec![page(&["a"], None)];
    assert_eq!(svc.browse("", None).await.unwrap().objects[0].key, "a");
    let plan = svc.delete_plan("a", 4_000_000_000).await.unwrap();
    assert_eq!(plan.entries[0].key, "a");
    assert!(svc.execute_delete(&plan, true).await.unwrap()[0].is_ok());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.upload_keys, ["a"]);
    assert_eq!(calls.stat_keys, ["a", "a"]);
    assert_eq!(calls.list_prefixes, ["", "", "a"]);
    assert_eq!(calls.deletes, ["a"]);
}

#[tokio::test]
async fn sync_protection_crosses_parent_and_child_store_connections() {
    for (protected, store_prefix, relative_target) in [
        ("data", "data/child", "a"),
        ("data/child", "data", "child/a"),
    ] {
        let (store, calls) = scoped_fake(store_prefix);
        let directory = tempdir().unwrap();
        let guard = ObjectGuard::open(directory.path().join("scope.db"), &[31; 32]).unwrap();
        guard
            .register_storage_identity(
                &identity(store_prefix, "browser"),
                StorageIdentityAssociation::New,
            )
            .unwrap();
        guard
            .register(ProtectedMapping {
                identity: identity(protected, "sync-connection"),
                repository_id: "sync-repository".into(),
            })
            .unwrap();
        let svc = ObjectService::try_new(
            store,
            guard.clone(),
            identity(store_prefix, "ordinary-browser"),
            "rev",
        )
        .unwrap();
        svc.discover("").await.unwrap();
        assert!(matches!(
            svc.upload_stream_with_control(
                relative_target,
                body(),
                Arc::new(AtomicBool::new(false)),
                None
            )
            .await,
            Err(ObjectServiceError::Guard(_))
        ));
        assert!(matches!(
            svc.delete_plan(relative_target, 4_000_000_000).await,
            Err(ObjectServiceError::Guard(_))
        ));
        let calls = calls.lock().unwrap();
        assert_eq!((calls.stat, calls.upload, calls.put), (0, 0, 0));
        assert!(calls.deletes.is_empty());
    }
}

#[test]
fn s3_scope_reports_only_configured_physical_namespace_without_network() {
    use peerdav_lib::cloud::{
        S3Store,
        types::{ProxyMode, S3Config},
    };
    let store = S3Store::new(S3Config {
        endpoint: "https://example.test:8443".into(),
        bucket: "bucket".into(),
        region: "test-region".into(),
        prefix: "data".into(),
        path_style: true,
        access_key: "offline-test-access".into(),
        secret_key: "offline-test-secret".into(),
        session_token: Some("offline-test-token".into()),
        buckets: Vec::new(),
        allow_insecure_http: false,
        proxy: ProxyMode::Direct,
    })
    .unwrap();
    assert_eq!(
        store.scope(),
        Some(ObjectStoreScope {
            endpoint: "https://example.test:8443".into(),
            bucket: "bucket".into(),
            prefix: "data/".into(),
        })
    );
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[33; 32]).unwrap();
    let mut id = identity("data", "browser");
    id.endpoint = "https://example.test:8443/".into();
    assert!(ObjectService::try_new(Arc::new(store), guard, id, "rev").is_ok());
}

#[tokio::test]
async fn nonempty_store_scope_discovers_marker_at_physical_root_only() {
    let (mut store, calls) = FakeStore::new(vec![page(&["control/HEAD"], None)]);
    Arc::get_mut(&mut store).unwrap().scope = Some(scope("data/"));
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[34; 32]).unwrap();
    let svc =
        ObjectService::try_new(store, guard.clone(), identity("data", "browser"), "rev").unwrap();
    svc.discover("").await.unwrap();
    let mappings = guard.mappings().unwrap();
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].identity.prefix, "data");
    assert!(
        guard
            .is_protected(&identity("data/a", "sync-alias"))
            .unwrap()
    );
    assert!(!guard.is_protected(&identity("a", "other-scope")).unwrap());
    assert!(matches!(
        svc.upload_stream_with_control("a", body(), Arc::new(AtomicBool::new(false)), None)
            .await,
        Err(ObjectServiceError::Guard(_))
    ));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.list_prefixes, [""]);
    assert_eq!(calls.get, 1);
    assert_eq!(calls.upload, 0);
}

#[tokio::test]
async fn same_endpoint_ordinary_child_prefix_remains_writable() {
    let (store, calls) = scoped_fake("data/public");
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("scope.db"), &[32; 32]).unwrap();
    guard
        .register_storage_identity(
            &identity("data/public", "browser"),
            StorageIdentityAssociation::New,
        )
        .unwrap();
    guard
        .register(ProtectedMapping {
            identity: identity("data/repository", "sync"),
            repository_id: "sync-repository".into(),
        })
        .unwrap();
    let svc =
        ObjectService::try_new(store, guard, identity("data/public/", "browser"), "rev").unwrap();
    svc.discover("").await.unwrap();
    svc.upload_stream_with_control("a", body(), Arc::new(AtomicBool::new(false)), None)
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.upload_keys, ["a"]);
    assert_eq!(calls.put, 0);
}

#[tokio::test]
async fn download_correct_content_get_once_with_no_range() {
    let d = tempdir().unwrap();
    let (store, calls) = DownloadStore::new(b"payload", 7, DownloadMode::Good);
    download_service(store, d.path())
        .download_to("remote/object", "out", None, false, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"payload");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.get, 1);
    assert_eq!(calls.range, None);
}

#[tokio::test]
async fn download_stream_error_leaves_existing_target_unchanged() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("out"), b"old").unwrap();
    let (store, _) = DownloadStore::new(b"new", 3, DownloadMode::StreamError);
    let err = download_service(store, d.path())
        .download_to("remote/object", "out", None, true, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ObjectServiceError::Cloud(CloudError::Unsupported)
    ));
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
}

#[tokio::test]
async fn download_cancel_leaves_existing_target_unchanged() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("out"), b"old").unwrap();
    let (store, _) = DownloadStore::new(b"new", 7, DownloadMode::CancelAfterFirst);
    let cancel = Arc::new(AtomicBool::new(true));
    let err = download_service(store, d.path())
        .download_to("remote/object", "out", Some(cancel), true, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ObjectServiceError::Cancelled));
    assert_eq!(std::fs::read(d.path().join("out")).unwrap(), b"old");
}

#[tokio::test]
async fn download_length_mismatch_does_not_create_target() {
    let d = tempdir().unwrap();
    let (store, _) = DownloadStore::new(b"short", 99, DownloadMode::Good);
    let err = download_service(store, d.path())
        .download_to("remote/object", "out", None, false, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ObjectServiceError::SizeMismatch));
    assert!(!d.path().join("out").exists());
}

#[tokio::test]
async fn download_symlink_and_parent_traversal_names_are_rejected_without_external_change() {
    let d = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("outside"), b"safe").unwrap();
    std::os::unix::fs::symlink(outside.path().join("outside"), d.path().join("link")).unwrap();
    let (store, _) = DownloadStore::new(b"payload", 7, DownloadMode::Good);
    let svc = download_service(store, d.path());
    assert!(matches!(
        svc.download_to("remote/object", "link", None, true, None).await,
        Err(ObjectServiceError::DestinationSymlink)
    ));
    assert!(matches!(
        svc.download_to("remote/object", "../outside", None, true, None)
            .await,
        Err(ObjectServiceError::InvalidKey)
    ));
    assert_eq!(
        std::fs::read(outside.path().join("outside")).unwrap(),
        b"safe"
    );
}

#[tokio::test]
async fn download_selected_directory_fd_survives_path_rename_and_replacement() {
    let parent = tempdir().unwrap();
    let selected = parent.path().join("selected");
    std::fs::create_dir(&selected).unwrap();
    let (store, _) = DownloadStore::new(b"payload", 7, DownloadMode::Good);
    let svc = download_service(store, &selected);
    let renamed = parent.path().join("renamed");
    std::fs::rename(&selected, &renamed).unwrap();
    std::fs::create_dir(&selected).unwrap();
    svc.download_to("remote/object", "out", None, false, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(renamed.join("out")).unwrap(), b"payload");
    assert!(!selected.join("out").exists());
}

#[tokio::test]
async fn discovered_cas_root_stays_protected_for_all_aliases_after_reopen() {
    let (store, calls) = FakeStore::new(vec![page(&["control/HEAD"], None)]);
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let guard = ObjectGuard::open(&path, &[17; 32]).unwrap();
    let service =
        ObjectService::try_new(store, guard.clone(), identity("", "browser"), "revision").unwrap();
    service.discover("").await.unwrap();
    assert!(
        guard
            .is_protected(&identity("ordinary/file", "another-connection"))
            .unwrap()
    );
    assert!(
        service
            .upload_stream_with_control(
                "ordinary/file",
                body(),
                Arc::new(AtomicBool::new(false)),
                None
            )
            .await
            .is_err()
    );
    assert_eq!(calls.lock().unwrap().upload, 0);
    drop(service);
    drop(guard);
    let reopened = ObjectGuard::open(&path, &[17; 32]).unwrap();
    assert!(
        reopened
            .is_protected(&identity("anywhere", "third"))
            .unwrap()
    );
}

#[tokio::test]
async fn forged_plan_cannot_change_the_confirmed_object_set() {
    let (store, calls) = FakeStore::new(vec![page(&["root/a"], None)]);
    let service = service(store, "root");
    service.discover("root").await.unwrap();
    let mut plan = service.delete_plan("root", 4_000_000_000).await.unwrap();
    plan.entries[0].key = "root/unconfirmed".into();
    assert!(matches!(
        service.execute_delete(&plan, true).await,
        Err(ObjectServiceError::StaleDeletePlan)
    ));
    assert!(calls.lock().unwrap().deletes.is_empty());
}

#[tokio::test]
async fn discover_then_browse_normal_scope() {
    let (s, _) = FakeStore::new(vec![page(&["root/a"], None)]);
    let svc = service(s, "root");
    svc.discover("root").await.unwrap();
    let got = svc.browse("root", None).await.unwrap();
    assert_eq!(got.objects[0].key, "root/a");
    assert!(got.complete);
}
#[tokio::test]
async fn upload_without_discovery_is_rejected_without_store_change() {
    let (s, calls) = FakeStore::new(vec![]);
    let svc = service(s, "ordinary");
    let err = svc
        .upload_stream_with_control("ordinary/a", body(), Arc::new(AtomicBool::new(false)), None)
        .await
        .unwrap_err();
    assert!(matches!(err, ObjectServiceError::Guard(_)));
    assert_eq!(calls.lock().unwrap().upload, 0);
}
#[tokio::test]
async fn registered_sync_root_blocks_upload_and_delete_without_changes() {
    let (s, calls) = FakeStore::new(vec![page(&["root/a"], None)]);
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("r.db"), &[1u8; 32]).unwrap();
    g.register(ProtectedMapping {
        identity: identity("root", "sync"),
        repository_id: "sync".into(),
    })
    .unwrap();
    let svc = ObjectService::try_new(s, g, identity("", "browser"), "rev").unwrap();
    assert!(matches!(
        svc.upload_stream_with_control("root/a", body(), Arc::new(AtomicBool::new(false)), None)
            .await,
        Err(ObjectServiceError::Guard(
            peerdav_lib::cloud::ObjectGuardError::IdentityUnconfirmed
        ))
    ));
    let plan = svc.delete_plan("root", 4_000_000_000).await;
    assert!(matches!(
        plan,
        Err(ObjectServiceError::Guard(
            peerdav_lib::cloud::ObjectGuardError::IdentityUnconfirmed
        ))
    ));
    assert!(calls.lock().unwrap().deletes.is_empty());
}
#[tokio::test]
async fn ancestor_and_alias_are_protected() {
    let (s, _) = FakeStore::new(vec![]);
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("r.db"), &[2u8; 32]).unwrap();
    g.register(ProtectedMapping {
        identity: identity("root", "one"),
        repository_id: "r".into(),
    })
    .unwrap();
    assert!(
        g.begin_mutation(&identity("root/child", "different-alias"))
            .is_err()
    );
    assert!(
        g.begin_mutation(&identity("root/child/grandchild", "other"))
            .is_err()
    );
    let _ = s;
}
#[tokio::test]
async fn pagination_failure_during_discover_does_not_authorize() {
    let (s, calls) = FakeStore::new(vec![page(&["control/HEAD"], Some("1"))]);
    let svc = service(s, "root");
    let err = svc.discover("root").await.unwrap_err();
    assert!(matches!(err, ObjectServiceError::Cloud(_)));
    assert_eq!(calls.lock().unwrap().list, 2);
    assert!(matches!(
        svc.upload_stream_with_control("root/a", body(), Arc::new(AtomicBool::new(false)), None)
            .await,
        Err(ObjectServiceError::Guard(_))
    ));
}
#[tokio::test]
async fn unknown_identity_discovery_allows_browse_but_only_explicit_confirmation_allows_writes() {
    use peerdav_lib::cloud::ObjectGuardError;
    let (store, calls) = FakeStore::new(vec![page(&["ordinary/a"], None)]);
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("identity.db"), &[41; 32]).unwrap();
    let epoch = guard.registry_epoch().unwrap();
    let svc = ObjectService::try_new(store, guard.clone(), identity("", "browser"), "rev").unwrap();
    svc.discover("").await.unwrap();
    assert_eq!(
        svc.browse("", None).await.unwrap().objects[0].key,
        "ordinary/a"
    );
    assert_eq!(guard.registry_epoch().unwrap(), epoch);
    assert!(matches!(
        svc.upload_stream_with_control(
            "ordinary/new",
            body(),
            Arc::new(AtomicBool::new(false)),
            None
        )
        .await,
        Err(ObjectServiceError::Guard(
            ObjectGuardError::IdentityUnconfirmed
        ))
    ));
    assert!(matches!(
        svc.delete_plan("ordinary", 4_000_000_000).await,
        Err(ObjectServiceError::Guard(
            ObjectGuardError::IdentityUnconfirmed
        ))
    ));
    {
        let calls = calls.lock().unwrap();
        assert_eq!((calls.stat, calls.upload, calls.put), (0, 0, 0));
        assert!(calls.deletes.is_empty());
    }
    guard
        .register_storage_identity(&identity("", "confirmed"), StorageIdentityAssociation::New)
        .unwrap();
    svc.upload_stream_with_control(
        "ordinary/new",
        body(),
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .await
    .unwrap();
    let plan = svc.delete_plan("ordinary", 4_000_000_000).await.unwrap();
    assert_eq!(plan.registry_epoch, guard.registry_epoch().unwrap());
    assert!(svc.execute_delete(&plan, true).await.unwrap()[0].is_ok());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.upload_keys, ["ordinary/new"]);
    assert_eq!(calls.deletes, ["ordinary/a"]);
}

#[tokio::test]
async fn identity_registry_change_invalidates_old_delete_plan_before_any_delete() {
    let (store, calls) = FakeStore::new(vec![page(&["ordinary/a"], None)]);
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("identity.db"), &[42; 32]).unwrap();
    guard
        .register_storage_identity(&identity("", "browser"), StorageIdentityAssociation::New)
        .unwrap();
    let svc = ObjectService::try_new(store, guard.clone(), identity("", "browser"), "rev").unwrap();
    svc.discover("").await.unwrap();
    let plan = svc.delete_plan("ordinary", 4_000_000_000).await.unwrap();
    assert_eq!(plan.registry_epoch, guard.registry_epoch().unwrap());
    let mut other = identity("", "unrelated");
    other.endpoint = "https://other.test".into();
    guard
        .register_storage_identity(&other, StorageIdentityAssociation::New)
        .unwrap();
    assert_ne!(plan.registry_epoch, guard.registry_epoch().unwrap());
    assert!(matches!(
        svc.execute_delete(&plan, true).await,
        Err(ObjectServiceError::StaleDeletePlan)
    ));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.stat, 0);
    assert!(calls.deletes.is_empty());
}

#[tokio::test]
async fn newly_protected_root_blocks_existing_delete_plan_via_guard_lease() {
    use peerdav_lib::cloud::ObjectGuardError;
    let (store, calls) = FakeStore::new(vec![page(&["ordinary/a"], None)]);
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("identity.db"), &[43; 32]).unwrap();
    guard
        .register_storage_identity(&identity("", "browser"), StorageIdentityAssociation::New)
        .unwrap();
    let svc = ObjectService::try_new(store, guard.clone(), identity("", "browser"), "rev").unwrap();
    svc.discover("").await.unwrap();
    let plan = svc.delete_plan("ordinary", 4_000_000_000).await.unwrap();
    guard
        .register(ProtectedMapping {
            identity: identity("ordinary", "sync"),
            repository_id: "repo".into(),
        })
        .unwrap();
    assert_eq!(plan.registry_epoch, guard.registry_epoch().unwrap());
    assert!(matches!(
        svc.execute_delete(&plan, true).await,
        Err(ObjectServiceError::Guard(ObjectGuardError::Protected))
    ));
    assert!(calls.lock().unwrap().deletes.is_empty());
}

#[tokio::test]
async fn associated_distinct_endpoint_service_cannot_mutate_protected_namespace() {
    use peerdav_lib::cloud::ObjectGuardError;
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("identity.db"), &[44; 32]).unwrap();
    let physical = guard
        .register_storage_identity(&identity("", "sync"), StorageIdentityAssociation::New)
        .unwrap();
    guard
        .register(ProtectedMapping {
            identity: identity("data/repo", "sync"),
            repository_id: "repo".into(),
        })
        .unwrap();
    let mut other = identity("data", "browser");
    other.endpoint = "https://distinct.test".into();
    guard
        .register_storage_identity(&other, StorageIdentityAssociation::Existing(physical))
        .unwrap();
    let (mut store, calls) = scoped_fake("data");
    Arc::get_mut(&mut store)
        .unwrap()
        .scope
        .as_mut()
        .unwrap()
        .endpoint = other.endpoint.clone();
    let svc = ObjectService::try_new(store, guard, other, "rev").unwrap();
    svc.discover("").await.unwrap();
    assert!(matches!(
        svc.upload_stream_with_control("repo/a", body(), Arc::new(AtomicBool::new(false)), None)
            .await,
        Err(ObjectServiceError::Guard(ObjectGuardError::Protected))
    ));
    assert!(matches!(
        svc.delete_plan("repo", 4_000_000_000).await,
        Err(ObjectServiceError::Guard(ObjectGuardError::Protected))
    ));
    let calls = calls.lock().unwrap();
    assert_eq!((calls.stat, calls.upload, calls.put), (0, 0, 0));
    assert!(calls.deletes.is_empty());
    assert_eq!(calls.list_prefixes, [""]);
}

#[tokio::test]
async fn delete_requires_confirmation() {
    let (s, _) = FakeStore::new(vec![page(&["root/a"], None)]);
    let svc = service(s, "root");
    svc.discover("root").await.unwrap();
    let plan = svc.delete_plan("root", 4_000_000_000).await.unwrap();
    assert!(matches!(
        svc.execute_delete(&plan, false).await,
        Err(ObjectServiceError::ConfirmationRequired)
    ));
}
#[tokio::test]
async fn delete_expired_plan_is_rejected() {
    let (s, _) = FakeStore::new(vec![page(&["root/a"], None)]);
    let svc = service(s, "root");
    svc.discover("root").await.unwrap();
    assert!(matches!(
        svc.delete_plan("root", 0).await,
        Err(ObjectServiceError::StaleDeletePlan)
    ));
}
#[tokio::test]
async fn delete_repeated_execution_is_rejected_by_guard_lease_policy() {
    let (s, calls) = FakeStore::new(vec![page(&["root/a"], None)]);
    let svc = service(s, "root");
    svc.discover("root").await.unwrap();
    let plan = svc.delete_plan("root", 4_000_000_000).await.unwrap();
    svc.execute_delete(&plan, true).await.unwrap();
    let before = calls.lock().unwrap().deletes.len();
    assert!(matches!(
        svc.execute_delete(&plan, true).await,
        Err(ObjectServiceError::StaleDeletePlan)
    ));
    assert_eq!(calls.lock().unwrap().deletes.len(), before);
}
#[tokio::test]
async fn delete_returns_partial_results() {
    let (s, calls) = FakeStore::new(vec![page(&["root/a", "root/b"], None)]);
    calls.lock().unwrap().delete_fail = Some("root/b".into());
    let svc = service(s, "root");
    svc.discover("root").await.unwrap();
    let plan = svc.delete_plan("root", 4_000_000_000).await.unwrap();
    let result = svc.execute_delete(&plan, true).await.unwrap();
    assert_eq!(result.len(), 2);
    assert!(result[0].is_ok());
    assert!(matches!(result[1], Err(CloudError::Precondition)));
    assert_eq!(calls.lock().unwrap().deletes, vec!["root/a"]);
}
#[tokio::test]
async fn upload_uses_multipart_not_put_and_honors_cancel() {
    let (s, calls) = FakeStore::new(vec![page(&[], None)]);
    let svc = service(s, "root");
    svc.discover("root").await.unwrap();
    let cancel = Arc::new(AtomicBool::new(true));
    assert!(matches!(
        svc.upload_stream_with_control("root/a", body(), cancel, None)
            .await,
        Err(ObjectServiceError::Cloud(CloudError::UploadCancelled))
    ));
    let c = calls.lock().unwrap();
    assert_eq!(c.upload, 1);
    assert_eq!(c.put, 0);
    assert!(c.cancelled);
}
#[tokio::test]
async fn traversal_key_is_rejected_before_store() {
    let (s, calls) = FakeStore::new(vec![]);
    let svc = service(s, "root");
    assert!(matches!(
        svc.browse("root/../x", None).await,
        Err(ObjectServiceError::InvalidKey)
    ));
    assert_eq!(calls.lock().unwrap().list, 0);
}
#[tokio::test]
async fn reopened_registry_still_protects_root() {
    let d = tempdir().unwrap();
    let db = d.path().join("registry.db");
    let key = [4u8; 32];
    let g = ObjectGuard::open(&db, &key).unwrap();
    g.register(ProtectedMapping {
        identity: identity("root", "x"),
        repository_id: "r".into(),
    })
    .unwrap();
    drop(g);
    let reopened = ObjectGuard::open(&db, &key).unwrap();
    assert!(
        reopened
            .begin_mutation(&identity("root/file", "alias"))
            .is_err()
    );
}
