//! PeerDav backend: encrypted vault, SQLite metadata, LAN WebDAV server,
//! UDP discovery, and the PeerDav client with loopback (self-connect) support.

mod admin;
mod commands;
mod crypto;
mod db;
mod discovery;
mod error;
mod fs;
mod ignore_rules;
mod keyfile;
mod logging;
pub mod paths;
mod peer;
mod policy;
mod server;
mod traits;
mod types;
mod vault;

pub use error::{AppError, DomainError};
pub use peer::{PeerLimits, PeerManager};
pub use policy::{PeerContext, PolicyDecision, evaluate_rules};
pub use server::{ServerConfig, ServerController};
pub use traits::{PeerEvents, ServerEvents};
pub use types::{
    ACCESS_GRANT_TTL, ACCESS_PROTOCOL, ACCESS_REQUEST_HEADER, ACCESS_STATUS_PATH,
    AccessRequestStatus, AccessRule, AccessStatusResponse, AdminLoginResult, ApprovePendingResult,
    AuthorizationBindingResponse, AuthorizationInfo, AuthorizationStatus,
    AuthorizationStatusResponse, ConflictPolicy, DiscoveredPeer, ExternalEditInfo,
    InitializeVaultResult, MAX_ACCESS_GRANTS, MAX_ACCESS_REQUEST_ID_CHARS, MAX_PENDING_ATTEMPTS,
    MAX_PENDING_PATH_CHARS, MAX_PENDING_REQUESTS, MIN_ACCESS_REQUEST_ID_CHARS,
    PEER_ACCESS_POLL_INTERVAL, PEER_ACCESS_WAIT, PeerAccessRequest, PeerEntry, PeerHealth,
    PeerInfo, PeerListing, PeerRevocationInfo, PeerShare, PeerStatus, PendingRequestInfo,
    PreviewPayload, RuleKind, RuleMatcher, ServerStatus, ShareInfo, SharePermissions,
    TransferResult, VaultStatus,
};
pub use vault::Vault;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};

use crate::commands::{AppEvents, SharedPeers, SharedServer, SharedVault};
use crate::error::error_chain;
use crate::types::{
    DB_FILE_NAME, MAIN_WINDOW_LABEL, QUIT_REQUEST_EVENT, TRAY_QUIT_MENU_ID, TRAY_SHOW_MENU_ID,
};

/// Tracks whether the system tray was created, so closing the window can fall
/// back to a normal exit instead of hiding the app with no way to restore it.
#[derive(Clone, Copy)]
struct TrayAvailable(bool);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::init();

    let application = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let data_dir = match paths::dev_data_root() {
                Some(dir) => dir,
                None => app
                    .path()
                    .app_data_dir()
                    .map_err(|error| DomainError::Internal {
                        detail: format!("failed to resolve app data directory: {error}"),
                    })?,
            };
            std::fs::create_dir_all(&data_dir).map_err(|error| DomainError::Internal {
                detail: format!(
                    "failed to create app data directory {}: {error}",
                    data_dir.display()
                ),
            })?;
            let vault = Arc::new(Vault::open(&data_dir.join(DB_FILE_NAME))?);
            let events = Arc::new(AppEvents::new(app.handle().clone()));
            let server_events: Arc<dyn ServerEvents> = Arc::clone(&events) as Arc<dyn ServerEvents>;
            let peer_events: Arc<dyn PeerEvents> = Arc::clone(&events) as Arc<dyn PeerEvents>;
            let server = Arc::new(ServerController::new(Arc::clone(&vault), server_events)?);
            let peers = Arc::new(PeerManager::with_events(
                Arc::clone(&vault),
                data_dir.join("peer-temp"),
                peer_events,
            )?);
            // The setup hook is synchronous and has no entered Tokio runtime, so
            // the peer background tasks (including the UDP discovery listener)
            // start on the async runtime instead of constructing a socket here.
            let startup_peers = Arc::clone(&peers);
            let startup_server = Arc::clone(&server);
            tauri::async_runtime::spawn(async move {
                startup_peers.start_background();
                startup_server.set_discovery_listener(startup_peers.discovery_listener());
            });
            app.manage(Arc::clone(&vault) as SharedVault);
            app.manage(server as SharedServer);
            app.manage(peers as SharedPeers);
            setup_tray(app);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let tray_available = window
                    .app_handle()
                    .try_state::<TrayAvailable>()
                    .is_some_and(|state| state.0);
                if tray_available {
                    api.prevent_close();
                    if let Err(error) = window.hide() {
                        log::warn!("failed to hide the main window: {error}");
                    }
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::vault_status,
            commands::initialize_vault,
            commands::reload_vault_key,
            commands::reset_vault,
            commands::lock_vault,
            commands::quit_app,
            commands::admin_login,
            commands::admin_session,
            commands::admin_logout,
            commands::change_admin_password,
            commands::server_status,
            commands::start_server,
            commands::stop_server,
            commands::resume_server,
            commands::list_shares,
            commands::create_share,
            commands::update_share_permissions,
            commands::set_share_rules,
            commands::delete_share,
            commands::list_pending_requests,
            commands::approve_pending_request,
            commands::list_authorizations,
            commands::revoke_authorization,
            commands::scan_lan,
            commands::discovered_peers,
            commands::local_networks,
            commands::list_peers,
            commands::request_peer_access,
            commands::wait_for_peer_access,
            commands::cancel_peer_access,
            commands::connect_peer,
            commands::check_peer_health,
            commands::forget_peer,
            commands::sync_peers,
            commands::list_peer_shares,
            commands::list_peer_dir,
            commands::delete_peer_entry,
            commands::download_peer_file,
            commands::download_peer_file_to,
            commands::prepare_peer_directory_download,
            commands::open_local_file,
            commands::reveal_local_file,
            commands::collect_upload_paths,
            commands::upload_peer_file,
            commands::preview_peer_file,
            commands::prepare_peer_external_edit,
            commands::commit_peer_external_edit,
            commands::discard_peer_external_edit,
        ])
        .build(tauri::generate_context!());

    match application {
        Ok(app) => app.run(|app_handle, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(server) = app_handle.try_state::<SharedServer>() {
                    server.request_shutdown();
                }
                if let Some(vault) = app_handle.try_state::<SharedVault>()
                    && let Err(error) = vault.lock()
                {
                    log::error!(
                        "failed to lock vault during shutdown: {}",
                        error_chain(&error)
                    );
                }
                if let Some(peers) = app_handle.try_state::<SharedPeers>() {
                    peers.clear_temp_now();
                }
            }
        }),
        Err(error) => log::error!("failed to start peerdav: {error}"),
    }
}

fn setup_tray(app: &tauri::App) {
    match build_tray(app) {
        Ok(()) => {
            app.manage(TrayAvailable(true));
        }
        Err(error) => {
            log::warn!("system tray is unavailable: {error}");
            app.manage(TrayAvailable(false));
        }
    }
}

fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let icon = app
        .default_window_icon()
        .ok_or("default window icon is unavailable")?;
    let show_item = MenuItem::with_id(
        app,
        TRAY_SHOW_MENU_ID,
        "显示 PeerDav / Show PeerDav",
        true,
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(
        app,
        TRAY_QUIT_MENU_ID,
        "退出 PeerDav / Quit PeerDav",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon.clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            if id == TRAY_SHOW_MENU_ID {
                show_main_window(app);
            } else if id == TRAY_QUIT_MENU_ID {
                request_quit(app);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        if let Err(error) = window.show() {
            log::warn!("failed to show the main window: {error}");
        }
        if let Err(error) = window.set_focus() {
            log::warn!("failed to focus the main window: {error}");
        }
    }
}

fn request_quit(app: &tauri::AppHandle) {
    show_main_window(app);
    if let Err(error) = app.emit(QUIT_REQUEST_EVENT, ()) {
        log::error!("failed to emit the quit request: {error}");
    }
}
