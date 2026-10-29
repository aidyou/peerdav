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
#[cfg(test)]
mod machine;
pub mod paths;
mod peer;
mod policy;
mod server;
mod tls;
mod traits;
mod transfer;
mod types;
mod updater;
mod upload;
mod vault;
mod window;

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
    InitializeVaultResult, IgnoreOptions, MACHINE_ID_HEADER, MAX_ACCESS_GRANTS, MAX_ACCESS_REQUEST_ID_CHARS,
    MAX_PENDING_ATTEMPTS, MAX_PENDING_PATH_CHARS, MAX_PENDING_REQUESTS,
    MIN_ACCESS_REQUEST_ID_CHARS, OBSERVED_PEER_HEADER, OutboundAccessRequest, OutboundAccessStatus,
    PEER_ACCESS_POLL_INTERVAL, PEER_ACCESS_WAIT, PeerAccessRequest, PeerEntry, PeerHealth,
    PeerInfo, PeerListing, PeerRevocationInfo, PeerShare, PeerStatus, PendingRequestInfo,
    PreviewPayload, RuleKind, RuleMatcher, ServerStatus, ShareInfo, SharePermissions,
    TransferResult, UploadPhase, VaultStatus,
};
pub use vault::Vault;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};

use crate::commands::{AppEvents, SharedPeers, SharedServer, SharedVault};
use crate::error::error_chain;
use crate::types::{
    DB_FILE_NAME, MAIN_WINDOW_LABEL, QUIT_REQUEST_EVENT, TRAY_ICON_STYLE_META, TRAY_ID,
    TRAY_QUIT_MENU_ID, TRAY_SHOW_MENU_ID, TrayIconStyle,
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
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
            let events = Arc::new(AppEvents::new(
                app.handle().clone(),
                vault.fingerprint().to_owned(),
            ));
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
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                window::restore(&window, &vault)?;
            }
            app.manage(Arc::clone(&vault) as SharedVault);
            app.manage(server as SharedServer);
            app.manage(peers as SharedPeers);
            app.manage(updater::UpdateManager::new(
                app.package_info().version.to_string(),
                app.config().plugins.0.get("updater"),
            ));
            setup_tray(app);
            Ok(())
        })
        .on_page_load(|webview, payload| {
            log::info!(
                "webview {} page load {:?}",
                webview.label(),
                payload.event()
            );
        })
        .on_window_event(|window, event| {
            if window.label() == MAIN_WINDOW_LABEL
                && matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_))
            {
                window::save(window);
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let tray_available = window
                    .app_handle()
                    .try_state::<TrayAvailable>()
                    .is_some_and(|state| state.0);
                if tray_available {
                    api.prevent_close();
                    if let Err(error) = window.hide() {
                        log::warn!("failed to hide the main window: {error}");
                    } else {
                        if let Err(error) = window.set_skip_taskbar(true) {
                            log::warn!("failed to hide taskbar entry: {error}");
                        }
                        #[cfg(target_os = "macos")]
                        if let Err(error) = window
                            .app_handle()
                            .set_activation_policy(tauri::ActivationPolicy::Accessory)
                        {
                            log::warn!("failed to hide Dock icon: {error}");
                        }
                    }
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(|invoke| {
            let command = invoke.message.command();
            let allowed_when_locked = matches!(
                command,
                "vault_status"
                    | "admin_session"
                    | "lock_interface"
                    | "unlock_interface"
                    | "quit_app"
                    | "update_status"
                    | "check_for_updates"
            );
            if !allowed_when_locked {
                let vault = invoke
                    .message
                    .webview()
                    .state::<SharedVault>()
                    .inner()
                    .clone();
                if let Err(error) = vault.require_interface_unlocked() {
                    invoke.resolver.reject(AppError::from(error));
                    return true;
                }
            }
            let handler: Box<dyn Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool> =
                Box::new(tauri::generate_handler![
                    updater::manager::update_status,
                    updater::manager::check_for_updates,
                    updater::manager::install_update,
                    commands::vault_status,
                    commands::initialize_vault,
                    commands::reload_vault_key,
                    commands::reset_vault,
                    commands::lock_vault,
                    commands::quit_app,
                    commands::admin_login,
                    commands::admin_session,
                    commands::lock_interface,
                    commands::configure_shortcuts,
                    commands::refresh_tray_menu,
                    commands::unlock_interface,
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
                    commands::scan_lan_tcp,
                    commands::service_settings,
                    commands::set_tcp_discovery,
                    commands::tray_icon_style,
                    commands::set_tray_icon_style,
                    commands::discovered_peers,
                    commands::local_networks,
                    commands::local_addresses,
                    commands::list_peers,
                    commands::request_peer_access,
                    commands::device_fingerprint,
                    commands::inbound_connections,
                    commands::outbound_connections,
                    commands::wait_for_peer_access,
                    commands::cancel_peer_access,
                    commands::access_requests,
                    commands::connect_peer,
                    commands::check_peer_health,
                    commands::forget_peer,
                    commands::remove_local_peer,
                    commands::sync_peers,
                    commands::list_peer_shares,
                    commands::list_peer_dir,
                    commands::delete_peer_entry,
                    commands::download_peer_file,
                    commands::download_peer_file_to,
                    commands::prepare_peer_directory_download,
                    commands::open_local_file,
                    commands::reveal_local_file,
                    commands::reveal_local_directory,
                    commands::collect_upload_paths,
                    commands::begin_peer_upload,
                    commands::cancel_peer_upload,
                    commands::upload_peer_file,
                    commands::preview_peer_file,
                    commands::prepare_peer_external_edit,
                    commands::commit_peer_external_edit,
                    commands::discard_peer_external_edit,
                ]);
            handler(invoke)
        })
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

pub(crate) fn tray_display_name(locale: &str) -> &'static str {
    if locale == "en" { "PeerDav" } else { "邻盘" }
}

pub(crate) fn tray_menu_labels(locale: &str) -> (&'static str, &'static str, &'static str) {
    if locale == "en" {
        ("Home", "Settings", "Quit")
    } else {
        ("首页", "设置", "退出")
    }
}

fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let language = app
        .state::<SharedVault>()
        .get_setting("ui.locale")?
        .unwrap_or_else(|| "zh-CN".into());
    let (home, settings, quit) = tray_menu_labels(&language);
    let style = load_tray_icon_style(app);
    let icon = tray_icon_image(style)?;
    let show_item = MenuItem::with_id(app, TRAY_SHOW_MENU_ID, home, true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, TRAY_QUIT_MENU_ID, quit, true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "tray-settings", settings, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &settings_item, &quit_item])?;

    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .title(tray_display_name(&language))
        .tooltip(tray_display_name(&language))
        .icon(icon.clone())
        .show_menu_on_left_click(false);
    #[cfg(target_os = "linux")]
    let builder = builder.menu(&menu);
    #[cfg(not(target_os = "linux"))]
    app.manage(std::sync::Mutex::new(menu));
    let tray = builder
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            if id == TRAY_SHOW_MENU_ID || id == "tray-settings" || id.starts_with("tray-peer-") {
                show_main_window(app);
                let route = if id == TRAY_SHOW_MENU_ID {
                    "home".to_owned()
                } else if id == "tray-settings" {
                    "settings".to_owned()
                } else {
                    format!("peer/{}", id.trim_start_matches("tray-peer-"))
                };
                if let Some(vault) = app.try_state::<SharedVault>()
                    && vault.require_interface_unlocked().is_err()
                {
                    return;
                }
                if let Err(error) = app.emit("app://navigate", route) {
                    log::warn!("failed to navigate from tray: {error}");
                }
            } else if id == TRAY_QUIT_MENU_ID {
                request_quit(app);
            }
        })
        .on_tray_icon_event(|tray, event| {
            #[cfg(not(target_os = "linux"))]
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = &event
            {
                if let Some(window) = tray.app_handle().get_webview_window(MAIN_WINDOW_LABEL)
                    && let Some(menu) = tray
                        .app_handle()
                        .try_state::<std::sync::Mutex<Menu<tauri::Wry>>>()
                {
                    match menu.lock() {
                        Ok(menu) => {
                            if let Err(error) = window.popup_menu_at(&*menu, *position) {
                                log::warn!("failed to show tray menu: {error}");
                            }
                        }
                        Err(error) => log::error!("tray menu lock poisoned: {error}"),
                    }
                }
            }
            if let TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    // 单色样式走 macOS 模板图标，系统按 alpha 形状适配深浅色外观；
    // 蓝色与双色是固定颜色，不参与模板反色。
    #[cfg(target_os = "macos")]
    let tray = tray.icon_as_template(matches!(style, TrayIconStyle::Mono));

    tray.build(app)?;
    Ok(())
}

/// 托盘图标按样式选择对应内置 PNG：单色、蓝色、双色。
fn tray_icon_image(style: TrayIconStyle) -> Result<tauri::image::Image<'static>, tauri::Error> {
    let bytes: &'static [u8] = match style {
        TrayIconStyle::Mono => include_bytes!("../icons/tray-icon.png"),
        TrayIconStyle::Blue => include_bytes!("../icons/tray-icon-blue.png"),
        TrayIconStyle::Duo => include_bytes!("../icons/tray-icon-duo.png"),
    };
    tauri::image::Image::from_bytes(bytes)
}

fn load_tray_icon_style(app: &tauri::App) -> TrayIconStyle {
    app.try_state::<SharedVault>()
        .and_then(|vault| vault.get_setting(TRAY_ICON_STYLE_META).ok().flatten())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// 运行时切换托盘图标。托盘不可用时静默跳过，不影响设置保存。
pub(crate) fn apply_tray_icon_style(
    app: &tauri::AppHandle,
    style: TrayIconStyle,
) -> Result<(), DomainError> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let icon = tray_icon_image(style).map_err(|error| DomainError::Internal {
        detail: error.to_string(),
    })?;
    tray.set_icon(Some(icon))
        .map_err(|error| DomainError::Internal {
            detail: error.to_string(),
        })?;
    #[cfg(target_os = "macos")]
    tray.set_icon_as_template(matches!(style, TrayIconStyle::Mono))
        .map_err(|error| DomainError::Internal {
            detail: error.to_string(),
        })?;
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    if let Err(error) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        log::warn!("failed to restore Dock icon: {error}");
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        if let Err(error) = window.set_skip_taskbar(false) {
            log::warn!("failed to restore taskbar entry: {error}");
        }
        if let Err(error) = window.unminimize() {
            log::warn!("failed to restore the main window: {error}");
        }
        if let Err(error) = window.show() {
            log::warn!("failed to show the main window: {error}");
        }
        if let Err(error) = window.set_focus() {
            log::warn!("failed to focus the main window: {error}");
        }
    }
}

#[cfg(test)]
mod tray_tests {
    #[test]
    fn menu_labels_use_one_language() {
        assert_eq!(super::tray_display_name("zh-CN"), "邻盘");
        assert_eq!(super::tray_display_name("en"), "PeerDav");
        assert_eq!(super::tray_menu_labels("en"), ("Home", "Settings", "Quit"));
        assert_eq!(super::tray_menu_labels("zh-CN"), ("首页", "设置", "退出"));
    }

    #[test]
    fn all_embedded_icons_decode() {
        for style in [
            super::TrayIconStyle::Mono,
            super::TrayIconStyle::Blue,
            super::TrayIconStyle::Duo,
        ] {
            let image = super::tray_icon_image(style).expect("embedded PNG");
            assert!(image.width() > 0 && image.height() > 0);
        }
    }
}

fn request_quit(app: &tauri::AppHandle) {
    show_main_window(app);
    if let Err(error) = app.emit(QUIT_REQUEST_EVENT, ()) {
        log::error!("failed to emit the quit request: {error}");
    }
}
