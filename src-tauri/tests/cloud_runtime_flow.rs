use std::sync::Arc;

use peerdav_lib::Vault;
use peerdav_lib::cloud::config_types::{AddMappingInput, SaveS3Input, ScheduleConfig};
use peerdav_lib::cloud::runtime::CloudRuntime;
use peerdav_lib::cloud::types::ProxyMode;
use peerdav_lib::sync::traits::NoopSyncEvents;
use tempfile::TempDir;

fn runtime() -> (TempDir, Arc<CloudRuntime>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Arc::new(Vault::open(&dir.path().join("vault.pd")).expect("open"));
    vault
        .initialize(dir.path().to_str().expect("path"), "admin", "password 123")
        .expect("init");
    let runtime = CloudRuntime::new(vault, Arc::new(NoopSyncEvents), dir.path().to_owned());
    (dir, runtime)
}

fn connection() -> SaveS3Input {
    SaveS3Input {
        id: None,
        name: "test".into(),
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

fn mapping(connection_id: String, root: &std::path::Path) -> AddMappingInput {
    AddMappingInput {
        connection_id,
        repository_id: "repo".into(),
        local_root: root.to_owned(),
        logical_path: "root".into(),
        schedule: ScheduleConfig::default(),
    }
}

#[tokio::test]
async fn recovery_requires_open_runtime() {
    let (_dir, runtime) = runtime();
    let connection = runtime.save_connection(connection()).expect("connection");
    let root_area = tempfile::tempdir().expect("separate root area");
    let root = root_area.path().join("root");
    std::fs::create_dir(&root).expect("root");
    runtime.resume().expect("resume");
    let mapping = runtime
        .save_mapping(None, mapping(connection.id, &root))
        .await
        .expect("mapping");
    runtime.stop_and_clear().await.expect("stop runtime");
    let error = runtime
        .recovery_records(&mapping.id)
        .await
        .expect_err("closed");
    assert_eq!(error.code, "CLOUD_GATE_LOCKED");
}

#[tokio::test]
async fn recovery_reports_missing_mapping_without_network() {
    let (_dir, runtime) = runtime();
    runtime.resume().expect("resume");
    let error = runtime
        .recovery_records("missing")
        .await
        .expect_err("missing");
    assert_eq!(error.code, "CLOUD_CONFIG_NOT_FOUND");
}

#[tokio::test]
async fn recovery_index_is_empty_for_new_mapping() {
    let (_dir, runtime) = runtime();
    let connection = runtime.save_connection(connection()).expect("connection");
    let root_area = tempfile::tempdir().expect("separate root area");
    let root = root_area.path().join("root");
    std::fs::create_dir(&root).expect("root");
    runtime.resume().expect("resume");
    let mapping = runtime
        .save_mapping(None, mapping(connection.id, &root))
        .await
        .expect("mapping");
    runtime.resume().expect("resume");
    let records = runtime
        .recovery_records(&mapping.id)
        .await
        .expect("records");
    assert!(records.is_empty());
}

#[tokio::test]
async fn restore_missing_record_does_not_write_target() {
    let (_dir, runtime) = runtime();
    let connection = runtime.save_connection(connection()).expect("connection");
    let root_area = tempfile::tempdir().expect("separate root area");
    let root = root_area.path().join("root");
    std::fs::create_dir(&root).expect("root");
    runtime.resume().expect("resume");
    let mapping = runtime
        .save_mapping(None, mapping(connection.id, &root))
        .await
        .expect("mapping");
    runtime.resume().expect("resume");
    let target = root.join("restored");
    let error = runtime
        .restore(&mapping.id, "missing", target.clone())
        .await
        .expect_err("missing");
    assert_eq!(error.code, "CLOUD_RECOVERY_NOT_FOUND");
    assert!(!target.exists());
}

#[tokio::test]
async fn append_commands_fail_closed_while_locked_and_validate_ipc_shapes() {
    use peerdav_lib::cloud::runtime_types::{
        CreateAppendRepositoryInput, JoinAppendRepositoryInput, SaveAppendMappingInput,
    };
    let (_directory, runtime) = runtime();
    let create: CreateAppendRepositoryInput = serde_json::from_value(serde_json::json!({"connectionId":"missing","mode":"authenticated_plain","password":"secret","passwordConfirmation":"secret"})).unwrap();
    assert_eq!(
        runtime
            .create_append_repository(create)
            .await
            .unwrap_err()
            .code,
        "CLOUD_GATE_LOCKED"
    );
    assert_eq!(
        runtime
            .discover_append_repositories("missing")
            .await
            .unwrap_err()
            .code,
        "CLOUD_GATE_LOCKED"
    );
    let join: JoinAppendRepositoryInput = serde_json::from_value(serde_json::json!({"connectionId":"missing","anchorKey":"key","anchorFingerprint":"fingerprint","repositoryId":"repo","mode":"encrypted","acceptTofu":false,"trustedFingerprint":null,"password":"secret"})).unwrap();
    assert_eq!(
        runtime.join_append_repository(join).await.unwrap_err().code,
        "CLOUD_GATE_LOCKED"
    );
    assert!(serde_json::from_value::<SaveAppendMappingInput>(serde_json::json!({"mappingId":null,"mapping":{},"selectedRoot":"root","implicitTrust":true})).is_err());
    assert!(serde_json::from_value::<CreateAppendRepositoryInput>(serde_json::json!({"connectionId":"missing","mode":"plain","password":"secret","passwordConfirmation":"secret"})).is_err());
}

#[test]
fn credentials_are_not_present_in_debug_config() {
    let (_dir, runtime) = runtime();
    let connection = runtime.save_connection(connection()).expect("connection");
    assert!(!format!("{connection:?}").contains("secret"));
}
