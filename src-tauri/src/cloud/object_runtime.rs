use futures_util::{FutureExt, StreamExt};
use rand::RngCore;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

use super::{
    ObjectGuard, ObjectService, ObjectServiceError, ProtectedIdentity, StorageIdentityAssociation,
    object_runtime_types::*, runtime::CloudRuntime, traits::ObjectStore, types::CloudError,
};
use crate::{AppError, DomainError};

fn app(error: impl Into<DomainError>) -> AppError {
    AppError::from(error.into())
}
fn local(error: std::io::Error) -> AppError {
    app(ObjectServiceError::Io { source: error })
}
fn state_error() -> AppError {
    app(ObjectServiceError::InvalidLocalCapability)
}
pub(crate) fn random_id() -> Result<String, AppError> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|source| app(CloudError::Random { source }))?;
    Ok(hex::encode(bytes))
}
#[cfg(test)]
mod tests {
    use super::super::{
        config_types::{CloudConnection, ScheduleConfig},
        types::{
            ObjectListing, ObjectMetadata, ObjectPage, ObjectStoreScope, ObjectStream, ProxyMode,
            PutCondition, S3Config, StorageCapabilities,
        },
    };
    use super::*;
    use futures_util::{future::BoxFuture, stream};
    use std::sync::atomic::{AtomicU64, AtomicUsize};
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("{0:?}")]
        App(AppError),
        #[error(transparent)]
        Io(#[from] std::io::Error),
        #[error(transparent)]
        Domain(#[from] DomainError),
        #[error(transparent)]
        Guard(#[from] super::super::ObjectGuardError),
        #[error(transparent)]
        Timeout(#[from] tokio::time::error::Elapsed),
        #[error("{0}")]
        Missing(&'static str),
    }
    impl From<AppError> for TestError {
        fn from(value: AppError) -> Self {
            Self::App(value)
        }
    }
    impl From<&'static str> for TestError {
        fn from(value: &'static str) -> Self {
            Self::Missing(value)
        }
    }
    #[derive(Default)]
    struct FakeStore {
        reads: AtomicUsize,
        stats: AtomicUsize,
        mutations: AtomicUsize,
        probes: AtomicUsize,
        deleted_keys: tokio::sync::Mutex<Vec<String>>,
        listed_prefixes: tokio::sync::Mutex<Vec<String>>,
        extra_objects: tokio::sync::Mutex<Vec<String>>,
        behavior: AtomicUsize,
        preview_chunks: Arc<AtomicUsize>,
        stat_started: tokio::sync::Notify,
        stat_release: tokio::sync::Notify,
        upload_cleanup_started: tokio::sync::Notify,
        upload_cleanup_release: tokio::sync::Notify,
        upload_cleanups: AtomicUsize,
        /// Simulates the store the runtime binds after a bucket switch.
        bucket: std::sync::Mutex<String>,
    }
    impl ObjectStore for FakeStore {
        fn scope(&self) -> Option<ObjectStoreScope> {
            let selected = self.bucket.lock().ok()?.clone();
            Some(ObjectStoreScope {
                endpoint: "https://offline.invalid".into(),
                bucket: if selected.is_empty() {
                    "bucket".into()
                } else {
                    selected
                },
                prefix: String::new(),
            })
        }
        fn capabilities(&self) -> StorageCapabilities {
            self.probes.fetch_add(1, Ordering::SeqCst);
            StorageCapabilities::default()
        }
        fn list<'a>(&'a self, _: &'a str) -> ObjectListing<'a> {
            stream::empty().boxed()
        }
        fn list_page<'a>(
            &'a self,
            prefix: &'a str,
            token: Option<&'a str>,
        ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
            Box::pin(async move {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.listed_prefixes.lock().await.push(prefix.to_owned());
                if self.behavior.load(Ordering::SeqCst) == 3 {
                    return Ok(ObjectPage {
                        objects: Vec::new(),
                        next_token: Some(
                            (token.and_then(|t| t.parse::<usize>().ok()).unwrap_or(0) + 1)
                                .to_string(),
                        ),
                    });
                }
                if self.behavior.load(Ordering::SeqCst) == 4 {
                    return if token.is_some() {
                        Err(CloudError::NotFound)
                    } else {
                        Ok(ObjectPage {
                            objects: Vec::new(),
                            next_token: Some("retry".into()),
                        })
                    };
                }
                // S3 LIST uses a lexical prefix, including any trailing slash.
                let mut keys = vec!["data/file".to_owned(), "docs/a".to_owned(),
                    "docs/nested/b".to_owned(), "docs-sibling/keep".to_owned()];
                keys.extend(self.extra_objects.lock().await.iter().cloned());
                Ok(ObjectPage {
                    objects: keys.into_iter().filter(|key| key.starts_with(prefix))
                        .map(|key| ObjectMetadata { key, size: 1, etag: Some("etag".into()) })
                        .collect(),
                    next_token: None,
                })
            })
        }
        fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                self.stats.fetch_add(1, Ordering::SeqCst);
                self.reads.fetch_add(1, Ordering::SeqCst);
                if self.behavior.load(Ordering::SeqCst) == 7 {
                    self.stat_started.notify_one();
                    self.stat_release.notified().await;
                }
                let preview_size = match key {
                    "preview/small.md" => Some(6),
                    "preview/large.bin" => Some(crate::types::MAX_PREVIEW_BYTES + 1),
                    "preview/lines.txt" => Some((crate::types::MAX_PREVIEW_LINES as u64 + 1) * 2),
                    "preview/lying.bin" | "preview/invalid.txt" | "preview/image.png" => Some(1),
                    _ => None,
                };
                if let Some(size) = preview_size {
                    return Ok(ObjectMetadata {
                        key: key.into(),
                        size,
                        etag: None,
                    });
                }
                if matches!(key, "data/file" | "docs/a" | "docs/nested/b")
                    || (self.behavior.load(Ordering::SeqCst) == 8 && key.starts_with("data/"))
                {
                    Ok(ObjectMetadata {
                        key: key.into(),
                        size: 1,
                        etag: match self.behavior.load(Ordering::SeqCst) {
                            5 => Some("changed".into()),
                            6 => None,
                            _ => Some("etag".into()),
                        },
                    })
                } else {
                    Err(CloudError::NotFound)
                }
            })
        }
        fn get<'a>(
            &'a self,
            key: &'a str,
            _: Option<std::ops::Range<u64>>,
        ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
            Box::pin(async move {
                self.reads.fetch_add(1, Ordering::SeqCst);
                if key == "preview/lying.bin" {
                    let counter = Arc::clone(&self.preview_chunks);
                    let body = stream::iter([crate::types::MAX_PREVIEW_BYTES as usize, 1, 1])
                        .map(move |size| {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(bytes::Bytes::from(vec![0; size]))
                        })
                        .boxed();
                    return Ok(body);
                }
                let bytes = match key {
                    "preview/small.md" => b"hello\n".to_vec(),
                    "preview/lines.txt" => "x\n"
                        .repeat(crate::types::MAX_PREVIEW_LINES + 1)
                        .into_bytes(),
                    "preview/invalid.txt" | "preview/image.png" => vec![0xff],
                    _ => b"x".to_vec(),
                };
                Ok(crate::cloud::bytes_stream(bytes::Bytes::from(bytes)))
            })
        }
        fn put<'a>(
            &'a self,
            key: &'a str,
            _: ObjectStream,
            _: PutCondition,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                self.mutations.fetch_add(1, Ordering::SeqCst);
                Ok(ObjectMetadata {
                    key: key.into(),
                    size: 0,
                    etag: None,
                })
            })
        }
        fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
            Box::pin(async move {
                self.deleted_keys.lock().await.push(key.to_owned());
                self.mutations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
        fn upload_stream_with_control<'a>(
            &'a self,
            key: &'a str,
            mut body: ObjectStream,
            cancel: Arc<AtomicBool>,
        ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                self.mutations.fetch_add(1, Ordering::SeqCst);
                if self.behavior.load(Ordering::SeqCst) == 1 {
                    return Err(CloudError::CompleteUnknown {
                        source: object_store::Error::Generic {
                            store: "offline",
                            source: Box::new(std::io::Error::other("completion lost")),
                        },
                    });
                }
                if matches!(self.behavior.load(Ordering::SeqCst), 2 | 9 | 10) {
                    while !cancel.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                    if self.behavior.load(Ordering::SeqCst) == 10 {
                        futures_util::future::pending::<()>().await;
                    }
                    if self.behavior.load(Ordering::SeqCst) == 9 {
                        self.upload_cleanup_started.notify_one();
                        self.upload_cleanup_release.notified().await;
                        self.upload_cleanups.fetch_add(1, Ordering::SeqCst);
                    }
                    return Err(CloudError::UploadCancelled);
                }
                while let Some(chunk) = body.next().await {
                    chunk?;
                }
                Ok(ObjectMetadata {
                    key: key.into(),
                    size: 1,
                    etag: None,
                })
            })
        }
    }
    #[derive(Default)]
    struct Events(AtomicUsize);
    impl crate::sync::traits::SyncEvents for Events {
        fn progress(
            &self,
            _: crate::sync::manager_types::SyncStatus,
        ) -> Result<(), crate::sync::traits::BoxError> {
            Ok(())
        }
        fn error(&self, _: AppError) -> Result<(), crate::sync::traits::BoxError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    struct Fixture {
        _dir: tempfile::TempDir,
        runtime: Arc<CloudRuntime>,
        store: Arc<FakeStore>,
        loads: Arc<AtomicUsize>,
        revision: Arc<AtomicU64>,
        events: Arc<Events>,
    }
    async fn fixture() -> Result<Fixture, TestError> {
        let dir = tempfile::tempdir()?;
        let vault = Arc::new(crate::Vault::open(&dir.path().join("vault.db"))?);
        vault.initialize(&dir.path().to_string_lossy(), "admin", "password 123")?;
        let events = Arc::new(Events::default());
        let runtime = CloudRuntime::new(vault, events.clone(), dir.path().to_owned());
        let store = Arc::new(FakeStore::default());
        let loads = Arc::new(AtomicUsize::new(0));
        let revision = Arc::new(AtomicU64::new(1));
        let counter = Arc::clone(&loads);
        let rev = Arc::clone(&revision);
        runtime.objects.lock().await.backend = Some(ObjectTestBackend {
            store: store.clone(),
            load: Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(CloudConnection {
                    id: "connection".into(),
                    name: "offline".into(),
                    revision: rev.load(Ordering::SeqCst),
                    schedule: ScheduleConfig::default(),
                    config: S3Config {
                        endpoint: "https://offline.invalid".into(),
                        bucket: "bucket".into(),
                        region: "region".into(),
                        prefix: String::new(),
                        path_style: true,
                        access_key: "fake-access".into(),
                        secret_key: "fake-secret".into(),
                        session_token: None,
                        buckets: vec!["alternate".into()],
                        allow_insecure_http: false,
                        proxy: ProxyMode::Direct,
                    },
                })
            }),
        });
        Ok(Fixture {
            _dir: dir,
            runtime,
            store,
            loads,
            revision,
            events,
        })
    }
    async fn authorize(f: &Fixture) -> Result<(), AppError> {
        f.runtime.resume()?;
        f.runtime
            .object_register_identity("connection", 1, ObjectIdentityInput::New {})
            .await?;
        f.runtime.object_discover("connection", "").await?;
        Ok(())
    }
    async fn directory_settled(
        f: &Fixture,
        id: &str,
    ) -> Result<super::super::directory_types::ObjectUploadDirectoryDto, TestError> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let status = f
                    .runtime
                    .object_status_upload_directory("connection", id)
                    .await?;
                if matches!(status.phase.as_str(), "completed" | "cancelled" | "failed") {
                    return Ok(status);
                }
                tokio::task::yield_now().await;
            }
        })
        .await?
    }
    async fn directory_conflict(
        f: &Fixture,
        id: &str,
    ) -> Result<super::super::directory_types::DirectoryConflictDto, TestError> {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let status = f
                    .runtime
                    .object_status_upload_directory("connection", id)
                    .await?;
                if let Some(conflict) = status.conflict {
                    return Ok::<_, AppError>(conflict);
                }
                tokio::task::yield_now().await;
            }
        })
        .await?
        .map_err(Into::into)
    }
    #[tokio::test]
    async fn streaming_directory_conflict_replay_cannot_resolve_next_conflict()
    -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("data");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("first"), b"x")?;
        std::fs::write(root.join("second"), b"x")?;
        f.store.behavior.store(8, Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Ask)
            .await?;
        let first = directory_conflict(&f, &selected.batch_id).await?;
        assert!(!first.conflict_id.is_empty());
        f.runtime
            .object_resolve_upload_directory_conflict(
                "connection",
                &selected.batch_id,
                &first.conflict_id,
                Policy::Skip,
                false,
            )
            .await?;
        let second = directory_conflict(&f, &selected.batch_id).await?;
        assert_ne!(first.conflict_id, second.conflict_id);
        assert_ne!(first.key, second.key);
        assert!(
            f.runtime
                .object_resolve_upload_directory_conflict(
                    "connection",
                    &selected.batch_id,
                    &first.conflict_id,
                    Policy::Overwrite,
                    true
                )
                .await
                .is_err()
        );
        let unchanged = f
            .runtime
            .object_status_upload_directory("connection", &selected.batch_id)
            .await?;
        assert_eq!(unchanged.phase, "conflict");
        assert_eq!(
            unchanged.conflict.as_ref().map(|c| &c.conflict_id),
            Some(&second.conflict_id)
        );
        assert_eq!(unchanged.skipped_files, 1);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert!(
            f.runtime.objects.lock().await.upload_directories[&selected.batch_id]
                .resolution
                .is_none()
        );
        f.runtime
            .object_resolve_upload_directory_conflict(
                "connection",
                &selected.batch_id,
                &second.conflict_id,
                Policy::Skip,
                false,
            )
            .await?;
        let done = directory_settled(&f, &selected.batch_id).await?;
        assert_eq!(done.phase, "completed");
        assert_eq!(done.skipped_files, 2);
        assert_eq!(done.completed_files, 0);
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_cancel_preserves_upload_cleanup() -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("cleanup");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("first"), b"x")?;
        std::fs::write(root.join("second"), b"x")?;
        f.store.behavior.store(9, Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Overwrite)
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while f.store.mutations.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let cleanup = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                f.store.upload_cleanup_started.notified(),
            )
            .await?;
            // The lower-level upload must remain polled after observing cancellation.
            tokio::task::yield_now().await;
            f.store.upload_cleanup_release.notify_one();
            Ok::<_, TestError>(())
        };
        let (stopped, cleaned) = tokio::join!(
            f.runtime
                .object_cancel_upload_directory("connection", &selected.batch_id),
            cleanup
        );
        stopped?;
        cleaned?;
        let status = directory_settled(&f, &selected.batch_id).await?;
        assert_eq!(status.phase, "cancelled");
        assert!(!status.recovery_required);
        assert_eq!(f.store.upload_cleanups.load(Ordering::SeqCst), 1);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 1);
        // Directory preflight and the service's conditional-upload stat only;
        // the second local file must never reach either stage.
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 2);
        assert!(f.runtime.object_recovery_read()?.is_empty());
        assert_eq!(f.events.0.load(Ordering::SeqCst), 0);
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_cancel_cleanup_timeout_retains_recovery() -> Result<(), TestError>
    {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("timeout");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), b"x")?;
        f.store.behavior.store(10, Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Overwrite)
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while f.store.mutations.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        f.runtime
            .object_cancel_upload_directory("connection", &selected.batch_id)
            .await?;
        let status = directory_settled(&f, &selected.batch_id).await?;
        assert_eq!(status.phase, "failed");
        assert!(status.recovery_required);
        assert_eq!(
            status.error.as_ref().map(|e| e.code.as_str()),
            Some("INTERNAL_ERROR")
        );
        assert_eq!(f.runtime.object_recovery_read()?.len(), 1);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 1);
        assert_eq!(f.events.0.load(Ordering::SeqCst), 1);
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_cancel_during_stat_prevents_later_dispatch()
    -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("data");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), b"x")?;
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.store.behavior.store(7, Ordering::SeqCst);
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Overwrite)
            .await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            f.store.stat_started.notified(),
        )
        .await?;
        let reads = f.store.reads.load(Ordering::SeqCst);
        let (first, second) = tokio::join!(
            f.runtime
                .object_cancel_upload_directory("connection", &selected.batch_id),
            f.runtime
                .object_cancel_upload_directory("connection", &selected.batch_id),
        );
        first?;
        second?;
        f.store.stat_release.notify_one();
        tokio::task::yield_now().await;
        assert_eq!(f.store.reads.load(Ordering::SeqCst), reads);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(
            directory_settled(&f, &selected.batch_id).await?.phase,
            "cancelled"
        );
        assert!(
            f.runtime.objects.lock().await.upload_directories[&selected.batch_id]
                .handle
                .is_none()
        );
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_conflict_accepts_only_one_pending_decision()
    -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("data");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), b"x")?;
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Ask)
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if f.runtime
                    .object_status_upload_directory("connection", &selected.batch_id)
                    .await?
                    .phase
                    == "conflict"
                {
                    return Ok::<_, AppError>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        let conflict = directory_conflict(&f, &selected.batch_id).await?;
        let (first, duplicate) = tokio::join!(
            f.runtime.object_resolve_upload_directory_conflict(
                "connection",
                &selected.batch_id,
                &conflict.conflict_id,
                Policy::Skip,
                false
            ),
            f.runtime.object_resolve_upload_directory_conflict(
                "connection",
                &selected.batch_id,
                &conflict.conflict_id,
                Policy::Overwrite,
                false
            ),
        );
        assert_ne!(first.is_ok(), duplicate.is_ok());
        f.runtime
            .object_cancel_upload_directory("connection", &selected.batch_id)
            .await?;
        assert!(
            f.runtime
                .object_resolve_upload_directory_conflict(
                    "connection",
                    &selected.batch_id,
                    &conflict.conflict_id,
                    Policy::Skip,
                    false
                )
                .await
                .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_over_128_without_status_driving() -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("many");
        std::fs::create_dir(&root)?;
        for n in 0..257 {
            std::fs::write(root.join(format!("{n}")), b"x")?;
        }
        let reads_before_selection = f.store.reads.load(Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "dest/".into(), &root)
            .await?;
        assert_eq!(f.store.reads.load(Ordering::SeqCst), reads_before_selection);
        f.store.behavior.store(2, Ordering::SeqCst);
        f.runtime
            .object_start_upload_directory(
                "connection",
                &selected.batch_id,
                DirectoryConflictPolicy::Overwrite,
            )
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while f.store.mutations.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        // First stream is already running: no recursive pre-scan or per-file task registration.
        {
            let state = f.runtime.objects.lock().await;
            assert!(state.tasks.is_empty());
            assert_eq!(state.upload_directories.len(), 1);
            assert_eq!(
                state.upload_directories[&selected.batch_id]
                    .status
                    .completed_files,
                0
            );
        }
        f.runtime
            .object_cancel_upload_directory("connection", &selected.batch_id)
            .await?;
        assert_eq!(
            directory_settled(&f, &selected.batch_id).await?.phase,
            "cancelled"
        );
        assert_eq!(f.events.0.load(Ordering::SeqCst), 0);
        // The store confirms cancellation cleanup, so no unknown outcome remains.
        assert!(
            !directory_settled(&f, &selected.batch_id)
                .await?
                .recovery_required
        );
        assert!(f.runtime.object_recovery_read()?.is_empty());
        f.runtime
            .object_release_upload_directory("connection", &selected.batch_id)
            .await?;
        f.store.behavior.store(0, Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "dest/".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory(
                "connection",
                &selected.batch_id,
                DirectoryConflictPolicy::Overwrite,
            )
            .await?;
        // Await the owned job, not object_status: execution is independent of UI polling.
        let handle = f
            .runtime
            .objects
            .lock()
            .await
            .upload_directories
            .get_mut(&selected.batch_id)
            .ok_or("batch")?
            .handle
            .take()
            .ok_or("handle")?;
        tokio::time::timeout(std::time::Duration::from_secs(15), handle)
            .await?
            .map_err(|_| "directory worker join failed")?;
        let status = f
            .runtime
            .object_status_upload_directory("connection", &selected.batch_id)
            .await?;
        assert_eq!(status.phase, "completed");
        assert_eq!(status.completed_files, 257);
        assert_eq!(status.completed_bytes, 257);
        assert!(f.runtime.objects.lock().await.tasks.is_empty());
        assert!(f.runtime.object_recovery_read()?.is_empty());
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_direct_pages_tokens_and_fixed_root() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("root");
        std::fs::create_dir_all(root.join("nested"))?;
        std::fs::write(root.join("nested/deep"), b"deep")?;
        for n in 0..140 {
            std::fs::write(root.join(format!("{n}")), b"x")?;
        }
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        std::fs::rename(&root, f._dir.path().join("moved"))?;
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("replacement"), b"wrong")?;
        let mut cursor = None;
        let mut total = 0;
        let mut nested = None;
        loop {
            let page = f
                .runtime
                .object_page_upload_directory(
                    "connection",
                    &selected.batch_id,
                    None,
                    cursor.clone(),
                    Some(32),
                )
                .await?;
            assert!(page.entries.len() <= 32);
            for entry in &page.entries {
                assert!(!entry.relative_path.ends_with("/deep"));
                assert_ne!(entry.name, "replacement");
                if entry.name == "nested" {
                    nested = entry.directory_token.clone();
                }
            }
            total += page.entries.len();
            let expired = cursor;
            cursor = page.next_cursor;
            if let Some(expired) = expired {
                assert_eq!(
                    f.runtime
                        .object_page_upload_directory(
                            "connection",
                            &selected.batch_id,
                            None,
                            Some(expired),
                            None
                        )
                        .await
                        .err()
                        .ok_or("cursor")?
                        .code,
                    "CLOUD_DIRECTORY_CURSOR_RESET"
                );
            }
            // Only current page directory tokens are retained; enter it immediately if present.
            if let Some(token) = nested.take() {
                let child = f
                    .runtime
                    .object_page_upload_directory(
                        "connection",
                        &selected.batch_id,
                        Some(token),
                        None,
                        None,
                    )
                    .await?;
                assert_eq!(child.entries.len(), 1);
                assert_eq!(child.entries[0].name, "deep");
                let back = f
                    .runtime
                    .object_page_upload_directory(
                        "connection",
                        &selected.batch_id,
                        child.parent_token,
                        None,
                        Some(128),
                    )
                    .await?;
                assert_eq!(back.entries.len(), 128);
                if cursor.is_some() {
                    assert!(
                        f.runtime
                            .object_page_upload_directory(
                                "connection",
                                &selected.batch_id,
                                None,
                                cursor.clone(),
                                None
                            )
                            .await
                            .is_err()
                    );
                }
                break;
            }
            if cursor.is_none() {
                break;
            }
        }
        assert!(total > 0);
        let other = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        let page = f
            .runtime
            .object_page_upload_directory("connection", &selected.batch_id, None, None, Some(1))
            .await?;
        assert!(
            f.runtime
                .object_page_upload_directory(
                    "connection",
                    &other.batch_id,
                    Some(page.directory_token),
                    page.next_cursor,
                    None
                )
                .await
                .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_conflicts_and_unknown_stop() -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("data");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), b"x")?;
        for decision in [Policy::Skip, Policy::Overwrite, Policy::Cancel] {
            let selected = f
                .runtime
                .object_select_upload_directory("connection", 1, "".into(), &root)
                .await?;
            f.runtime
                .object_start_upload_directory("connection", &selected.batch_id, Policy::Ask)
                .await?;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if f.runtime
                        .object_status_upload_directory("connection", &selected.batch_id)
                        .await?
                        .phase
                        == "conflict"
                    {
                        return Ok::<_, AppError>(());
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await??;
            let conflict = directory_conflict(&f, &selected.batch_id).await?;
            f.runtime
                .object_resolve_upload_directory_conflict(
                    "connection",
                    &selected.batch_id,
                    &conflict.conflict_id,
                    decision,
                    true,
                )
                .await?;
            let status = directory_settled(&f, &selected.batch_id).await?;
            if decision == Policy::Skip {
                assert_eq!(status.skipped_files, 1);
            }
            if decision == Policy::Overwrite {
                assert_eq!(status.completed_files, 1);
            }
            if decision == Policy::Cancel {
                assert_eq!(status.phase, "cancelled");
            }
            f.runtime
                .object_release_upload_directory("connection", &selected.batch_id)
                .await?;
        }
        assert_eq!(f.events.0.load(Ordering::SeqCst), 0);
        f.store.behavior.store(6, Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Overwrite)
            .await?;
        assert_eq!(
            directory_settled(&f, &selected.batch_id).await?.phase,
            "failed"
        );
        assert_eq!(f.events.0.load(Ordering::SeqCst), 1);
        f.runtime
            .object_release_upload_directory("connection", &selected.batch_id)
            .await?;
        f.store.behavior.store(1, Ordering::SeqCst);
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Overwrite)
            .await?;
        let status = directory_settled(&f, &selected.batch_id).await?;
        assert!(status.recovery_required);
        assert_eq!(f.runtime.object_recovery_read()?.len(), 1);
        assert_eq!(f.events.0.load(Ordering::SeqCst), 2);
        Ok(())
    }
    #[tokio::test]
    async fn streaming_directory_invalidate_conflict_and_unsafe_entries() -> Result<(), TestError> {
        use super::super::directory_types::DirectoryConflictPolicy as Policy;
        let f = fixture().await?;
        authorize(&f).await?;
        let root = f._dir.path().join("data");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), b"x")?;
        let selected = f
            .runtime
            .object_select_upload_directory("connection", 1, "".into(), &root)
            .await?;
        f.runtime
            .object_start_upload_directory("connection", &selected.batch_id, Policy::Ask)
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if f.runtime
                    .object_status_upload_directory("connection", &selected.batch_id)
                    .await?
                    .phase
                    == "conflict"
                {
                    return Ok::<_, AppError>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        f.runtime.object_invalidate(Some("connection")).await?;
        assert!(f.runtime.objects.lock().await.upload_directories.is_empty());
        assert_eq!(f.events.0.load(Ordering::SeqCst), 0);
        #[cfg(unix)]
        {
            let link = f._dir.path().join("link");
            std::os::unix::fs::symlink(&root, &link)?;
            assert!(
                f.runtime
                    .object_select_upload_directory("connection", 1, "".into(), &link)
                    .await
                    .is_err()
            );
            std::fs::remove_file(root.join("file"))?;
            std::os::unix::fs::symlink("missing", root.join("symlink"))?;
            std::fs::create_dir(root.join("empty"))?;
            let selected = f
                .runtime
                .object_select_upload_directory("connection", 1, "".into(), &root)
                .await?;
            f.runtime
                .object_start_upload_directory("connection", &selected.batch_id, Policy::Skip)
                .await?;
            let status = directory_settled(&f, &selected.batch_id).await?;
            assert_eq!(status.skipped_symlinks, 1);
            assert_eq!(status.skipped_empty_directories, 1);
            f.runtime
                .object_release_upload_directory("connection", &selected.batch_id)
                .await?;
            rustix::fs::mkfifoat(
                &cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority())?,
                "fifo",
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .map_err(std::io::Error::from)?;
            let selected = f
                .runtime
                .object_select_upload_directory("connection", 1, "".into(), &root)
                .await?;
            f.runtime
                .object_start_upload_directory("connection", &selected.batch_id, Policy::Skip)
                .await?;
            assert_eq!(
                directory_settled(&f, &selected.batch_id).await?.phase,
                "failed"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn upload_directory_cancel_event_policy() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let token = grant(&f).await?;
        let task = f
            .runtime
            .object_upload("connection", &token, "target".into(), None, false)
            .await?;
        settled(&f, &task.transfer_id).await?;
        f.runtime
            .object_finish(
                &task.transfer_id,
                Err(app(ObjectServiceError::Cancelled)),
                false,
            )
            .await;
        let status = f
            .runtime
            .object_status()
            .await?
            .into_iter()
            .find(|s| s.transfer_id == task.transfer_id)
            .ok_or("task")?;
        assert_eq!(status.phase, "cancelled");
        assert_eq!(status.error.ok_or("error")?.code, "CLOUD_CANCELLED");
        assert_eq!(f.events.0.load(Ordering::SeqCst), 0);
        f.runtime
            .object_finish(&task.transfer_id, Err(state_error()), false)
            .await;
        assert_eq!(f.events.0.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn bucket_selection_is_reused_by_later_lookups() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        *f.store.bucket.lock().map_err(|_| "bucket lock")? = "alternate".into();
        f.runtime
            .object_select_bucket("connection", "alternate")
            .await?;
        let session = f.runtime.object_session("connection").await?;
        assert_eq!(session.bucket, "alternate");
        // Browsing after the switch must keep the selected bucket instead of
        // rebuilding the saved default bucket session.
        f.runtime.object_list("connection", "", None).await?;
        let session = f.runtime.object_session("connection").await?;
        assert_eq!(session.bucket, "alternate");
        Ok(())
    }
    #[tokio::test]
    async fn cloud_object_preview_text_and_binary() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        let payload = f
            .runtime
            .object_preview("connection", "preview/small.md")
            .await?;
        assert_eq!(payload.file_name, "small.md");
        assert_eq!(payload.encoding, "utf8");
        assert_eq!(payload.mime, "text/markdown");
        assert_eq!(payload.data, "hello\n");
        assert_eq!(payload.size, 6);
        for (key, mime) in [
            ("preview/invalid.txt", "application/octet-stream"),
            ("preview/image.png", "image/png"),
        ] {
            let payload = f.runtime.object_preview("connection", key).await?;
            assert_eq!(payload.encoding, "base64");
            assert_eq!(payload.mime, mime);
            assert_eq!(payload.data, "/w==");
        }
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 3);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 6);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.probes.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cloud_object_preview_stat_rejects_large_before_get() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        let error = f
            .runtime
            .object_preview("connection", "preview/large.bin")
            .await
            .expect_err("oversized metadata must reject preview");
        assert_eq!(error.code, "PEER_PREVIEW_TOO_LARGE");
        assert_eq!(
            error.context,
            Some(serde_json::json!({
                "size": crate::types::MAX_PREVIEW_BYTES + 1,
                "limit": crate::types::MAX_PREVIEW_BYTES,
            }))
        );
        // reads includes stat: exactly one read proves get was never called.
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 1);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 1);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.probes.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cloud_object_preview_stops_stream_with_inaccurate_size() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        let error = f
            .runtime
            .object_preview("connection", "preview/lying.bin")
            .await
            .expect_err("stream must enforce preview limit independently of stat");
        assert_eq!(error.code, "PEER_PREVIEW_TOO_LARGE");
        assert_eq!(
            error.context,
            Some(serde_json::json!({
                "size": crate::types::MAX_PREVIEW_BYTES + 1,
                "limit": crate::types::MAX_PREVIEW_BYTES,
            }))
        );
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 1);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 2);
        assert_eq!(f.store.preview_chunks.load(Ordering::SeqCst), 2);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.probes.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cloud_object_preview_rejects_excess_lines() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        let error = f
            .runtime
            .object_preview("connection", "preview/lines.txt")
            .await
            .expect_err("too many lines must reject preview");
        assert_eq!(error.code, "PEER_PREVIEW_TOO_MANY_LINES");
        assert_eq!(
            error.context,
            Some(serde_json::json!({
                "lines": crate::types::MAX_PREVIEW_LINES + 1,
                "limit": crate::types::MAX_PREVIEW_LINES,
            }))
        );
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 1);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 2);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.probes.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn prepare_reuses_identity_and_skips_completed_discovery() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        let first = f.runtime.object_prepare("connection").await?;
        assert!(first.discovered);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 1);
        let second = f.runtime.object_prepare("connection").await?;
        assert!(second.discovered);
        assert_eq!(second.physical_id, first.physical_id);
        // A completed discovery is persisted, so reopening the drive must not
        // scan the connection scope again.
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 1);
        Ok(())
    }

    async fn grant(f: &Fixture) -> Result<String, TestError> {
        let path = f._dir.path().join("source");
        std::fs::write(&path, b"x")?;
        Ok(f.runtime
            .object_grant_selected("connection", 1, &path, true)
            .await?
            .token)
    }
    async fn settled(f: &Fixture, id: &str) -> Result<ObjectTransferDto, TestError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(status) = f.runtime.object_status().await?.into_iter().find(|s| {
                    s.transfer_id == id && !matches!(s.phase.as_str(), "running" | "cancelling")
                }) {
                    return Ok(status);
                }
                tokio::task::yield_now().await;
            }
        })
        .await?
    }
    #[tokio::test]
    async fn stuck_and_join_error_release_permits_and_allow_lock() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        f.runtime.object_delete_plan("connection", "data").await?;
        grant(&f).await?;
        for (id, fail) in [("stuck", false), ("join-error", true)] {
            let permit = Arc::clone(&f.runtime.network).read_owned().await;
            let handle = tokio::spawn(async move {
                let _permit = permit;
                if fail {
                    panic!("deterministic JoinError");
                }
                futures_util::future::pending::<()>().await;
            });
            f.runtime.objects.lock().await.tasks.insert(
                id.into(),
                ObjectTask {
                    created: std::time::Instant::now(),
                    status: ObjectTransferDto {
                        transfer_id: id.into(),
                        connection_id: "connection".into(),
                        connection_revision: 1,
                        direction: "upload".into(),
                        key: Some("target".into()),
                        size_hint: Some(1),
                        phase: "running".into(),
                        error: None,
                        recovery_required: false,
                    },
                    cancel: Arc::new(AtomicBool::new(false)),
                    handle: Some(handle),
                },
            );
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            f.runtime.stop_and_clear(),
        )
        .await??;
        let _permit = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            f.runtime.network.write(),
        )
        .await?;
        let state = f.runtime.objects.lock().await;
        assert!(
            state.sessions.is_empty()
                && state.plans.is_empty()
                && state.grants.is_empty()
                && state.guard.is_none()
        );
        assert!(state.tasks.values().all(|t| t.handle.is_none()));
        assert!(f.events.0.load(Ordering::SeqCst) >= 2);
        f.runtime.vault.lock()?;
        assert!(!f.runtime.vault.status()?.unlocked);
        Ok(())
    }
    #[tokio::test]
    async fn encrypted_recovery_ack_is_local_explicit_and_keeps_target() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let token = grant(&f).await?;
        assert!(
            f.runtime
                .object_upload("connection", "forged", "no-record".into(), None, false)
                .await
                .is_err()
        );
        assert!(f.runtime.object_recovery_read()?.is_empty());
        f.store.behavior.store(1, Ordering::SeqCst);
        let task = f
            .runtime
            .object_upload("connection", &token, "private-target".into(), None, false)
            .await?;
        assert!(!task.recovery_required);
        settled(&f, &task.transfer_id).await?;
        let reads = f.store.reads.load(Ordering::SeqCst);
        let mutations = f.store.mutations.load(Ordering::SeqCst);
        assert!(
            f.runtime
                .object_recovery_acknowledge(&task.transfer_id, "connection", 1, false)
                .await
                .is_err()
        );
        assert!(
            f.runtime
                .object_recovery_acknowledge(&task.transfer_id, "connection", 2, true)
                .await
                .is_err()
        );
        f.runtime.objects.lock().await.tasks.clear();
        let unknown = f
            .runtime
            .object_status()
            .await?
            .into_iter()
            .find(|s| s.transfer_id == task.transfer_id)
            .ok_or("missing recovery")?;
        assert_eq!(unknown.key.as_deref(), Some("private-target"));
        assert_eq!(unknown.size_hint, Some(1));
        let acknowledged = f
            .runtime
            .object_recovery_acknowledge(&task.transfer_id, "connection", 1, true)
            .await?;
        assert_eq!(acknowledged.phase, "acknowledged");
        assert!(!acknowledged.recovery_required);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), reads);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), mutations);
        assert_eq!(
            f.runtime.object_recovery_read()?[0].key.as_deref(),
            Some("private-target")
        );
        f.store.behavior.store(0, Ordering::SeqCst);
        let token = grant(&f).await?;
        let completed = f
            .runtime
            .object_upload("connection", &token, "completed-target".into(), None, false)
            .await?;
        assert_eq!(
            settled(&f, &completed.transfer_id).await?.phase,
            "completed"
        );
        assert!(
            !f.runtime
                .object_recovery_read()?
                .iter()
                .any(|s| s.transfer_id == completed.transfer_id)
        );
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), mutations + 1);
        let bytes = std::fs::read(f._dir.path().join("cloud-object-recovery.sqlite"))?;
        for secret in ["private-target", "fake-secret", "fake-access"] {
            assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
        }
        Ok(())
    }
    #[tokio::test]
    async fn expired_grants_and_plans_are_reclaimed() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let plan = f.runtime.object_delete_plan("connection", "data").await?;
        assert_eq!(plan.entries.len(), plan.object_count);
        assert_eq!(plan.entries[0].key, "data/file");
        assert_eq!(plan.entries[0].size, 1);
        assert_eq!(plan.entries[0].etag.as_deref(), Some("etag"));
        let token = grant(&f).await?;
        {
            let mut state = f.runtime.objects.lock().await;
            state.grants.get_mut(&token).ok_or("grant")?.expires = std::time::Instant::now();
            state
                .plans
                .get_mut(&plan.plan_id)
                .ok_or("plan")?
                .1
                .expires_at_unix = 0;
        }
        assert!(
            f.runtime
                .object_upload("connection", &token, "new".into(), None, false)
                .await
                .is_err()
        );
        assert!(
            f.runtime
                .object_delete_execute("connection", &plan.plan_id, true)
                .await
                .is_err()
        );
        let state = f.runtime.objects.lock().await;
        assert!(state.grants.is_empty() && state.plans.is_empty());
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        Ok(())
    }
    #[tokio::test]
    async fn locked_rejects_before_credentials_and_network() -> Result<(), TestError> {
        let f = fixture().await?;
        assert_eq!(
            f.runtime
                .object_list("connection", "", None)
                .await
                .err()
                .ok_or("expected gate error")?
                .code,
            "CLOUD_GATE_LOCKED"
        );
        assert_eq!(f.loads.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.reads.load(Ordering::SeqCst), 0);
        f.runtime.resume()?;
        f.runtime.stop_and_clear().await?;
        assert!(f.runtime.object_discover("connection", "").await.is_err());
        assert_eq!(f.loads.load(Ordering::SeqCst), 0);
        Ok(())
    }
    #[tokio::test]
    async fn browse_discovery_never_probe_or_grant_trust() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        f.runtime.object_list("connection", "", None).await?;
        f.runtime.object_discover("connection", "").await?;
        assert!(
            f.runtime
                .object_delete_plan("connection", "data")
                .await
                .is_err()
        );
        assert_eq!(f.store.probes.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        let state = f.runtime.objects.lock().await;
        assert_eq!(state.guard.as_ref().ok_or("guard")?.registry_epoch()?, 0);
        Ok(())
    }
    #[tokio::test]
    async fn list_protection_is_readonly_even_for_unknown_identity() -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        let root = f.runtime.object_list("connection", "", None).await?;
        assert!(!root.prefix_protected);
        let state = f.runtime.objects.lock().await;
        let guard = state.guard.as_ref().ok_or("guard")?;
        guard.register(crate::cloud::ProtectedMapping {
            identity: ProtectedIdentity {
                endpoint: "https://offline.invalid".into(),
                bucket: "bucket".into(),
                prefix: "data".into(),
                alias: String::new(),
            },
            repository_id: "repo".into(),
        })?;
        let epoch = guard.registry_epoch()?;
        drop(state);
        let root = f.runtime.object_list("connection", "", None).await?;
        assert!(root.prefix_protected);
        let page = f.runtime.object_list("connection", "data", None).await?;
        assert!(page.prefix_protected && page.objects[0].protected);
        assert_eq!(f.store.probes.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 0);
        assert_eq!(
            f.runtime
                .objects
                .lock()
                .await
                .guard
                .as_ref()
                .ok_or("guard")?
                .registry_epoch()?,
            epoch
        );
        assert!(
            f.runtime
                .object_delete_plan("connection", "untrusted")
                .await
                .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn delete_folder_trailing_slash_snapshot_lifecycle() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        f.store.listed_prefixes.lock().await.clear();
        let plan = f.runtime.object_delete_plan("connection", "docs/").await?;
        let keys: Vec<_> = plan.entries.iter().map(|entry| entry.key.clone()).collect();
        assert_eq!(keys, vec!["docs/a", "docs/nested/b"]);
        assert_eq!(plan.object_count, 2);
        assert!(plan.entries.iter().all(|entry| entry.size == 1
            && entry.etag.as_deref() == Some("etag") && !entry.protected));
        assert_eq!(*f.store.listed_prefixes.lock().await, vec!["docs/"]);
        assert_eq!(f.store.stats.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        assert!(f.store.deleted_keys.lock().await.is_empty());
        assert!(f.runtime.object_delete_execute("connection", &plan.plan_id, false).await.is_err());
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        // An object appearing after confirmation's snapshot must not be deleted.
        f.store.extra_objects.lock().await.push("docs/new-after-plan".into());
        let result = f.runtime.object_delete_execute("connection", &plan.plan_id, true).await?;
        assert_eq!(result.deleted, keys);
        assert!(result.failed.is_empty());
        assert_eq!(*f.store.deleted_keys.lock().await, keys);
        assert_eq!(*f.store.listed_prefixes.lock().await, vec!["docs/"]);
        assert_eq!(f.runtime.object_delete_execute("connection", &plan.plan_id, true)
            .await.err().ok_or("replay error")?.code, "CLOUD_DELETE_PLAN_STALE");
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 2);
        Ok(())
    }
    #[tokio::test]
    async fn delete_preview_uses_list_snapshot_without_stat() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let before = f.store.stats.load(Ordering::SeqCst);
        let plan = f.runtime.object_delete_plan("connection", "data").await?;
        assert_eq!(plan.object_count, plan.entries.len());
        assert_eq!(plan.entries[0].key, "data/file");
        assert_eq!(plan.entries[0].size, 1);
        assert_eq!(plan.entries[0].etag.as_deref(), Some("etag"));
        assert!(!plan.entries[0].protected);
        assert_eq!(f.store.stats.load(Ordering::SeqCst), before);
        Ok(())
    }
    #[tokio::test]
    async fn explicit_refresh_restarts_partial_browse_but_token_retries_are_bounded()
    -> Result<(), TestError> {
        let f = fixture().await?;
        f.runtime.resume()?;
        f.store.behavior.store(3, Ordering::SeqCst);
        let first = f.runtime.object_list("connection", "", None).await?;
        assert_eq!(first.next.as_deref(), Some("1"));
        f.runtime.object_list("connection", "other", None).await?;
        let refresh = f.runtime.object_list("connection", "", None).await?;
        assert_eq!(refresh.next, first.next);
        assert!(
            f.runtime
                .object_list("connection", "unseen", first.next.as_deref())
                .await
                .is_err()
        );
        let continued = f
            .runtime
            .object_list("connection", "", refresh.next.as_deref())
            .await?;
        assert_eq!(
            f.runtime
                .object_list("connection", "", refresh.next.as_deref())
                .await?
                .next,
            continued.next
        );
        let mut next = continued.next;
        for _ in 3..1000 {
            next = f
                .runtime
                .object_list("connection", "", next.as_deref())
                .await?
                .next;
        }
        let reads = f.store.reads.load(Ordering::SeqCst);
        assert!(
            f.runtime
                .object_list("connection", "", next.as_deref())
                .await
                .is_err()
        );
        assert_eq!(f.store.reads.load(Ordering::SeqCst), reads);
        assert_eq!(
            f.runtime
                .object_list("connection", "", None)
                .await?
                .next
                .as_deref(),
            Some("1")
        );
        f.store.behavior.store(4, Ordering::SeqCst);
        let first = f.runtime.object_list("connection", "failed", None).await?;
        for _ in 1..1000 {
            assert!(
                f.runtime
                    .object_list("connection", "failed", first.next.as_deref())
                    .await
                    .is_err()
            );
        }
        let reads = f.store.reads.load(Ordering::SeqCst);
        assert!(
            f.runtime
                .object_list("connection", "failed", first.next.as_deref())
                .await
                .is_err()
        );
        assert_eq!(f.store.reads.load(Ordering::SeqCst), reads);
        Ok(())
    }
    #[tokio::test]
    async fn protected_rejection_has_zero_mutations_and_no_stat() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let identity = ProtectedIdentity {
            endpoint: "https://offline.invalid".into(),
            bucket: "bucket".into(),
            prefix: "protected".into(),
            alias: String::new(),
        };
        f.runtime
            .objects
            .lock()
            .await
            .guard
            .as_ref()
            .ok_or("guard")?
            .register(crate::cloud::ProtectedMapping {
                identity,
                repository_id: "repo".into(),
            })?;
        let before = f.store.reads.load(Ordering::SeqCst);
        assert_eq!(
            f.runtime
                .object_delete_plan("connection", "protected")
                .await
                .err()
                .ok_or("protected error")?
                .code,
            "CLOUD_OBJECT_PROTECTED"
        );
        let token = grant(&f).await?;
        let task = f
            .runtime
            .object_upload("connection", &token, "protected/file".into(), None, false)
            .await?;
        assert_eq!(
            settled(&f, &task.transfer_id)
                .await?
                .error
                .ok_or("upload error")?
                .code,
            "CLOUD_OBJECT_PROTECTED"
        );
        assert_eq!(f.store.reads.load(Ordering::SeqCst), before);
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        Ok(())
    }
    #[tokio::test]
    async fn cached_delete_plan_works_once_but_revision_change_invalidates() -> Result<(), TestError>
    {
        let f = fixture().await?;
        authorize(&f).await?;
        let plan = f.runtime.object_delete_plan("connection", "data").await?;
        assert!(
            f.runtime
                .object_delete_execute("connection", &plan.plan_id, false)
                .await
                .is_err()
        );
        assert_eq!(
            f.runtime
                .object_delete_execute("connection", &plan.plan_id, true)
                .await?
                .deleted,
            vec!["data/file"]
        );
        assert!(
            f.runtime
                .object_delete_execute("connection", &plan.plan_id, true)
                .await
                .is_err()
        );
        let stale = f.runtime.object_delete_plan("connection", "data").await?;
        let token = grant(&f).await?;
        f.revision.store(2, Ordering::SeqCst);
        assert_eq!(
            f.runtime
                .object_delete_execute("connection", &stale.plan_id, true)
                .await
                .err()
                .ok_or("stale error")?
                .code,
            "CLOUD_DELETE_PLAN_STALE"
        );
        assert!(
            f.runtime
                .object_upload("connection", &token, "new".into(), None, false)
                .await
                .is_err()
        );
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 1);
        Ok(())
    }
    #[tokio::test]
    async fn cancel_is_joined_and_unknown_upload_survives_reload_without_secrets()
    -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        f.store.behavior.store(2, Ordering::SeqCst);
        let token = grant(&f).await?;
        let task = f
            .runtime
            .object_upload("connection", &token, "new".into(), None, false)
            .await?;
        while f.store.mutations.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        f.runtime.object_cancel(&task.transfer_id).await?;
        assert_eq!(settled(&f, &task.transfer_id).await?.phase, "cancelled");
        assert!(f.runtime.object_recovery_read()?.is_empty());
        f.store.behavior.store(1, Ordering::SeqCst);
        let token = grant(&f).await?;
        let task = f
            .runtime
            .object_upload("connection", &token, "new".into(), None, false)
            .await?;
        let result = settled(&f, &task.transfer_id).await?;
        assert!(result.recovery_required);
        assert_eq!(
            result.error.ok_or("unknown error")?.code,
            "CLOUD_UPLOAD_PENDING_RECOVERY"
        );
        assert_eq!(f.runtime.object_recovery_read()?.len(), 1);
        f.runtime.objects.lock().await.tasks.clear();
        let recovered = f.runtime.object_status().await?;
        assert_eq!(recovered[0].phase, "unknown");
        assert!(recovered[0].recovery_required);
        let bytes = std::fs::read(f._dir.path().join("cloud-object-recovery.sqlite"))?;
        assert!(
            !bytes
                .windows(b"fake-secret".len())
                .any(|w| w == b"fake-secret")
        );
        // Expected cancellation is audited in status, not globally reported;
        // the unknown completion still emits its recovery error.
        assert_eq!(f.events.0.load(Ordering::SeqCst), 1);
        f.runtime.stop_and_clear().await?;
        Ok(())
    }
    #[tokio::test]
    async fn download_capability_preserves_existing_target_without_confirmation()
    -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let target = f._dir.path().join("target");
        std::fs::create_dir(&target)?;
        std::fs::write(target.join("file"), b"original")?;
        let token = f
            .runtime
            .object_grant_selected("connection", 1, &target, false)
            .await?
            .token;
        let task = f
            .runtime
            .object_download(
                "connection",
                &token,
                "data/file".into(),
                "file".into(),
                false,
            )
            .await?;
        assert_eq!(
            settled(&f, &task.transfer_id)
                .await?
                .error
                .ok_or("expected conflict")?
                .code,
            "CLOUD_PRECONDITION"
        );
        assert_eq!(std::fs::read(target.join("file"))?, b"original");
        let token = f
            .runtime
            .object_grant_selected("connection", 1, &target, false)
            .await?
            .token;
        let task = f
            .runtime
            .object_download(
                "connection",
                &token,
                "data/file".into(),
                "file".into(),
                true,
            )
            .await?;
        assert_eq!(settled(&f, &task.transfer_id).await?.phase, "completed");
        assert_eq!(std::fs::read(target.join("file"))?, b"x");
        assert!(
            f.runtime
                .object_download(
                    "connection",
                    &token,
                    "data/file".into(),
                    "file".into(),
                    true
                )
                .await
                .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn batch_download_writes_relative_paths_and_keeps_grant_for_confirmation()
    -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let target = f._dir.path().join("batch-target");
        std::fs::create_dir(&target)?;
        let token = f
            .runtime
            .object_grant_selected("connection", 1, &target, false)
            .await?
            .token;
        let items = vec![
            ObjectDownloadItemDto {
                key: "data/file".into(),
                relative_path: "one/file".into(),
            },
            ObjectDownloadItemDto {
                key: "data/file".into(),
                relative_path: "two/deep/file".into(),
            },
        ];
        let transfers = f
            .runtime
            .object_download_batch("connection", &token, items.clone(), false)
            .await?;
        assert_eq!(transfers.transfers.len(), 2);
        assert_eq!(transfers.local_root, target.to_string_lossy());
        for transfer in &transfers.transfers {
            assert_eq!(settled(&f, &transfer.transfer_id).await?.phase, "completed");
        }
        assert_eq!(std::fs::read(target.join("one/file"))?, b"x");
        assert_eq!(std::fs::read(target.join("two/deep/file"))?, b"x");
        // The capability token covers the whole batch and is consumed by it.
        assert!(
            f.runtime
                .object_download_batch("connection", &token, items, false)
                .await
                .is_err()
        );
        let token = f
            .runtime
            .object_grant_selected("connection", 1, &target, false)
            .await?
            .token;
        let conflict = f
            .runtime
            .object_download_batch(
                "connection",
                &token,
                vec![ObjectDownloadItemDto {
                    key: "data/file".into(),
                    relative_path: "one/file".into(),
                }],
                false,
            )
            .await
            .expect_err("existing destinations must ask for confirmation");
        assert_eq!(conflict.code, "TRANSFER_CONFLICT");
        // The rejected preflight must not consume the grant.
        let transfers = f
            .runtime
            .object_download_batch(
                "connection",
                &token,
                vec![ObjectDownloadItemDto {
                    key: "data/file".into(),
                    relative_path: "one/file".into(),
                }],
                true,
            )
            .await?;
        assert_eq!(
            settled(&f, &transfers.transfers[0].transfer_id)
                .await?
                .phase,
            "completed"
        );
        assert_eq!(std::fs::read(target.join("one/file"))?, b"x");
        Ok(())
    }
    #[tokio::test]
    async fn vault_stop_cancels_joins_and_clears_file_grants() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        f.store.behavior.store(2, Ordering::SeqCst);
        let token = grant(&f).await?;
        f.runtime
            .object_upload("connection", &token, "new".into(), None, false)
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while f.store.mutations.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.runtime.stop_and_clear(),
        )
        .await??;
        let state = f.runtime.objects.lock().await;
        assert!(state.sessions.is_empty() && state.grants.is_empty() && state.plans.is_empty());
        assert!(state.guard.is_none());
        assert!(
            state
                .tasks
                .values()
                .all(|task| task.handle.is_none() && task.status.phase == "cancelled")
        );
        Ok(())
    }
    #[tokio::test]
    async fn direct_vault_lock_refuses_cached_credentials() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        let loads = f.loads.load(Ordering::SeqCst);
        f.runtime.vault.lock()?;
        assert!(f.runtime.object_list("connection", "", None).await.is_err());
        assert!(f.runtime.object_status().await.is_err());
        assert_eq!(f.loads.load(Ordering::SeqCst), loads);
        Ok(())
    }
    #[tokio::test]
    async fn forged_local_token_and_overwrite_without_confirmation_fail() -> Result<(), TestError> {
        let f = fixture().await?;
        authorize(&f).await?;
        assert!(
            f.runtime
                .object_upload("connection", "/etc/passwd", "new".into(), None, false)
                .await
                .is_err()
        );
        let token = grant(&f).await?;
        assert!(
            f.runtime
                .object_upload(
                    "connection",
                    &token,
                    "data/file".into(),
                    Some("etag".into()),
                    false
                )
                .await
                .is_err()
        );
        assert_eq!(f.store.mutations.load(Ordering::SeqCst), 0);
        Ok(())
    }
}

fn config_error(source: super::config_types::ConfigError) -> AppError {
    app(DomainError::Cloud {
        code: "CLOUD_OBJECT_CONFIG",
        user_reason: "云存储配置不可用 / Cloud configuration unavailable",
        source: Box::new(source),
    })
}

impl CloudRuntime {
    pub(crate) fn object_admit(&self) -> Result<(), AppError> {
        self.admit()?;
        if !self.vault.status().map_err(AppError::from)?.unlocked {
            return Err(AppError::from(DomainError::VaultLocked));
        }
        Ok(())
    }
    /// Caller holds the shared network permit; credentials are loaded only after admission.
    pub(crate) async fn object_session(
        &self,
        connection_id: &str,
    ) -> Result<Arc<ObjectSession>, AppError> {
        self.object_session_for(connection_id, None).await
    }
    /// Switch the active bucket for an open connection. The session is rebuilt
    /// with that bucket; the saved connection is unchanged.
    pub async fn object_select_bucket(
        &self,
        connection_id: &str,
        bucket: &str,
    ) -> Result<(), AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let bucket = bucket.trim();
        if bucket.is_empty() {
            return Err(app(CloudError::InvalidConfig("bucket is required".into())));
        }
        // object_session_for validates the bucket against the saved connection
        // and records the selection for every later lookup.
        self.object_session_for(connection_id, Some(bucket)).await?;
        Ok(())
    }
    async fn object_session_for(
        &self,
        connection_id: &str,
        bucket: Option<&str>,
    ) -> Result<Arc<ObjectSession>, AppError> {
        self.object_admit()?;
        let mut state = self.objects.lock().await;
        state.prune();
        let load = || {
            self.config
                .snapshot()
                .map_err(config_error)?
                .connections
                .into_iter()
                .find(|c| c.id == connection_id)
                .ok_or_else(|| app(CloudError::NotFound))
        };
        #[cfg(not(test))]
        let connection = load()?;
        #[cfg(test)]
        let connection = match &state.backend {
            Some(backend) => (backend.load)()?,
            None => load()?,
        };
        let wanted_bucket = match bucket {
            Some(selected) => {
                if selected != connection.config.bucket
                    && !connection
                        .config
                        .buckets
                        .iter()
                        .any(|item| item == selected)
                {
                    return Err(app(CloudError::NotFound));
                }
                state
                    .active_buckets
                    .insert(connection_id.to_owned(), selected.to_owned());
                selected.to_owned()
            }
            None => {
                // Keep the bucket the user switched to; only fall back to the
                // saved bucket when the selection is gone from the config.
                let selected = state
                    .active_buckets
                    .get(connection_id)
                    .filter(|selected| {
                        *selected == &connection.config.bucket
                            || connection
                                .config
                                .buckets
                                .iter()
                                .any(|item| item == *selected)
                    })
                    .cloned();
                if selected.is_none() {
                    state.active_buckets.remove(connection_id);
                }
                selected.unwrap_or_else(|| connection.config.bucket.clone())
            }
        };
        if let Some(session) = state.sessions.get(connection_id) {
            if session.revision == connection.revision && session.bucket == wanted_bucket {
                return Ok(Arc::clone(session));
            }
        }
        state.sessions.remove(connection_id);
        state.plans.retain(|_, (id, _)| id != connection_id);
        state
            .grants
            .retain(|_, grant| grant.connection_id != connection_id);
        for task in state.tasks.values_mut().filter(|t| {
            t.status.connection_id == connection_id
                && t.status.connection_revision != connection.revision
        }) {
            task.cancel.store(true, Ordering::Release);
        }
        if state.guard.is_none() {
            let key = self
                .config
                .get_or_create_index_key()
                .map_err(config_error)?;
            state.guard = Some(
                ObjectGuard::open(
                    self.data_dir.join("cloud-object-guard.sqlite"),
                    key.as_ref(),
                )
                .map_err(app)?,
            );
        }
        let guard = state.guard.as_ref().ok_or_else(state_error)?.clone();
        let mut config = connection.config.clone();
        config.bucket = wanted_bucket.clone();
        let identity = ProtectedIdentity {
            endpoint: config.endpoint.clone(),
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
            alias: String::new(),
        };
        // Deliberately no verify_capabilities, repository join, or encrypted tree reads.
        let production_store = || -> Result<Arc<dyn ObjectStore>, AppError> {
            let raw = Arc::new(super::S3Store::new(config).map_err(app)?);
            let backend = peerdav_protocol_s3::CoreS3Backend::new(raw, connection.revision)
                .map_err(|source| app(super::types::CloudError::Storage {
                    source: object_store::Error::Generic { store: "file-core", source: Box::new(source) },
                }))?;
            Ok(Arc::new(peerdav_protocol_s3::CoreS3ReadBridge(backend)))
        };
        #[cfg(not(test))]
        let store = production_store()?;
        #[cfg(test)]
        let store: Arc<dyn ObjectStore> = match &state.backend {
            Some(backend) => Arc::clone(&backend.store),
            None => production_store()?,
        };
        let service = Arc::new(
            ObjectService::try_new(
                Arc::clone(&store),
                guard,
                identity.clone(),
                connection.revision.to_string(),
            )
            .map_err(app)?,
        );
        let session = Arc::new(ObjectSession {
            browse: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            revision: connection.revision,
            bucket: wanted_bucket,
            service,
            store,
            identity,
        });
        state
            .sessions
            .insert(connection_id.to_owned(), Arc::clone(&session));
        Ok(session)
    }
    /// Validates an address and its credentials by listing buckets. This runs
    /// before a connection is saved, so it never reads or writes configuration.
    pub async fn probe_buckets(
        &self,
        input: ProbeBucketsInput,
    ) -> Result<Vec<super::service::BucketEntry>, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        super::service::probe(
            &input.endpoint,
            &input.region,
            &input.access_key,
            &input.secret_key,
            input.session_token.as_deref(),
            input.allow_insecure_http,
            &input.proxy,
        )
        .await
        .map_err(app)
    }
    /// Returns saved credentials for the edit form. Requires an unlocked vault
    /// and is intentionally separate from the redacted catalog.
    pub fn connection_credentials(
        &self,
        connection_id: &str,
    ) -> Result<super::config_types::ConnectionCredentials, AppError> {
        self.object_admit()?;
        let connection = self
            .config
            .snapshot()
            .map_err(config_error)?
            .connections
            .into_iter()
            .find(|c| c.id == connection_id)
            .ok_or_else(|| app(CloudError::NotFound))?;
        Ok(super::config_types::ConnectionCredentials {
            access_key: connection.config.access_key,
            secret_key: connection.config.secret_key,
            session_token: connection.config.session_token,
        })
    }
    pub async fn object_preview(
        &self,
        connection_id: &str,
        key: &str,
    ) -> Result<crate::types::PreviewPayload, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        let limit = crate::types::MAX_PREVIEW_BYTES;
        let metadata = session.store.stat(key).await.map_err(app)?;
        if metadata.size > limit {
            return Err(DomainError::PreviewTooLarge {
                size: metadata.size,
                limit,
            }
            .into());
        }
        let mut stream = session.store.get(key, None).await.map_err(app)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(app)?;
            let size = (bytes.len() as u64).saturating_add(chunk.len() as u64);
            if size > limit {
                return Err(DomainError::PreviewTooLarge { size, limit }.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let file_name = key
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("object")
            .to_owned();
        crate::peer::preview_payload(file_name, bytes).map_err(AppError::from)
    }

    pub async fn object_list(
        &self,
        connection_id: &str,
        prefix: &str,
        next: Option<&str>,
    ) -> Result<ObjectPageDto, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        let mut operations = session.browse.lock().await;
        // No token is an explicit user browse/refresh, not an automatic retry.
        if next.is_none() {
            operations.insert(prefix.to_owned(), ObjectBrowseOperation::default());
        }
        let operation = operations
            .get_mut(prefix)
            .ok_or_else(|| app(ObjectServiceError::DiscoveryIncomplete))?;
        let retry = next.is_some() && next == operation.last_token.as_deref();
        if !retry && (operation.complete || next != operation.next.as_deref()) {
            return Err(app(ObjectServiceError::DiscoveryIncomplete));
        }
        let page = session
            .service
            .browse_with_budget(prefix, next, &mut operation.ledger)
            .await
            .map_err(app)?;
        if !retry {
            if let Some(token) = &page.next
                && !operation.seen.insert(token.clone())
            {
                operation.complete = true;
                return Err(app(ObjectServiceError::DiscoveryIncomplete));
            }
        } else if page.next != operation.next {
            operation.complete = true;
            return Err(app(ObjectServiceError::DiscoveryIncomplete));
        }
        operation.last_token = next.map(str::to_owned);
        operation.next = page.next.clone();
        operation.complete = page.complete;
        // The browse prefix carries a trailing separator for folders; the
        // protection query speaks precise keys, so trim it like the store does.
        let prefix_protected = session
            .service
            .is_protected(page.prefix.trim_end_matches('/'))
            .map_err(app)?;
        let objects = page
            .objects
            .into_iter()
            .map(|metadata| {
                Ok(ObjectEntryDto {
                    protected: session.service.is_protected(&metadata.key).map_err(app)?,
                    key: metadata.key,
                    size: metadata.size,
                    etag: metadata.etag,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        Ok(ObjectPageDto {
            prefix: page.prefix,
            objects,
            next: page.next,
            complete: page.complete,
            prefix_protected,
        })
    }
    pub async fn object_prepare(&self, connection_id: &str) -> Result<ObjectPreparedDto, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        let (guard, physical_id) = {
            let mut state = self.objects.lock().await;
            state.prune();
            let guard = state.guard.as_ref().ok_or_else(state_error)?.clone();
            let physical_id = guard
                .ensure_storage_identity(&session.identity)
                .map_err(app)?;
            (guard, physical_id)
        };
        if !guard
            .has_completed_discovery(&session.identity)
            .map_err(app)?
        {
            // `discover` takes a prefix relative to the connection scope; the
            // connection prefix itself is already part of the service identity.
            session.service.discover("").await.map_err(app)?;
        }
        if !guard
            .has_completed_discovery(&session.identity)
            .map_err(app)?
        {
            return Err(app(ObjectServiceError::DiscoveryIncomplete));
        }
        Ok(ObjectPreparedDto {
            physical_id,
            discovered: true,
        })
    }

    pub async fn object_discover(
        &self,
        connection_id: &str,
        prefix: &str,
    ) -> Result<ObjectDiscoveryDto, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        self.object_session(connection_id)
            .await?
            .service
            .discover(prefix)
            .await
            .map_err(app)?;
        Ok(ObjectDiscoveryDto { complete: true })
    }
    pub(crate) async fn object_selection_scope(
        &self,
        connection_id: &str,
    ) -> Result<(u64, ProtectedIdentity), AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        Ok((session.revision, session.identity.clone()))
    }
    /// Only the backend dialog command may call this after local user confirmation.
    pub(crate) async fn object_register_identity(
        &self,
        connection_id: &str,
        expected_revision: u64,
        association: ObjectIdentityInput,
    ) -> Result<ObjectIdentityDto, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        if session.revision != expected_revision {
            return Err(app(ObjectServiceError::StaleDeletePlan));
        }
        let mut state = self.objects.lock().await;
        state.prune();
        let guard = state.guard.as_ref().ok_or_else(state_error)?;
        let association = match association {
            ObjectIdentityInput::New {} => StorageIdentityAssociation::New,
            ObjectIdentityInput::Existing { physical_id } => {
                StorageIdentityAssociation::Existing(physical_id)
            }
        };
        let physical_id = guard
            .register_storage_identity(&session.identity, association)
            .map_err(app)?;
        state.plans.clear();
        Ok(ObjectIdentityDto { physical_id })
    }
    pub async fn object_delete_plan(
        &self,
        connection_id: &str,
        prefix: &str,
    ) -> Result<ObjectDeletePlanDto, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| {
                app(DomainError::Internal {
                    detail: e.to_string(),
                })
            })?
            .as_secs();
        let plan = session
            .service
            .delete_plan(prefix, now.saturating_add(300))
            .await
            .map_err(app)?;
        let entries = session
            .service
            .delete_plan_snapshot(&plan.id)
            .map_err(app)?
            .into_iter()
            .map(|(key, size, etag)| {
                Ok(ObjectEntryDto {
                    protected: session.service.is_protected(&key).map_err(app)?,
                    key,
                    size,
                    etag,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        if entries.len() != plan.entries.len() {
            return Err(app(ObjectServiceError::StaleDeletePlan));
        }
        let result = ObjectDeletePlanDto {
            plan_id: plan.id.clone(),
            expires_at_unix: plan.expires_at_unix,
            object_count: plan.entries.len(),
            connection_revision: session.revision,
            entries,
        };
        // The service bounded the full escaped DTO before cloning its snapshot.
        let mut state = self.objects.lock().await;
        state.prune();
        if state.plans.len() >= 128 {
            return Err(state_error());
        }
        state
            .plans
            .insert(plan.id.clone(), (connection_id.to_owned(), plan));
        Ok(result)
    }
    pub async fn object_delete_execute(
        &self,
        connection_id: &str,
        plan_id: &str,
        confirmed: bool,
    ) -> Result<ObjectDeleteResultDto, AppError> {
        self.object_admit()?;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        if !confirmed {
            return Err(app(ObjectServiceError::ConfirmationRequired));
        }
        let plan = {
            let mut state = self.objects.lock().await;
            state.prune();
            let (id, plan) = state
                .plans
                .get(plan_id)
                .ok_or_else(|| app(ObjectServiceError::StaleDeletePlan))?;
            if id != connection_id || plan.revision != session.revision.to_string() {
                return Err(app(ObjectServiceError::StaleDeletePlan));
            }
            let plan = plan.clone();
            state.plans.remove(plan_id);
            plan
        };
        let entries = plan.entries.clone();
        let results = session
            .service
            .execute_delete(&plan, true)
            .await
            .map_err(app)?;
        let mut result = ObjectDeleteResultDto {
            deleted: Vec::new(),
            failed: Vec::new(),
        };
        for (entry, outcome) in entries.into_iter().zip(results) {
            match outcome {
                Ok(key) => result.deleted.push(key),
                Err(error) => result.failed.push(ObjectDeleteFailureDto {
                    key: entry.key,
                    error: app(error),
                }),
            }
        }
        Ok(result)
    }
    /// Paths arrive only from native backend dialogs, never from IPC strings.
    pub(crate) async fn object_grant_selected(
        &self,
        connection_id: &str,
        expected_revision: u64,
        path: &Path,
        upload: bool,
    ) -> Result<ObjectLocalCapabilityDto, AppError> {
        self.object_admit()?;
        // Serialize capability publication against credential invalidation.
        let _lifecycle = self.object_lifecycle.lock().await;
        let _permit = self.network.read().await;
        let session = self.object_session(connection_id).await?;
        if session.revision != expected_revision {
            return Err(app(ObjectServiceError::StaleDeletePlan));
        }
        let grant = if upload {
            let metadata = std::fs::symlink_metadata(path).map_err(local)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(state_error());
            }
            #[cfg(unix)]
            let file = {
                let fd = rustix::fs::open(
                    path,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|e| local(e.into()))?;
                std::fs::File::from(fd)
            };
            #[cfg(not(unix))]
            let file = std::fs::File::open(path).map_err(local)?;
            if !file.metadata().map_err(local)?.is_file() {
                return Err(state_error());
            }
            LocalGrant::Upload(file)
        } else {
            LocalGrant::Download(super::PhysicalRoot::open(path).map_err(app)?)
        };
        let token = random_id()?;
        let mut state = self.objects.lock().await;
        state.prune();
        if state.local_handle_count() >= MAX_LOCAL_HANDLES {
            return Err(state_error());
        }
        state.grants.insert(
            token.clone(),
            BoundGrant {
                connection_id: connection_id.to_owned(),
                revision: session.revision,
                expires: std::time::Instant::now() + std::time::Duration::from_secs(300),
                grant,
            },
        );
        Ok(ObjectLocalCapabilityDto {
            token,
            file_name: if upload {
                path.file_name().map(|n| n.to_string_lossy().into_owned())
            } else {
                None
            },
        })
    }
    pub async fn object_upload(
        self: &Arc<Self>,
        connection_id: &str,
        token: &str,
        key: String,
        overwrite_etag: Option<String>,
        overwrite_confirmed: bool,
    ) -> Result<ObjectTransferDto, AppError> {
        if overwrite_etag.is_some() && !overwrite_confirmed {
            return Err(app(ObjectServiceError::ConfirmationRequired));
        }
        self.object_start_transfer(
            connection_id,
            token,
            key,
            None,
            overwrite_etag,
            overwrite_confirmed,
        )
        .await
    }
    pub async fn object_download(
        self: &Arc<Self>,
        connection_id: &str,
        token: &str,
        key: String,
        file_name: String,
        overwrite_confirmed: bool,
    ) -> Result<ObjectTransferDto, AppError> {
        self.object_start_transfer(
            connection_id,
            token,
            key,
            Some(file_name),
            None,
            overwrite_confirmed,
        )
        .await
    }
    /// Downloads several objects below the folder covered by one capability
    /// token. Each item becomes its own transfer so the UI reports per-file
    /// progress, and relative paths let a folder download recreate its tree.
    pub async fn object_download_batch(
        self: &Arc<Self>,
        connection_id: &str,
        token: &str,
        items: Vec<ObjectDownloadItemDto>,
        overwrite_confirmed: bool,
    ) -> Result<ObjectDownloadBatchDto, AppError> {
        self.object_admit()?;
        if items.is_empty() || items.len() > crate::types::MAX_CLOUD_DOWNLOAD_BATCH {
            return Err(DomainError::DownloadQueueTooLarge {
                count: items.len(),
                limit: crate::types::MAX_CLOUD_DOWNLOAD_BATCH,
            }
            .into());
        }
        let mut relative_paths = std::collections::HashSet::new();
        for item in &items {
            super::object_service::valid_relative_path(&item.relative_path).map_err(app)?;
            if !relative_paths.insert(item.relative_path.clone()) {
                return Err(app(ObjectServiceError::InvalidKey));
            }
        }
        let _lifecycle = self.object_lifecycle.lock().await;
        let session = self.object_session(connection_id).await?;
        let mut state = self.objects.lock().await;
        state.prune();
        // Conflicts are detected before the token is consumed, so the user can
        // confirm overwriting and replay the same batch with the same grant.
        let conflicts = {
            let bound = state.grants.get(token).ok_or_else(state_error)?;
            if bound.connection_id != connection_id || bound.revision != session.revision {
                return Err(state_error());
            }
            let LocalGrant::Download(root) = &bound.grant else {
                return Err(state_error());
            };
            items.iter().try_fold(0usize, |count, item| {
                root.relative_target_exists(&item.relative_path)
                    .map_err(app)
                    .map(|exists| count + usize::from(exists))
            })?
        };
        if conflicts > 0 && !overwrite_confirmed {
            return Err(DomainError::TransferConflict {
                path: format!("{conflicts} download destination(s) already exist"),
            }
            .into());
        }
        let bound = state.grants.remove(token).ok_or_else(state_error)?;
        if bound.connection_id != connection_id || bound.revision != session.revision {
            return Err(state_error());
        }
        let LocalGrant::Download(root) = bound.grant else {
            return Err(state_error());
        };
        if state.tasks.len() + items.len() > crate::types::MAX_CLOUD_TASKS {
            let excess = state.tasks.len() + items.len() - crate::types::MAX_CLOUD_TASKS;
            let mut finished: Vec<(std::time::Instant, String)> = state
                .tasks
                .iter()
                .filter(|(_, task)| task.handle.is_none())
                .map(|(id, task)| (task.created, id.clone()))
                .collect();
            finished.sort_by_key(|(created, _)| *created);
            for (_, id) in finished.into_iter().take(excess) {
                state.tasks.remove(&id);
            }
            if state.tasks.len() + items.len() > crate::types::MAX_CLOUD_TASKS {
                return Err(state_error());
            }
        }
        let executor = tokio::runtime::Handle::try_current().map_err(|source| {
            app(DomainError::Internal {
                detail: source.to_string(),
            })
        })?;
        let guard = state.guard.as_ref().ok_or_else(state_error)?.clone();
        let slots = Arc::new(tokio::sync::Semaphore::new(
            crate::types::MAX_CLOUD_DOWNLOAD_CONCURRENCY,
        ));
        let mut statuses = Vec::with_capacity(items.len());
        for item in items {
            let root = root.try_clone().map_err(app)?;
            let id = random_id()?;
            let status = ObjectTransferDto {
                transfer_id: id.clone(),
                connection_id: connection_id.to_owned(),
                connection_revision: session.revision,
                direction: "download".into(),
                key: Some(item.key.clone()),
                size_hint: None,
                phase: "running".into(),
                error: None,
                recovery_required: false,
            };
            let cancel = Arc::new(AtomicBool::new(false));
            state.tasks.insert(
                id.clone(),
                ObjectTask {
                    created: std::time::Instant::now(),
                    status: status.clone(),
                    cancel: Arc::clone(&cancel),
                    handle: None,
                },
            );
            let runtime = Arc::clone(self);
            let task_id = id.clone();
            let key = item.key;
            let relative_path = item.relative_path;
            let connection = connection_id.to_owned();
            let revision = session.revision;
            let guard = guard.clone();
            let store = Arc::clone(&session.store);
            let identity = session.identity.clone();
            let slots = Arc::clone(&slots);
            let handle = executor.spawn(async move {
                let slot = slots.acquire_owned().await;
                let Ok(_slot) = slot else {
                    runtime
                        .object_finish(
                            &task_id,
                            Err(app(DomainError::Internal {
                                detail: "cloud download queue closed".into(),
                            })),
                            false,
                        )
                        .await;
                    return;
                };
                let _permit = runtime.network.read().await;
                let operation = async {
                    runtime.object_admit()?;
                    if cancel.load(Ordering::Acquire) {
                        return Err(app(ObjectServiceError::Cancelled));
                    }
                    let current = runtime.object_session(&connection).await?;
                    if current.revision != revision {
                        return Err(app(ObjectServiceError::StaleDeletePlan));
                    }
                    let service =
                        ObjectService::try_new(store, guard, identity, revision.to_string())
                            .map_err(app)?
                            .with_physical_root(root);
                    service
                        .download_to_relative(
                            &key,
                            &relative_path,
                            Some(cancel),
                            overwrite_confirmed,
                        )
                        .await
                        .map_err(app)?;
                    Ok(())
                };
                let result = match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                    Ok(result) => result,
                    Err(_) => Err(app(DomainError::Internal {
                        detail: "Object transfer task panicked".into(),
                    })),
                };
                runtime.object_finish(&task_id, result, false).await;
            });
            if let Some(entry) = state.tasks.get_mut(&id) {
                entry.handle = Some(handle);
            }
            statuses.push(status);
        }
        drop(state);
        Ok(ObjectDownloadBatchDto {
            transfers: statuses,
            local_root: root.path().to_string_lossy().into_owned(),
        })
    }
    async fn object_start_transfer(
        self: &Arc<Self>,
        connection_id: &str,
        token: &str,
        key: String,
        file_name: Option<String>,
        overwrite: Option<String>,
        confirmed: bool,
    ) -> Result<ObjectTransferDto, AppError> {
        let _lifecycle = self.object_lifecycle.lock().await;
        self.object_admit()?;
        let permit = Arc::clone(&self.network).read_owned().await;
        let session = self.object_session(connection_id).await?;
        let mut state = self.objects.lock().await;
        state.prune();
        if state.tasks.len() >= crate::types::MAX_CLOUD_TASKS {
            if let Some(oldest) = state
                .tasks
                .iter()
                .filter(|(_, task)| task.handle.is_none())
                .min_by_key(|(_, task)| task.created)
                .map(|(id, _)| id.clone())
            {
                state.tasks.remove(&oldest);
            } else {
                return Err(state_error());
            }
        }
        let bound = state.grants.remove(token).ok_or_else(state_error)?;
        if bound.connection_id != connection_id || bound.revision != session.revision {
            return Err(state_error());
        }
        let upload = file_name.is_none();
        if upload != matches!(bound.grant, LocalGrant::Upload(_)) {
            return Err(state_error());
        }
        let executor = tokio::runtime::Handle::try_current().map_err(|source| {
            app(DomainError::Internal {
                detail: source.to_string(),
            })
        })?;
        let guard = state.guard.as_ref().ok_or_else(state_error)?.clone();
        let size_hint = match &bound.grant {
            LocalGrant::Upload(file) => Some(file.metadata().map_err(local)?.len()),
            LocalGrant::Download(_) => None,
        };
        let id = random_id()?;
        let status = ObjectTransferDto {
            transfer_id: id.clone(),
            connection_id: connection_id.to_owned(),
            connection_revision: session.revision,
            direction: if upload { "upload" } else { "download" }.into(),
            key: Some(key.clone()),
            size_hint,
            phase: "running".into(),
            error: None,
            recovery_required: false,
        };
        // Write-ahead conservative recovery record: a crash never claims cleanup.
        if upload {
            self.object_recovery_write(&status)?;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.tasks.insert(
            id.clone(),
            ObjectTask {
                created: std::time::Instant::now(),
                status: status.clone(),
                cancel: Arc::clone(&cancel),
                handle: None,
            },
        );
        let runtime = Arc::clone(self);
        let task_id = id.clone();
        let task = executor.spawn(async move {
            let _permit = permit;
            let operation = async {
                runtime.object_admit()?;
                if cancel.load(Ordering::Acquire) {
                    return Err(app(ObjectServiceError::Cancelled));
                }
                let current = runtime.object_session(&status.connection_id).await?;
                if current.revision != session.revision {
                    return Err(app(ObjectServiceError::StaleDeletePlan));
                }
                match bound.grant {
                    LocalGrant::Upload(file) => {
                        let stream = futures_util::stream::try_unfold(
                            tokio::fs::File::from_std(file),
                            |mut file| async move {
                                let mut buffer = vec![0u8; 1024 * 1024];
                                let count = file.read(&mut buffer).await.map_err(|source| {
                                    CloudError::Storage {
                                        source: object_store::Error::Generic {
                                            store: "local file",
                                            source: Box::new(source),
                                        },
                                    }
                                })?;
                                if count == 0 {
                                    return Ok(None);
                                }
                                buffer.truncate(count);
                                Ok(Some((bytes::Bytes::from(buffer), file)))
                            },
                        )
                        .boxed();
                        session
                            .service
                            .upload_stream_with_control(&key, stream, cancel, overwrite)
                            .await
                            .map_err(app)?;
                    }
                    LocalGrant::Download(root) => {
                        let service = ObjectService::try_new(
                            Arc::clone(&session.store),
                            guard,
                            session.identity.clone(),
                            session.revision.to_string(),
                        )
                        .map_err(app)?
                        .with_physical_root(root);
                        service
                            .download_to(
                                &key,
                                file_name.as_deref().ok_or_else(state_error)?,
                                Some(cancel),
                                confirmed,
                            )
                            .await
                            .map_err(app)?;
                    }
                }
                Ok(())
            };
            let result = match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                Ok(result) => result,
                Err(_) => Err(app(DomainError::Internal {
                    detail: "Object transfer task panicked".into(),
                })),
            };
            runtime.object_finish(&task_id, result, upload).await;
        });
        if let Some(entry) = state.tasks.get_mut(&id) {
            entry.handle = Some(task);
        }
        drop(state);
        Ok(self
            .objects
            .lock()
            .await
            .tasks
            .get(&id)
            .ok_or_else(state_error)?
            .status
            .clone())
    }
    async fn object_finish(&self, id: &str, result: Result<(), AppError>, upload: bool) {
        let mut state = self.objects.lock().await;
        state.prune();
        if let Some(task) = state.tasks.get_mut(id) {
            task.status.phase = match &result {
                Ok(()) => "completed",
                Err(e) if e.code == "CLOUD_CANCELLED" => "cancelled",
                Err(_) => "failed",
            }
            .into();
            task.status.recovery_required = upload
                && result.as_ref().err().is_some_and(|e| {
                    matches!(
                        e.code.as_str(),
                        "CLOUD_UPLOAD_PENDING_RECOVERY" | "INTERNAL_ERROR"
                    )
                });
            task.status.error = result.err();
            let persistence = if task.status.recovery_required {
                self.object_recovery_write(&task.status)
            } else {
                self.object_recovery_remove(id)
            };
            if let Err(error) = persistence {
                task.status.recovery_required = upload;
                self.object_report(error);
            }
            if let Some(error) = task.status.error.clone() {
                if error.code == "CLOUD_CANCELLED" {
                    log::info!("object task cancelled: {error:?}");
                } else {
                    self.object_report(error);
                }
            }
        }
    }
    pub(crate) fn object_report(&self, error: AppError) {
        log::error!("object task error: {error:?}");
        if let Err(source) = self.object_events.error(error) {
            log::error!("object error event failed: {source}");
        }
    }
    pub async fn object_cancel(&self, transfer_id: &str) -> Result<ObjectTransferDto, AppError> {
        self.object_admit()?;
        let mut state = self.objects.lock().await;
        state.prune();
        let task = state
            .tasks
            .get_mut(transfer_id)
            .ok_or_else(|| app(CloudError::NotFound))?;
        task.cancel.store(true, Ordering::Release);
        if task.status.phase == "running" {
            task.status.phase = "cancelling".into();
        }
        Ok(task.status.clone())
    }
    pub async fn object_status(&self) -> Result<Vec<ObjectTransferDto>, AppError> {
        self.object_admit()?;
        let mut state = self.objects.lock().await;
        state.prune();
        let ids: Vec<_> = state
            .tasks
            .iter()
            .filter(|(_, task)| task.handle.as_ref().is_some_and(|h| h.is_finished()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(handle) = state.tasks.get_mut(&id).and_then(|t| t.handle.take()) {
                if let Err(source) = handle.await {
                    let error = app(DomainError::Internal {
                        detail: source.to_string(),
                    });
                    self.object_report(error.clone());
                    if let Some(task) = state.tasks.get_mut(&id) {
                        task.status.error = Some(error);
                        task.status.phase = "failed".into();
                    }
                }
            }
        }
        let mut statuses: Vec<_> = state.tasks.values().map(|t| t.status.clone()).collect();
        for status in self.object_recovery_read()? {
            if !statuses.iter().any(|s| s.transfer_id == status.transfer_id)
                && !state
                    .upload_directories
                    .get(&status.transfer_id)
                    .is_some_and(|b| matches!(b.status.phase.as_str(), "running" | "conflict"))
            {
                statuses.push(status);
            }
        }
        Ok(statuses)
    }
    /// Lifecycle helper: cancel and join before credential changes or vault locking.
    pub(crate) async fn object_invalidate(
        &self,
        connection_id: Option<&str>,
    ) -> Result<(), AppError> {
        let batches: Vec<_> = {
            let state = self.objects.lock().await;
            state
                .upload_directories
                .iter()
                .filter(|(_, b)| connection_id.is_none_or(|id| b.status.connection_id == id))
                .map(|(id, b)| (id.clone(), b.status.connection_id.clone()))
                .collect()
        };
        for (id, connection) in batches {
            self.object_release_upload_directory(&connection, &id)
                .await?;
        }
        let handles = {
            let mut state = self.objects.lock().await;
            state.prune();
            let mut handles = Vec::new();
            for task in state
                .tasks
                .values_mut()
                .filter(|t| connection_id.is_none_or(|id| t.status.connection_id == id))
            {
                task.cancel.store(true, Ordering::Release);
                if let Some(handle) = task.handle.take() {
                    handles.push((task.status.transfer_id.clone(), handle));
                }
            }
            state
                .sessions
                .retain(|id, _| connection_id.is_some_and(|selected| id != selected));
            state
                .active_buckets
                .retain(|id, _| connection_id.is_some_and(|selected| id != selected));
            state
                .plans
                .retain(|_, (id, _)| connection_id.is_some_and(|selected| id != selected));
            state.upload_directories.retain(|_, manifest| {
                connection_id.is_some_and(|selected| manifest.status.connection_id != selected)
            });
            state.grants.retain(|_, grant| {
                connection_id.is_some_and(|selected| grant.connection_id != selected)
            });
            handles
        };
        // One deadline for the entire batch, not N times the timeout.
        #[cfg(test)]
        let grace = std::time::Duration::from_millis(50);
        #[cfg(not(test))]
        let grace = std::time::Duration::from_secs(3);
        let deadline = tokio::time::Instant::now() + grace;
        for (id, mut handle) in handles {
            let failure = match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(())) => None,
                Ok(Err(source)) => Some(app(DomainError::Internal {
                    detail: source.to_string(),
                })),
                Err(source) => {
                    let error = app(DomainError::Cloud {
                        code: "CLOUD_OBJECT_STOP_TIMEOUT",
                        user_reason: "传输停止超时，状态未知 / Transfer stop timed out; outcome unknown",
                        source: Box::new(source),
                    });
                    handle.abort();
                    if let Err(source) = handle.await {
                        self.object_report(app(DomainError::Internal {
                            detail: source.to_string(),
                        }));
                    }
                    Some(error)
                }
            };
            if let Some(error) = failure {
                self.object_report(error.clone());
                let mut state = self.objects.lock().await;
                state.prune();
                if let Some(task) = state.tasks.get_mut(&id) {
                    task.status.phase = "unknown".into();
                    task.status.error = Some(error);
                    task.status.recovery_required = task.status.direction == "upload";
                }
            }
        }
        let mut state = self.objects.lock().await;
        state.prune();
        state
            .sessions
            .retain(|id, _| connection_id.is_some_and(|selected| id != selected));
        if connection_id.is_none() {
            state.guard = None;
        }
        // Failures are reported through app://error, never prevent vault locking.
        Ok(())
    }
    pub(crate) fn object_recovery_db(&self) -> Result<rusqlite::Connection, AppError> {
        let db = rusqlite::Connection::open_with_flags(
            self.data_dir.join("cloud-object-recovery.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|source| {
            app(DomainError::Database {
                operation: "open object recovery",
                source,
            })
        })?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS pending_object_upload (id TEXT PRIMARY KEY, connection_id TEXT NOT NULL, revision INTEGER NOT NULL)").map_err(|source| app(DomainError::Database { operation: "initialize object recovery", source }))?;
        // Additive migration keeps legacy records locatable by connection/revision.
        let has_payload = db
            .prepare("SELECT payload FROM pending_object_upload LIMIT 0")
            .is_ok();
        if !has_payload {
            db.execute_batch("ALTER TABLE pending_object_upload ADD COLUMN payload BLOB")
                .map_err(|source| {
                    app(DomainError::Database {
                        operation: "migrate object recovery",
                        source,
                    })
                })?;
        }
        Ok(db)
    }
    pub(crate) fn object_recovery_write(&self, status: &ObjectTransferDto) -> Result<(), AppError> {
        use chacha20poly1305::{
            ChaCha20Poly1305, KeyInit, Nonce,
            aead::{Aead, Payload},
        };
        let key = self
            .config
            .get_or_create_index_key()
            .map_err(config_error)?;
        let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| state_error())?;
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|source| app(CloudError::Random { source }))?;
        // No paths, signed URLs, credentials, or raw provider errors enter this payload.
        let plaintext = serde_json::to_vec(&(
            status.key.clone(),
            status.size_hint,
            status.phase == "acknowledged",
        ))
        .map_err(|source| {
            app(DomainError::Internal {
                detail: source.to_string(),
            })
        })?;
        let aad = format!(
            "object-recovery-v1:{}:{}:{}",
            status.transfer_id, status.connection_id, status.connection_revision
        );
        let mut payload = nonce.to_vec();
        payload.extend(
            cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &plaintext,
                        aad: aad.as_bytes(),
                    },
                )
                .map_err(|_| state_error())?,
        );
        self.object_recovery_db()?.execute("INSERT OR REPLACE INTO pending_object_upload(id,connection_id,revision,payload) VALUES (?1,?2,?3,?4)", rusqlite::params![status.transfer_id, status.connection_id, status.connection_revision, payload]).map_err(|source| app(DomainError::Database { operation: "record unknown object upload", source }))?;
        Ok(())
    }
    pub(crate) fn object_recovery_remove(&self, id: &str) -> Result<(), AppError> {
        self.object_recovery_db()?
            .execute("DELETE FROM pending_object_upload WHERE id=?1", [id])
            .map_err(|source| {
                app(DomainError::Database {
                    operation: "complete object upload record",
                    source,
                })
            })?;
        Ok(())
    }
    pub async fn object_recovery_acknowledge(
        &self,
        transfer_id: &str,
        connection_id: &str,
        connection_revision: u64,
        confirmed: bool,
    ) -> Result<ObjectTransferDto, AppError> {
        let _lifecycle = self.object_lifecycle.lock().await;
        self.object_admit()?;
        if !confirmed {
            return Err(app(ObjectServiceError::ConfirmationRequired));
        }
        let mut state = self.objects.lock().await;
        state.prune();
        if state
            .tasks
            .get(transfer_id)
            .is_some_and(|t| matches!(t.status.phase.as_str(), "running" | "cancelling"))
        {
            return Err(state_error());
        }
        if state
            .upload_directories
            .get(transfer_id)
            .is_some_and(|b| matches!(b.status.phase.as_str(), "running" | "conflict"))
        {
            return Err(state_error());
        }
        let mut status = self
            .object_recovery_read()?
            .into_iter()
            .find(|s| {
                s.transfer_id == transfer_id
                    && s.connection_id == connection_id
                    && s.connection_revision == connection_revision
            })
            .ok_or_else(|| app(CloudError::NotFound))?;
        if !status.recovery_required {
            return Err(state_error());
        }
        status.phase = "acknowledged".into();
        status.recovery_required = false;
        self.object_recovery_write(&status)?;
        if let Some(task) = state.tasks.get_mut(transfer_id) {
            task.status = status.clone();
        }
        if let Some(batch) = state.upload_directories.get_mut(transfer_id) {
            batch.status.recovery_required = false;
        }
        Ok(status)
    }
    pub(crate) fn object_recovery_read(&self) -> Result<Vec<ObjectTransferDto>, AppError> {
        use chacha20poly1305::{
            ChaCha20Poly1305, KeyInit, Nonce,
            aead::{Aead, Payload},
        };
        let key = self
            .config
            .get_or_create_index_key()
            .map_err(config_error)?;
        let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| state_error())?;
        let db = self.object_recovery_db()?;
        let mut statement = db
            .prepare("SELECT id, connection_id, revision, payload FROM pending_object_upload ORDER BY id")
            .map_err(|source| {
                app(DomainError::Database {
                    operation: "read unknown object uploads",
                    source,
                })
            })?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    ObjectTransferDto {
                        transfer_id: row.get(0)?,
                        connection_id: row.get(1)?,
                        connection_revision: row.get(2)?,
                        direction: "upload".into(),
                        key: None,
                        size_hint: None,
                        phase: "unknown".into(),
                        error: None,
                        recovery_required: true,
                    },
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            })
            .map_err(|source| {
                app(DomainError::Database {
                    operation: "query unknown object uploads",
                    source,
                })
            })?;
        let mut statuses = Vec::new();
        for row in rows {
            let (mut status, payload) = row.map_err(|source| {
                app(DomainError::Database {
                    operation: "decode unknown object uploads",
                    source,
                })
            })?;
            if let Some(payload) = payload {
                if payload.len() < 12 {
                    return Err(state_error());
                }
                let aad = format!(
                    "object-recovery-v1:{}:{}:{}",
                    status.transfer_id, status.connection_id, status.connection_revision
                );
                let plaintext = cipher
                    .decrypt(
                        Nonce::from_slice(&payload[..12]),
                        Payload {
                            msg: &payload[12..],
                            aad: aad.as_bytes(),
                        },
                    )
                    .map_err(|_| state_error())?;
                let (key, size_hint, acknowledged): (Option<String>, Option<u64>, bool) =
                    serde_json::from_slice(&plaintext).map_err(|_| state_error())?;
                status.key = key;
                status.size_hint = size_hint;
                if acknowledged {
                    status.phase = "acknowledged".into();
                    status.recovery_required = false;
                }
            }
            statuses.push(status);
        }
        Ok(statuses)
    }
}
