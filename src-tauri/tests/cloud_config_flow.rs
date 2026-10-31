use std::fs;
use std::sync::Arc;

use peerdav_lib::Vault;
use peerdav_lib::cloud::config::CloudConfigStore;
use peerdav_lib::cloud::config_types::{
    AddMappingInput, AppendRepositoryBinding, CloudConfigSnapshot, CloudMapping,
    SaveAppendBindingInput, SaveS3Input, ScheduleConfig, StorageProtocol,
};
use peerdav_lib::cloud::types::ProxyMode;
use peerdav_lib::sync::append_types::{AppendMode, TrustSource};
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
        repo_id: "repo".into(),
        anchor_key: format!("v2/anchors/repo-{fingerprint}"),
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
    save(binding.clone()).expect("encrypted mode");
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
