use crate::{
    AppError,
    commands::{SharedCloud, SharedPeers},
    download_types::*,
    traits::TransferQueueAdapter,
};
use std::sync::Arc;
use tauri::State;
#[tauri::command]
pub async fn transfer_select_download(
    app: tauri::AppHandle,
    queues: State<'_, SharedDownloads>,
    peers: State<'_, SharedPeers>,
    cloud: State<'_, SharedCloud>,
    backend: DownloadBackend,
    selection: DownloadSelection,
) -> Result<DownloadStatus, AppError> {
    let adapter: Arc<dyn TransferQueueAdapter> = match &backend {
        DownloadBackend::Peer { peer_id, share_id } => Arc::new(PeerDownloadAdapter {
            manager: Arc::clone(peers.inner()),
            peer: peer_id.clone(),
            share: share_id.clone(),
            scope: peers.download_scope(peer_id)?,
        }),
        DownloadBackend::S3 { connection_id } => {
            let session = cloud.0.object_session(connection_id).await?;
            Arc::new(S3DownloadAdapter {
                runtime: Arc::clone(&cloud.0),
                connection: connection_id.clone(),
                session,
            })
        }
    };
    adapter.validate().await?;
    let vault = Arc::clone(&cloud.0.vault);
    let selected = tokio::task::spawn_blocking(move || {
        let path = crate::picker_prefs::dialog(&app, crate::types::PickerDirection::Download)
            .blocking_pick_folder()
            .ok_or_else(|| AppError::from(crate::DomainError::TransferCanceled))?
            .into_path()
            .map_err(|_| crate::local_upload::invalid())?;
        let root = crate::cloud::PhysicalRoot::open(&path)
            .map_err(|e| AppError::from(crate::DomainError::from(e)))?;
        crate::picker_prefs::remember(&vault, crate::types::PickerDirection::Download, Some(&path), true);
        Ok::<_, AppError>(root)
    })
    .await
    .map_err(|e| {
        AppError::from(crate::DomainError::Internal {
            detail: e.to_string(),
        })
    })??;
    queues.select(backend, selection, selected, adapter).await
}
#[tauri::command]
pub async fn transfer_start_download(
    queues: State<'_, SharedDownloads>,
    queue_id: String,
) -> Result<DownloadStatus, AppError> {
    queues.inner().start(&queue_id).await
}
#[tauri::command]
pub async fn transfer_status_download(
    queues: State<'_, SharedDownloads>,
    queue_id: String,
) -> Result<DownloadStatus, AppError> {
    queues.status(&queue_id).await
}
#[tauri::command]
pub async fn transfer_page_download(
    queues: State<'_, SharedDownloads>,
    queue_id: String,
    offset: usize,
    limit: usize,
) -> Result<DownloadPage, AppError> {
    queues.page(&queue_id, offset, limit).await
}
#[tauri::command]
pub async fn transfer_resolve_download(
    queues: State<'_, SharedDownloads>,
    queue_id: String,
    conflict_id: String,
    decision: ConflictResolution,
    apply_to_remaining: bool,
) -> Result<(), AppError> {
    queues
        .resolve(&queue_id, &conflict_id, decision, apply_to_remaining)
        .await
}
#[tauri::command]
pub async fn transfer_cancel_download(
    queues: State<'_, SharedDownloads>,
    queue_id: String,
) -> Result<(), AppError> {
    queues.cancel(&queue_id).await
}
#[tauri::command]
pub async fn transfer_release_download(
    queues: State<'_, SharedDownloads>,
    queue_id: String,
) -> Result<(), AppError> {
    queues.release(&queue_id).await
}
#[tauri::command]
pub async fn transfer_reveal_download(
    app: tauri::AppHandle,
    queues: State<'_, SharedDownloads>,
    queue_id: String,
) -> Result<(), AppError> {
    use tauri_plugin_opener::OpenerExt;
    let path = queues.reveal_root(&queue_id).await?;
    app.opener().open_path(path, None::<&str>).map_err(|e| {
        AppError::from(crate::DomainError::Internal {
            detail: e.to_string(),
        })
    })
}
