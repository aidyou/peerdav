//! Recoverable S3 move: copy, authenticate bytes, then delete the source.
//! Conditional writes/deletes are used when the provider enforces them; MinIO
//! and similar gateways fall back to a revision recheck immediately before an
//! ordinary delete. This is deliberately not advertised as atomic; retry
//! repeats the frozen task.
use super::{runtime::CloudRuntime, object_types::{MoveObject, MoveTask}, types::{CloudError, PutCondition}};
use crate::{AppError, DomainError};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

fn app<E: Into<DomainError>>(error: E) -> AppError { AppError::from(error.into()) }
fn invalid() -> AppError { AppError { code: "CLOUD_MOVE_CONFLICT".into(), message: "移动目标已存在、源已变化或任务超过安全限制 / Move target exists, source changed or safety limit reached".into(), detail: None, context: None } }
async fn hash(store: &dyn super::traits::ObjectStore, key: &str, size: u64) -> Result<String, AppError> {
    let mut stream = store.get(key, None).await.map_err(app)?;
    let mut total = 0u64;
    let mut hash = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(app)?;
        total = total.checked_add(bytes.len() as u64).ok_or_else(invalid)?;
        if total > size { return Err(invalid()); }
        hash.update(&bytes);
    }
    if total != size { return Err(invalid()); }
    Ok(hex::encode(hash.finalize()))
}
fn checkpoint(index: &mut crate::sync::index::SyncIndex, task: &mut MoveTask, ordinal: usize) -> Result<(), AppError> {
    let entry = task.entries.get_mut(ordinal).ok_or_else(invalid)?;
    if entry.requests >= 256 { return Err(invalid()); }
    entry.requests += 1;
    index.save("move", "task", task).map_err(super::runtime::append::append_error)
}
impl CloudRuntime {
    pub(crate) async fn object_move(&self, connection: &str, revision: u64, identity: &super::ProtectedIdentity, source: &str, destination: &str) -> Result<(), AppError> {
        self.admit()?;
        let _network = self.network.write().await;
        self.admit()?;
        let session = self.object_session(connection).await?;
        if revision != session.revision || *identity != session.identity { return Err(invalid()); }
        super::object_service::valid_relative_path(source.trim_end_matches('/')).map_err(app)?;
        if !destination.is_empty() { super::object_service::valid_relative_path(destination.trim_end_matches('/')).map_err(app)?; }
        let name = source.trim_end_matches('/').rsplit('/').next().ok_or_else(invalid)?;
        let target = format!("{}{name}{}", if destination.is_empty() { String::new() } else { format!("{}/", destination.trim_end_matches('/')) }, if source.ends_with('/') { "/" } else { "" });
        if source.is_empty() || source == target || target.starts_with(&format!("{}/", source.trim_end_matches('/'))) { return Err(invalid()); }
        let _source = session.service.move_lease(source).map_err(app)?;
        let _target = session.service.move_lease(&target).map_err(app)?;
        let capabilities = match session.store.verify_move_capabilities().await {
            Ok(capabilities) => capabilities,
            Err(CloudError::Unsupported) => super::types::StorageCapabilities::default(),
            Err(error) => return Err(app(error)),
        };
        let conditional = capabilities.create_only && capabilities.conditional_writes_verified;
        let scope = hex::encode(Sha256::digest(serde_json::to_vec(&(identity, revision, source, &target)).map_err(|source| AppError::from(DomainError::Internal { detail: source.to_string() }))?));
        let mut index = crate::sync::index::SyncIndex::open(&self.data_dir.join("cloud-moves.sqlite"), self.config.get_or_create_index_key().map_err(super::runtime::config_error)?, "object-moves-v1".into(), scope).map_err(super::runtime::append::append_error)?;
        let mut task: MoveTask = match index.load::<MoveTask>("move", "task").map_err(super::runtime::append::append_error)? {
            Some(task) if !task.complete => task,
            _ => {
                let mut objects = Vec::new();
                if source.ends_with('/') {
                    let existing = session.store.list_page(&target, None).await.map_err(app)?;
                    if !existing.objects.is_empty() || existing.next_token.is_some() { return Err(invalid()); }
                    let mut next = None;
                    let mut tokens = std::collections::HashSet::new();
                    loop {
                        let page = session.store.list_page(source, next.as_deref()).await.map_err(app)?;
                        objects.extend(page.objects);
                        if objects.len() > 10_000 { return Err(invalid()); }
                        match page.next_token {
                            Some(token) if tokens.len() < 1000 && tokens.insert(token.clone()) => next = Some(token),
                            Some(_) => return Err(invalid()),
                            None => break,
                        }
                    }
                } else { objects.push(session.store.stat(source).await.map_err(app)?); }
                if objects.is_empty() { return Err(invalid()); }
                let mut entries = Vec::new();
                let mut seen = std::collections::HashSet::new();
                let mut total = 0u64;
                for object in objects {
                    if !object.key.starts_with(source) || !seen.insert(object.key.clone()) { return Err(invalid()); }
                    total = total.checked_add(object.size).ok_or_else(invalid)?;
                    if object.key.len() > 4096 || object.size > super::types::MAX_PUT_BYTES as u64 || total > 100 * 1024 * 1024 * 1024 { return Err(invalid()); }
                    let to = if source.ends_with('/') { format!("{target}{}", object.key.strip_prefix(source).ok_or_else(invalid)?) } else { target.clone() };
                    match session.store.stat(&to).await {
                        Err(CloudError::NotFound) => {},
                        Err(error) => return Err(app(error)),
                        Ok(_) => return Err(invalid()),
                    }
                    entries.push(MoveObject { source: object.key, destination: to, revision: object.etag.filter(|etag| !etag.is_empty()).ok_or_else(invalid)?, size: object.size, digest: None, attempted: false, completed: false, requests: 0 });
                }
                let task = MoveTask { entries, complete: false };
                index.save("move", "task", &task).map_err(super::runtime::append::append_error)?;
                task
            }
        };
        for ordinal in 0..task.entries.len() {
            if task.entries[ordinal].completed { continue; }
            self.admit()?;
            let entry = task.entries[ordinal].clone();
            checkpoint(&mut index, &mut task, ordinal)?;
            let source_meta = match session.store.stat(&entry.source).await {
                Ok(meta) => Some(meta),
                Err(CloudError::NotFound) if entry.attempted && entry.digest.is_some() => None,
                Err(error) => return Err(app(error)),
            };
            if source_meta.as_ref().is_some_and(|meta| meta.etag.as_deref() != Some(entry.revision.as_str()) || meta.size != entry.size) { return Err(invalid()); }
            if task.entries[ordinal].digest.is_none() {
                checkpoint(&mut index, &mut task, ordinal)?;
                let digest = hash(session.store.as_ref(), &entry.source, entry.size).await?;
                task.entries[ordinal].digest = Some(digest);
                index.save("move", "task", &task).map_err(super::runtime::append::append_error)?;
            }
            let digest = task.entries[ordinal].digest.clone().ok_or_else(invalid)?;
            checkpoint(&mut index, &mut task, ordinal)?;
            let exists = match session.store.stat(&entry.destination).await {
                Ok(meta) if meta.size == entry.size && entry.attempted => true,
                Ok(_) => return Err(invalid()),
                Err(CloudError::NotFound) if source_meta.is_some() => false,
                Err(error) => return Err(app(error)),
            };
            if !exists {
                task.entries[ordinal].attempted = true;
                checkpoint(&mut index, &mut task, ordinal)?;
                let stream = session.store.get(&entry.source, None).await.map_err(app)?;
                checkpoint(&mut index, &mut task, ordinal)?;
                let copied = if conditional {
                    session.store.copy_move_stream(&entry.destination, stream).await
                } else {
                    // The destination was checked immediately before this call.
                    // Providers without conditional create cannot close the
                    // remaining race, but must not reject an ordinary copy.
                    session.store.put(&entry.destination, stream, PutCondition::Any).await
                };
                copied.map_err(app)?;
            }
            checkpoint(&mut index, &mut task, ordinal)?;
            if hash(session.store.as_ref(), &entry.destination, entry.size).await? != digest { return Err(invalid()); }
            if source_meta.is_some() {
                checkpoint(&mut index, &mut task, ordinal)?;
                if conditional {
                    // Never replace this with HEAD + unconditional DELETE.
                    session.store.delete_if_match(&entry.source, &entry.revision).await.map_err(app)?;
                } else {
                    // MinIO does not enforce If-Match on DELETE. Recheck the
                    // source as late as possible, then delete by key.
                    let latest = session.store.stat(&entry.source).await.map_err(app)?;
                    if latest.etag.as_deref() != Some(entry.revision.as_str()) || latest.size != entry.size { return Err(invalid()); }
                    session.store.delete(&entry.source).await.map_err(app)?;
                }
            }
            task.entries[ordinal].completed = true;
            index.save("move", "task", &task).map_err(super::runtime::append::append_error)?;
        }
        task.complete = true;
        index.save("move", "task", &task).map_err(super::runtime::append::append_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::traits::ObjectStore;
    use super::super::types::*;
    use futures_util::{future::BoxFuture, stream};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    struct Store {
        data: tokio::sync::Mutex<std::collections::BTreeMap<String, (Vec<u8>, String)>>,
        fail_delete: AtomicBool,
        conditional: AtomicBool,
        allow_unconditional_delete: AtomicBool,
    }
    impl Default for Store {
        fn default() -> Self {
            Self {
                data: Default::default(),
                fail_delete: AtomicBool::new(false),
                conditional: AtomicBool::new(true),
                allow_unconditional_delete: AtomicBool::new(false),
            }
        }
    }
    impl ObjectStore for Store {
        fn scope(&self) -> Option<ObjectStoreScope> { Some(ObjectStoreScope { endpoint: "https://move.invalid".into(), bucket: "bucket".into(), prefix: String::new() }) }
        fn capabilities(&self) -> StorageCapabilities {
            let conditional = self.conditional.load(Ordering::SeqCst);
            StorageCapabilities { create_only: conditional, conditional_replace: conditional, range_read: false, conditional_writes_verified: conditional }
        }
        fn list<'a>(&'a self, _: &'a str) -> ObjectListing<'a> { stream::empty().boxed() }
        fn list_page<'a>(&'a self, prefix: &'a str, _: Option<&'a str>) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
            Box::pin(async move { Ok(ObjectPage { objects: self.data.lock().await.iter().filter(|(key, _)| key.starts_with(prefix)).map(|(key, (bytes, revision))| ObjectMetadata { key: key.clone(), size: bytes.len() as u64, etag: Some(revision.clone()) }).collect(), next_token: None }) })
        }
        fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move { let data = self.data.lock().await; let (bytes, revision) = data.get(key).ok_or(CloudError::NotFound)?; Ok(ObjectMetadata { key: key.into(), size: bytes.len() as u64, etag: Some(revision.clone()) }) })
        }
        fn get<'a>(&'a self, key: &'a str, _: Option<std::ops::Range<u64>>) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
            Box::pin(async move { let bytes = self.data.lock().await.get(key).ok_or(CloudError::NotFound)?.0.clone(); Ok(stream::once(async move { Ok(bytes::Bytes::from(bytes)) }).boxed()) })
        }
        fn put<'a>(&'a self, key: &'a str, mut body: ObjectStream, condition: PutCondition) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
            Box::pin(async move {
                let conditional = self.conditional.load(Ordering::SeqCst);
                if conditional { assert!(matches!(condition, PutCondition::CreateOnly)); }
                let mut bytes = Vec::new(); while let Some(chunk) = body.next().await { bytes.extend_from_slice(&chunk?); }
                let mut data = self.data.lock().await;
                if conditional && data.contains_key(key) { return Err(CloudError::Precondition); }
                let size = bytes.len() as u64;
                data.insert(key.into(), (bytes, "copy-version".into()));
                Ok(ObjectMetadata { key: key.into(), size, etag: Some("copy-version".into()) })
            })
        }
        fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
            Box::pin(async move {
                if !self.allow_unconditional_delete.load(Ordering::SeqCst) { panic!("unconditional delete is forbidden"); }
                self.data.lock().await.remove(key);
                Ok(())
            })
        }
        fn delete_if_match<'a>(&'a self, key: &'a str, revision: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
            Box::pin(async move {
                if self.fail_delete.swap(false, Ordering::SeqCst) { return Err(CloudError::Precondition); }
                let mut data = self.data.lock().await;
                if data.get(key).is_none_or(|(_, current)| current != revision) { return Err(CloudError::Precondition); }
                data.remove(key); Ok(())
            })
        }
    }
    #[tokio::test]
    #[ignore = "requires the user-authorized local minio-test Docker container"]
    async fn local_minio_move_contract() {
        use super::super::{config_types::*, object_runtime_types::*, StorageIdentityAssociation};
        let output = std::process::Command::new("docker").args(["inspect", "minio-test"]).output().expect("local Docker is required");
        assert!(output.status.success(), "local MinIO container unavailable");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("container metadata");
        let env = value[0]["Config"]["Env"].as_array().expect("container environment");
        let get = |name: &str| env.iter().filter_map(serde_json::Value::as_str).find_map(|v| v.strip_prefix(&format!("{name}="))).expect("test credential configured").to_owned();
        let access = get("MINIO_ROOT_USER"); let secret = get("MINIO_ROOT_PASSWORD");
        let bucket = "peerdav-move-contract";
        let created = std::process::Command::new("curl").args(["--silent", "--output", "/dev/null", "--write-out", "%{http_code}", "--aws-sigv4", "aws:amz:us-east-1:s3", "--user", &format!("{access}:{secret}"), "-X", "PUT", &format!("http://127.0.0.1:9000/{bucket}")]).output().expect("curl");
        assert!(created.status.success() && matches!(created.stdout.as_slice(), b"200" | b"409"), "dedicated test bucket creation failed");
        let dir = tempfile::tempdir().expect("temp");
        let vault = Arc::new(crate::Vault::open(&dir.path().join("vault.db")).expect("vault"));
        vault.initialize(&dir.path().to_string_lossy(), "admin", "password 123").expect("initialize");
        struct Events;
        impl crate::sync::traits::SyncEvents for Events {
            fn progress(&self, _: crate::sync::manager_types::SyncStatus) -> Result<(), crate::sync::traits::BoxError> { Ok(()) }
            fn error(&self, _: AppError) -> Result<(), crate::sync::traits::BoxError> { Ok(()) }
        }
        let config = S3Config { endpoint: "http://127.0.0.1:9000".into(), bucket: bucket.into(), region: "us-east-1".into(), prefix: String::new(), path_style: true, access_key: access.clone(), secret_key: secret.clone(), session_token: None, buckets: vec![], allow_insecure_http: true, proxy: ProxyMode::Direct };
        let raw = Arc::new(peerdav_protocol_s3::S3Store::new(config.clone()).expect("S3 client"));
        let core = peerdav_protocol_s3::CoreS3Backend::new(raw.clone(), 1).expect("core adapter");
        let store = Arc::new(peerdav_protocol_s3::CoreS3ReadBridge(core));
        let runtime = CloudRuntime::new(vault, Arc::new(Events), dir.path().into());
        runtime.resume().expect("resume");
        runtime.objects.lock().await.backend = Some(ObjectTestBackend { store: store.clone(), load: Arc::new(move || Ok(CloudConnection { id: "c".into(), name: "local contract".into(), protocol_id: CloudProtocolId::S3, revision: 1, schedule: Default::default(), config: config.clone() })) });
        let session = runtime.object_session("c").await.expect("session");
        let guard = runtime.objects.lock().await.guard.clone().expect("guard");
        guard.register_storage_identity(&session.identity, StorageIdentityAssociation::New).expect("identity");
        guard.mark_discovery_complete(&session.identity).expect("discovery");
        let prefix = format!("contract-{}/", crate::local_upload::token().expect("unique test scope"));
        let source = format!("{prefix}source/"); let destination = format!("{prefix}destination/");
        for (name, data) in [("a.txt", b"hello".as_slice()), ("nested/b.txt", b"world".as_slice())] {
            assert!(raw.put(&format!("{source}{name}"), stream::once({ let bytes = bytes::Bytes::copy_from_slice(data); async move { Ok(bytes) } }).boxed(), PutCondition::CreateOnly).await.is_ok(), "seed test object failed");
        }
        let probe = format!("{prefix}delete-check");
        let probe_meta = raw.put(&probe, stream::once(async { Ok(bytes::Bytes::from_static(b"probe")) }).boxed(), PutCondition::CreateOnly).await.expect("probe creation");
        let delete_probe = raw.delete_if_match(&probe, probe_meta.etag.as_deref().expect("probe revision")).await;
        assert!(
            delete_probe.is_ok() || matches!(delete_probe, Err(CloudError::Unsupported)),
            "MinIO conditional-delete capability probe failed (no user source deleted)"
        );
        if matches!(delete_probe, Err(CloudError::Unsupported)) {
            eprintln!("MinIO contract: conditional deletion unsupported; using revision-checked fallback");
        }
        let moved = runtime.object_move("c", 1, &session.identity, &source, &destination).await;
        assert!(moved.is_ok(), "MinIO move failed with code {:?}", moved.err().map(|e| e.code));
        assert!(matches!(store.stat(&format!("{source}a.txt")).await, Err(CloudError::NotFound)));
        let target = format!("{destination}source/");
        assert_eq!(hash(store.as_ref(), &format!("{target}a.txt"), 5).await.expect("target digest"), hex::encode(Sha256::digest(b"hello")));
        assert_eq!(hash(store.as_ref(), &format!("{target}nested/b.txt"), 5).await.expect("nested digest"), hex::encode(Sha256::digest(b"world")));
        for name in ["a.txt", "nested/b.txt"] { assert!(raw.delete(&format!("{target}{name}")).await.is_ok(), "test cleanup failed"); }
        // Ordinary MinIO operations must not depend on conditional writes.
        let directory = format!("{prefix}empty-folder/");
        session.service.create_directory(&directory).await.expect("create empty directory");
        let root = runtime.object_list("c", &prefix, None).await.expect("list root");
        assert!(root.objects.iter().any(|entry| entry.key == directory));
        assert!(root.objects.iter().all(|entry| !entry.key.contains(".peerdav-directory")));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_secs();
        let plan = session.service.delete_plan(&directory, now + 300).await.expect("folder delete plan");
        assert_eq!(session.service.execute_delete(&plan, true).await.expect("delete folder").len(), 1);
        assert!(runtime.object_list("c", &prefix, None).await.expect("list after delete").objects.iter().all(|entry| entry.key != directory));
        let edit_key = format!("{prefix}edit.txt");
        raw.put(&edit_key, stream::once(async { Ok(bytes::Bytes::from_static(b"old")) }).boxed(), PutCondition::Any).await.expect("seed edit");
        let revision = raw.stat(&edit_key).await.expect("edit stat").etag.expect("edit revision");
        let edited = session.service.replace_edit(&edit_key, b"new".to_vec(), &revision).await.expect("edit fallback");
        assert_eq!(edited.size, 3);
        let mut body = raw.get(&edit_key, None).await.expect("read edited");
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await { bytes.extend_from_slice(&chunk.expect("edited chunk")); }
        assert_eq!(bytes, b"new");
        assert!(raw.delete(&edit_key).await.is_ok(), "edit cleanup");
        let upload_key = format!("{prefix}upload.txt");
        session.store.upload_stream(&upload_key, stream::once(async { Ok(bytes::Bytes::from_static(b"multipart")) }).boxed()).await.expect("multipart upload");
        assert_eq!(raw.stat(&upload_key).await.expect("uploaded stat").size, 9);
        assert!(raw.delete(&upload_key).await.is_ok(), "upload cleanup");
        // Sync drive creation must reject an overlapping first binding before
        // writing the remote anchor, otherwise a failed bind leaves an orphan.
        let connection = runtime.config.save_connection(SaveS3Input {
            id: None, name: "minio-contract".into(), endpoint: "http://127.0.0.1:9000".into(),
            bucket: bucket.into(), buckets: vec![bucket.into()], region: "us-east-1".into(),
            prefix: String::new(), path_style: true, access_key: Some(access.clone()), secret_key: Some(secret.clone()),
            session_token: None, clear_credentials: false, clear_session_token: false, clear_proxy_auth: false,
            allow_insecure_http: true, proxy: ProxyMode::Direct, schedule: None,
        }).expect("save connection");
        let local = tempfile::tempdir().expect("local root");
        runtime.config.add_mapping(AddMappingInput {
            connection_id: connection.id.clone(), repository_id: "existing-repo".into(),
            local_root: local.path().to_owned(), logical_path: "documents".into(),
            schedule: ScheduleConfig::default(),
        }, &[]).expect("existing mapping");
        let before = runtime.config.snapshot().expect("snapshot").append_repositories.len();
        let overlap = runtime.create_sync_drive(crate::cloud::runtime_types::CreateAppendRepositoryInput {
            bucket: Some(bucket.into()), connection_id: connection.id.clone(),
            mode: crate::sync::append_types::AppendMode::Encrypted,
            password: "password 123".into(), password_confirmation: "password 123".into(),
            local_root: Some(local.path().to_string_lossy().into_owned()), logical_path: Some("documents".into()),
        }).await.expect_err("overlap must be rejected before the remote anchor");
        assert!(overlap.message.contains("mapping overlaps"), "{overlap:?}");
        assert_eq!(runtime.config.snapshot().expect("snapshot").append_repositories.len(), before, "no orphan repository");
        let fresh = tempfile::tempdir().expect("fresh local root");
        let created_drive = runtime.create_sync_drive(crate::cloud::runtime_types::CreateAppendRepositoryInput {
            bucket: Some(bucket.into()), connection_id: connection.id,
            mode: crate::sync::append_types::AppendMode::Encrypted,
            password: "password 123".into(), password_confirmation: "password 123".into(),
            local_root: Some(fresh.path().to_string_lossy().into_owned()), logical_path: Some("fresh".into()),
        }).await.expect("create non-overlapping sync drive");
        assert!(!created_drive.binding.repo_id.is_empty());
        assert!(runtime.config.snapshot().expect("snapshot").append_repositories.len() > before);
        assert!(raw.delete(&created_drive.binding.anchor_key).await.is_ok(), "anchor cleanup");
        assert!(raw.delete(&probe).await.is_ok(), "test probe cleanup");
    }

    #[tokio::test]
    async fn move_resumes_verified_copy_after_reopen_and_preserves_conflicts() {
        use super::super::{config_types::*, object_runtime_types::*, StorageIdentityAssociation};
        let dir = tempfile::tempdir().expect("temp");
        let vault = Arc::new(crate::Vault::open(&dir.path().join("vault.db")).expect("vault"));
        vault.initialize(&dir.path().to_string_lossy(), "admin", "password 123").expect("initialize");
        struct Events;
        impl crate::sync::traits::SyncEvents for Events {
            fn progress(&self, _: crate::sync::manager_types::SyncStatus) -> Result<(), crate::sync::traits::BoxError> { Ok(()) }
            fn error(&self, _: AppError) -> Result<(), crate::sync::traits::BoxError> { Ok(()) }
        }
        let runtime = CloudRuntime::new(vault, Arc::new(Events), dir.path().into());
        runtime.resume().expect("resume");
        let store = Arc::new(Store::default());
        store.data.lock().await.insert("docs/a.txt".into(), (b"hello".to_vec(), "v1".into()));
        store.data.lock().await.insert("docs/b.txt".into(), (b"world".to_vec(), "v2".into()));
        runtime.objects.lock().await.backend = Some(ObjectTestBackend { store: store.clone(), load: Arc::new(|| Ok(CloudConnection { id: "c".into(), name: "move".into(), protocol_id: CloudProtocolId::S3, revision: 1, schedule: Default::default(), config: S3Config { endpoint: "https://move.invalid".into(), bucket: "bucket".into(), region: "region".into(), prefix: String::new(), path_style: true, access_key: "fake".into(), secret_key: "fake".into(), session_token: None, buckets: vec![], allow_insecure_http: false, proxy: ProxyMode::Direct } })) });
        let session = runtime.object_session("c").await.expect("session");
        let guard = runtime.objects.lock().await.guard.clone().expect("guard");
        guard.register_storage_identity(&session.identity, StorageIdentityAssociation::New).expect("identity");
        guard.mark_discovery_complete(&session.identity).expect("discovery");
        assert!(runtime.object_move("c", 1, &session.identity, "docs/", "docs/sub/").await.is_err());
        store.fail_delete.store(true, Ordering::SeqCst);
        let failure = runtime.object_move("c", 1, &session.identity, "docs/", "target/").await.expect_err("injected delete failure");
        assert!(store.data.lock().await.contains_key("target/docs/a.txt"), "copy missing: {failure:?}");
        assert!(store.data.lock().await.contains_key("docs/a.txt"));
        // A new invocation reopens the encrypted task and verifies the retained copy.
        runtime.object_move("c", 1, &session.identity, "docs/", "target/").await.expect("resume");
        let data = store.data.lock().await;
        assert!(!data.contains_key("docs/a.txt")); assert!(!data.contains_key("docs/b.txt"));
        assert_eq!(data["target/docs/a.txt"].0, b"hello"); assert_eq!(data["target/docs/b.txt"].0, b"world");
        drop(data);
        store.data.lock().await.insert("fresh.txt".into(), (b"new".to_vec(), "v3".into()));
        store.data.lock().await.insert("target/fresh.txt".into(), (b"keep".to_vec(), "v4".into()));
        assert!(runtime.object_move("c", 1, &session.identity, "fresh.txt", "target/").await.is_err());
        assert_eq!(store.data.lock().await["target/fresh.txt"].0, b"keep");
        assert!(store.data.lock().await.contains_key("fresh.txt"));
        assert!(runtime.object_move("c", 2, &session.identity, "fresh.txt", "target/").await.is_err());
    }

    #[tokio::test]
    async fn move_falls_back_when_conditional_writes_are_unavailable() {
        use super::super::{config_types::*, object_runtime_types::*, StorageIdentityAssociation};
        let dir = tempfile::tempdir().expect("temp");
        let vault = Arc::new(crate::Vault::open(&dir.path().join("vault.db")).expect("vault"));
        vault.initialize(&dir.path().to_string_lossy(), "admin", "password 123").expect("initialize");
        struct Events;
        impl crate::sync::traits::SyncEvents for Events {
            fn progress(&self, _: crate::sync::manager_types::SyncStatus) -> Result<(), crate::sync::traits::BoxError> { Ok(()) }
            fn error(&self, _: AppError) -> Result<(), crate::sync::traits::BoxError> { Ok(()) }
        }
        let runtime = CloudRuntime::new(vault, Arc::new(Events), dir.path().into());
        runtime.resume().expect("resume");
        let store = Arc::new(Store::default());
        store.conditional.store(false, Ordering::SeqCst);
        store.allow_unconditional_delete.store(true, Ordering::SeqCst);
        store.data.lock().await.insert("docs/a.txt".into(), (b"hello".to_vec(), "v1".into()));
        store.data.lock().await.insert("docs/nested/b.txt".into(), (b"world".to_vec(), "v2".into()));
        runtime.objects.lock().await.backend = Some(ObjectTestBackend { store: store.clone(), load: Arc::new(|| Ok(CloudConnection { id: "c".into(), name: "move".into(), protocol_id: CloudProtocolId::S3, revision: 1, schedule: Default::default(), config: S3Config { endpoint: "https://move.invalid".into(), bucket: "bucket".into(), region: "region".into(), prefix: String::new(), path_style: true, access_key: "fake".into(), secret_key: "fake".into(), session_token: None, buckets: vec![], allow_insecure_http: false, proxy: ProxyMode::Direct } })) });
        let session = runtime.object_session("c").await.expect("session");
        let guard = runtime.objects.lock().await.guard.clone().expect("guard");
        guard.register_storage_identity(&session.identity, StorageIdentityAssociation::New).expect("identity");
        guard.mark_discovery_complete(&session.identity).expect("discovery");
        runtime.object_move("c", 1, &session.identity, "docs/", "target/").await.expect("fallback move");
        let data = store.data.lock().await;
        assert!(!data.contains_key("docs/a.txt") && !data.contains_key("docs/nested/b.txt"));
        assert_eq!(data["target/docs/a.txt"].0, b"hello");
        assert_eq!(data["target/docs/nested/b.txt"].0, b"world");
    }
}
