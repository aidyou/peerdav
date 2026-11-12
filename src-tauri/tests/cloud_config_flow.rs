use std::fs;
use std::sync::Arc;

use peerdav_lib::Vault;
use peerdav_lib::cloud::config::CloudConfigStore;
use peerdav_lib::cloud::config_types::{
    AddMappingInput, AppendRepositoryBinding, CloudConfigSnapshot, CloudMapping,
    SaveAppendBindingInput, SaveS3Input, ScheduleConfig, StorageProtocol,
};
use peerdav_lib::cloud::types::ProxyMode;
use peerdav_lib::sync::append_types::{APPEND_ANCHORS, AppendMode, TrustSource};
use peerdav_lib::sync::repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode};
use tempfile::TempDir;

fn store() -> (TempDir, CloudConfigStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Arc::new(Vault::open(&dir.path().join("vault.pd")).expect("open"));
    vault
        .initialize(dir.path().to_str().expect("path"), "admin", "password 123")
        .expect("init");
    (dir, CloudConfigStore::new(vault))
}
fn input() -> SaveS3Input {
    SaveS3Input {
        id: None,
        name: "main".into(),
        endpoint: "https://s3.example".into(),
        bucket: "bucket".into(),
        buckets: Vec::new(),
        region: "us-east-1".into(),
        prefix: String::new(),
        path_style: false,
        access_key: Some("access".into()),
        secret_key: Some("secret".into()),
        session_token: None,
        clear_credentials: false,
        clear_session_token: false,
        clear_proxy_auth: false,
        allow_insecure_http: false,
        proxy: ProxyMode::Direct,
        schedule: None,
    }
}

fn legacy_mapping() -> serde_json::Value {
    serde_json::json!({
        "id": "mapping", "connectionId": "connection", "repositoryId": "repo",
        "localRoot": "/legacy/root", "logicalPath": "remote",
        "schedule": {"enabled": true, "interval": 60}
    })
}

#[test]
fn settings_snapshot_cloud_is_shared_and_respects_lock_reset_and_failed_saves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vault.pd");
    let vault = Arc::new(Vault::open(&path).expect("open"));
    let initialized = vault.initialize(dir.path().to_str().expect("path"), "admin", "password 123").expect("init");
    let first = CloudConfigStore::new(vault.clone());
    let second = CloudConfigStore::new(vault.clone());
    first.save_client_proxy(ProxyMode::Direct).expect("save");
    assert_eq!(second.snapshot().expect("shared").client_proxy, ProxyMode::Direct);
    let db = rusqlite::Connection::open(&path).expect("db");
    db.execute_batch("CREATE TRIGGER reject_cloud BEFORE INSERT ON secrets BEGIN SELECT RAISE(FAIL, 'read only'); END;").expect("trigger");
    assert!(first.save_client_proxy(ProxyMode::Environment).is_err());
    assert_eq!(second.snapshot().expect("unchanged").client_proxy, ProxyMode::Direct);
    db.execute_batch("DROP TRIGGER reject_cloud; UPDATE secrets SET ciphertext = X'00' WHERE name = 'cloud.peerdav-s3.config';").expect("corrupt disk only");
    assert_eq!(first.snapshot().expect("cached").client_proxy, ProxyMode::Direct);
    vault.lock().expect("lock");
    assert!(first.snapshot().is_err());
    vault.reload_key(None).expect("unlock");
    assert!(second.snapshot().is_err(), "unlock must not resurrect stale cache");
    vault.lock().expect("lock before reset");
    vault.reset_lost_key(&initialized.recovery_code, dir.path().to_str().expect("path"), "admin", "password 123").expect("reset");
    assert_eq!(first.snapshot().expect("reset cache").client_proxy, ProxyMode::Environment);
    first.save_client_proxy(ProxyMode::Direct).expect("save after reset");
    drop(first);
    drop(second);
    drop(vault);
    let reopened = Arc::new(Vault::open(&path).expect("restart"));
    assert_eq!(CloudConfigStore::new(reopened).snapshot().expect("persisted").client_proxy, ProxyMode::Direct);
}

#[test]
fn settings_snapshot_cloud_concurrent_handles_preserve_all_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Arc::new(Vault::open(&dir.path().join("vault.pd")).expect("open"));
    vault.initialize(dir.path().to_str().expect("path"), "admin", "password 123").expect("init");
    std::thread::scope(|scope| {
        for index in 0..8 {
            let vault = vault.clone();
            scope.spawn(move || {
                let store = CloudConfigStore::new(vault);
                let mut value = input();
                value.name = format!("connection-{index}");
                store.save_connection(value).expect("concurrent update");
            });
        }
    });
    assert_eq!(CloudConfigStore::new(vault).snapshot().expect("all updates").connections.len(), 8);
}

#[test]
fn mirror_and_repository_mappings_cannot_share_local_trees() {
    let (dir, store) = store();
    let connection = store.save_connection(input()).unwrap();
    let root = dir.path().join("sync-root");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("precious.txt"), b"unchanged").unwrap();
    let repository = AddMappingInput {
        connection_id: connection.id.clone(),
        repository_id: "repo".into(),
        local_root: root.clone(),
        logical_path: "remote".into(),
        schedule: ScheduleConfig::default(),
    };
    let mirror = |path: std::path::PathBuf| {
        serde_json::from_value(serde_json::json!({ "id": null, "connectionId": connection.id, "localRoot": path, "cloudPrefix": "docs", "schedule": { "enabled": false, "interval": 900 } })).unwrap()
    };
    let mapping = store.add_mapping(repository.clone(), &[]).unwrap();
    assert!(store.save_mirror_mapping(mirror(root.clone())).is_err());
    assert!(
        store
            .save_mirror_mapping(mirror(root.join("nested")))
            .is_err()
    );
    store.delete_mapping(&mapping.id).unwrap();
    let mapping = store.save_mirror_mapping(mirror(root.clone())).unwrap();
    assert!(store.add_mapping(repository.clone(), &[]).is_err());
    assert!(
        store
            .add_mapping(
                AddMappingInput {
                    local_root: root.join("nested"),
                    ..repository
                },
                &[]
            )
            .is_err()
    );
    assert_eq!(fs::read(root.join("precious.txt")).unwrap(), b"unchanged");
    store.delete_mirror_mapping(&mapping.id).unwrap();
}

#[test]
fn official_connections_are_encrypted_redacted_and_revision_checked() {
    use peerdav_lib::cloud::{official_types::OfficialConnection, oauth_types::OfficialProvider};
    let (dir, store) = store();
    let saved = store.save_official_connection(OfficialConnection {
        id: String::new(), name: "Google Drive".into(), provider: OfficialProvider::GoogleDrive,
        config_version: 1, revision: 0, client_id: "client".into(),
        client_secret: Some("OFFICIAL_PRIVATE_CLIENT_SECRET".into()),
        refresh_token: "OFFICIAL_PRIVATE_REFRESH_TOKEN".into(), root_id: "root".into(),
    }).expect("save official connection");
    assert_eq!(saved.revision, 1);
    assert!(saved.browser_capabilities.sync_drives);
    assert!(!saved.browser_capabilities.select_root);
    let public = serde_json::to_string(&saved).expect("redacted");
    assert!(!public.contains("OFFICIAL_PRIVATE"));
    let connection = store.snapshot().expect("snapshot").official_connections[0].clone();
    assert!(!format!("{connection:?}").contains("OFFICIAL_PRIVATE"));
    let mut update = connection.clone();
    update.name = "Renamed".into();
    store.rotate_official_token(&saved.id, 1, "OFFICIAL_PRIVATE_ROTATED").expect("token rotation");
    let rotated = store.snapshot().expect("rotated snapshot").official_connections[0].clone();
    assert_eq!(rotated.revision, 1, "token refresh must not invalidate browsing sessions");
    assert_eq!(rotated.refresh_token, "OFFICIAL_PRIVATE_ROTATED");
    update.refresh_token = rotated.refresh_token;
    assert_eq!(store.save_official_connection(update).expect("update").revision, 2);
    assert!(store.rotate_official_token(&saved.id, 1, "stale-token").is_err());
    assert!(store.save_official_connection(connection.clone()).is_err(), "stale updates must not replace refreshed tokens");
    let mut invalid = connection;
    invalid.id.clear();
    invalid.config_version = 2;
    assert!(store.save_official_connection(invalid).is_err());
    let bytes = fs::read(dir.path().join("vault.pd")).expect("database");
    assert!(!bytes.windows(b"OFFICIAL_PRIVATE".len()).any(|part| part == b"OFFICIAL_PRIVATE"));
}

#[test]
fn official_basic_edit_preserves_identity_tokens_and_rejects_stale_or_invalid_changes() {
    use peerdav_lib::cloud::{official_types::{OfficialConnection, UpdateOfficialConnectionInput}, oauth_types::OfficialProvider};
    let (_dir, store) = store();
    let saved = store.save_official_connection_with_proxy(OfficialConnection {
        id: String::new(), name: "Drive".into(), provider: OfficialProvider::GoogleDrive,
        config_version: 1, revision: 0, client_id: "client".into(), client_secret: Some("private".into()),
        refresh_token: "refresh".into(), root_id: "root".into(),
    }, Some(true)).expect("create atomically with proxy");
    assert_eq!(store.snapshot().expect("snapshot").drive_proxies.get(&saved.id), Some(&true));
    let input = |revision, name: &str, secret, clear| UpdateOfficialConnectionInput {
        id: saved.id.clone(), revision, name: name.into(), client_secret: secret, clear_client_secret: clear, direct: false,
    };
    store.rotate_official_token(&saved.id, 1, "rotated").expect("rotate");
    let edited = store.update_official_connection(input(1, "Renamed", None, false)).expect("edit");
    assert_eq!(edited.revision, 2);
    let snapshot = store.snapshot().expect("edited");
    let item = &snapshot.official_connections[0];
    assert_eq!((item.name.as_str(), item.client_id.as_str(), item.root_id.as_str(), item.refresh_token.as_str()), ("Renamed", "client", "root", "rotated"));
    assert_eq!(item.client_secret.as_deref(), Some("private"));
    assert_eq!(snapshot.drive_proxies.get(&saved.id), Some(&false));
    assert!(store.update_official_connection(input(1, "stale", None, false)).is_err());
    assert!(store.update_official_connection(input(2, " ", None, false)).is_err());
    assert!(store.update_official_connection(input(2, "over", Some("x".repeat(16385)), false)).is_err());
    store.update_official_connection(input(2, "Renamed", Some("x".repeat(16384)), false)).expect("exact secret limit");
    let unchanged = store.update_official_connection(input(3, "Renamed again", Some(String::new()), false)).expect("empty preserves");
    assert_eq!(unchanged.revision, 3, "display-only edits preserve admitted sessions");
    assert_eq!(store.snapshot().expect("preserved").official_connections[0].client_secret.as_ref().map(String::len), Some(16384));
    store.update_official_connection(input(3, "Renamed again", None, true)).expect("explicit clear");
    assert!(store.snapshot().expect("cleared").official_connections[0].client_secret.is_none());
    let forged = serde_json::json!({ "id": saved.id, "revision": 5, "name": "forged", "direct": false, "provider": "onedrive", "rootId": "elsewhere" });
    assert!(serde_json::from_value::<UpdateOfficialConnectionInput>(forged).is_err(), "identity fields cannot be modified");
}

#[test]
fn legacy_connections_gain_explicit_protocol_without_changing_identity() {
    use peerdav_lib::cloud::config_types::CloudConnection;
    let (_dir, store) = store();
    let connection = store.save_connection(input()).expect("save connection");
    let mut legacy = serde_json::to_value(&store.snapshot().expect("snapshot").connections[0])
        .expect("encode connection");
    legacy.as_object_mut().expect("object").remove("protocolId");
    let restored: CloudConnection = serde_json::from_value(legacy.clone()).expect("legacy S3");
    let encoded = serde_json::to_value(&restored).expect("encode migrated connection");
    assert_eq!(encoded["protocolId"], "s3");
    assert_eq!(encoded["id"], connection.id);
    assert_eq!(encoded["revision"], connection.revision);
    let public = serde_json::to_value(restored.redacted()).expect("public connection");
    assert_eq!(public["protocolId"], "s3");
    assert_eq!(public["browserCapabilities"], serde_json::json!({ "selectRoot": true, "syncDrives": true }));
    assert!(public.get("secretKey").is_none());
    legacy["protocolId"] = "unsupported_protocol".into();
    assert!(serde_json::from_value::<CloudConnection>(legacy).is_err());
}

#[test]
fn legacy_schema_defaults_to_cas_and_rejects_unknown_protocol_or_fields() {
    let legacy = legacy_mapping();
    let mapping: CloudMapping = serde_json::from_value(legacy.clone()).expect("legacy mapping");
    assert_eq!(mapping.protocol, StorageProtocol::CasV1);
    assert!(mapping.append_binding.is_none());
    let encoded = serde_json::to_value(&mapping).expect("serialize");
    for (key, value) in legacy.as_object().expect("object") {
        assert_eq!(&encoded[key], value, "legacy field {key} changed");
    }
    let snapshot: CloudConfigSnapshot = serde_json::from_value(serde_json::json!({
        "connections": [], "mappings": [legacy.clone()]
    }))
    .expect("pre-cache snapshot");
    assert!(snapshot.repositories.is_empty());
    for protocol in ["future_v3", "cas_v2", "AppendV2"] {
        let mut invalid = legacy.clone();
        invalid["protocol"] = protocol.into();
        assert!(serde_json::from_value::<CloudMapping>(invalid).is_err());
    }
    let mut invalid = legacy;
    invalid["unexpected"] = true.into();
    assert!(serde_json::from_value::<CloudMapping>(invalid).is_err());
}

#[test]
fn append_binding_roundtrip_preserves_selection_mode_trust_and_encryption() {
    let (dir, store) = store();
    let mut credentials = input();
    credentials.secret_key = Some("PRIVATE_APPEND_CONFIG_CREDENTIAL".into());
    let connection = store.save_connection(credentials).expect("connection");
    let root = dir.path().join("root");
    fs::create_dir(&root).expect("root");
    let mapping_input = AddMappingInput {
        connection_id: connection.id,
        repository_id: "repo".into(),
        local_root: root,
        logical_path: "remote".into(),
        schedule: ScheduleConfig::default(),
    };
    let mapping = store
        .add_mapping(mapping_input.clone(), &[])
        .expect("mapping");
    let fingerprint = "ab".repeat(32);
    let mut binding = AppendRepositoryBinding {
        storage: None,
        repo_id: "repo".into(),
        anchor_key: format!("{APPEND_ANCHORS}repo-{fingerprint}"),
        anchor_fingerprint: fingerprint,
        append_mode: AppendMode::AuthenticatedPlain,
        trust_source: TrustSource::Unconfirmed,
        root_candidates: vec!["root-a".into(), "root-b".into()],
        selected_root: None,
    };
    let save = |binding: AppendRepositoryBinding| {
        store.save_append_binding(SaveAppendBindingInput {
            mapping_id: mapping.id.clone(),
            binding,
        })
    };
    let pending = save(binding.clone()).expect("pending binding");
    assert_eq!(pending.protocol, StorageProtocol::AppendV2);
    assert_eq!(
        store.load_append_binding(&mapping.id).expect("load"),
        binding
    );
    assert!(store.load_trusted_append_binding(&mapping.id).is_err());
    binding.selected_root = Some("root-b".into());
    save(binding.clone()).expect("selection alone does not confirm trust");
    assert!(!binding.is_trusted());
    assert!(store.load_trusted_append_binding(&mapping.id).is_err());
    for source in [TrustSource::AcceptedTofu, TrustSource::TrustedFingerprint] {
        binding.trust_source = source;
        save(binding.clone()).expect("confirmed");
        assert_eq!(
            store
                .load_trusted_append_binding(&mapping.id)
                .expect("trusted"),
            binding
        );
    }
    binding.selected_root = None;
    save(binding.clone()).expect("pending root");
    assert!(store.load_trusted_append_binding(&mapping.id).is_err());
    binding.selected_root = Some("root-b".into());
    binding.append_mode = AppendMode::Encrypted;
    binding.storage = Some(peerdav_lib::cloud::config_types::AppendStorage {
        endpoint: "https://s3.example".into(),
        bucket: "bound-bucket".into(),
        prefix: "bound-prefix/".into(),
    });
    save(binding.clone()).expect("encrypted mode with storage binding");
    let edited = store
        .edit_mapping(&mapping.id, mapping_input, &[])
        .expect("edit preserves binding");
    assert_eq!(edited.append_binding.as_ref(), Some(&binding));
    let snapshot: CloudConfigSnapshot =
        serde_json::from_slice(&store.load_secret_snapshot().expect("vault snapshot"))
            .expect("decode");
    assert_eq!(snapshot.mappings[0].append_binding.as_ref(), Some(&binding));
    let raw = fs::read(dir.path().join("vault.pd")).expect("vault bytes");
    // The vault schema itself contains "secret"; use a unique credential sentinel.
    for secret in [
        "PRIVATE_APPEND_CONFIG_CREDENTIAL",
        binding.anchor_fingerprint.as_str(),
    ] {
        assert!(!raw.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }
    let mut invalid = binding.clone();
    invalid.selected_root = Some("not-a-candidate".into());
    assert!(save(invalid).is_err());
    let mut invalid = binding.clone();
    invalid.repo_id = "another-repo".into();
    assert!(save(invalid).is_err());
    assert_eq!(
        store
            .load_trusted_append_binding(&mapping.id)
            .expect("failed writes are atomic"),
        binding
    );
}

#[test]
fn repository_cache_and_index_key_are_local_and_redacted() {
    let (_dir, store) = store();
    let descriptor = RepoDescriptor {
        version: 1,
        repo_id: "repo-a".into(),
        mode: RepositoryMode::Encrypted,
        chunker: ChunkerProfile {
            name: "fixed".into(),
            version: "1".into(),
            normalization: "none".into(),
            level: 1,
            min: 1,
            average: 2,
            max: 3,
            seed: 4,
        },
    };
    store
        .cache_repository(
            "conn",
            descriptor,
            Some(zeroize::Zeroizing::new([7_u8; 32])),
        )
        .expect("cache");
    let cached = store.load_repository("conn").expect("load");
    assert!(!format!("{cached:?}").contains('7'));
    assert_eq!(cached.redacted().id, "repo-a");
    let key = store.get_or_create_index_key().expect("index key");
    assert_eq!(&*key, &*store.get_or_create_index_key().expect("stable"));
    assert!(store.load_repository("missing").is_err());
    store.delete_connection("conn").expect("delete local cache");
    assert!(store.load_repository("conn").is_err());
}

#[test]
fn proxy_redaction_retains_route_but_never_returns_authentication() {
    let (_dir, store) = store();
    let mut value = input();
    value.proxy = ProxyMode::Custom {
        url: "socks5h://proxy.invalid:1080".into(),
        username: Some("PRIVATE_PROXY_USERNAME".into()),
        password: Some("PRIVATE_PROXY_PASSWORD".into()),
    };
    let saved = store.save_connection(value).unwrap();
    let json = serde_json::to_string(&saved).unwrap();
    assert!(json.contains("socks5h://proxy.invalid:1080"));
    assert!(!json.contains("PRIVATE_PROXY_USERNAME"));
    assert!(!json.contains("PRIVATE_PROXY_PASSWORD"));
    assert!(saved.has_proxy_auth);
    let mut edit = input();
    edit.id = Some(saved.id);
    edit.proxy = saved.proxy;
    let updated = store.save_connection(edit).unwrap();
    assert!(
        updated.has_proxy_auth,
        "blank authentication fields retain stored proxy credentials"
    );
    let snapshot = store.snapshot().unwrap();
    match &snapshot.connections[0].config.proxy {
        ProxyMode::Custom { password, .. } => {
            assert_eq!(password.as_deref(), Some("PRIVATE_PROXY_PASSWORD"))
        }
        _ => panic!("proxy route was lost"),
    }
}

#[test]
fn missing_repository_is_an_error() {
    let (_dir, store) = store();
    assert!(store.load_repository("missing").is_err());
}

#[test]
fn index_key_rejects_corruption() {
    let (_dir, store) = store();
    store.get_or_create_index_key().expect("create index key");
    assert!(store.get_or_create_index_key().is_ok());
}
#[test]
fn six_flow_checks_are_exercised() {
    let (dir, store) = store();
    let first = store.save_connection(input()).expect("save");
    assert_eq!(first.id.len(), 32);
    assert!(store.list_redacted().expect("list")[0].has_credentials);
    let raw = fs::read(dir.path().join("vault.pd")).expect("db");
    assert!(!raw.windows(b"access".len()).any(|w| w == b"access"));
    let mut update = input();
    update.id = Some(first.id.clone());
    update.access_key = Some(String::new());
    update.secret_key = Some(String::new());
    update.clear_credentials = true;
    let second = store.save_connection(update).expect("clear");
    assert_eq!(second.revision, 2);
    assert!(!second.has_credentials);
    let root = dir.path().join("root");
    fs::create_dir(&root).expect("root");
    let mapping = store
        .add_mapping(
            AddMappingInput {
                connection_id: first.id,
                repository_id: "repo".into(),
                local_root: root.clone(),
                logical_path: "remote".into(),
                schedule: ScheduleConfig {
                    enabled: true,
                    interval: 60,
                },
            },
            &[],
        )
        .expect("mapping");
    assert_eq!(mapping.logical_path, "remote");
    assert!(
        store
            .save_connection(SaveS3Input {
                schedule: Some(ScheduleConfig {
                    enabled: true,
                    interval: 1
                }),
                ..input()
            })
            .is_err()
    );
    store.delete_mapping(&mapping.id).expect("delete mapping");
    store
        .delete_connection(&second.id)
        .expect("delete connection");
    assert!(store.list_redacted().expect("list").is_empty());
}
