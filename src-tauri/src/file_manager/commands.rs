use super::types::*;
use crate::{
    AppError, PreviewPayload,
    commands::{SharedCloud, SharedPeers},
};
use tauri::State;

#[tauri::command]
pub async fn file_prepare_session(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String) -> Result<(), AppError> {
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    session.backend.prepare(&peers, &cloud.0).await?;
    sessions.admitted(&peers, &cloud.0, &session_id).await?;
    Ok(())
}

#[tauri::command]
pub async fn file_prepare_edit(app: tauri::AppHandle, sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, id: String) -> Result<FileEditDto, AppError> {
    use tauri_plugin_opener::OpenerExt;
    let info = sessions.prepare_edit(&peers, &cloud.0, &session_id, &id).await?;
    if let Err(error) = super::editor::start_watch(app.clone(), sessions.inner().clone(), peers.inner().clone(), cloud.0.clone(), info.clone()) {
        cloud.0.object_report(error);
    }
    if let Err(error) = app.opener().open_path(&info.local_path, None::<&str>) {
        sessions.discard_edit(&info.edit_id).await?;
        return Err(crate::DomainError::ExternalOpen { path: info.local_path, detail: error.to_string() }.into());
    }
    Ok(info)
}

#[tauri::command]
pub async fn file_commit_edit(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, edit_id: String) -> Result<bool, AppError> {
    sessions.commit_edit(&peers, &cloud.0, &edit_id).await
}

#[tauri::command]
pub async fn file_discard_edit(sessions: State<'_, SharedFileSessions>, edit_id: String) -> Result<(), AppError> {
    sessions.discard_edit(&edit_id).await
}

#[tauri::command]
pub async fn file_online_preview(app: tauri::AppHandle, sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, id: String) -> Result<(), AppError> {
    use tauri_plugin_opener::OpenerExt;
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    let url = session.backend.online_preview(&peers, &cloud.0, &id).await?;
    sessions.admitted(&peers, &cloud.0, &session_id).await?;
    app.opener().open_url(url, None::<&str>).map_err(|error| crate::DomainError::Internal { detail: error.to_string() }.into())
}

#[tauri::command]
pub async fn file_upload_resolve(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, batch_id: String, conflict_id: String, decision: crate::cloud::directory_types::DirectoryConflictPolicy, apply_to_remaining: bool) -> Result<(), AppError> {
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    session.backend.upload_resolve(peers.inner(), &cloud.0, &batch_id, &conflict_id, decision, apply_to_remaining).await
}

#[tauri::command]
pub async fn file_upload_page(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, batch_id: String, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> Result<crate::local_upload_types::DirectoryPageDto, AppError> {
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    session.backend.upload_page(&peers, &cloud.0, &batch_id, directory_token, cursor, limit).await
}

#[tauri::command]
pub async fn file_upload_action(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, batch_id: String, action: String) -> Result<super::types::FileUploadDto, AppError> {
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    let action = match action.as_str() {
        "start" => super::types::UploadAction::Start,
        "status" => super::types::UploadAction::Status,
        "cancel" => super::types::UploadAction::Cancel,
        _ => return Err(crate::local_upload::invalid()),
    };
    session.backend.upload_action(peers.inner(), &cloud.0, &batch_id, action).await
}

#[tauri::command]
pub async fn file_release_upload(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, batch_id: String) -> Result<(), AppError> {
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    session.backend.release_upload(&peers, &cloud.0, &batch_id).await
}

#[tauri::command]
pub async fn file_create_directory(sessions: State<'_, SharedFileSessions>, peers: State<'_, SharedPeers>, cloud: State<'_, SharedCloud>, session_id: String, id: String, name: String) -> Result<FileEntryDto, AppError> {
    sessions.create_directory(&peers, &cloud.0, &session_id, &id, &name).await
}

#[tauri::command]
pub async fn file_open_session(
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    location: FileLocation,
) -> Result<FileSessionDto, AppError> {
    sessions.open(&peers, &cloud.0, location).await
}
#[tauri::command]
pub async fn file_list(
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    session_id: String,
    id: String,
    next: Option<String>,
) -> Result<FilePageDto, AppError> {
    sessions
        .list(&peers, &cloud.0, &session_id, &id, next.as_deref())
        .await
}
#[tauri::command]
pub async fn file_preview(
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    session_id: String,
    id: String,
) -> Result<PreviewPayload, AppError> {
    sessions.preview(&peers, &cloud.0, &session_id, &id).await
}
#[tauri::command]
pub async fn file_delete_plan(
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    session_id: String,
    id: String,
) -> Result<FileDeletePlanDto, AppError> {
    sessions
        .delete_plan(&peers, &cloud.0, &session_id, &id)
        .await
}
#[tauri::command]
pub async fn file_delete_execute(
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    session_id: String,
    plan_id: String,
    confirmed: bool,
) -> Result<FileDeleteResultDto, AppError> {
    sessions
        .delete_execute(&peers, &cloud.0, &session_id, &plan_id, confirmed)
        .await
}
#[tauri::command]
pub async fn file_select_download(
    app: tauri::AppHandle,
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    queues: State<'_, crate::download_types::SharedDownloads>,
    session_id: String,
    selection: crate::download_types::DownloadSelection,
) -> Result<crate::download_types::DownloadStatus, AppError> {
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    let (backend, adapter) = session.backend.download(peers.inner(), &cloud.0).await?;
    let selected = crate::download_commands::select_download_with_adapter(
        app,
        queues.inner(),
        &cloud.0.vault,
        backend,
        selection,
        adapter,
    )
    .await?;
    if let Err(error) = sessions.admitted(&peers, &cloud.0, &session_id).await {
        queues.release(&selected.queue_id).await?;
        return Err(error);
    }
    Ok(selected)
}

#[tauri::command]
pub async fn file_select_upload(
    app: tauri::AppHandle,
    sessions: State<'_, SharedFileSessions>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    session_id: String,
    id: String,
    mode: String,
    options: Option<crate::types::IgnoreOptions>,
) -> Result<Option<FileUploadDto>, AppError> {
    if !matches!(mode.as_str(), "folder" | "files" | "single") {
        return Err(crate::local_upload::invalid());
    }
    let session = sessions.admitted(&peers, &cloud.0, &session_id).await?;
    if !session.backend.capabilities().upload {
        return Err(AppError { code: "FILE_CAPABILITY_UNAVAILABLE".into(), message: "此网盘暂不支持上传 / Upload is not available for this drive".into(), detail: None, context: None });
    }
    let folder = mode == "folder";
    let single = mode == "single";
    let picker_app = app.clone();
    let selected = tokio::task::spawn_blocking(move || {
        let dialog =
            crate::picker_prefs::dialog(&picker_app, crate::types::PickerDirection::Upload);
        if folder {
            dialog.blocking_pick_folder().map(|path| vec![path])
        } else if single {
            dialog.blocking_pick_file().map(|path| vec![path])
        } else {
            dialog.blocking_pick_files()
        }
    })
    .await
    .map_err(|error| {
        AppError::from(crate::DomainError::Internal {
            detail: error.to_string(),
        })
    })?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    sessions.admitted(&peers, &cloud.0, &session_id).await?;
    let paths = selected
        .into_iter()
        .map(|path| path.into_path().map_err(|_| crate::local_upload::invalid()))
        .collect::<Result<Vec<_>, _>>()?;
    if paths.is_empty() || paths.len() > 256 || ((folder || single) && paths.len() != 1) {
        return Err(crate::local_upload::invalid());
    }
    let remembered = paths.first().cloned();
    let result = session
        .backend
        .select_upload(
            &peers,
            &cloud.0,
            id,
            paths,
            match mode.as_str() { "single" => super::types::UploadMode::Single, "files" => super::types::UploadMode::Files, "folder" => super::types::UploadMode::Folder, _ => return Err(crate::local_upload::invalid()) },
            options.unwrap_or_default(),
        )
        .await?;
    if let Err(error) = sessions.admitted(&peers, &cloud.0, &session_id).await {
        let batch_id = match &result {
            FileUploadDto::Peer(summary) => &summary.batch_id,
            FileUploadDto::Cloud(summary) => &summary.batch_id,
        };
        session
            .backend
            .release_upload(&peers, &cloud.0, batch_id)
            .await?;
        return Err(error);
    }
    crate::picker_prefs::remember(
        &cloud.0.vault,
        crate::types::PickerDirection::Upload,
        remembered.as_deref(),
        folder,
    );
    Ok(Some(result))
}

#[tauri::command]
pub async fn file_close_session(
    sessions: State<'_, SharedFileSessions>,
    session_id: String,
) -> Result<(), AppError> {
    sessions.close(&session_id).await;
    Ok(())
}
