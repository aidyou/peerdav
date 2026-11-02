use super::directory_types::*;
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
    crate::file_manager::cloud_list(&cloud.0, &connection_id, &prefix, next.as_deref()).await
}
#[tauri::command]
pub async fn cloud_object_preview(
    connection_id: String,
    key: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<crate::types::PreviewPayload, AppError> {
    crate::file_manager::cloud_preview(&cloud.0, &connection_id, &key).await
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
    let picker_direction = if upload { crate::types::PickerDirection::Upload } else { crate::types::PickerDirection::Download };
    let selected = tokio::task::spawn_blocking(move || {
        let dialog = crate::picker_prefs::dialog(&app, picker_direction);
        if upload {
            dialog.blocking_pick_file()
        } else {
            dialog.blocking_pick_folder()
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
    let capability = cloud
        .0
        .object_grant_selected(&connection_id, revision, &path, upload)
        .await?;
    crate::picker_prefs::remember(&cloud.0.vault, picker_direction, Some(&path), !upload);
    Ok(capability)
}
#[tauri::command]
pub async fn cloud_object_select_upload_directory(
    app: tauri::AppHandle,
    connection_id: String,
    prefix: String,
    mode: Option<String>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<Option<ObjectUploadDirectoryDto>, AppError> {
    let mode = mode.unwrap_or_else(|| "folder".into());
    if !matches!(mode.as_str(), "folder" | "files" | "single") { return Err(crate::local_upload::invalid()); }
    let folder = mode == "folder";
    let single = mode == "single";
    let (revision, _) = cloud.0.object_selection_scope(&connection_id).await?;
    let selected = tokio::task::spawn_blocking(move || {
        let dialog = crate::picker_prefs::dialog(&app, crate::types::PickerDirection::Upload);
        if folder { dialog.blocking_pick_folder().map(|p| vec![p]) }
        else if single { dialog.blocking_pick_file().map(|p| vec![p]) }
        else { dialog.blocking_pick_files() }
    }).await.map_err(task_error)?;
    let Some(selected) = selected else { return Ok(None); };
    let mut paths = selected.into_iter().map(|p| p.into_path().map_err(|_| crate::local_upload::invalid())).collect::<Result<Vec<_>, _>>()?;
    let remembered = paths.first().cloned();
    let result = if folder {
        let path = paths.pop().ok_or_else(crate::local_upload::invalid)?;
        cloud.0.object_select_upload_directory(&connection_id, revision, prefix, &path).await?
    } else {
        cloud.0.object_select_upload_files(&connection_id, revision, prefix, paths).await?
    };
    crate::picker_prefs::remember(&cloud.0.vault, crate::types::PickerDirection::Upload, remembered.as_deref(), folder);
    Ok(Some(result))
}
#[tauri::command]
pub async fn cloud_object_start_upload_directory(
    connection_id: String,
    batch_id: String,
    conflict_policy: Option<DirectoryConflictPolicy>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectUploadDirectoryDto, AppError> {
    cloud
        .0
        .object_start_upload_directory(
            &connection_id,
            &batch_id,
            conflict_policy.unwrap_or_default(),
        )
        .await
}
#[tauri::command]
pub async fn cloud_object_status_upload_directory(
    connection_id: String,
    batch_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectUploadDirectoryDto, AppError> {
    cloud
        .0
        .object_status_upload_directory(&connection_id, &batch_id)
        .await
}
#[tauri::command]
pub async fn cloud_object_page_upload_directory(
    connection_id: String,
    batch_id: String,
    directory_token: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<DirectoryPageDto, AppError> {
    cloud
        .0
        .object_page_upload_directory(&connection_id, &batch_id, directory_token, cursor, limit)
        .await
}
#[tauri::command]
pub async fn cloud_object_resolve_upload_directory_conflict(
    connection_id: String,
    batch_id: String,
    conflict_id: String,
    decision: DirectoryConflictPolicy,
    apply_to_remaining: bool,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud
        .0
        .object_resolve_upload_directory_conflict(
            &connection_id,
            &batch_id,
            &conflict_id,
            decision,
            apply_to_remaining,
        )
        .await
}
#[tauri::command]
pub async fn cloud_object_cancel_upload_directory(
    connection_id: String,
    batch_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud
        .0
        .object_cancel_upload_directory(&connection_id, &batch_id)
        .await
}
#[tauri::command]
pub async fn cloud_object_release_upload_directory(
    connection_id: String,
    batch_id: String,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<(), AppError> {
    cloud
        .0
        .object_release_upload_directory(&connection_id, &batch_id)
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
    let _ = (cloud, connection_id, local_token, key, overwrite_etag, overwrite_confirmed);
    Err(crate::local_upload::invalid())
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
    let _ = (cloud, connection_id, local_token, key, file_name, overwrite_confirmed);
    Err(crate::local_upload::invalid())
}
#[tauri::command]
pub async fn cloud_object_download_batch(
    connection_id: String,
    local_token: String,
    items: Vec<ObjectDownloadItemDto>,
    overwrite_confirmed: bool,
    cloud: tauri::State<'_, SharedCloud>,
) -> Result<ObjectDownloadBatchDto, AppError> {
    let _ = (cloud, connection_id, local_token, items, overwrite_confirmed);
    Err(crate::local_upload::invalid())
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
