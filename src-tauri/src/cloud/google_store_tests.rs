use super::*;
use crate::cloud::{oauth_types::*, official_types::OfficialConnection, runtime::CloudRuntime};
use std::{collections::HashMap, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn google_append_objects_roundtrip_and_reject_immutable_collision() {
    google_sync_roundtrip(crate::sync::append_types::AppendMode::Encrypted, crate::cloud::directory_types::DirectoryConflictPolicy::Overwrite).await;
}

#[tokio::test]
async fn google_authenticated_plain_sync_roundtrip() {
    google_sync_roundtrip(crate::sync::append_types::AppendMode::AuthenticatedPlain, crate::cloud::directory_types::DirectoryConflictPolicy::Skip).await;
}

async fn google_sync_roundtrip(mode: crate::sync::append_types::AppendMode, choice: crate::cloud::directory_types::DirectoryConflictPolicy) {
    let data = tempfile::tempdir().expect("data");
    let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
    vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("init");
    let runtime = CloudRuntime::new(vault, Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned()); runtime.resume().expect("resume");
    let connection = runtime.config.save_official_connection(OfficialConnection { id: String::new(), name: "Google".into(), provider: OfficialProvider::GoogleDrive, config_version: 1, revision: 0, client_id: "client".into(), client_secret: None, refresh_token: "offline".into(), root_id: "root".into() }).expect("connection");
    runtime.official_refresh.lock().await.insert(connection.id.clone(), CachedAccessToken { revision: connection.revision, token: zeroize::Zeroizing::new("test".into()), expires: std::time::Instant::now() + std::time::Duration::from_secs(600) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    *runtime.official_fixture.lock().expect("fixture") = Some(format!("http://{}", listener.local_addr().expect("address")));
    let task = tokio::spawn(async move {
        let mut entries: HashMap<String, (String, String, bool, Vec<u8>)> = HashMap::new();
        let mut next_id = 0u64;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { break; };
            let mut bytes = Vec::new();
            let start = loop {
                let mut chunk = [0; 8192]; let n = socket.read(&mut chunk).await.expect("read"); if n == 0 { break None; } bytes.extend_from_slice(&chunk[..n]);
                if let Some(start) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..start]);
                    let length = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|s| s.parse::<usize>().ok())).unwrap_or(0);
                    if bytes.len() >= start + 4 + length { break Some(start + 4); }
                }
            };
            let Some(start) = start else { continue; };
            let header = String::from_utf8_lossy(&bytes[..start]);
            let line: Vec<_> = header.lines().next().expect("line").split_whitespace().collect();
            let url = url::Url::parse(&format!("http://fixture{}", line[1])).expect("url");
            let body = &bytes[start..];
            let response = if line[0] == "GET" && url.path() == "/drive/v3/files" {
                let q = url.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()).unwrap_or_default();
                let parent = q.split('\'').nth(1).unwrap_or("");
                serde_json::to_vec(&serde_json::json!({"files": entries.iter().filter(|(_, (p, _, _, _))| p == parent).map(|(id, (_, name, folder, payload))| serde_json::json!({"id":id,"name":name,"mimeType":if *folder {"application/vnd.google-apps.folder"} else {"application/octet-stream"},"size":payload.len().to_string(),"version":"1"})).collect::<Vec<_>>() })).expect("json")
            } else if line[0] == "POST" {
                let (meta, payload) = if url.path().starts_with("/upload/") {
                    let meta_start = body.windows(4).position(|s| s == b"\r\n\r\n").expect("meta") + 4;
                    let meta_end = body[meta_start..].windows(2).position(|s| s == b"\r\n").expect("meta end") + meta_start;
                    let payload_start = body[meta_end + 2..].windows(4).position(|s| s == b"\r\n\r\n").expect("payload") + meta_end + 6;
                    let payload_end = body[payload_start..].windows(4).position(|s| s == b"\r\n--").expect("payload end") + payload_start;
                    (serde_json::from_slice::<serde_json::Value>(&body[meta_start..meta_end]).expect("meta"), body[payload_start..payload_end].to_vec())
                } else { (serde_json::from_slice(body).expect("meta"), Vec::new()) };
                let id = format!("id{next_id}"); next_id += 1;
                entries.insert(id.clone(), (meta["parents"][0].as_str().expect("parent").into(), meta["name"].as_str().expect("name").into(), meta["mimeType"] == "application/vnd.google-apps.folder", payload));
                serde_json::to_vec(&serde_json::json!({"id":id})).expect("id")
            } else if line[0] == "PATCH" {
                let id = url.path().rsplit('/').next().expect("id");
                if url.path().starts_with("/upload/") { entries.get_mut(id).expect("entry").3 = body.to_vec(); }
                else { entries.remove(id); }
                br#"{"version":"1"}"#.to_vec()
            } else if url.query_pairs().any(|(k, v)| k == "alt" && v == "media") {
                entries.get(url.path().rsplit('/').next().expect("id")).expect("entry").3.clone()
            } else { serde_json::to_vec(&serde_json::json!({"version":"1"})).expect("metadata") };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).as_bytes()).await.expect("header");
            socket.write_all(&response).await.expect("body");
        }
    });
    let store = runtime.google_store(&connection.id).expect("store").expect("Google");
    let renamed = runtime.config.update_official_connection(crate::cloud::official_types::UpdateOfficialConnectionInput {
        id: connection.id.clone(), revision: connection.revision, name: "GoogleDrive".into(),
        client_secret: None, clear_client_secret: false, direct: false,
    }).expect("rename connection");
    assert_eq!(renamed.name, "GoogleDrive");
    // An already admitted store must remain usable after a display-only edit.
    let key = "_peerdav_encrypt/objects/immutable";
    store.put(key, crate::cloud::bytes_stream(bytes::Bytes::from_static(b"payload")), PutCondition::Any).await.expect("put");
    assert_eq!(store.stat(key).await.expect("stat").size, 7);
    let listed = store.list_page("_peerdav_encrypt/", None).await.expect("list"); assert_eq!(listed.objects.len(), 1);
    let mut body = store.get(key, None).await.expect("get"); assert_eq!(body.next().await.expect("chunk").expect("bytes"), bytes::Bytes::from_static(b"payload"));
    store.put(key, crate::cloud::bytes_stream(bytes::Bytes::from_static(b"payload")), PutCondition::Any).await.expect("idempotent");
    assert!(store.put(key, crate::cloud::bytes_stream(bytes::Bytes::from_static(b"changed")), PutCondition::Any).await.is_err());
    assert!(store.put(key, crate::cloud::bytes_stream(bytes::Bytes::new()), PutCondition::CreateOnly).await.is_err());
    assert!(store.delete("/").await.is_err());
    let created = runtime.create_sync_drive(crate::cloud::runtime_types::CreateAppendRepositoryInput { bucket: Some("root".into()), connection_id: connection.id.clone(), mode, password: "password 123".into(), password_confirmation: "password 123".into() }).await.expect("create Google sync repository");
    assert!(!created.binding.repo_id.is_empty());
    let discovered = runtime.discover_append_in_bucket(&connection.id, Some("root")).await.expect("discover Google repository");
    assert_eq!(discovered.len(), 1);
    let local_directory = tempfile::tempdir().expect("local directory");
    let local = local_directory.path().to_owned();
    let bind = runtime.prepare_mapping_bind(crate::cloud::runtime_types::SaveAppendMappingInput {
        mapping_id: None, selected_root: "files".into(), mapping: crate::cloud::config_types::AddMappingInput { connection_id: connection.id.clone(), repository_id: created.binding.repo_id, local_root: local, logical_path: "files".into(), schedule: Default::default() },
    }).await.expect("bind Google sync folder");
    let mapping = bind.mapping.expect("mapping");
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let status = runtime.manager.status(&mapping.id);
            match status.phase {
                crate::sync::manager_types::SyncPhase::Completed => break,
                crate::sync::manager_types::SyncPhase::Failed => panic!("sync failed: {:?}", status.detail),
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }).await.expect("initial sync completes");
    let (first, second) = tokio::join!(runtime.local_sync_entries(&mapping.id), runtime.local_sync_entries(&mapping.id));
    first.expect("first concurrent metadata listing");
    second.expect("second concurrent metadata listing");
    std::fs::write(local_directory.path().join("hello.txt"), "Google sync payload").expect("local file");
    let plan = runtime.preview(&mapping.id).await.expect("preview upload");
    runtime.manual_sync(&mapping.id, &plan.preview_id).await.expect("start upload sync");
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let status = runtime.manager.status(&mapping.id);
            match status.phase {
                crate::sync::manager_types::SyncPhase::Completed => break,
                crate::sync::manager_types::SyncPhase::Failed => panic!("upload failed: {:?}", status.detail),
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }).await.expect("upload sync completes");
    assert!(runtime.preview(&mapping.id).await.is_ok());
    std::fs::remove_file(local_directory.path().join("hello.txt")).expect("local deletion");
    let deletion = runtime.preview(&mapping.id).await.expect("preview deletion");
    runtime.manual_sync(&mapping.id, &deletion.preview_id).await.expect("start deletion sync");
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let status = runtime.manager.status(&mapping.id);
            match status.phase {
                crate::sync::manager_types::SyncPhase::Completed => break,
                crate::sync::manager_types::SyncPhase::Failed => panic!("delete sync failed: {:?}", status.detail),
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }).await.expect("delete sync completes");
    let entries = runtime.local_sync_entries(&mapping.id).await.expect("projection after tombstone");
    assert!(!entries.iter().any(|entry| entry.path == "hello.txt"));
    let second_root = tempfile::tempdir().expect("second local root");
    let second = runtime.prepare_mapping_bind(crate::cloud::runtime_types::SaveAppendMappingInput {
        mapping_id: None, selected_root: "second".into(), mapping: crate::cloud::config_types::AddMappingInput { connection_id: connection.id.clone(), repository_id: mapping.repository_id.clone(), local_root: second_root.path().to_owned(), logical_path: "second".into(), schedule: Default::default() },
    }).await;
    let second = second.expect("new empty logical scope may bind directly");
    let second_mapping = second.mapping.expect("second mapping");
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            match runtime.manager.status(&second_mapping.id).phase {
                crate::sync::manager_types::SyncPhase::Completed => break,
                crate::sync::manager_types::SyncPhase::Failed => panic!("second scope sync failed"),
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }).await.expect("second scope completes");
    let other_data = tempfile::tempdir().expect("other client data");
    let other_vault = Arc::new(crate::Vault::open(&other_data.path().join("vault.pd")).expect("other vault"));
    other_vault.initialize(other_data.path().to_str().expect("other path"), "admin", "password 123").expect("other init");
    let other_runtime = CloudRuntime::new(other_vault, Arc::new(crate::sync::traits::NoopSyncEvents), other_data.path().to_owned());
    other_runtime.resume().expect("other resume");
    let mut other_connection = runtime.config.snapshot().expect("original account").official_connections.into_iter().find(|entry| entry.id == connection.id).expect("account");
    other_connection.id.clear(); other_connection.revision = 0;
    let other_connection = other_runtime.config.save_official_connection(other_connection).expect("other Google account binding");
    *other_runtime.official_fixture.lock().expect("other fixture") = runtime.official_fixture.lock().expect("fixture").clone();
    other_runtime.official_refresh.lock().await.insert(other_connection.id.clone(), CachedAccessToken { revision: other_connection.revision, token: zeroize::Zeroizing::new("test".into()), expires: std::time::Instant::now() + std::time::Duration::from_secs(600) });
    let anchor = &discovered[0];
    let joined = other_runtime.join_append_repository(crate::cloud::runtime_types::JoinAppendRepositoryInput { bucket: Some("root".into()), connection_id: other_connection.id.clone(), anchor_key: anchor.key.clone(), anchor_fingerprint: anchor.fingerprint.clone(), repository_id: anchor.repo_id.clone(), mode: anchor.descriptor.mode.clone(), accept_tofu: false, trusted_fingerprint: Some(anchor.fingerprint.clone()), password: "password 123".into() }).await.expect("second Google client joins verified anchor");
    assert_eq!(joined.binding.repo_id, mapping.repository_id);
    let other_local = tempfile::tempdir().expect("other local folder");
    let other_bind = other_runtime.prepare_mapping_bind(crate::cloud::runtime_types::SaveAppendMappingInput {
        mapping_id: None, selected_root: "files".into(), mapping: crate::cloud::config_types::AddMappingInput { connection_id: other_connection.id.clone(), repository_id: joined.binding.repo_id, local_root: other_local.path().to_owned(), logical_path: "files".into(), schedule: Default::default() },
    }).await.expect("second client binds existing Google sync root");
    let other_mapping = other_bind.mapping.expect("empty local folder binds without merge prompt");
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let status = other_runtime.manager.status(&other_mapping.id);
            match status.phase {
                crate::sync::manager_types::SyncPhase::Completed => break,
                crate::sync::manager_types::SyncPhase::Failed => panic!("second client initial sync failed: {:?}", status.detail),
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }).await.expect("second client initial sync completes");
    assert!(!other_local.path().join("hello.txt").exists(), "remote tombstone is respected by a newly joined client");
    other_runtime.stop_and_clear().await.expect("stop other client");
    let mirror_root = tempfile::tempdir().expect("mirror root");
    std::fs::write(mirror_root.path().join("mirror.txt"), "mirror payload").expect("mirror file");
    let mirror = runtime.save_mirror_mapping(crate::mirror::SaveMirrorMappingInput { id: None, connection_id: connection.id.clone(), bucket: Some("root".into()), local_root: mirror_root.path().to_owned(), cloud_prefix: "sample".into(), schedule: Default::default() }).expect("Google mirror mapping");
    assert!(runtime.mirror_folders(&connection.id, Some("forged-root"), "sample", None).await.is_err(), "mirror picker must enforce the selected official root scope");
    let confirmation = runtime.mirror_sync(&mirror.id, false, None).await.expect_err("first merge confirmation");
    assert_eq!(confirmation.code, "MIRROR_CONFIRM_MERGE");
    let ticket = confirmation.context.as_ref().and_then(|c| c.get("planId")).and_then(serde_json::Value::as_str).expect("plan").to_owned();
    runtime.mirror_sync(&mirror.id, false, Some(ticket)).await.expect("Google mirror upload");
    assert_eq!(store.stat("_peerdav_mirror/sample/mirror.txt").await.expect("mirror stored").size, 14);
    std::fs::write(mirror_root.path().join("mirror.txt"), "modified mirror").expect("mirror update");
    runtime.mirror_sync(&mirror.id, false, None).await.expect("Google mirror replacement");
    assert_eq!(store.stat("_peerdav_mirror/sample/mirror.txt").await.expect("mirror updated").size, 15);
    std::fs::remove_file(mirror_root.path().join("mirror.txt")).expect("remove local mirror file");
    store.put("_peerdav_mirror/sample/mirror.txt", crate::cloud::bytes_stream(bytes::Bytes::from_static(b"remote update")), PutCondition::Any).await.expect("remote change");
    runtime.mirror_sync(&mirror.id, false, None).await.expect("cloud update restores deleted local file");
    assert_eq!(std::fs::read(mirror_root.path().join("mirror.txt")).expect("downloaded mirror"), b"remote update");
    runtime.stop_and_clear().await.expect("stop background work");
    let endpoint = runtime.official_fixture.lock().expect("fixture").clone();
    let runtime = CloudRuntime::new(runtime.vault.clone(), Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
    *runtime.official_fixture.lock().expect("restored fixture") = endpoint;
    runtime.resume().expect("resume recreated runtime");
    runtime.official_refresh.lock().await.insert(connection.id.clone(), CachedAccessToken { revision: connection.revision, token: zeroize::Zeroizing::new("test".into()), expires: std::time::Instant::now() + std::time::Duration::from_secs(600) });
    runtime.mirror_sync(&mirror.id, false, None).await.expect("mirror after restart");
    let store = runtime.google_store(&connection.id).expect("restored store").expect("Google");
    assert!(runtime.preview(&mapping.id).await.is_ok());
    let peers = Arc::new(crate::PeerManager::new(runtime.vault.clone(), data.path().join("peer-temp")).expect("peers"));
    let sessions = crate::file_manager::types::FileSessions::default();
    let opened = sessions.open(&peers, &runtime, crate::file_manager::types::FileLocation::Cloud { connection_id: connection.id.clone(), root_id: None }).await.expect("browser");
    let session = sessions.admitted(&peers, &runtime, &opened.session_id).await.expect("browser admission");
    let created = sessions.create_directory(&peers, &runtime, &opened.session_id, "", "same-created-name").await.expect("create native directory");
    let sibling = sessions.create_directory(&peers, &runtime, &opened.session_id, "", "same-created-name").await.expect("create same-name sibling");
    assert_ne!(created.id, sibling.id);
    assert_eq!(created.parent_id, "");
    assert_eq!(created.name, "same-created-name");
    let nested = sessions.create_directory(&peers, &runtime, &opened.session_id, &created.id, "nested").await.expect("created ID immediately usable without parent refresh");
    assert_eq!(nested.parent_id, created.id);
    let navigation = sessions.list(&peers, &runtime, &opened.session_id, &nested.id, None).await.expect("complete native breadcrumbs");
    assert_eq!(navigation.breadcrumbs.iter().map(|crumb| crumb.id.as_str()).collect::<Vec<_>>(), vec!["", created.id.as_str(), nested.id.as_str()]);
    assert_eq!(navigation.breadcrumbs[0].name, "GoogleDrive");
    assert_eq!(navigation.breadcrumbs[1].name, "same-created-name");
    let source = tempfile::tempdir().expect("upload source");
    let source_file = source.path().join("existing.txt");
    std::fs::write(&source_file, "original").expect("source");
    let (_, api) = runtime.official_api(&connection.id).await.expect("api");
    api.upload_new("root", "existing.txt", std::fs::File::open(&source_file).expect("file"), 8).await.expect("existing remote file");
    std::fs::write(&source_file, "replacement").expect("new upload bytes");
    let selected = session.backend.select_upload(&peers, &runtime, String::new(), vec![source_file], crate::file_manager::types::UploadMode::Files, Default::default()).await.expect("select existing name");
    let crate::file_manager::types::FileUploadDto::Cloud(selected) = selected else { panic!("cloud queue") };
    session.backend.upload_action(&peers, &runtime, &selected.batch_id, crate::file_manager::types::UploadAction::Start).await.expect("start conflict queue");
    let conflict = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &selected.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("cloud status") };
            if let Some(conflict) = status.conflict { break conflict; }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("conflict wait");
    session.backend.upload_resolve(&peers, &runtime, &selected.batch_id, &conflict.conflict_id, choice, false).await.expect("resolve conflict");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &selected.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("cloud status") };
            if status.phase == "completed" {
                let overwrite = choice == crate::cloud::directory_types::DirectoryConflictPolicy::Overwrite;
                assert_eq!(status.skipped_files, u64::from(!overwrite));
                assert_eq!(status.completed_files, u64::from(overwrite));
                let page = session.backend.upload_page(&peers, &runtime, &selected.batch_id, None, None, Some(1)).await.expect("terminal item page");
                assert_eq!(page.entries[0].phase, if overwrite { "completed" } else { "skipped" });
                assert_eq!(page.entries[0].transferred, Some(if overwrite { 11 } else { 0 }));
                break;
            }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("skip completes");
    session.backend.release_upload(&peers, &runtime, &selected.batch_id).await.expect("release skipped upload");
    let (_, api) = runtime.official_api(&connection.id).await.expect("official API");
    let entries = api.list("root", None).await.expect("root entries").entries;
    let existing: Vec<_> = entries.iter().filter(|entry| entry.name == "existing.txt").collect();
    assert_eq!(existing.len(), 1, "conflict must never create same-name duplicates");
    let payload = api.media(&existing[0].id).await.expect("uploaded media").bytes().await.expect("payload");
    let expected: &[u8] = if choice == crate::cloud::directory_types::DirectoryConflictPolicy::Overwrite { b"replacement" } else { b"original" };
    assert_eq!(payload.as_ref(), expected);
    let cancel_source = source.path().join("existing.txt");
    let cancelled = session.backend.select_upload(&peers, &runtime, String::new(), vec![cancel_source], crate::file_manager::types::UploadMode::Single, Default::default()).await.expect("select cancelled conflict");
    let crate::file_manager::types::FileUploadDto::Cloud(cancelled) = cancelled else { panic!("cancel queue") };
    session.backend.upload_action(&peers, &runtime, &cancelled.batch_id, crate::file_manager::types::UploadAction::Start).await.expect("start cancelled queue");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &cancelled.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("cancel status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("cancel status") };
            if status.phase == "conflict" { break; }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("cancel waits at conflict");
    let status = session.backend.upload_action(&peers, &runtime, &cancelled.batch_id, crate::file_manager::types::UploadAction::Cancel).await.expect("cancel joins worker");
    let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("cancelled status") };
    assert_eq!(status.phase, "cancelled");
    assert!(status.conflict.is_none());
    assert_eq!(status.completed_files, 0);
    session.backend.release_upload(&peers, &runtime, &cancelled.batch_id).await.expect("release cancelled queue");
    let first = source.path().join("batch-a.txt");
    let second = source.path().join("batch-b.txt");
    std::fs::write(&first, "batch a").expect("batch a");
    std::fs::write(&second, "batch b").expect("batch b");
    let batch = session.backend.select_upload(&peers, &runtime, String::new(), vec![first, second], crate::file_manager::types::UploadMode::Files, Default::default()).await.expect("select multiple files");
    let crate::file_manager::types::FileUploadDto::Cloud(batch) = batch else { panic!("batch queue") };
    assert_eq!(batch.total_files, 2);
    session.backend.upload_action(&peers, &runtime, &batch.batch_id, crate::file_manager::types::UploadAction::Start).await.expect("start multiple files");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &batch.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("batch status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("batch status") };
            if status.phase == "completed" { assert_eq!(status.completed_files, 2); assert_eq!(status.completed_bytes, 14); break; }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("batch completes");
    session.backend.release_upload(&peers, &runtime, &batch.batch_id).await.expect("release batch");
    let roots = api.list("root", None).await.expect("batch root");
    for (name, bytes) in [("batch-a.txt", b"batch a"), ("batch-b.txt", b"batch b")] {
        let entry = roots.entries.iter().find(|entry| entry.name == name).expect("batch entry");
        assert_eq!(api.media(&entry.id).await.expect("batch media").bytes().await.expect("batch bytes").as_ref(), bytes);
    }
    let folder_path = source.path().join("tree");
    std::fs::create_dir_all(folder_path.join("nested")).expect("upload folder");
    std::fs::write(folder_path.join("nested/new.txt"), "folder payload").expect("folder source");
    let tree_id = api.create_folder_id("root", "tree").await.expect("existing tree");
    let nested_id = api.create_folder_id(&tree_id, "nested").await.expect("existing nested");
    let folder = session.backend.select_upload(&peers, &runtime, String::new(), vec![folder_path], crate::file_manager::types::UploadMode::Folder, Default::default()).await.expect("select folder");
    let crate::file_manager::types::FileUploadDto::Cloud(folder) = folder else { panic!("folder queue") };
    session.backend.upload_action(&peers, &runtime, &folder.batch_id, crate::file_manager::types::UploadAction::Start).await.expect("start folder");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &folder.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("folder status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("folder status") };
            if status.phase == "completed" {
                assert_eq!(status.completed_files, 1);
                let page = session.backend.upload_page(&peers, &runtime, &folder.batch_id, None, None, None).await.expect("folder entries");
                assert!(page.entries.iter().all(|entry| entry.phase == "completed"));
                break;
            }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("folder completes");
    session.backend.release_upload(&peers, &runtime, &folder.batch_id).await.expect("release folder");
    let roots = api.list("root", None).await.expect("root folders");
    assert_eq!(roots.entries.iter().filter(|entry| entry.name == "tree").count(), 1);
    let children = api.list(&tree_id, None).await.expect("nested folders");
    assert_eq!(children.entries.iter().filter(|entry| entry.name == "nested").count(), 1);
    let children = api.list(&nested_id, None).await.expect("folder files");
    assert_eq!(children.entries.len(), 1);
    assert_eq!(api.media(&children.entries[0].id).await.expect("folder media").bytes().await.expect("folder bytes").as_ref(), b"folder payload");
    let listing = sessions.list(&peers, &runtime, &opened.session_id, "", None).await.expect("listed upload results");
    let selected_paths = listing.entries.iter().filter(|entry| matches!(entry.name.as_str(), "batch-a.txt" | "batch-b.txt" | "tree")).map(|entry| entry.id.clone()).collect::<Vec<_>>();
    assert_eq!(selected_paths.len(), 3);
    let (backend, adapter) = session.backend.download(&peers, &runtime).await.expect("shared download adapter");
    let target = tempfile::tempdir().expect("download target");
    let queues = Arc::new(crate::download_types::DownloadQueues::default());
    let queue = queues.select(backend, crate::download_types::DownloadSelection::Files { paths: selected_paths }, crate::cloud::PhysicalRoot::open(target.path()).expect("target capability"), adapter).await.expect("select mixed batch download");
    queues.start(&queue.queue_id).await.expect("start shared download");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = queues.status(&queue.queue_id).await.expect("download status");
            if status.phase == "completed" { break; }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("batch download completes");
    assert_eq!(std::fs::read(target.path().join("batch-a.txt")).expect("download a"), b"batch a");
    assert_eq!(std::fs::read(target.path().join("batch-b.txt")).expect("download b"), b"batch b");
    assert_eq!(std::fs::read(target.path().join("tree/nested/new.txt")).expect("recursive download"), b"folder payload");
    queues.release(&queue.queue_id).await.expect("release download");
    let edit_path = listing.entries.iter().find(|entry| entry.name == "batch-b.txt").expect("edit target").id.clone();
    let edit = sessions.prepare_edit(&peers, &runtime, &opened.session_id, &edit_path).await.expect("prepare Google external edit");
    assert_eq!(std::fs::read(&edit.local_path).expect("editor snapshot"), b"batch b");
    std::fs::write(&edit.local_path, "edited through shared contract").expect("edited local content");
    assert!(sessions.commit_edit(&peers, &runtime, &edit.edit_id).await.expect("Google edit writeback"));
    sessions.discard_edit(&edit.edit_id).await.expect("release edit");
    let remaining = api.list("root", None).await.expect("edited root");
    let edited = remaining.entries.iter().find(|entry| entry.name == "batch-b.txt").expect("edited target ID");
    assert_eq!(api.media(&edited.id).await.expect("edited media").bytes().await.expect("edited bytes").as_ref(), b"edited through shared contract");
    let delete_path = listing.entries.iter().find(|entry| entry.name == "batch-a.txt").expect("delete target").id.clone();
    let plan = sessions.delete_plan(&peers, &runtime, &opened.session_id, &delete_path).await.expect("Google delete plan");
    assert!(sessions.delete_execute(&peers, &runtime, &opened.session_id, &plan.plan_id, false).await.is_err());
    let result = sessions.delete_execute(&peers, &runtime, &opened.session_id, &plan.plan_id, true).await.expect("confirmed Google delete");
    assert_eq!(result.deleted, vec![delete_path]);
    assert!(sessions.delete_execute(&peers, &runtime, &opened.session_id, &plan.plan_id, true).await.is_err(), "delete confirmation is single use");
    assert!(sessions.delete_plan(&peers, &runtime, &opened.session_id, "").await.is_err(), "root remains protected");
    let remaining = api.list("root", None).await.expect("remaining files");
    assert!(!remaining.entries.iter().any(|entry| entry.name == "batch-a.txt"));
    assert!(remaining.entries.iter().any(|entry| entry.name == "batch-b.txt"));
    api.create_folder_id("root", "tree").await.expect("legal duplicate ordinary folder");
    let duplicate_listing = sessions.list(&peers, &runtime, &opened.session_id, "", None).await.expect("duplicate directory listing");
    let twins = duplicate_listing.entries.iter().filter(|entry| entry.name == "tree").collect::<Vec<_>>();
    assert_eq!(twins.len(), 2);
    assert_ne!(twins[0].id, twins[1].id);
    let ambiguous = session.backend.select_upload(&peers, &runtime, String::new(), vec![source.path().join("tree")], crate::file_manager::types::UploadMode::Folder, Default::default()).await.expect("select ambiguous folder upload");
    let crate::file_manager::types::FileUploadDto::Cloud(ambiguous) = ambiguous else { panic!("ambiguous queue") };
    session.backend.upload_action(&peers, &runtime, &ambiguous.batch_id, crate::file_manager::types::UploadAction::Start).await.expect("start ambiguous queue");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &ambiguous.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("ambiguous status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("ambiguous status") };
            if status.phase == "failed" { assert_eq!(status.error.expect("original cause").code, "FILE_TARGET_AMBIGUOUS"); assert_eq!(status.completed_files, 0); break; }
            assert_ne!(status.phase, "completed", "ambiguous sibling must never be selected by name");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("ambiguous selection fails safely");
    session.backend.release_upload(&peers, &runtime, &ambiguous.batch_id).await.expect("release ambiguous queue");
    let chosen = twins.iter().find(|entry| entry.id == tree_id).expect("original directory ID");
    let exact_source = source.path().join("chosen-only.txt");
    std::fs::write(&exact_source, "chosen ID").expect("chosen source");
    let exact = session.backend.select_upload(&peers, &runtime, chosen.id.clone(), vec![exact_source], crate::file_manager::types::UploadMode::Single, Default::default()).await.expect("upload to selected duplicate directory ID");
    let crate::file_manager::types::FileUploadDto::Cloud(exact) = exact else { panic!("exact queue") };
    session.backend.upload_action(&peers, &runtime, &exact.batch_id, crate::file_manager::types::UploadAction::Start).await.expect("start ID-bound upload");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = session.backend.upload_action(&peers, &runtime, &exact.batch_id, crate::file_manager::types::UploadAction::Status).await.expect("exact status");
            let crate::file_manager::types::FileUploadDto::Cloud(status) = status else { panic!("exact status") };
            if status.phase == "completed" { assert_eq!(status.completed_files, 1); break; }
            assert_ne!(status.phase, "failed", "{:?}", status.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("ID-bound upload completes");
    session.backend.release_upload(&peers, &runtime, &exact.batch_id).await.expect("release exact upload");
    assert!(api.list(&tree_id, None).await.expect("chosen directory").entries.iter().any(|entry| entry.name == "chosen-only.txt"));
    let other = twins.iter().find(|entry| entry.id != chosen.id).expect("other same-name directory");
    let other_page = sessions.list(&peers, &runtime, &opened.session_id, &other.id, None).await.expect("other directory remains separate");
    assert!(other_page.entries.is_empty());
    let chosen_page = sessions.list(&peers, &runtime, &opened.session_id, &chosen.id, None).await.expect("chosen directory before deletion");
    let child_id = chosen_page.entries.iter().find(|entry| entry.name == "chosen-only.txt").expect("listed child identity").id.clone();
    let plan = sessions.delete_plan(&peers, &runtime, &opened.session_id, &chosen.id).await.expect("directory deletion binds native ID");
    sessions.delete_execute(&peers, &runtime, &opened.session_id, &plan.plan_id, true).await.expect("delete only selected same-name directory");
    assert!(sessions.list(&peers, &runtime, &opened.session_id, &chosen.id, None).await.is_err());
    assert!(sessions.preview(&peers, &runtime, &opened.session_id, &child_id).await.is_err(), "deleted descendants lose cached authority");
    assert!(sessions.list(&peers, &runtime, &opened.session_id, &other.id, None).await.expect("same-name sibling remains admitted").entries.is_empty());
    api.create_folder_id("root", "_peerdav_mirror").await.expect("same-name sibling directory");
    assert!(store.list_page("_peerdav_mirror/", None).await.is_err(), "same-name folders must not merge into one virtual sync namespace");
    runtime.stop_and_clear().await.expect("stop resumed runtime");
    runtime.config.delete_connection(&connection.id).expect("delete connection"); assert!(store.stat(key).await.is_err());
    task.abort(); assert!(task.await.expect_err("aborted").is_cancelled());
}
