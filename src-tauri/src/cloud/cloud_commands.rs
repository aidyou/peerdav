use super::config_types::{
    AddMappingInput, CloudMapping, ConnectionRedacted, RepositoryRedacted, SaveS3Input,
};
use super::runtime::CloudRuntime;
use super::runtime_types::*;
use crate::AppError;
use std::sync::Arc;

#[tauri::command]
pub fn client_proxy_settings(cloud: tauri::State<'_, SharedCloud>) -> Result<super::types::ProxyMode, AppError> {
    cloud.0.admit()?;
    let proxy = cloud.0.config.snapshot().map_err(super::runtime::config_error)?.client_proxy;
    Ok(match proxy {
        super::types::ProxyMode::Custom { url, username, .. } => super::types::ProxyMode::Custom { url, username, password: None },
        other => other,
    })
}

#[tauri::command]
pub async fn save_client_proxy_settings(proxy: super::types::ProxyMode, cloud: tauri::State<'_, SharedCloud>) -> Result<(), AppError> {
    cloud.0.admit()?;
    let _network = cloud.0.network.write().await;
    cloud.0.admit()?;
    cloud.0.config.save_client_proxy(proxy).map_err(super::runtime::config_error)?;
    cloud.0.official_refresh.lock().await.clear();
    cloud.0.objects.lock().await.sessions.clear();
    Ok(())
}

#[tauri::command]
pub fn drive_proxy_settings(connection_id: String, cloud: tauri::State<'_, SharedCloud>) -> Result<bool, AppError> {
    cloud.0.admit()?;
    let snapshot = cloud.0.config.snapshot().map_err(super::runtime::config_error)?;
    if let Some(connection) = snapshot.connections.iter().find(|entry| entry.id == connection_id) {
        return Ok(matches!(connection.config.proxy, super::types::ProxyMode::Direct));
    }
    if !snapshot.official_connections.iter().any(|entry| entry.id == connection_id) { return Err(super::runtime::config_error(super::config_types::ConfigError::NotFound("connection".into()))); }
    Ok(snapshot.drive_proxies.get(&connection_id).copied().unwrap_or(false))
}

#[tauri::command]
pub async fn save_drive_proxy_settings(connection_id: String, direct: bool, cloud: tauri::State<'_, SharedCloud>) -> Result<(), AppError> {
    cloud.0.admit()?;
    let _network = cloud.0.network.write().await;
    cloud.0.admit()?;
    cloud.0.config.save_drive_proxy(&connection_id, direct).map_err(super::runtime::config_error)?;
    cloud.0.official_refresh.lock().await.remove(&connection_id);
    cloud.0.objects.lock().await.sessions.remove(&connection_id);
    Ok(())
}

#[tauri::command]
pub fn cloud_protocols() -> Result<Vec<super::protocol_types::ProtocolDescriptor>, AppError> {
    Ok(super::protocol_registry::descriptors())
}

#[tauri::command]
pub fn cloud_official_authorize(
    provider: super::oauth_types::OfficialProvider, client_id: String, redirect_uri: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::oauth_types::AuthorizationStart, AppError> {
    cloud.0.begin_official_authorization(provider, client_id, redirect_uri)
}

#[tauri::command]
pub async fn cloud_official_cancel_authorization(
    request_id: String, cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.cancel_official_authorization(&request_id).await
}

#[tauri::command]
pub async fn cloud_official_begin_loopback(
    app: tauri::AppHandle,
    provider: super::oauth_types::OfficialProvider, client_id: String, redirect_uri: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::oauth_types::AuthorizationStart, AppError> {
    use tauri_plugin_opener::OpenerExt;
    let start = cloud.0.begin_loopback_authorization(provider, client_id, redirect_uri).await?;
    if let Err(source) = app.opener().open_url(&start.authorization_url, None::<&str>) {
        cloud.0.cancel_official_authorization(&start.request_id).await?;
        return Err(AppError::from(crate::DomainError::Internal { detail: source.to_string() }));
    }
    Ok(start)
}

#[tauri::command]
pub async fn cloud_official_wait_loopback(
    request_id: String, name: String, client_secret: Option<String>, root_id: String, direct: Option<bool>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
    cloud.0.wait_loopback_authorization_with_proxy(&request_id, name, client_secret, root_id, direct.unwrap_or(false)).await
}

#[tauri::command]
pub async fn cloud_official_complete_authorization(
    request_id: String, callback: String, name: String, client_secret: Option<String>, root_id: String, direct: Option<bool>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
    cloud.0.complete_official_authorization_with_proxy(&request_id, &callback, name, client_secret, root_id, direct.unwrap_or(false)).await
}

#[tauri::command]
pub async fn cloud_official_update_connection(
    input: super::official_types::UpdateOfficialConnectionInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::official_types::OfficialConnectionRedacted, AppError> {
    cloud.0.admit()?;
    let _network = cloud.0.network.write().await;
    cloud.0.admit()?;
    let revision = input.revision;
    let result = cloud.0.config.update_official_connection(input).map_err(super::runtime::config_error)?;
    if result.revision != revision {
        cloud.0.official_refresh.lock().await.remove(&result.id);
        cloud.0.objects.lock().await.sessions.remove(&result.id);
    }
    Ok(result)
}

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
pub async fn cloud_local_entries(
    mapping_id: String,
    path: Option<String>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::runtime_types::LocalDirectoryListing, AppError> {
    cloud.0.local_directory_listing(&mapping_id, path.as_deref().unwrap_or("")).await
}

#[tauri::command]
pub async fn cloud_local_preview(mapping_id: String, path: String, cloud: tauri::State<'_, SharedCloud>) -> Result<crate::PreviewPayload, AppError> {
    cloud.0.local_file_preview(&mapping_id, &path).await
}
#[tauri::command]
pub async fn cloud_local_open(app: tauri::AppHandle, mapping_id: String, path: String, cloud: tauri::State<'_, SharedCloud>) -> Result<(), AppError> {
    cloud.0.local_file_open(&app, &mapping_id, &path).await
}
#[tauri::command]
pub async fn cloud_local_mutate(mapping_id: String, path: String, destination: Option<String>, confirmed: bool, cloud: tauri::State<'_, SharedCloud>) -> Result<(), AppError> {
    if !confirmed { return Err(AppError { code: "CLOUD_CONFIRMATION_REQUIRED".into(), message: "请确认本地文件操作 / Confirm local file operation".into(), detail: None, context: None }); }
    cloud.0.local_file_mutate(&mapping_id, &path, destination.as_deref()).await
}

#[tauri::command]
pub async fn cloud_local_status(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<LocalSyncEntry>, AppError> {
    cloud.0.local_sync_entries(&mapping_id).await
}

#[tauri::command]
pub async fn cloud_preview(
    mapping_id: String,
    selection: Option<Vec<String>>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<CloudPreview, AppError> {
    cloud.0.preview_selection(&mapping_id, &selection.unwrap_or_default()).await
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
pub async fn mirror_folders(
    connection_id: String, bucket: Option<String>, prefix: String, next: Option<String>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<super::object_runtime_types::ObjectPageDto, AppError> {
    cloud.0.mirror_folders(&connection_id, bucket.as_deref(), &prefix, next.as_deref()).await
}

#[tauri::command]
pub async fn mirror_save_mapping(
    input: crate::mirror::SaveMirrorMappingInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<crate::mirror::MirrorMapping, AppError> {
    let mapping = cloud.0.save_mirror_mapping(input)?;
    cloud.0.refresh_schedules().await?;
    Ok(mapping)
}

#[tauri::command]
pub async fn mirror_delete_mapping(
    mapping_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.delete_mirror_mapping(&mapping_id)?;
    cloud.0.refresh_schedules().await
}

#[tauri::command]
pub async fn mirror_sync(
    mapping_id: String,
    force: Option<bool>,
    plan_id: Option<String>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<crate::mirror::MirrorSyncResult, AppError> {
    cloud
        .0
        .mirror_sync(&mapping_id, force.unwrap_or(false), plan_id)
        .await
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
    bucket: Option<String>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<crate::sync::append_types::Candidate>, AppError> {
    cloud.0.discover_append_in_bucket(&connection_id, bucket.as_deref()).await
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

#[tauri::command]
pub async fn cloud_prepare_mapping_bind(input: SaveAppendMappingInput, cloud: tauri::State<'_, SharedCloud>) -> Result<MappingBindResult, AppError> {
    let result = cloud.0.prepare_mapping_bind(input).await?;
    if result.mapping.is_some() { cloud.0.refresh_schedules().await?; }
    Ok(result)
}
#[tauri::command]
pub async fn cloud_confirm_mapping_bind(plan_id: String, confirmed: bool, cloud: tauri::State<'_, SharedCloud>) -> Result<MappingBindResult, AppError> {
    let result = cloud.0.confirm_mapping_bind(&plan_id, confirmed).await?;
    cloud.0.refresh_schedules().await?;
    Ok(result)
}
#[tauri::command]
pub async fn cloud_cancel_mapping_bind(plan_id: String, cloud: tauri::State<'_, SharedCloud>) -> Result<(), AppError> {
    cloud.0.cancel_mapping_bind(&plan_id).await
}

pub fn shared(runtime: Arc<CloudRuntime>) -> SharedCloud {
    SharedCloud(runtime)
}
