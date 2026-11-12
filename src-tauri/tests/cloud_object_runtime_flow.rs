use peerdav_lib::{
    Vault,
    cloud::{
        object_runtime_types::{ObjectDeletePlanDto, ObjectIdentityInput, ObjectTransferDto},
        runtime::CloudRuntime,
    },
    sync::traits::NoopSyncEvents,
};
use std::sync::Arc;

#[tokio::test]
async fn locked_runtime_rejects_object_commands_without_connection_or_network()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let vault = Arc::new(Vault::open(&directory.path().join("vault.db"))?);
    let runtime = CloudRuntime::new(vault, Arc::new(NoopSyncEvents), directory.path().to_owned());
    for error in [
        runtime.object_list("missing", "", None).await.err(),
        runtime.object_discover("missing", "").await.err(),
        runtime.object_delete_plan("missing", "").await.err(),
        runtime
            .object_delete_execute("missing", "forged", true)
            .await
            .err(),
        runtime
            .object_upload("missing", "/etc/passwd", "file".into(), None, false)
            .await
            .err(),
        runtime
            .object_download("missing", "/tmp", "file".into(), "file".into(), false)
            .await
            .err(),
        runtime.object_cancel("forged").await.err(),
        runtime.object_status().await.err(),
    ] {
        assert_eq!(
            error.ok_or("expected locked error")?.code,
            "CLOUD_GATE_LOCKED"
        );
    }
    assert!(!directory.path().join("cloud-object-guard.sqlite").exists());
    assert!(
        !directory
            .path()
            .join("cloud-object-recovery.sqlite")
            .exists()
    );
    Ok(())
}

#[test]
fn ipc_shapes_include_complete_delete_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let plan = ObjectDeletePlanDto {
        plan_id: "opaque".into(),
        expires_at_unix: 10,
        object_count: 1,
        connection_revision: 3,
        entries: vec![peerdav_lib::cloud::object_runtime_types::ObjectEntryDto {
            key: "data/file".into(),
            size: 42,
            etag: Some("etag".into()),
            protected: false,
        }],
    };
    assert_eq!(
        serde_json::to_value(plan)?,
        serde_json::json!({ "planId": "opaque", "expiresAtUnix": 10, "objectCount": 1, "connectionRevision": 3, "entries": [{"key":"data/file","size":42,"etag":"etag","protected":false}] })
    );
    let page = peerdav_lib::cloud::object_runtime_types::ObjectPageDto {
        breadcrumbs: Vec::new(),
        prefix: String::new(),
        objects: vec![peerdav_lib::cloud::object_runtime_types::ObjectEntryDto {
            key: "protected/file".into(),
            size: 42,
            etag: None,
            protected: true,
        }],
        next: None,
        complete: true,
        prefix_protected: true,
    };
    assert_eq!(
        serde_json::to_value(page)?,
        serde_json::json!({"prefix":"","objects":[{"key":"protected/file","size":42,"etag":null,"protected":true}],"next":null,"complete":true,"prefixProtected":true})
    );
    assert!(matches!(
        serde_json::from_value::<ObjectIdentityInput>(serde_json::json!({"kind":"new"}))?,
        ObjectIdentityInput::New {}
    ));
    assert!(
        matches!(serde_json::from_value::<ObjectIdentityInput>(serde_json::json!({"kind":"existing","physicalId":"id"}))?, ObjectIdentityInput::Existing { physical_id } if physical_id == "id")
    );
    assert!(
        serde_json::from_value::<ObjectIdentityInput>(
            serde_json::json!({"kind":"new","path":"forged"})
        )
        .is_err()
    );
    let status = ObjectTransferDto {
        transfer_id: "opaque".into(),
        connection_id: "connection".into(),
        connection_revision: 1,
        direction: "upload".into(),
        key: Some("data/file".into()),
        size_hint: Some(42),
        phase: "unknown".into(),
        error: None,
        recovery_required: true,
    };
    assert_eq!(serde_json::to_value(status)?["recoveryRequired"], true);
    Ok(())
}
