//! Tauri command boundary. Every command maps a domain error exactly once
//! through [`AppError::from`] and never builds user-facing strings itself.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;
use zeroize::Zeroizing;

use crate::error::{AppError, DomainError};
use crate::peer::PeerManager;
use crate::server::{ServerConfig, ServerController};
use crate::traits::ServerEvents;
use crate::types::{
    AccessRule, AdminLoginResult, ApprovePendingResult, AuthorizationInfo, ConflictPolicy,
    DEFAULT_DISCOVERY_PORT, DEFAULT_SERVER_PORT, DiscoveredPeer, ERROR_EVENT, ExternalEditInfo,
    InitializeVaultResult, PENDING_REQUEST_EVENT, PeerAccessRequest, PeerHealth, PeerInfo,
    PeerListing, PeerShare, PendingRequestInfo, PreviewPayload, ServerStatus, ShareInfo,
    SharePermissions, TransferResult, VaultStatus,
};
use crate::vault::Vault;

pub type SharedVault = Arc<Vault>;
pub type SharedServer = Arc<ServerController>;
pub type SharedPeers = Arc<PeerManager>;

/// Forwards server background events to the same structured UI outlet used by
/// command failures.
pub(crate) struct AppEvents {
    app: AppHandle,
}

impl AppEvents {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ServerEvents for AppEvents {
    fn error(&self, error: &AppError) {
        if let Err(emit_error) = self.app.emit(ERROR_EVENT, error) {
            log::error!("failed to emit {ERROR_EVENT}: {emit_error}");
        }
    }

    fn pending_request(&self, request: &PendingRequestInfo) {
        if let Err(emit_error) = self.app.emit(PENDING_REQUEST_EVENT, request) {
            log::error!("failed to emit {PENDING_REQUEST_EVENT}: {emit_error}");
        }
    }
}

#[tauri::command]
pub async fn vault_status(vault: State<'_, SharedVault>) -> Result<VaultStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    run_blocking(move || vault.status()).await
}

#[tauri::command]
pub async fn initialize_vault(
    vault: State<'_, SharedVault>,
    master_password: String,
    admin_username: String,
    admin_password: String,
) -> Result<InitializeVaultResult, AppError> {
    let vault = Arc::clone(vault.inner());
    let master_password = Zeroizing::new(master_password);
    let admin_password = Zeroizing::new(admin_password);
    run_blocking(move || {
        vault.initialize(
            master_password.as_str(),
            &admin_username,
            admin_password.as_str(),
        )
    })
    .await
}

#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub async fn unlock_vault(
    vault: State<'_, SharedVault>,
    master_password: String,
) -> Result<VaultStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    let master_password = Zeroizing::new(master_password);
    run_blocking(move || vault.unlock(master_password.as_str())).await
}

#[tauri::command]
pub async fn lock_vault(
    vault: State<'_, SharedVault>,
    peers: State<'_, SharedPeers>,
) -> Result<VaultStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    let status = run_blocking(move || vault.lock()).await?;
    // Client codes left in memory and external-edit copies are dropped with
    // the vault lock.
    Arc::clone(peers.inner()).clear_temp().await;
    Ok(status)
}

#[tauri::command]
pub async fn admin_login(
    vault: State<'_, SharedVault>,
    username: String,
    password: String,
) -> Result<AdminLoginResult, AppError> {
    let vault = Arc::clone(vault.inner());
    let password = Zeroizing::new(password);
    run_blocking(move || vault.admin_login(&username, password.as_str())).await
}

#[tauri::command]
pub async fn change_admin_password(
    vault: State<'_, SharedVault>,
    username: String,
    current_password: String,
    new_password: String,
) -> Result<AdminLoginResult, AppError> {
    let vault = Arc::clone(vault.inner());
    let current_password = Zeroizing::new(current_password);
    let new_password = Zeroizing::new(new_password);
    run_blocking(move || {
        vault.change_admin_password(&username, current_password.as_str(), new_password.as_str())
    })
    .await
}

#[tauri::command]
pub async fn change_master_password(
    vault: State<'_, SharedVault>,
    current_password: String,
    new_password: String,
) -> Result<VaultStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    let current_password = Zeroizing::new(current_password);
    let new_password = Zeroizing::new(new_password);
    run_blocking(move || {
        vault.change_master_password(current_password.as_str(), new_password.as_str())
    })
    .await
}

/// Admin commands re-verify the admin username/password every time. The app
/// never keeps a long-lived login state; the UI may retain the password in
/// memory for its own session but it is sent with each operation.
async fn verify_admin(
    vault: SharedVault,
    username: String,
    password: String,
) -> Result<(), AppError> {
    let password = Zeroizing::new(password);
    run_blocking(move || vault.verify_admin(&username, password.as_str())).await
}

#[tauri::command]
pub async fn server_status(server: State<'_, SharedServer>) -> Result<ServerStatus, AppError> {
    let server = Arc::clone(server.inner());
    Ok(server.status())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_server(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    bind_address: Option<String>,
    port: Option<u16>,
    discovery_port: Option<u16>,
    server_name: Option<String>,
) -> Result<ServerStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    let server_name = server_name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "PeerDav".to_owned());
    let config = ServerConfig {
        bind_address: bind_address.unwrap_or_else(|| "0.0.0.0".to_owned()),
        port: port.unwrap_or(DEFAULT_SERVER_PORT),
        discovery_port: discovery_port.unwrap_or(DEFAULT_DISCOVERY_PORT),
        server_name,
    };
    server.start(config).await.map_err(AppError::from)
}

#[tauri::command]
pub async fn stop_server(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
) -> Result<ServerStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    Ok(server.stop().await)
}

#[tauri::command]
pub async fn list_shares(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
) -> Result<Vec<ShareInfo>, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || Ok(server.list_shares())).await
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn create_share(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    name: String,
    root_path: String,
    permissions: SharePermissions,
    rules: Vec<AccessRule>,
) -> Result<ShareInfo, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.create_share(&name, &root_path, permissions, &rules)).await
}

#[tauri::command]
pub async fn update_share_permissions(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    share_id: String,
    permissions: SharePermissions,
) -> Result<ShareInfo, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.update_share_permissions(&share_id, permissions)).await
}

#[tauri::command]
pub async fn set_share_rules(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    share_id: String,
    rules: Vec<AccessRule>,
) -> Result<ShareInfo, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.set_share_rules(&share_id, &rules)).await
}

#[tauri::command]
pub async fn delete_share(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    share_id: String,
) -> Result<(), AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.delete_share(&share_id)).await
}

#[tauri::command]
pub async fn list_pending_requests(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
) -> Result<Vec<PendingRequestInfo>, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || Ok(server.list_pending())).await
}

#[tauri::command]
pub async fn approve_pending_request(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    request_id: String,
) -> Result<ApprovePendingResult, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.approve_pending(&request_id)).await
}

#[tauri::command]
pub async fn list_authorizations(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
) -> Result<Vec<AuthorizationInfo>, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || Ok(server.list_authorizations())).await
}

#[tauri::command]
pub async fn revoke_authorization(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
    authorization_id: String,
) -> Result<(), AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.revoke_authorization(&authorization_id)).await
}

#[tauri::command]
pub async fn scan_lan(
    peers: State<'_, SharedPeers>,
    port: Option<u16>,
    timeout_ms: Option<u64>,
) -> Result<Vec<DiscoveredPeer>, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .scan_lan(port, timeout_ms)
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn list_peers(peers: State<'_, SharedPeers>) -> Result<Vec<PeerInfo>, AppError> {
    let peers = Arc::clone(peers.inner());
    run_blocking(move || peers.list_peers()).await
}

#[tauri::command]
pub async fn request_peer_access(
    peers: State<'_, SharedPeers>,
    url: String,
) -> Result<PeerAccessRequest, AppError> {
    let peers = Arc::clone(peers.inner());
    peers.request_access(&url).await.map_err(AppError::from)
}

#[tauri::command]
pub async fn connect_peer(
    peers: State<'_, SharedPeers>,
    url: String,
    code: String,
    label: Option<String>,
) -> Result<PeerInfo, AppError> {
    let peers = Arc::clone(peers.inner());
    let code = Zeroizing::new(code);
    peers
        .connect(&url, code.as_str(), label.as_deref())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn check_peer_health(
    peers: State<'_, SharedPeers>,
    peer_id: String,
) -> Result<PeerHealth, AppError> {
    let peers = Arc::clone(peers.inner());
    peers.check_health(&peer_id).await.map_err(AppError::from)
}

#[tauri::command]
pub async fn forget_peer(peers: State<'_, SharedPeers>, peer_id: String) -> Result<(), AppError> {
    let peers = Arc::clone(peers.inner());
    peers.forget(&peer_id).await.map_err(AppError::from)
}

#[tauri::command]
pub async fn list_peer_shares(
    peers: State<'_, SharedPeers>,
    peer_id: String,
) -> Result<Vec<PeerShare>, AppError> {
    let peers = Arc::clone(peers.inner());
    peers.shares(&peer_id).await.map_err(AppError::from)
}

#[tauri::command]
pub async fn list_peer_dir(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    path: Option<String>,
) -> Result<PeerListing, AppError> {
    let peers = Arc::clone(peers.inner());
    let path = path.unwrap_or_else(|| "/".to_owned());
    peers
        .list_dir(&peer_id, &share_id, &path)
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn download_peer_file(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    remote_path: String,
    local_dir: String,
    conflict: Option<ConflictPolicy>,
) -> Result<TransferResult, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .download(
            &peer_id,
            &share_id,
            &remote_path,
            &local_dir,
            conflict.unwrap_or_default(),
        )
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn upload_peer_file(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    local_path: String,
    remote_dir: String,
    conflict: Option<ConflictPolicy>,
) -> Result<TransferResult, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .upload(
            &peer_id,
            &share_id,
            &local_path,
            &remote_dir,
            conflict.unwrap_or_default(),
        )
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn preview_peer_file(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    remote_path: String,
) -> Result<PreviewPayload, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .preview(&peer_id, &share_id, &remote_path)
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn prepare_peer_external_edit(
    app: AppHandle,
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    remote_path: String,
) -> Result<ExternalEditInfo, AppError> {
    let peers = Arc::clone(peers.inner());
    let info = peers
        .prepare_external_edit(&peer_id, &share_id, &remote_path)
        .await
        .map_err(AppError::from)?;
    // Opening through the Rust API keeps this path out of the broader opener
    // ACL; only the backend-selected temp file is ever opened.
    if let Err(error) = app.opener().open_path(&info.local_path, None::<&str>) {
        if let Err(cleanup) = peers.discard_external_edit(&info.edit_id).await {
            log::warn!("failed to discard an unopened external edit: {cleanup}");
        }
        return Err(AppError::from(DomainError::ExternalOpen {
            path: info.local_path,
            detail: error.to_string(),
        }));
    }
    Ok(info)
}

#[tauri::command]
pub async fn commit_peer_external_edit(
    peers: State<'_, SharedPeers>,
    edit_id: String,
    conflict: Option<ConflictPolicy>,
) -> Result<TransferResult, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .commit_external_edit(&edit_id, conflict.unwrap_or_default())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn discard_peer_external_edit(
    peers: State<'_, SharedPeers>,
    edit_id: String,
) -> Result<(), AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .discard_external_edit(&edit_id)
        .await
        .map_err(AppError::from)
}

async fn run_blocking<T, F>(task: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DomainError> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(task).await {
        Ok(result) => result.map_err(AppError::from),
        Err(error) => Err(AppError::from(DomainError::Internal {
            detail: format!("blocking vault task failed: {error}"),
        })),
    }
}
