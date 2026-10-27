//! Tauri command boundary. Every command maps a domain error exactly once
//! through [`AppError::from`] and never builds user-facing strings itself.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;
use zeroize::Zeroizing;

use crate::error::{AppError, DomainError};
use crate::ignore_rules::is_builtin_ignored;
use crate::peer::{PeerManager, TransferProgressCallback};
use crate::server::{ServerConfig, ServerController};
use crate::traits::{PeerEvents, ServerEvents};
use crate::types::{
    AccessRule, AdminLoginResult, AdminSessionInfo, ApprovePendingResult, AuthorizationInfo,
    ConflictPolicy, DEFAULT_DISCOVERY_PORT, DEFAULT_SERVER_PORT, DISCOVERED_PEERS_EVENT,
    DirectoryDownloadPlan, DiscoveredPeer, ERROR_EVENT, ExternalEditInfo, InitializeVaultResult,
    OutboundAccessRequest, PEER_REVOKED_EVENT, PEER_STATUS_EVENT, PENDING_CANCELED_EVENT,
    PENDING_REQUEST_EVENT, PeerHealth, PeerInfo, PeerListing, PeerRevocationInfo, PeerShare,
    PeerStatus, PendingRequestInfo, PreviewPayload, ResetVaultResult, SERVER_BIND_META,
    SERVER_DISCOVERY_PORT_META, SERVER_ENABLED_META, SERVER_NAME_META, SERVER_PORT_META,
    ServerStatus, ShareInfo, SharePermissions, TRANSFER_PROGRESS_EVENT, TransferProgress,
    TransferResult, UploadCandidate, VaultStatus,
};
use crate::vault::Vault;

pub type SharedVault = Arc<Vault>;
pub type SharedServer = Arc<ServerController>;
pub type SharedPeers = Arc<PeerManager>;

const MAX_UPLOAD_FILES: usize = 10_000;

/// Forwards server background events to the same structured UI outlet used by
/// command failures.
pub(crate) struct AppEvents {
    app: AppHandle,
    local_fingerprint: String,
}

impl AppEvents {
    pub(crate) fn new(app: AppHandle, local_fingerprint: String) -> Self {
        Self {
            app,
            local_fingerprint,
        }
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

    fn pending_canceled(&self, request: &PendingRequestInfo) {
        if let Err(emit_error) = self.app.emit(PENDING_CANCELED_EVENT, request) {
            log::error!("failed to emit {PENDING_CANCELED_EVENT}: {emit_error}");
        }
    }
}

impl PeerEvents for AppEvents {
    fn error(&self, error: &AppError) {
        if let Err(emit_error) = self.app.emit(ERROR_EVENT, error) {
            log::error!("failed to emit {ERROR_EVENT}: {emit_error}");
        }
    }

    fn peer_revoked(&self, peer: &PeerRevocationInfo) {
        if let Err(emit_error) = self.app.emit(PEER_REVOKED_EVENT, peer) {
            log::error!("failed to emit {PEER_REVOKED_EVENT}: {emit_error}");
        }
    }

    fn discovered_peers(&self, peers: &[DiscoveredPeer]) {
        let visible: Vec<_> = peers
            .iter()
            .filter(|peer| {
                crate::tls::self_connection_allowed(&self.local_fingerprint, &peer.server_id)
            })
            .cloned()
            .collect();
        if let Err(emit_error) = self.app.emit(DISCOVERED_PEERS_EVENT, visible) {
            log::error!("failed to emit {DISCOVERED_PEERS_EVENT}: {emit_error}");
        }
    }

    fn peer_status(&self, status: &PeerStatus) {
        if let Err(emit_error) = self.app.emit(PEER_STATUS_EVENT, status) {
            log::error!("failed to emit {PEER_STATUS_EVENT}: {emit_error}");
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
    key_directory: String,
    admin_username: String,
    admin_password: String,
) -> Result<InitializeVaultResult, AppError> {
    let vault = Arc::clone(vault.inner());
    let admin_password = Zeroizing::new(admin_password);
    run_blocking(move || vault.initialize(&key_directory, &admin_username, admin_password.as_str()))
        .await
}

#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub async fn reload_vault_key(
    vault: State<'_, SharedVault>,
    path: Option<String>,
) -> Result<VaultStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    run_blocking(move || vault.reload_key(path.as_deref())).await
}

#[tauri::command]
pub async fn reset_vault(
    vault: State<'_, SharedVault>,
    recovery_code: String,
    key_directory: String,
    admin_username: String,
    admin_password: String,
) -> Result<ResetVaultResult, AppError> {
    let vault = Arc::clone(vault.inner());
    let recovery_code = Zeroizing::new(recovery_code);
    let admin_password = Zeroizing::new(admin_password);
    run_blocking(move || {
        vault.reset_lost_key(
            recovery_code.as_str(),
            &key_directory,
            &admin_username,
            admin_password.as_str(),
        )
    })
    .await
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

/// Returns the in-memory admin session so the UI can restore the signed-in
/// state after a webview reload. `None` means the admin must sign in again.
#[tauri::command]
pub async fn admin_session(
    vault: State<'_, SharedVault>,
) -> Result<Option<AdminSessionInfo>, AppError> {
    let vault = Arc::clone(vault.inner());
    run_blocking(move || vault.admin_session()).await
}

/// Clears the in-memory admin session without locking the vault.
#[tauri::command]
pub async fn admin_logout(vault: State<'_, SharedVault>) -> Result<(), AppError> {
    let vault = Arc::clone(vault.inner());
    run_blocking(move || vault.admin_logout()).await
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

/// Admin commands accept the current in-memory session when the UI omits the
/// password after a reload; otherwise the supplied credentials are verified.
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
    peers: State<'_, SharedPeers>,
    username: String,
    password: String,
    bind_address: Option<String>,
    port: Option<u16>,
    discovery_port: Option<u16>,
    server_name: Option<String>,
) -> Result<ServerStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(Arc::clone(&vault), username, password).await?;

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
    // `start` consumes the config, so keep the values needed for persistence.
    let persisted = (
        config.bind_address.clone(),
        config.port,
        config.discovery_port,
        config.server_name.clone(),
    );
    if let Err(error) = peers.ensure_discovery_listener(config.discovery_port) {
        log::warn!("passive discovery listener could not bind: {error}");
    }
    server.set_discovery_listener(peers.discovery_listener());
    let status = server.start(config).await.map_err(AppError::from)?;
    let (bind_address, port, discovery_port, server_name) = persisted;
    run_blocking(move || {
        vault.set_setting(SERVER_ENABLED_META, "1")?;
        vault.set_setting(SERVER_BIND_META, &bind_address)?;
        vault.set_setting(SERVER_PORT_META, &port.to_string())?;
        vault.set_setting(SERVER_DISCOVERY_PORT_META, &discovery_port.to_string())?;
        vault.set_setting(SERVER_NAME_META, &server_name)?;
        Ok(())
    })
    .await?;
    Ok(status)
}

#[tauri::command]
pub async fn stop_server(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    username: String,
    password: String,
) -> Result<ServerStatus, AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(Arc::clone(&vault), username, password).await?;

    let server = Arc::clone(server.inner());
    let status = server.stop().await;
    run_blocking(move || vault.set_setting(SERVER_ENABLED_META, "0")).await?;
    Ok(status)
}

/// Restores the WebDAV server after an app start when the user left it enabled
/// in the previous session. It intentionally runs while the vault is locked:
/// the WebDAV service only needs the non-secret share and authorization
/// catalog, while the vault key protects sensitive records. Missing opt-in or
/// an already running server only reports the current status.
#[tauri::command]
pub async fn resume_server(
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    peers: State<'_, SharedPeers>,
) -> Result<ServerStatus, AppError> {
    let server = Arc::clone(server.inner());
    if server.status().running {
        return Ok(server.status());
    }

    let vault = Arc::clone(vault.inner());
    let config = run_blocking(move || {
        if vault.get_setting(SERVER_ENABLED_META)?.as_deref() != Some("1") {
            return Ok(None);
        }
        let bind_address = vault
            .get_setting(SERVER_BIND_META)?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "0.0.0.0".to_owned());
        let port = vault
            .get_setting(SERVER_PORT_META)?
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_SERVER_PORT);
        let discovery_port = vault
            .get_setting(SERVER_DISCOVERY_PORT_META)?
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_DISCOVERY_PORT);
        let server_name = vault
            .get_setting(SERVER_NAME_META)?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "PeerDav".to_owned());
        Ok(Some(ServerConfig {
            bind_address,
            port,
            discovery_port,
            server_name,
        }))
    })
    .await?;

    let Some(config) = config else {
        return Ok(server.status());
    };

    if let Err(error) = peers.ensure_discovery_listener(config.discovery_port) {
        log::warn!("passive discovery listener could not bind: {error}");
    }
    server.set_discovery_listener(peers.discovery_listener());
    match server.start(config).await {
        Ok(status) => Ok(status),
        Err(DomainError::ServerAlreadyRunning) => Ok(server.status()),
        Err(error) => Err(AppError::from(error)),
    }
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
    app: AppHandle,
    vault: State<'_, SharedVault>,
    server: State<'_, SharedServer>,
    peers: State<'_, SharedPeers>,
    username: String,
    password: String,
    authorization_id: String,
) -> Result<(), AppError> {
    let vault = Arc::clone(vault.inner());
    verify_admin(vault, username, password).await?;

    let server = Arc::clone(server.inner());
    run_blocking(move || server.revoke_authorization(&authorization_id)).await?;

    // A revoked authorization is removed from this process immediately. Remote
    // installations are reconciled by their own periodic backend sync.
    let peers = Arc::clone(peers.inner());
    if let Err(error) = peers.sync_saved_peers().await {
        let app_error = AppError::from(error);
        log::error!(
            "failed to reconcile peers after authorization revoke [{}]: {}",
            app_error.code,
            app_error.detail.as_deref().unwrap_or(&app_error.message)
        );
        if let Err(emit_error) = app.emit(ERROR_EVENT, &app_error) {
            log::error!("failed to emit {ERROR_EVENT}: {emit_error}");
        }
    }
    Ok(())
}

/// Synchronizes saved peers against their server bindings. The backend invokes
/// this automatically, while the UI can request an immediate check after a
/// relevant server change.
#[tauri::command]
pub async fn sync_peers(peers: State<'_, SharedPeers>) -> Result<usize, AppError> {
    let peers = Arc::clone(peers.inner());
    peers.start_background();
    peers.sync_saved_peers().await.map_err(AppError::from)
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

/// Returns the current passive discovery snapshot. The UI reads this once on
/// startup and then follows `app://discovered-peers` events.
#[tauri::command]
pub fn discovered_peers(peers: State<'_, SharedPeers>) -> Vec<DiscoveredPeer> {
    peers.discovered_peers()
}

/// Returns the local IPv4 LAN subnets used as the default allow rule for a
/// newly created share.
#[tauri::command]
pub async fn local_networks() -> Result<Vec<String>, AppError> {
    Ok(crate::discovery::local_ipv4_networks())
}

/// Returns the machine's private IPv4 interface addresses, offered in the
/// service pane as the LAN bind scope.
#[tauri::command]
pub async fn local_addresses() -> Result<Vec<String>, AppError> {
    Ok(crate::discovery::local_ipv4_addresses())
}

#[tauri::command]
pub async fn list_peers(peers: State<'_, SharedPeers>) -> Result<Vec<PeerInfo>, AppError> {
    let peers = Arc::clone(peers.inner());
    peers.start_background();
    run_blocking(move || peers.list_peers()).await
}

#[tauri::command]
pub fn device_fingerprint(vault: State<'_, SharedVault>) -> Result<String, AppError> {
    Ok(vault.fingerprint().to_owned())
}

#[tauri::command]
pub async fn inbound_connections(
    server: State<'_, SharedServer>,
) -> Result<Vec<crate::types::ConnectionInfo>, AppError> {
    let server = Arc::clone(server.inner());
    run_blocking(move || server.inbound_connections()).await
}

#[tauri::command]
pub async fn outbound_connections(
    peers: State<'_, SharedPeers>,
) -> Result<Vec<crate::types::ConnectionInfo>, AppError> {
    let peers = Arc::clone(peers.inner());
    run_blocking(move || peers.outbound_connections()).await
}

#[tauri::command]
pub async fn request_peer_access(
    peers: State<'_, SharedPeers>,
    url: String,
    label: Option<String>,
    fingerprint: Option<String>,
) -> Result<OutboundAccessRequest, AppError> {
    let peers = Arc::clone(peers.inner());
    if let Some(fingerprint) = fingerprint {
        peers
            .set_pairing_fingerprint(&url, &fingerprint)
            .map_err(AppError::from)?;
    }
    peers
        .begin_access(&url, label.as_deref())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn wait_for_peer_access(
    peers: State<'_, SharedPeers>,
    url: String,
    label: Option<String>,
) -> Result<PeerInfo, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .await_access(&url, label.as_deref())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn cancel_peer_access(
    peers: State<'_, SharedPeers>,
    url: String,
) -> Result<OutboundAccessRequest, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .cancel_access_request(&url)
        .await
        .map_err(AppError::from)
}

/// 出站访问申请的内存快照。列表按这份状态渲染，页面重建或 webview 重载后
/// 「已发送，等待批准」不会退回「可以申请」。
#[tauri::command]
pub fn access_requests(peers: State<'_, SharedPeers>) -> Vec<OutboundAccessRequest> {
    peers.access_requests()
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
pub async fn forget_peer(
    peers: State<'_, SharedPeers>,
    server: State<'_, SharedServer>,
    peer_id: String,
) -> Result<(), AppError> {
    let peers = Arc::clone(peers.inner());
    let server = Arc::clone(server.inner());
    // 解绑是双向的：先撤销对端的授权（失败则保留记录以便重试），再删本地记录，
    // 最后按机器码清掉本机为同一台设备开过的入站授权，避免「其他设备访问此设备」
    // 里留下已经解绑的幽灵设备。
    let machine_id = peers.forget(&peer_id).await?;
    if let Some(machine_id) = machine_id {
        let revoked =
            run_blocking(move || server.revoke_authorizations_for_machine(&machine_id)).await?;
        if revoked > 0 {
            log::info!("unbinding {peer_id} revoked {revoked} inbound authorization(s)");
        }
    }
    Ok(())
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
pub async fn delete_peer_entry(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    remote_path: String,
) -> Result<(), AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .delete_entry(&peer_id, &share_id, &remote_path)
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
pub async fn prepare_peer_directory_download(
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    remote_path: String,
    local_parent: String,
    conflict: Option<ConflictPolicy>,
) -> Result<DirectoryDownloadPlan, AppError> {
    let peers = Arc::clone(peers.inner());
    peers
        .prepare_directory_download(
            &peer_id,
            &share_id,
            &remote_path,
            &local_parent,
            conflict.unwrap_or_default(),
        )
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn download_peer_file_to(
    app: AppHandle,
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    remote_path: String,
    local_path: String,
    transfer_id: String,
) -> Result<TransferResult, AppError> {
    let peers = Arc::clone(peers.inner());
    let file_name = remote_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("file")
        .to_owned();
    let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
    let transfer_id_for_progress = transfer_id.clone();
    let progress: TransferProgressCallback = Arc::new(move |transferred, total| {
        let now = Instant::now();
        let should_emit = {
            let Ok(mut last) = last_emit.lock() else {
                return;
            };
            if transferred == total || now.duration_since(*last) >= Duration::from_millis(100) {
                *last = now;
                true
            } else {
                false
            }
        };
        if !should_emit {
            return;
        }
        let payload = TransferProgress {
            transfer_id: transfer_id_for_progress.clone(),
            direction: "download".to_owned(),
            file_name: file_name.clone(),
            transferred,
            total,
            phase: None,
            bytes_per_second: None,
        };
        if let Err(error) = app.emit(TRANSFER_PROGRESS_EVENT, &payload) {
            log::warn!("failed to emit {TRANSFER_PROGRESS_EVENT}: {error}");
        }
    });
    peers
        .download_to_with_progress(
            &peer_id,
            &share_id,
            &remote_path,
            &local_path,
            Some(progress),
        )
        .await
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn collect_upload_paths(paths: Vec<String>) -> Result<Vec<UploadCandidate>, AppError> {
    run_blocking(move || collect_upload_candidates(&paths)).await
}

fn collect_upload_candidates(paths: &[String]) -> Result<Vec<UploadCandidate>, DomainError> {
    let mut candidates = Vec::new();
    for raw in paths {
        let path = std::fs::canonicalize(raw)
            .map_err(|error| local_upload_io("resolve upload path", raw, error))?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| local_upload_io("inspect upload path", &path, error))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| DomainError::RemotePathInvalid {
                    detail: "the selected file has no usable name".to_owned(),
                })?;
            candidates.push(candidate_for(&path, Path::new(name))?);
        } else if metadata.is_dir() {
            let root = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| DomainError::RemotePathInvalid {
                    detail: "the selected folder has no usable name".to_owned(),
                })?;
            walk_upload_dir(&path, Path::new(root), &mut candidates)?;
        } else {
            return Err(DomainError::RemotePathInvalid {
                detail: "only regular files and folders can be uploaded".to_owned(),
            });
        }
    }
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if candidates.len() > MAX_UPLOAD_FILES {
        return Err(DomainError::UploadQueueTooLarge {
            count: candidates.len(),
            limit: MAX_UPLOAD_FILES,
        });
    }
    Ok(candidates)
}

fn walk_upload_dir(
    directory: &Path,
    relative: &Path,
    candidates: &mut Vec<UploadCandidate>,
) -> Result<(), DomainError> {
    let mut builder = WalkBuilder::new(directory);
    builder
        .hidden(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .git_ignore(true)
        .ignore(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            !is_builtin_ignored(&entry.file_name().to_string_lossy(), is_dir)
        });

    for result in builder.build() {
        let entry = result.map_err(|error| {
            local_upload_io(
                "walk upload folder",
                directory,
                std::io::Error::other(error),
            )
        })?;
        let path = entry.path();
        if path == directory {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        if candidates.len() >= MAX_UPLOAD_FILES {
            return Err(DomainError::UploadQueueTooLarge {
                count: candidates.len() + 1,
                limit: MAX_UPLOAD_FILES,
            });
        }
        let relative_path = path
            .strip_prefix(directory)
            .map(|path| relative.join(path))
            .map_err(|error| {
                local_upload_io(
                    "resolve upload path",
                    path,
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string()),
                )
            })?;
        candidates.push(candidate_for(path, &relative_path)?);
    }
    Ok(())
}

fn candidate_for(path: &Path, relative: &Path) -> Result<UploadCandidate, DomainError> {
    let size = std::fs::metadata(path)
        .map_err(|error| local_upload_io("inspect upload file", path, error))?
        .len();
    Ok(UploadCandidate {
        local_path: path.display().to_string(),
        relative_path: relative.to_string_lossy().replace('\\', "/"),
        size,
    })
}

fn local_upload_io(
    operation: &'static str,
    path: impl AsRef<Path>,
    source: std::io::Error,
) -> DomainError {
    DomainError::Io {
        operation,
        path: Some(PathBuf::from(path.as_ref())),
        source,
    }
}

#[tauri::command]
pub async fn open_local_file(app: AppHandle, path: String) -> Result<(), AppError> {
    ensure_downloaded_file(&path).await?;
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|error| {
            AppError::from(DomainError::ExternalOpen {
                path: path.clone(),
                detail: error.to_string(),
            })
        })
}

#[tauri::command]
pub async fn reveal_local_file(app: AppHandle, path: String) -> Result<(), AppError> {
    ensure_downloaded_file(&path).await?;
    app.opener().reveal_item_in_dir(&path).map_err(|error| {
        AppError::from(DomainError::ExternalReveal {
            path: path.clone(),
            detail: error.to_string(),
        })
    })
}

async fn ensure_downloaded_file(path: &str) -> Result<(), AppError> {
    let target = Path::new(path);
    if path.trim().is_empty() {
        return Err(AppError::from(DomainError::Io {
            operation: "inspect downloaded file",
            path: None,
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "the file path is empty"),
        }));
    }
    let metadata = tokio::fs::symlink_metadata(target).await.map_err(|error| {
        AppError::from(DomainError::Io {
            operation: "inspect downloaded file",
            path: Some(target.to_path_buf()),
            source: error,
        })
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::from(DomainError::Io {
            operation: "inspect downloaded file",
            path: Some(target.to_path_buf()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the path is not a regular file",
            ),
        }));
    }
    Ok(())
}

#[tauri::command]
pub async fn begin_peer_upload(peers: State<'_, SharedPeers>) -> Result<String, AppError> {
    Ok(peers.begin_upload().await)
}

#[tauri::command]
pub async fn cancel_peer_upload(
    peers: State<'_, SharedPeers>,
    transfer_id: String,
) -> Result<(), AppError> {
    peers.cancel_upload(&transfer_id).await;
    Ok(())
}

#[tauri::command]
pub async fn upload_peer_file(
    app: AppHandle,
    peers: State<'_, SharedPeers>,
    peer_id: String,
    share_id: String,
    local_path: String,
    remote_dir: String,
    conflict: Option<ConflictPolicy>,
    transfer_id: String,
) -> Result<TransferResult, AppError> {
    let peers = Arc::clone(peers.inner());
    let file_name = Path::new(&local_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let state = Mutex::new((
        None,
        Instant::now() - Duration::from_secs(1),
        0u64,
        0u64,
        0.0,
    ));
    let event_id = transfer_id.clone();
    let progress: crate::types::UploadProgressCallback =
        Arc::new(move |phase, transferred, total, sent| {
            let Ok(mut state) = state.lock() else {
                return;
            };
            let now = Instant::now();
            let elapsed = now.duration_since(state.1).as_secs_f64();
            let changed = state.0 != Some(phase);
            if !changed && elapsed < 0.1 && transferred != total {
                return;
            }
            if changed {
                state.2 = sent;
                state.3 = 0;
                state.4 = 0.0;
            } else if phase == crate::types::UploadPhase::Uploading && elapsed > 0.0 {
                state.4 = sent.saturating_sub(state.2) as f64 / elapsed;
            }
            state.0 = Some(phase);
            state.1 = now;
            state.2 = sent.max(state.2);
            state.3 = transferred.max(state.3);
            let payload = TransferProgress {
                transfer_id: event_id.clone(),
                direction: "upload".to_owned(),
                file_name: file_name.clone(),
                transferred: state.3.min(total),
                total,
                phase: Some(phase),
                bytes_per_second: Some(state.4),
            };
            if let Err(error) = app.emit(TRANSFER_PROGRESS_EVENT, &payload) {
                log::warn!("failed to emit {TRANSFER_PROGRESS_EVENT}: {error}");
            }
        });
    peers
        .upload_with_progress(
            &peer_id,
            &share_id,
            &local_path,
            &remote_dir,
            conflict.unwrap_or_default(),
            &transfer_id,
            progress,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_upload_files_with_relative_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(docs.join("sub")).expect("nested dir");
        std::fs::write(docs.join("a.txt"), b"a").expect("root file");
        std::fs::write(docs.join("sub").join("b.txt"), b"bb").expect("nested file");

        let candidates =
            collect_upload_candidates(&[docs.display().to_string()]).expect("collect uploads");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].relative_path, "docs/a.txt");
        assert_eq!(candidates[1].relative_path, "docs/sub/b.txt");
        assert_eq!(candidates[0].size, 1);
        assert_eq!(candidates[1].size, 2);
    }

    #[test]
    fn upload_walk_honors_gitignore_and_builtin_temporary_rules() {
        let temp = tempfile::tempdir().expect("temp dir");
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(docs.join("sub")).expect("nested dir");
        std::fs::create_dir_all(docs.join("build")).expect("ignored dir");
        std::fs::create_dir_all(docs.join("__pycache__")).expect("cache dir");
        std::fs::write(docs.join(".gitignore"), b"*.log\nbuild/\n!keep.log\n").expect("gitignore");
        std::fs::write(docs.join("a.txt"), b"a").expect("root file");
        std::fs::write(docs.join("drop.log"), b"drop").expect("ignored log");
        std::fs::write(docs.join("keep.log"), b"keep").expect("kept log");
        std::fs::write(docs.join("module.pyc"), b"cache").expect("pyc file");
        std::fs::write(docs.join("build").join("generated.txt"), b"generated")
            .expect("ignored file");
        std::fs::write(docs.join("__pycache__").join("x.pyc"), b"cache").expect("cache file");
        std::fs::write(docs.join("sub").join("b.txt"), b"bb").expect("nested file");

        let candidates =
            collect_upload_candidates(&[docs.display().to_string()]).expect("collect uploads");
        let relative: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.relative_path.as_str())
            .collect();
        for expected in [
            "docs/.gitignore",
            "docs/a.txt",
            "docs/keep.log",
            "docs/sub/b.txt",
        ] {
            assert!(
                relative.contains(&expected),
                "{expected} should be queued: {relative:?}"
            );
        }
        for ignored in [
            "docs/drop.log",
            "docs/module.pyc",
            "docs/build/generated.txt",
            "docs/__pycache__/x.pyc",
        ] {
            assert!(
                !relative.contains(&ignored),
                "{ignored} should be ignored: {relative:?}"
            );
        }
    }

    #[tokio::test]
    async fn downloaded_file_actions_only_accept_regular_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let file = temp.path().join("saved.txt");
        std::fs::write(&file, b"saved").expect("saved file");
        ensure_downloaded_file(file.to_str().expect("utf8"))
            .await
            .expect("regular file");

        let missing = temp.path().join("missing.txt");
        let error = ensure_downloaded_file(missing.to_str().expect("utf8"))
            .await
            .expect_err("missing file");
        assert_eq!(error.code, "IO_FAILED");
    }
}
