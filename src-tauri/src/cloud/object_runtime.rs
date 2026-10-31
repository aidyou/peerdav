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
fn random_id() -> Result<String, AppError> {
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
        behavior: AtomicUsize,
    }
    impl ObjectStore for FakeStore {
        fn scope(&self) -> Option<ObjectStoreScope> {
            Some(ObjectStoreScope {
                endpoint: "https://offline.invalid".into(),
                bucket: "bucket".into(),
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
                Ok(ObjectPage {
                    objects: if prefix == "data" {
                        vec![ObjectMetadata {
                            key: "data/file".into(),
                            size: 1,
                            etag: Some("etag".into()),
                        }]
                    } else {
                        Vec::new()
                    },
                    next_token: None,
                })
            })
        }
        fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                self.stats.fetch_add(1, Ordering::SeqCst);
                self.reads.fetch_add(1, Ordering::SeqCst);
                if key == "data/file" {
                    Ok(ObjectMetadata {
                        key: key.into(),
                        size: 1,
                        etag: Some("etag".into()),
                    })
                } else {
                    Err(CloudError::NotFound)
                }
            })
        }
        fn get<'a>(
            &'a self,
            _: &'a str,
            _: Option<std::ops::Range<u64>>,
        ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
            Box::pin(async move {
                self.reads.fetch_add(1, Ordering::SeqCst);
                Ok(crate::cloud::bytes_stream(bytes::Bytes::from_static(b"x")))
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
        fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
            Box::pin(async move {
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
                if self.behavior.load(Ordering::SeqCst) == 2 {
                    while !cancel.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
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
        assert!(f.events.0.load(Ordering::SeqCst) >= 2);
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
    async fn object_session(&self, connection_id: &str) -> Result<Arc<ObjectSession>, AppError> {
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
        if let Some(session) = state.sessions.get(connection_id) {
            if session.revision == connection.revision {
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
        let identity = ProtectedIdentity {
            endpoint: connection.config.endpoint.clone(),
            bucket: connection.config.bucket.clone(),
            prefix: connection.config.prefix.clone(),
            alias: String::new(),
        };
        // Deliberately no verify_capabilities, repository join, or encrypted tree reads.
        #[cfg(not(test))]
        let store: Arc<dyn ObjectStore> =
            Arc::new(super::S3Store::new(connection.config).map_err(app)?);
        #[cfg(test)]
        let store: Arc<dyn ObjectStore> = match &state.backend {
            Some(backend) => Arc::clone(&backend.store),
            None => Arc::new(super::S3Store::new(connection.config).map_err(app)?),
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
            service,
            store,
            identity,
        });
        state
            .sessions
            .insert(connection_id.to_owned(), Arc::clone(&session));
        Ok(session)
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
        let prefix_protected = session.service.is_protected(&page.prefix).map_err(app)?;
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
        if state.grants.len() >= 128 {
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
        if state.tasks.len() >= 128 {
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
                self.object_report(error);
            }
        }
    }
    fn object_report(&self, error: AppError) {
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
            if !statuses.iter().any(|s| s.transfer_id == status.transfer_id) {
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
                .plans
                .retain(|_, (id, _)| connection_id.is_some_and(|selected| id != selected));
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
    fn object_recovery_db(&self) -> Result<rusqlite::Connection, AppError> {
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
    fn object_recovery_write(&self, status: &ObjectTransferDto) -> Result<(), AppError> {
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
    fn object_recovery_remove(&self, id: &str) -> Result<(), AppError> {
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
        Ok(status)
    }
    fn object_recovery_read(&self) -> Result<Vec<ObjectTransferDto>, AppError> {
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
