use super::config_types::{
    AddMappingInput, CloudMapping, ConnectionRedacted, RepositoryRedacted, SaveS3Input,
};
use super::runtime::CloudRuntime;
use super::runtime_types::*;
use crate::AppError;
use std::sync::Arc;

#[tauri::command]
pub async fn cloud_test_connection(
    connection_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<serde_json::Value, AppError> {
    Ok(serde_json::json!({ "verified": cloud.0.test_connection(&connection_id).await? }))
}

#[tauri::command]
pub fn cloud_connection_credentials(
    connection_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::config_types::ConnectionCredentials, AppError> {
    cloud.0.connection_credentials(&connection_id)
}

#[tauri::command]
pub async fn cloud_probe_buckets(
    input: super::object_runtime_types::ProbeBucketsInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<super::service::BucketEntry>, AppError> {
    cloud.0.probe_buckets(input).await
}

#[tauri::command]
pub async fn cloud_recovery_records(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<RecoveryRecord>, AppError> {
    cloud.0.recovery_records(&mapping_id).await
}

#[tauri::command]
pub async fn cloud_restore(
    mapping_id: String,
    record_id: String,
    destination: std::path::PathBuf,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.restore(&mapping_id, &record_id, destination).await
}
#[tauri::command]
pub fn cloud_list(cloud: tauri::State<'_, SharedCloud>) -> Result<CloudRuntimeSnapshot, AppError> {
    cloud.0.snapshot()
}
#[tauri::command]
pub async fn cloud_save_connection(
    input: SaveS3Input,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ConnectionRedacted, AppError> {
    cloud.0.update_connection(input).await
}
#[tauri::command]
pub async fn cloud_delete_connection(
    connection_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.delete_connection(&connection_id).await?;
    cloud.0.refresh_schedules().await
}
#[tauri::command]
pub async fn cloud_create_repository(
    connection_id: String,
    encrypted: bool,
    password: String,
    password_confirmation: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<RepositoryRedacted, AppError> {
    cloud
        .0
        .create_repository(CreateRepositoryInput {
            connection_id,
            repository_id: None,
            encrypted,
            password,
            password_confirmation,
        })
        .await
}
#[tauri::command]
pub async fn cloud_join_repository(
    connection_id: String,
    password: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<RepositoryRedacted, AppError> {
    cloud
        .0
        .join_repository(JoinRepositoryInput {
            connection_id,
            password,
        })
        .await
}
#[tauri::command]
pub async fn cloud_save_mapping(
    mapping_id: Option<String>,
    input: AddMappingInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<CloudMapping, AppError> {
    let mapping = cloud.0.save_mapping(mapping_id.as_deref(), input).await?;
    cloud.0.refresh_schedules().await?;
    Ok(mapping)
}
#[tauri::command]
pub async fn cloud_delete_mapping(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.delete_mapping(&mapping_id).await?;
    cloud.0.refresh_schedules().await
}
#[tauri::command]
pub fn cloud_status(
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<crate::sync::manager_types::SyncStatus>, AppError> {
    Ok(cloud.0.manager.snapshots())
}
#[tauri::command]
pub async fn cloud_cancel(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.manager.stop_mapping(&mapping_id).await
}
#[tauri::command]
pub async fn cloud_preview(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<CloudPreview, AppError> {
    cloud.0.preview(&mapping_id).await
}
#[tauri::command]
pub async fn cloud_sync(
    mapping_id: String,
    preview_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<CloudSyncResult, AppError> {
    cloud.0.manual_sync(&mapping_id, &preview_id).await
}

#[tauri::command]
pub fn mirror_save_mapping(
    input: crate::mirror::SaveMirrorMappingInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<crate::mirror::MirrorMapping, AppError> {
    cloud.0.save_mirror_mapping(input)
}

#[tauri::command]
pub fn mirror_delete_mapping(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.delete_mirror_mapping(&mapping_id)
}

#[tauri::command]
pub async fn mirror_sync(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<crate::mirror::MirrorSyncResult, AppError> {
    cloud.0.mirror_sync(&mapping_id).await
}

#[tauri::command]
pub fn mirror_cancel(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.mirror_cancel(&mapping_id)
}

#[tauri::command]
pub async fn cloud_append_discover(
    connection_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<crate::sync::append_types::Candidate>, AppError> {
    cloud.0.discover_append_repositories(&connection_id).await
}
#[tauri::command]
pub async fn cloud_create_sync_drive(
    input: CreateAppendRepositoryInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::config_types::AppendRepositoryRedacted, AppError> {
    cloud.0.create_sync_drive(input).await
}
#[tauri::command]
pub async fn cloud_append_create(
    input: CreateAppendRepositoryInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::config_types::AppendRepositoryRedacted, AppError> {
    cloud.0.create_append_repository(input).await
}
#[tauri::command]
pub async fn cloud_append_join(
    input: JoinAppendRepositoryInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::config_types::AppendRepositoryRedacted, AppError> {
    cloud.0.join_append_repository(input).await
}
#[tauri::command]
pub async fn cloud_append_save_mapping(
    input: SaveAppendMappingInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<CloudMapping, AppError> {
    let result = cloud.0.save_append_mapping(input).await?;
    cloud.0.refresh_schedules().await?;
    Ok(result)
}

pub fn shared(runtime: Arc<CloudRuntime>) -> SharedCloud {
    SharedCloud(runtime)
}
