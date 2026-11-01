use super::{object_runtime_types::*, runtime_types::SharedCloud};
use crate::{AppError, DomainError};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

fn cancelled() -> AppError {
    AppError::from(DomainError::from(super::ObjectServiceError::Cancelled))
}
fn task_error(source: tokio::task::JoinError) -> AppError {
    AppError::from(DomainError::Internal {
        detail: source.to_string(),
    })
}

#[tauri::command]
pub async fn cloud_object_select_bucket(
    connection_id: String,
    bucket: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud.0.object_select_bucket(&connection_id, &bucket).await
}

#[tauri::command]
pub async fn cloud_object_list(
    connection_id: String,
    prefix: String,
    next: Option<String>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectPageDto, AppError> {
    cloud
        .0
        .object_list(&connection_id, &prefix, next.as_deref())
        .await
}
#[tauri::command]
pub async fn cloud_object_prepare(
    connection_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectPreparedDto, AppError> {
    cloud.0.object_prepare(&connection_id).await
}
#[tauri::command]
pub async fn cloud_object_discover(
    connection_id: String,
    prefix: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectDiscoveryDto, AppError> {
    cloud.0.object_discover(&connection_id, &prefix).await
}
#[tauri::command]
pub async fn cloud_object_register_physical_identity(
    app: tauri::AppHandle,
    connection_id: String,
    association: ObjectIdentityInput,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectIdentityDto, AppError> {
    let (revision, identity) = cloud.0.object_selection_scope(&connection_id).await?;
    let description = match &association {
        ObjectIdentityInput::New {} => "新物理存储 / New physical storage".to_owned(),
        ObjectIdentityInput::Existing { physical_id } => {
            format!("关联已有物理存储 / Associate existing physical storage: {physical_id}")
        }
    };
    let prompt = format!(
        "确认存储身份？仅在确认两个地址实际指向相同存储时关联。\nConfirm storage identity? Associate only when both addresses refer to the same physical storage.\nConnection: {connection_id}\nEndpoint: {}\nBucket: {}\nPrefix: {}\n{description}",
        identity.endpoint, identity.bucket, identity.prefix
    );
    let yes = tokio::task::spawn_blocking(move || {
        app.dialog()
            .message(prompt)
            .buttons(MessageDialogButtons::OkCancel)
            .blocking_show()
    })
    .await
    .map_err(task_error)?;
    if !yes {
        return Err(cancelled());
    }
    cloud
        .0
        .object_register_identity(&connection_id, revision, association)
        .await
}
#[tauri::command]
pub async fn cloud_object_select_local(
    app: tauri::AppHandle,
    connection_id: String,
    direction: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectLocalCapabilityDto, AppError> {
    let (revision, _) = cloud.0.object_selection_scope(&connection_id).await?;
    let upload = match direction.as_str() {
        "upload" => true,
        "download" => false,
        _ => {
            return Err(AppError::from(DomainError::from(
                super::ObjectServiceError::InvalidLocalCapability,
            )));
        }
    };
    let selected = tokio::task::spawn_blocking(move || {
        if upload {
            app.dialog().file().blocking_pick_file()
        } else {
            app.dialog().file().blocking_pick_folder()
        }
    })
    .await
    .map_err(task_error)?
    .ok_or_else(cancelled)?;
    let path = selected.into_path().map_err(|_| {
        AppError::from(DomainError::from(
            super::ObjectServiceError::InvalidLocalCapability,
        ))
    })?;
    cloud
        .0
        .object_grant_selected(&connection_id, revision, &path, upload)
        .await
}
#[tauri::command]
pub async fn cloud_object_upload(
    connection_id: String,
    local_token: String,
    key: String,
    overwrite_etag: Option<String>,
    overwrite_confirmed: bool,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectTransferDto, AppError> {
    cloud
        .0
        .object_upload(
            &connection_id,
            &local_token,
            key,
            overwrite_etag,
            overwrite_confirmed,
        )
        .await
}
#[tauri::command]
pub async fn cloud_object_download(
    connection_id: String,
    local_token: String,
    key: String,
    file_name: String,
    overwrite_confirmed: bool,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectTransferDto, AppError> {
    cloud
        .0
        .object_download(
            &connection_id,
            &local_token,
            key,
            file_name,
            overwrite_confirmed,
        )
        .await
}
#[tauri::command]
pub async fn cloud_object_delete_plan(
    connection_id: String,
    prefix: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectDeletePlanDto, AppError> {
    cloud.0.object_delete_plan(&connection_id, &prefix).await
}
#[tauri::command]
pub async fn cloud_object_delete_execute(
    connection_id: String,
    plan_id: String,
    confirmed: bool,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectDeleteResultDto, AppError> {
    cloud
        .0
        .object_delete_execute(&connection_id, &plan_id, confirmed)
        .await
}
#[tauri::command]
pub async fn cloud_object_cancel(
    transfer_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectTransferDto, AppError> {
    cloud.0.object_cancel(&transfer_id).await
}
#[tauri::command]
pub async fn cloud_object_recovery_acknowledge(
    transfer_id: String,
    connection_id: String,
    connection_revision: u64,
    confirmed: bool,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectTransferDto, AppError> {
    cloud
        .0
        .object_recovery_acknowledge(&transfer_id, &connection_id, connection_revision, confirmed)
        .await
}
#[tauri::command]
pub async fn cloud_object_status(
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Vec<ObjectTransferDto>, AppError> {
    cloud.0.object_status().await
}
