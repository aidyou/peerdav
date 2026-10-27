//! LAN WebDAV server: request authentication/policy gate, pending approval
//! flow, share catalog, task lifecycle, and the background error outlet.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use base64::Engine as _;
use dav_server::DavHandler;
use dav_server::body::Body;
use dav_server::davpath::DavPath;
use dav_server::fakels::FakeLs;
#[cfg(test)]
use http::header::AUTHORIZATION;
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, WWW_AUTHENTICATE};
use http::{HeaderMap, Method, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use crate::discovery::{self, DiscoveryListener, DiscoveryServer, LocalAdvert};
use crate::error::{AppError, DomainError};
use crate::fs::split_share_path;
use crate::fs::{RequestCredentials, RestrictedFs, ShareConfig, ShareMap, ShareRoot};
use crate::policy::{
    CompiledRule, PeerContext, PolicyDecision, canonical_ip, compile_rule, evaluate,
    required_for_method,
};
use crate::traits::ServerEvents;
use crate::types::{
    ACCESS_GRANT_TTL, ACCESS_PROTOCOL, ACCESS_REQUEST_HEADER, ACCESS_STATUS_PATH,
    AUTHORIZATION_PATH, AUTHORIZATION_PROTOCOL, AccessRequestStatus, AccessRule,
    AccessStatusResponse, ApprovePendingResult, AuthorizationBindingResponse, AuthorizationInfo,
    AuthorizationStatus, AuthorizationStatusResponse, CATALOG_PATH, CATALOG_PROTOCOL,
    CLIENT_ID_HEADER, CatalogResponse, DEFAULT_SERVER_PORT, MACHINE_ID_HEADER, MAX_ACCESS_GRANTS,
    MAX_ACCESS_REQUEST_ID_CHARS, MAX_CLIENT_ID_CHARS, MAX_PENDING_ATTEMPTS, MAX_PENDING_PATH_CHARS,
    MAX_PENDING_REQUESTS, MIN_ACCESS_REQUEST_ID_CHARS, MIN_CLIENT_ID_CHARS, PeerShare,
    PendingRequestInfo, ServerStatus, ShareInfo, SharePermissions,
};
use crate::vault::Vault;

const AUTH_CHALLENGE: &str = "Basic realm=\"PeerDav\", charset=\"UTF-8\"";
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_AGENT: &str = "PeerDav";

/// Publishes a structured error through the shared background outlet.
fn publish_error(events: &dyn ServerEvents, error: &DomainError) {
    events.error(&AppError::from_ref(error));
}

/// Configuration for one server start.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_address: String,
    pub port: u16,
    /// `0` disables UDP discovery (used by tests and headless setups).
    pub discovery_port: u16,
    pub server_name: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0".to_owned(),
            port: DEFAULT_SERVER_PORT,
            discovery_port: 0,
            server_name: SERVER_AGENT.to_owned(),
        }
    }
}

struct AuthEntry {
    info: AuthorizationInfo,
    client_id: Option<String>,
    revoked: bool,
}

/// An approved code waiting to be claimed by the requesting client.
struct PendingGrant {
    client_request_id: String,
    peer_ip: String,
    /// 申请方机器码；有值时按机器码认领，IP 变化不影响领取授权码。
    machine_id: Option<String>,
    authorization_id: String,
    code: String,
    expires_at: Instant,
}

struct RunningServer {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    discovery: Option<DiscoveryBinding>,
}

/// Discovery can either be a standalone responder owned by this server or the
/// application-wide listener shared with the client. The shared listener keeps
/// a single UDP socket on the discovery port and also publishes the passive
/// discovery list.
enum DiscoveryBinding {
    Standalone(DiscoveryServer),
    Shared(Arc<DiscoveryListener>),
}

impl DiscoveryBinding {
    fn port(&self) -> u16 {
        match self {
            Self::Standalone(server) => server.port(),
            Self::Shared(listener) => listener.port(),
        }
    }

    async fn stop(self) {
        match self {
            Self::Standalone(server) => server.stop().await,
            Self::Shared(listener) => listener.clear_advert(),
        }
    }

    fn request_shutdown(&mut self) {
        match self {
            Self::Standalone(server) => server.request_shutdown(),
            Self::Shared(listener) => listener.clear_advert(),
        }
    }
}

struct Gate {
    uploads: crate::types::UploadSessions,
    vault: Arc<Vault>,
    shares: Arc<std::sync::RwLock<ShareMap>>,
    authorizations: Arc<std::sync::RwLock<Vec<AuthEntry>>>,
    pending: Arc<Mutex<Vec<PendingRequestInfo>>>,
    grants: Arc<Mutex<Vec<PendingGrant>>>,
    events: Arc<dyn ServerEvents>,
    server_name: String,
    /// 本机机器码：随目录响应发给客户端，客户端据此把「已保存的对端」和本机
    /// 开出的入站授权对上。取不到时为 None，此时只能按 IP 对应。
    machine_id: Option<String>,
    connections: Arc<Mutex<HashMap<String, crate::types::ConnectionInfo>>>,
}

/// Owns the share/authorization catalog and the running server task.
pub struct ServerController {
    vault: Arc<Vault>,
    events: Arc<dyn ServerEvents>,
    shares: Arc<std::sync::RwLock<ShareMap>>,
    authorizations: Arc<std::sync::RwLock<Vec<AuthEntry>>>,
    pending: Arc<Mutex<Vec<PendingRequestInfo>>>,
    grants: Arc<Mutex<Vec<PendingGrant>>>,
    discovery_listener: Mutex<Option<Arc<DiscoveryListener>>>,
    lifecycle: Mutex<Option<RunningServer>>,
    server_id: String,
    connections: Arc<Mutex<HashMap<String, crate::types::ConnectionInfo>>>,
}

impl ServerController {
    /// Loads persisted shares and active authorizations. A single broken
    /// share is reported through the error outlet and skipped so the rest of
    /// the service stays available.
    pub fn new(vault: Arc<Vault>, events: Arc<dyn ServerEvents>) -> Result<Self, DomainError> {
        let mut shares = ShareMap::new();
        for record in vault.load_shares()? {
            match load_share(record) {
                Ok(share) => {
                    shares.insert(share.id.clone(), Arc::new(share));
                }
                Err(error) => publish_error(events.as_ref(), &error),
            }
        }

        let mut authorizations = Vec::new();
        for record in vault.list_authorization_records()? {
            match load_authorization(&record) {
                Ok(entry) => authorizations.push(entry),
                Err(error) => publish_error(events.as_ref(), &error),
            }
        }

        Ok(Self {
            server_id: vault.identity().fingerprint.clone(),
            vault,
            events,
            shares: Arc::new(std::sync::RwLock::new(shares)),
            authorizations: Arc::new(std::sync::RwLock::new(authorizations)),
            pending: Arc::new(Mutex::new(Vec::new())),
            grants: Arc::new(Mutex::new(Vec::new())),
            discovery_listener: Mutex::new(None),
            lifecycle: Mutex::new(None),
            connections: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Shares the application-wide passive discovery listener with the server
    /// so the server does not bind a second socket to the discovery port.
    pub(crate) fn set_discovery_listener(&self, listener: Option<Arc<DiscoveryListener>>) {
        match self.discovery_listener.lock() {
            Ok(mut slot) => *slot = listener,
            Err(_) => log::error!("server discovery listener mutex poisoned"),
        }
    }

    pub fn inbound_connections(&self) -> Result<Vec<crate::types::ConnectionInfo>, DomainError> {
        use crate::types::{ConnectionInfo, ConnectionState};
        let authorizations = self.list_authorizations();
        let pending = self.list_pending();
        let mut registry = self.connections.lock().map_err(|_| DomainError::Internal {
            detail: "inbound registry lock poisoned".into(),
        })?;
        for row in registry.values_mut() {
            row.authorized = false;
            row.state = if row.active_connections > 0 {
                ConnectionState::Connected
            } else {
                ConnectionState::Disconnected
            };
        }
        for authorization in authorizations {
            if let Some(fingerprint) = authorization.machine_id {
                let row = registry
                    .entry(fingerprint.clone())
                    .or_insert(ConnectionInfo {
                        fingerprint,
                        name: authorization.label.clone(),
                        address: String::new(),
                        state: ConnectionState::Approved,
                        authorized: true,
                        active_connections: 0,
                        last_seen: authorization.created_at,
                    });
                row.name = authorization.label;
                row.authorized = true;
                if row.active_connections > 0 {
                    row.state = ConnectionState::Connected;
                } else if row.state != ConnectionState::Disconnected {
                    row.state = ConnectionState::Approved;
                }
            }
        }
        for pending in pending {
            if let Some(fingerprint) = pending.machine_id {
                let row = registry
                    .entry(fingerprint.clone())
                    .or_insert(ConnectionInfo {
                        fingerprint,
                        name: pending.share_name,
                        address: pending.peer_ip.clone(),
                        state: ConnectionState::Pending,
                        authorized: false,
                        active_connections: 0,
                        last_seen: pending.last_seen,
                    });
                row.state = ConnectionState::Pending;
                row.address = pending.peer_ip;
                row.last_seen = pending.last_seen;
            }
        }
        let mut rows: Vec<_> = registry.values().cloned().collect();
        rows.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        Ok(rows)
    }

    pub fn status(&self) -> ServerStatus {
        let (running, bind_address, discovery_running, discovery_port) = match self.lifecycle.lock()
        {
            Ok(guard) => match guard.as_ref() {
                Some(server) => (
                    true,
                    Some(server.address.to_string()),
                    server.discovery.is_some(),
                    server.discovery.as_ref().map(DiscoveryBinding::port),
                ),
                None => (false, None, false, None),
            },
            Err(_) => {
                log::error!("server lifecycle mutex poisoned while reading status");
                (false, None, false, None)
            }
        };
        ServerStatus {
            running,
            bind_address,
            discovery_running,
            discovery_port,
            share_count: self.shares.read().map(|shares| shares.len()).unwrap_or(0),
            pending_count: self
                .pending
                .lock()
                .map(|pending| pending.len())
                .unwrap_or(0),
        }
    }

    /// Starts the server. Requires an already unlocked vault only through the
    /// command guard; the controller itself has no auth state.
    pub async fn start(&self, config: ServerConfig) -> Result<ServerStatus, DomainError> {
        {
            let guard = self.lifecycle.lock().map_err(|_| DomainError::Internal {
                detail: "server lifecycle mutex poisoned".to_owned(),
            })?;
            if guard.is_some() {
                return Err(DomainError::ServerAlreadyRunning);
            }
        }

        let address = format!("{}:{}", config.bind_address.trim(), config.port);
        let parsed = address
            .parse::<SocketAddr>()
            .map_err(|error| DomainError::ServerBind {
                address: address.clone(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string()),
            })?;
        let listener =
            TcpListener::bind(parsed)
                .await
                .map_err(|source| DomainError::ServerBind {
                    address: address.clone(),
                    source,
                })?;
        let bound = listener
            .local_addr()
            .map_err(|source| DomainError::ServerBind {
                address: address.clone(),
                source,
            })?;

        let handler: DavHandler<RequestCredentials> = DavHandler::builder()
            .filesystem(Box::new(RestrictedFs::new(Arc::clone(&self.shares))))
            .locksystem(FakeLs::new())
            .hide_symlinks(true)
            .build_handler();

        let gate = Arc::new(Gate {
            uploads: tokio::sync::Mutex::new(HashMap::new()),
            vault: Arc::clone(&self.vault),
            shares: Arc::clone(&self.shares),
            authorizations: Arc::clone(&self.authorizations),
            pending: Arc::clone(&self.pending),
            grants: Arc::clone(&self.grants),
            events: Arc::clone(&self.events),
            server_name: config.server_name.clone(),
            machine_id: Some(self.vault.identity().fingerprint.clone()),
            connections: Arc::clone(&self.connections),
        });

        let shared_listener = match self.discovery_listener.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => {
                log::error!("server discovery listener mutex poisoned");
                None
            }
        };
        let mut discovery = if config.discovery_port == 0 {
            if let Some(listener) = shared_listener {
                listener.clear_advert();
            }
            None
        } else {
            let hosts = discovery::advertised_hosts(&config.bind_address);
            if let Some(listener) = shared_listener {
                listener.set_advert(Some(LocalAdvert {
                    server_id: self.server_id.clone(),
                    name: config.server_name.clone(),
                    port: bound.port(),
                    hosts,
                    shares: Arc::clone(&self.shares),
                }));
                Some(DiscoveryBinding::Shared(listener))
            } else {
                match discovery::start(
                    config.discovery_port,
                    self.server_id.clone(),
                    config.server_name.clone(),
                    bound.port(),
                    hosts,
                    Arc::clone(&self.shares),
                )
                .await
                {
                    Ok(discovery) => Some(DiscoveryBinding::Standalone(discovery)),
                    Err(error) => {
                        publish_error(self.events.as_ref(), &error);
                        None
                    }
                }
            }
        };

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run_server(listener, handler, gate, shutdown_rx));

        let mut guard = self.lifecycle.lock().map_err(|_| DomainError::Internal {
            detail: "server lifecycle mutex poisoned".to_owned(),
        })?;
        if guard.is_some() {
            if let Err(error) = shutdown_tx.send(()) {
                log::debug!("failed to stop racing server start: {error:?}");
            }
            if let Some(mut discovery) = discovery.take() {
                discovery.request_shutdown();
            }
            return Err(DomainError::ServerAlreadyRunning);
        }
        *guard = Some(RunningServer {
            address: bound,
            shutdown: Some(shutdown_tx),
            task,
            discovery,
        });
        drop(guard);

        Ok(self.status())
    }

    pub async fn stop(&self) -> ServerStatus {
        let running = {
            match self.lifecycle.lock() {
                Ok(mut guard) => guard.take(),
                Err(_) => {
                    publish_error(
                        self.events.as_ref(),
                        &DomainError::Internal {
                            detail: "server lifecycle mutex poisoned".to_owned(),
                        },
                    );
                    None
                }
            }
        };

        if let Some(RunningServer {
            shutdown,
            mut task,
            discovery,
            ..
        }) = running
        {
            if let Some(discovery) = discovery {
                discovery.stop().await;
            }
            if let Some(sender) = shutdown
                && let Err(error) = sender.send(())
            {
                log::debug!("server already stopped before shutdown signal: {error:?}");
            }

            match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if !error.is_cancelled() {
                        publish_error(
                            self.events.as_ref(),
                            &DomainError::ServerTask {
                                detail: error.to_string(),
                            },
                        );
                    }
                }
                Err(_) => {
                    log::warn!("server did not stop within the timeout; aborting task");
                    task.abort();
                    if let Err(error) = task.await
                        && !error.is_cancelled()
                    {
                        publish_error(
                            self.events.as_ref(),
                            &DomainError::ServerTask {
                                detail: error.to_string(),
                            },
                        );
                    }
                }
            }
        }

        // 服务停了就不该再有人「待审批」：清掉队列，免得控台继续显示已经没人在等的
        // 申请，甚至被误批准出一条谁也领不到的授权。
        self.clear_pending_state();

        self.status()
    }

    /// 清空待审批队列与待领取的授权码。两者都只活在内存里，服务停止后必然过期。
    fn clear_pending_state(&self) {
        match self.pending.lock() {
            Ok(mut pending) => pending.clear(),
            Err(_) => log::warn!("pending request lock poisoned while stopping the server"),
        }
        match self.grants.lock() {
            Ok(mut grants) => grants.clear(),
            Err(_) => log::warn!("access grant lock poisoned while stopping the server"),
        }
    }

    /// Best-effort synchronous shutdown for application exit.
    pub fn request_shutdown(&self) {
        let running = match self.lifecycle.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => {
                log::error!("server lifecycle mutex poisoned during shutdown");
                None
            }
        };
        if let Some(mut server) = running {
            if let Some(mut discovery) = server.discovery.take() {
                discovery.request_shutdown();
            }
            if let Some(sender) = server.shutdown.take()
                && let Err(error) = sender.send(())
            {
                log::debug!("server shutdown signal was not delivered: {error:?}");
            }
        }
    }

    pub fn list_shares(&self) -> Vec<ShareInfo> {
        match self.shares.read() {
            Ok(shares) => {
                let mut list: Vec<ShareInfo> = shares.values().map(|share| share.info()).collect();
                list.sort_by(|left, right| left.name.cmp(&right.name));
                list
            }
            Err(_) => {
                publish_error(
                    self.events.as_ref(),
                    &DomainError::Internal {
                        detail: "share catalog lock poisoned".to_owned(),
                    },
                );
                Vec::new()
            }
        }
    }

    pub fn list_authorizations(&self) -> Vec<AuthorizationInfo> {
        match self.authorizations.read() {
            Ok(authorizations) => authorizations
                .iter()
                .filter(|entry| !entry.revoked)
                .map(|entry| entry.info.clone())
                .collect(),
            Err(_) => {
                publish_error(
                    self.events.as_ref(),
                    &DomainError::Internal {
                        detail: "authorization catalog lock poisoned".to_owned(),
                    },
                );
                Vec::new()
            }
        }
    }

    pub fn list_pending(&self) -> Vec<PendingRequestInfo> {
        match self.pending.lock() {
            Ok(pending) => pending.clone(),
            Err(_) => {
                publish_error(
                    self.events.as_ref(),
                    &DomainError::Internal {
                        detail: "pending request lock poisoned".to_owned(),
                    },
                );
                Vec::new()
            }
        }
    }

    pub fn create_share(
        &self,
        name: &str,
        root_path: &str,
        permissions: SharePermissions,
        rules: &[AccessRule],
    ) -> Result<ShareInfo, DomainError> {
        validate_share_name(name)?;
        if root_path.trim().is_empty() {
            return Err(DomainError::InvalidShareConfig {
                detail: "root path must not be empty".to_owned(),
            });
        }

        let normalized = crate::policy::normalize_rules(rules)?;
        let compiled = compile_rules(&normalized)?;
        let root = Arc::new(ShareRoot::open(std::path::Path::new(root_path))?);
        let id = random_hex(8);
        let record_rules = normalized
            .iter()
            .map(|rule| crate::types::ShareRuleRecord {
                kind: rule.kind,
                matcher: rule.matcher.clone(),
            })
            .collect::<Vec<_>>();

        self.vault
            .create_share(&id, name, root_path.trim(), permissions, &record_rules)?;

        let share = ShareConfig {
            id: id.clone(),
            name: name.to_owned(),
            root_path: root_path.trim().into(),
            permissions,
            rules: compiled,
            display_rules: normalized,
            root,
        };
        let info = share.info();
        let mut shares = self.shares.write().map_err(|_| DomainError::Internal {
            detail: "share catalog lock poisoned".to_owned(),
        })?;
        shares.insert(id, Arc::new(share));
        Ok(info)
    }

    pub fn update_share_permissions(
        &self,
        share_id: &str,
        permissions: SharePermissions,
    ) -> Result<ShareInfo, DomainError> {
        let mut shares = self.shares.write().map_err(|_| DomainError::Internal {
            detail: "share catalog lock poisoned".to_owned(),
        })?;
        let share = shares
            .get_mut(share_id)
            .ok_or_else(|| DomainError::ShareNotFound {
                id: share_id.to_owned(),
            })?;
        self.vault.update_share_permissions(share_id, permissions)?;
        if let Some(existing) = Arc::get_mut(share) {
            existing.permissions = permissions;
        } else {
            let mut updated = (**share).clone();
            updated.permissions = permissions;
            *share = Arc::new(updated);
        }
        Ok(share.info())
    }

    pub fn set_share_rules(
        &self,
        share_id: &str,
        rules: &[AccessRule],
    ) -> Result<ShareInfo, DomainError> {
        let normalized = crate::policy::normalize_rules(rules)?;
        let compiled = compile_rules(&normalized)?;
        let record_rules = normalized
            .iter()
            .map(|rule| crate::types::ShareRuleRecord {
                kind: rule.kind,
                matcher: rule.matcher.clone(),
            })
            .collect::<Vec<_>>();
        let mut shares = self.shares.write().map_err(|_| DomainError::Internal {
            detail: "share catalog lock poisoned".to_owned(),
        })?;
        let share = shares
            .get_mut(share_id)
            .ok_or_else(|| DomainError::ShareNotFound {
                id: share_id.to_owned(),
            })?;
        self.vault.set_share_rules(share_id, &record_rules)?;
        let mut updated = (**share).clone();
        updated.rules = compiled;
        updated.display_rules = normalized;
        *share = Arc::new(updated);
        Ok(share.info())
    }

    pub fn delete_share(&self, share_id: &str) -> Result<(), DomainError> {
        self.vault.delete_share(share_id)?;
        match self.shares.write() {
            Ok(mut shares) => {
                shares.remove(share_id);
            }
            Err(_) => publish_error(
                self.events.as_ref(),
                &DomainError::Internal {
                    detail: "share catalog lock poisoned after delete".to_owned(),
                },
            ),
        }
        match self.pending.lock() {
            Ok(mut pending) => {
                pending.retain(|request| request.share_id != share_id);
            }
            Err(_) => publish_error(
                self.events.as_ref(),
                &DomainError::Internal {
                    detail: "pending request lock poisoned after delete".to_owned(),
                },
            ),
        }
        Ok(())
    }

    /// Approves a pending client and returns the plaintext code exactly once.
    pub fn approve_pending(&self, request_id: &str) -> Result<ApprovePendingResult, DomainError> {
        let mut pending = self.pending.lock().map_err(|_| DomainError::Internal {
            detail: "pending request lock poisoned".to_owned(),
        })?;
        let position = pending
            .iter()
            .position(|request| request.id == request_id)
            .ok_or_else(|| DomainError::PendingRequestNotFound {
                id: request_id.to_owned(),
            })?;
        let request = pending[position].clone();

        let code = random_hex(32);
        let hash = hash_code(&code);
        // 标签以机器码开头：IP 只是当时的地址，识别设备靠机器码。
        let label = match request.machine_id.as_deref() {
            Some(machine_id) => format!(
                "{} · {} ({})",
                crate::tls::short_fingerprint(machine_id),
                request.peer_ip,
                request.share_name
            ),
            None => format!("{} ({})", request.peer_ip, request.share_name),
        };
        let authorization_id = random_hex(8);
        let record = self.vault.add_authorization(
            &authorization_id,
            &label,
            &hex::encode(hash),
            request.machine_id.as_deref(),
        )?;

        let mut authorizations =
            self.authorizations
                .write()
                .map_err(|_| DomainError::Internal {
                    detail: "authorization catalog lock poisoned".to_owned(),
                })?;
        authorizations.push(AuthEntry {
            info: AuthorizationInfo {
                id: record.id,
                label: record.label,
                machine_id: record.machine_id,
                created_at: record.created_at,
            },
            client_id: None,
            revoked: false,
        });
        pending.remove(position);

        let info = authorizations
            .last()
            .map(|entry| entry.info.clone())
            .ok_or_else(|| DomainError::Internal {
                detail: "authorization was not inserted".to_owned(),
            })?;
        drop(authorizations);

        let auto_delivery = match request.client_request_id.as_deref() {
            Some(client_request_id) => self.queue_grant(
                client_request_id,
                &request.peer_ip,
                request.machine_id.as_deref(),
                &info.id,
                &code,
            ),
            None => false,
        };
        Ok(ApprovePendingResult {
            authorization_code: code,
            authorization: info,
            auto_delivery,
        })
    }

    pub fn revoke_authorization(&self, authorization_id: &str) -> Result<(), DomainError> {
        revoke_authorization_state(
            self.vault.as_ref(),
            &self.authorizations,
            &self.grants,
            self.events.as_ref(),
            authorization_id,
        )
    }

    /// 撤销某台设备在本机上的全部入站授权，返回撤销的条数。
    ///
    /// 解绑一台已保存的对端设备时调用：两台设备互相授权过，只删掉本机的出站记录
    /// 会让对方还留在「其他设备访问此设备」里，看起来像幽灵设备。明确的解绑意味着
    /// 整段互信关系结束，而不只是本机不再访问对方。
    pub fn revoke_authorizations_for_machine(
        &self,
        machine_id: &str,
    ) -> Result<usize, DomainError> {
        let ids = self.vault.authorization_ids_for_machine(machine_id)?;
        let mut revoked = 0;
        for id in ids {
            match self.revoke_authorization(&id) {
                Ok(()) => revoked += 1,
                Err(DomainError::AuthorizationNotFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(revoked)
    }

    /// Queues the approved code for the client identified by its request key.
    /// The code is removed as soon as the client claims it.
    fn queue_grant(
        &self,
        client_request_id: &str,
        peer_ip: &str,
        machine_id: Option<&str>,
        authorization_id: &str,
        code: &str,
    ) -> bool {
        let mut grants = match self.grants.lock() {
            Ok(grants) => grants,
            Err(_) => {
                publish_error(
                    self.events.as_ref(),
                    &DomainError::Internal {
                        detail: "access grant lock poisoned".to_owned(),
                    },
                );
                return false;
            }
        };
        let now = Instant::now();
        grants.retain(|grant| grant.expires_at > now);
        if grants.len() >= MAX_ACCESS_GRANTS {
            grants.remove(0);
        }
        grants.push(PendingGrant {
            client_request_id: client_request_id.to_owned(),
            peer_ip: peer_ip.to_owned(),
            machine_id: machine_id.map(str::to_owned),
            authorization_id: authorization_id.to_owned(),
            code: code.to_owned(),
            expires_at: now + ACCESS_GRANT_TTL,
        });
        true
    }
}

fn validate_share_name(name: &str) -> Result<(), DomainError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 128 || trimmed.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidShareConfig {
            detail: "share name must be 1-128 characters without control codes".to_owned(),
        });
    }
    Ok(())
}

fn compile_rules(rules: &[AccessRule]) -> Result<Vec<CompiledRule>, DomainError> {
    rules.iter().map(compile_rule).collect()
}

fn load_share(record: crate::types::ShareRecord) -> Result<ShareConfig, DomainError> {
    let rules = record
        .rules
        .iter()
        .map(|rule| AccessRule {
            kind: rule.kind,
            matcher: rule.matcher.clone(),
        })
        .collect::<Vec<_>>();
    let compiled = compile_rules(&rules)?;
    let root = Arc::new(ShareRoot::open(std::path::Path::new(&record.root_path))?);
    Ok(ShareConfig {
        id: record.id,
        name: record.name,
        root_path: record.root_path.into(),
        permissions: record.permissions,
        rules: compiled,
        display_rules: rules,
        root,
    })
}

fn revoke_authorization_state(
    vault: &Vault,
    authorizations: &Arc<std::sync::RwLock<Vec<AuthEntry>>>,
    grants: &Arc<Mutex<Vec<PendingGrant>>>,
    events: &dyn ServerEvents,
    authorization_id: &str,
) -> Result<(), DomainError> {
    let is_active = match authorizations.read() {
        Ok(entries) => entries
            .iter()
            .any(|entry| entry.info.id == authorization_id && !entry.revoked),
        Err(_) => {
            publish_error(
                events,
                &DomainError::Internal {
                    detail: "authorization catalog lock poisoned while revoking".to_owned(),
                },
            );
            return Err(DomainError::Internal {
                detail: "authorization catalog lock poisoned while revoking".to_owned(),
            });
        }
    };
    if !is_active {
        return Err(DomainError::AuthorizationNotFound {
            id: authorization_id.to_owned(),
        });
    }

    match vault.revoke_authorization(authorization_id) {
        Ok(()) => {}
        Err(DomainError::AuthorizationNotFound { id }) if id == authorization_id => {}
        Err(error) => return Err(error),
    }

    match authorizations.write() {
        Ok(mut entries) => {
            for entry in &mut *entries {
                if entry.info.id == authorization_id {
                    entry.revoked = true;
                }
            }
        }
        Err(_) => publish_error(
            events,
            &DomainError::Internal {
                detail: "authorization catalog lock poisoned after revoke".to_owned(),
            },
        ),
    }

    match grants.lock() {
        Ok(mut grants) => {
            grants.retain(|grant| grant.authorization_id != authorization_id);
        }
        Err(_) => publish_error(
            events,
            &DomainError::Internal {
                detail: "access grant lock poisoned after revoke".to_owned(),
            },
        ),
    }
    Ok(())
}

fn load_authorization(
    record: &crate::types::AuthorizationRecord,
) -> Result<AuthEntry, DomainError> {
    let bytes = hex::decode(&record.code_hash).map_err(|error| DomainError::Corrupted {
        detail: format!("authorization hash is not valid hex: {error}"),
    })?;
    let _validated_hash: [u8; 32] = bytes.try_into().map_err(|_| DomainError::Corrupted {
        detail: "authorization hash has the wrong length".to_owned(),
    })?;
    Ok(AuthEntry {
        info: AuthorizationInfo {
            id: record.id.clone(),
            label: record.label.clone(),
            machine_id: record.machine_id.clone(),
            created_at: record.created_at,
        },
        client_id: record.client_id.clone(),
        revoked: record.revoked_at.is_some(),
    })
}

impl Drop for crate::types::InboundConnectionGuard {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock()
            && let Some(row) = registry.get_mut(&self.fingerprint)
        {
            row.active_connections = row.active_connections.saturating_sub(1);
            if row.active_connections == 0 && row.state == crate::types::ConnectionState::Connected
            {
                row.state = crate::types::ConnectionState::Disconnected;
            }
        }
    }
}

async fn run_server(
    listener: TcpListener,
    handler: DavHandler<RequestCredentials>,
    gate: Arc<Gate>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        let handler = handler.clone();
                        let gate = Arc::clone(&gate);
                        let tls = match gate.vault.current_identity().and_then(|identity| crate::tls::acceptor(&identity)) {
                            Ok(tls) => tls,
                            Err(error) => { publish_error(gate.events.as_ref(), &error); continue; }
                        };
                        connections.spawn(async move {
                            let stream = match tokio::time::timeout(Duration::from_secs(10), tls.accept(stream)).await {
                                Ok(Ok(stream)) => stream,
                                Ok(Err(error)) => { log::debug!("TLS connection rejected: {error}"); return; }
                                Err(_) => { log::debug!("TLS handshake timed out"); return; }
                            };
                            let Some(certificate) = stream.get_ref().1.peer_certificates().and_then(|certs| certs.first()) else { return; };
                            let identity = match crate::tls::fingerprint(certificate) {
                                Ok(identity) => identity,
                                Err(error) => { publish_error(gate.events.as_ref(), &error); return; }
                            };
                            if !crate::tls::self_connection_allowed(gate.vault.fingerprint(), &identity) { return; }
                            let _connection_guard = crate::types::InboundConnectionGuard { registry: Arc::clone(&gate.connections), fingerprint: identity.clone() };
                            if let Ok(mut registry) = gate.connections.lock() {
                                let row = registry.entry(identity.clone()).or_insert(crate::types::ConnectionInfo {
                                    fingerprint: identity.clone(), name: crate::tls::short_fingerprint(&identity), address: peer.ip().to_string(),
                                    state: crate::types::ConnectionState::Connected, authorized: false, active_connections: 0, last_seen: now_unix(),
                                });
                                row.active_connections += 1;
                                row.address = peer.ip().to_string();
                                row.last_seen = now_unix();
                                row.state = crate::types::ConnectionState::Connected;
                            }
                            let io = TokioIo::new(stream);
                            let service = service_fn(move |mut request: Request<Incoming>| {
                                // Only the TLS session determines identity. Never trust a
                                // caller-supplied machine-id or client-id header.
                                if let Ok(value) = http::HeaderValue::from_str(&identity) {
                                    request.headers_mut().insert(MACHINE_ID_HEADER, value.clone());
                                    request.headers_mut().insert(CLIENT_ID_HEADER, value);
                                }
                                let handler = handler.clone();
                                let gate = Arc::clone(&gate);
                                async move {
                                    Ok::<Response<Body>, Infallible>(
                                        handle_request(request, peer, gate, handler).await,
                                    )
                                }
                            });
                            if let Err(error) = http1::Builder::new()
                                .serve_connection(io, service)
                                .await
                            {
                                log::debug!("peerdav.server connection closed: {error}");
                            }
                        });
                    }
                    Err(error) => {
                        publish_error(
                            gate.events.as_ref(),
                            &DomainError::ServerTask {
                                detail: format!("accept failed: {error}"),
                            },
                        );
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = joined
                    && !error.is_cancelled()
                {
                    publish_error(
                        gate.events.as_ref(),
                        &DomainError::ServerTask {
                            detail: error.to_string(),
                        },
                    );
                }
            }
        }
    }

    connections.shutdown().await;
}

async fn handle_request(
    request: Request<Incoming>,
    peer: SocketAddr,
    gate: Arc<Gate>,
    handler: DavHandler<RequestCredentials>,
) -> Response<Body> {
    if request.method() == Method::OPTIONS {
        return options_response();
    }
    if request.method().as_str() == "PROPPATCH" {
        return empty_response(StatusCode::NOT_IMPLEMENTED);
    }
    if request.uri().path() == ACCESS_STATUS_PATH {
        return handle_access_status(request, peer, gate).await;
    }
    if request.uri().path() == AUTHORIZATION_PATH {
        return handle_authorization(request, gate).await;
    }
    if request.uri().path() == CATALOG_PATH {
        return handle_catalog(request, peer, gate).await;
    }

    // `DavPath` keeps malformed escapes as literal text; the gateway must not
    // turn an invalid request target into a pending approval.
    if !valid_percent_encoding(request.uri().path()) {
        log::warn!("peerdav.server rejected request with invalid percent encoding");
        return empty_response(StatusCode::BAD_REQUEST);
    }

    let path = match DavPath::new(request.uri().path()) {
        Ok(path) => path,
        Err(error) => {
            log::warn!("peerdav.server invalid request path: {error:?}");
            return empty_response(StatusCode::BAD_REQUEST);
        }
    };
    let Some((share_id, relative)) = split_share_path(&path) else {
        return empty_response(StatusCode::NOT_FOUND);
    };

    let share = match gate.shares.read() {
        Ok(shares) => shares.get(&share_id).cloned(),
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "share catalog lock poisoned".to_owned(),
                },
            );
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let Some(share) = share else {
        return empty_response(StatusCode::NOT_FOUND);
    };

    let policy_peer = PeerContext {
        ip: canonical_ip(peer.ip()),
        mac: crate::policy::neighbor_mac(peer.ip()),
    };
    if evaluate(&share.rules, &policy_peer) == PolicyDecision::Deny {
        log::info!(
            "peerdav.server denied peer {} for share {} by network policy",
            peer.ip(),
            share.id
        );
        return empty_response(StatusCode::FORBIDDEN);
    }

    let authorization = verified_authorization(&gate, request.headers());

    let Some(authorization) = authorization else {
        // Only read-style discovery on a structurally valid, in-share target
        // may ask an administrator for a code. Unknown/unrelated methods,
        // non-share paths, malformed targets, and traversal are rejected
        // above without touching the pending queue.
        if pending_eligible_method(request.method())
            && let Some(requested_path) = pending_target(&relative)
        {
            let client_request_id = access_request_id(request.headers());
            let machine_id = machine_id_value(request.headers());
            record_pending(
                &gate,
                &share,
                peer,
                &requested_path,
                client_request_id.as_deref(),
                machine_id.as_deref(),
            );
        }
        return auth_challenge();
    };

    if request.headers().contains_key("x-peerdav-transfer") {
        if !share.permissions.can_write {
            return empty_response(StatusCode::FORBIDDEN);
        }
        let Some(identity) = machine_id_value(request.headers()) else {
            return auth_challenge();
        };
        return match crate::upload::handle(
            request,
            &share,
            &relative,
            &identity,
            &gate.vault.transfer_directory(),
            &gate.uploads,
        )
        .await
        {
            Ok(status) => empty_response(status),
            Err(DomainError::TransferCanceled) => empty_response(StatusCode::CONFLICT),
            Err(error) => {
                publish_error(gate.events.as_ref(), &error);
                empty_response(StatusCode::BAD_REQUEST)
            }
        };
    }

    let Some(required) = required_for_method(request.method()) else {
        return method_not_allowed();
    };
    if !required.is_met_by(share.permissions) {
        log::info!(
            "peerdav.server denied {} on share {}: permissions {:?}",
            request.method(),
            share.id,
            share.permissions
        );
        return empty_response(StatusCode::FORBIDDEN);
    }

    let credentials = RequestCredentials {
        share_id: share.id.clone(),
        permissions: share.permissions,
    };
    handler
        .handle_guarded(request, format!("auth:{authorization}"), credentials)
        .await
}

/// Authenticated catalog used by the PeerDav client. It only lists shares the
/// peer is allowed to reach by network policy, and never exposes local paths.
/// An unauthenticated request records a pending approval against the first
/// allowed share, which is how a self-connecting client asks an administrator
/// for a code.
async fn handle_catalog(
    request: Request<Incoming>,
    peer: SocketAddr,
    gate: Arc<Gate>,
) -> Response<Body> {
    if request.method() != Method::GET {
        return method_not_allowed();
    }

    let peer_context = PeerContext {
        ip: canonical_ip(peer.ip()),
        mac: crate::policy::neighbor_mac(peer.ip()),
    };
    let allowed: Vec<Arc<ShareConfig>> = match gate.shares.read() {
        Ok(shares) => {
            let mut allowed: Vec<Arc<ShareConfig>> = shares
                .values()
                .filter(|share| evaluate(&share.rules, &peer_context) == PolicyDecision::Allow)
                .cloned()
                .collect();
            allowed.sort_by(|left, right| left.name.cmp(&right.name));
            allowed
        }
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "share catalog lock poisoned".to_owned(),
                },
            );
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if allowed.is_empty() {
        log::info!(
            "peerdav.server catalog denied peer {}: no share allows it",
            peer.ip()
        );
        return empty_response(StatusCode::FORBIDDEN);
    }

    let authorized = verified_authorization(&gate, request.headers());
    if authorized.is_none() {
        let client_request_id = access_request_id(request.headers());
        // 机器码是设备身份：同一台设备换 IP 后仍然只排一次队、审批一次。
        let machine_id = machine_id_value(request.headers());
        record_pending(
            &gate,
            &allowed[0],
            peer,
            "",
            client_request_id.as_deref(),
            machine_id.as_deref(),
        );
        return auth_challenge();
    }

    let shares = allowed
        .iter()
        .map(|share| PeerShare {
            id: share.id.clone(),
            name: share.name.clone(),
            permissions: share.permissions,
        })
        .collect();
    let catalog = CatalogResponse {
        protocol: CATALOG_PROTOCOL.to_owned(),
        server_name: gate.server_name.clone(),
        machine_id: gate.machine_id.clone(),
        shares,
    };
    match serde_json::to_vec(&catalog) {
        Ok(body) => json_response(&body),
        Err(error) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Serialization {
                    operation: "serialize catalog",
                    source: error,
                },
            );
            empty_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Returns the approval state for one client request. The approved code is
/// released exactly once and only to the peer IP that created the request.
async fn handle_access_status(
    request: Request<Incoming>,
    peer: SocketAddr,
    gate: Arc<Gate>,
) -> Response<Body> {
    match *request.method() {
        Method::GET => handle_access_poll(request, peer, gate).await,
        Method::DELETE => cancel_access_request(request, peer, gate).await,
        _ => method_not_allowed(),
    }
}

/// Cancels a request that has not been claimed yet. Removing a queued grant
/// also revokes the authorization created by the approval, so a cancel cannot
/// leave an orphaned server authorization behind.
async fn cancel_access_request(
    request: Request<Incoming>,
    peer: SocketAddr,
    gate: Arc<Gate>,
) -> Response<Body> {
    let Some(client_request_id) = access_request_id(request.headers()) else {
        return empty_response(StatusCode::BAD_REQUEST);
    };
    let peer_ip = canonical_ip(peer.ip()).to_string();
    let machine_id = machine_id_value(request.headers());

    let grant = match gate.grants.lock() {
        Ok(mut grants) => grants
            .iter()
            .position(|grant| {
                grant.client_request_id == client_request_id
                    && same_requester(
                        grant.machine_id.as_deref(),
                        &grant.peer_ip,
                        machine_id.as_deref(),
                        &peer_ip,
                    )
            })
            .map(|position| grants.remove(position)),
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "access grant lock poisoned while canceling".to_owned(),
                },
            );
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let canceled_pending = match gate.pending.lock() {
        Ok(mut pending) => pending
            .iter()
            .position(|request| {
                request.client_request_id.as_deref() == Some(client_request_id.as_str())
                    && same_requester(
                        request.machine_id.as_deref(),
                        &request.peer_ip,
                        machine_id.as_deref(),
                        &peer_ip,
                    )
            })
            .map(|position| pending.remove(position)),
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "pending request lock poisoned while canceling".to_owned(),
                },
            );
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    if let Some(request) = canceled_pending {
        gate.events.pending_canceled(&request);
    }

    if let Some(grant) = grant {
        match revoke_authorization_state(
            gate.vault.as_ref(),
            &gate.authorizations,
            &gate.grants,
            gate.events.as_ref(),
            &grant.authorization_id,
        ) {
            Ok(()) | Err(DomainError::AuthorizationNotFound { .. }) => {}
            Err(error) => {
                publish_error(gate.events.as_ref(), &error);
                return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    empty_response(StatusCode::NO_CONTENT)
}

async fn handle_access_poll(
    request: Request<Incoming>,
    peer: SocketAddr,
    gate: Arc<Gate>,
) -> Response<Body> {
    let Some(client_request_id) = access_request_id(request.headers()) else {
        return empty_response(StatusCode::BAD_REQUEST);
    };
    let peer_ip = canonical_ip(peer.ip()).to_string();
    let machine_id = machine_id_value(request.headers());
    let now = Instant::now();

    let grant = match gate.grants.lock() {
        Ok(mut grants) => {
            grants.retain(|grant| grant.expires_at > now);
            grants
                .iter()
                .position(|grant| {
                    grant.client_request_id == client_request_id
                        && same_requester(
                            grant.machine_id.as_deref(),
                            &grant.peer_ip,
                            machine_id.as_deref(),
                            &peer_ip,
                        )
                })
                .map(|position| grants.remove(position))
        }
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "access grant lock poisoned".to_owned(),
                },
            );
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    if let Some(grant) = grant {
        return access_response(
            gate.events.as_ref(),
            StatusCode::OK,
            AccessRequestStatus::Approved,
            Some(grant.code),
        );
    }

    let pending = match gate.pending.lock() {
        Ok(pending) => pending.iter().any(|request| {
            request.client_request_id.as_deref() == Some(client_request_id.as_str())
                && same_requester(
                    request.machine_id.as_deref(),
                    &request.peer_ip,
                    machine_id.as_deref(),
                    &peer_ip,
                )
        }),
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "pending request lock poisoned".to_owned(),
                },
            );
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    if pending {
        return access_response(
            gate.events.as_ref(),
            StatusCode::ACCEPTED,
            AccessRequestStatus::Pending,
            None,
        );
    }

    access_response(
        gate.events.as_ref(),
        StatusCode::NOT_FOUND,
        AccessRequestStatus::Expired,
        None,
    )
}

/// Handles the PeerDav authorization lifecycle. The code authenticates the
/// caller with the same Basic credentials used for WebDAV. This lets a client
/// discover that its server authorization was revoked and lets it revoke its
/// own binding without asking an administrator to do the second step.
async fn handle_authorization(request: Request<Incoming>, gate: Arc<Gate>) -> Response<Body> {
    let method = request.method().clone();
    if !matches!(method.as_str(), "GET" | "POST" | "DELETE") {
        return method_not_allowed();
    }
    let Some(identity) = machine_id_value(request.headers()) else {
        return auth_challenge();
    };
    let Some((authorization_id, stored_client_id, revoked)) =
        find_authorization_entry(&gate, &identity)
    else {
        return auth_challenge();
    };

    let status_response = AuthorizationStatusResponse {
        protocol: AUTHORIZATION_PROTOCOL.to_owned(),
        status: if revoked {
            AuthorizationStatus::Revoked
        } else {
            AuthorizationStatus::Active
        },
        authorization_id: authorization_id.clone(),
        client_id: stored_client_id.clone(),
    };
    if revoked {
        return json_struct_response(
            gate.events.as_ref(),
            StatusCode::GONE,
            "serialize revoked authorization",
            &status_response,
        );
    }

    match method.as_str() {
        "GET" => json_struct_response(
            gate.events.as_ref(),
            StatusCode::OK,
            "serialize authorization status",
            &status_response,
        ),
        "POST" => {
            let Some(client_id) = client_id_value(request.headers()) else {
                return empty_response(StatusCode::BAD_REQUEST);
            };
            if let Some(bound) = stored_client_id.as_deref()
                && bound != client_id
            {
                return json_struct_response(
                    gate.events.as_ref(),
                    StatusCode::CONFLICT,
                    "serialize authorization binding conflict",
                    &status_response,
                );
            }

            match bind_authorization_client(
                Arc::clone(&gate),
                authorization_id.clone(),
                client_id.clone(),
            )
            .await
            {
                Ok(()) => {}
                Err(DomainError::AuthorizationNotFound { .. }) => {
                    return json_struct_response(
                        gate.events.as_ref(),
                        StatusCode::GONE,
                        "serialize revoked authorization",
                        &AuthorizationStatusResponse {
                            protocol: AUTHORIZATION_PROTOCOL.to_owned(),
                            status: AuthorizationStatus::Revoked,
                            authorization_id,
                            client_id: None,
                        },
                    );
                }
                Err(error) => {
                    publish_error(gate.events.as_ref(), &error);
                    return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
                }
            }
            let body = AuthorizationBindingResponse {
                protocol: AUTHORIZATION_PROTOCOL.to_owned(),
                authorization_id,
                client_id,
            };
            json_struct_response(
                gate.events.as_ref(),
                StatusCode::OK,
                "serialize authorization binding",
                &body,
            )
        }
        "DELETE" => {
            match revoke_authorization_via_gate(Arc::clone(&gate), authorization_id.clone()).await {
                Ok(()) => empty_response(StatusCode::NO_CONTENT),
                Err(DomainError::AuthorizationNotFound { .. }) => json_struct_response(
                    gate.events.as_ref(),
                    StatusCode::GONE,
                    "serialize revoked authorization",
                    &AuthorizationStatusResponse {
                        protocol: AUTHORIZATION_PROTOCOL.to_owned(),
                        status: AuthorizationStatus::Revoked,
                        authorization_id,
                        client_id: None,
                    },
                ),
                Err(error) => {
                    publish_error(gate.events.as_ref(), &error);
                    empty_response(StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        }
        _ => method_not_allowed(),
    }
}

async fn bind_authorization_client(
    gate: Arc<Gate>,
    authorization_id: String,
    client_id: String,
) -> Result<(), DomainError> {
    let vault = Arc::clone(&gate.vault);
    let authorizations = Arc::clone(&gate.authorizations);
    let events = Arc::clone(&gate.events);
    tokio::task::spawn_blocking(move || {
        if !vault.bind_authorization(&authorization_id, &client_id)? {
            return Err(DomainError::AuthorizationNotFound {
                id: authorization_id,
            });
        }
        match authorizations.write() {
            Ok(mut entries) => {
                for entry in &mut *entries {
                    if entry.info.id == authorization_id {
                        entry.client_id = Some(client_id.clone());
                    }
                }
            }
            Err(_) => publish_error(
                events.as_ref(),
                &DomainError::Internal {
                    detail: "authorization catalog lock poisoned after bind".to_owned(),
                },
            ),
        }
        Ok(())
    })
    .await
    .map_err(|error| DomainError::Internal {
        detail: format!("authorization bind task failed: {error}"),
    })?
}

async fn revoke_authorization_via_gate(
    gate: Arc<Gate>,
    authorization_id: String,
) -> Result<(), DomainError> {
    let vault = Arc::clone(&gate.vault);
    let authorizations = Arc::clone(&gate.authorizations);
    let grants = Arc::clone(&gate.grants);
    let events = Arc::clone(&gate.events);
    tokio::task::spawn_blocking(move || {
        revoke_authorization_state(
            vault.as_ref(),
            &authorizations,
            &grants,
            events.as_ref(),
            &authorization_id,
        )
    })
    .await
    .map_err(|error| DomainError::Internal {
        detail: format!("authorization revoke task failed: {error}"),
    })?
}

fn json_struct_response<T: serde::Serialize>(
    events: &dyn ServerEvents,
    status: StatusCode,
    operation: &'static str,
    body: &T,
) -> Response<Body> {
    match serde_json::to_vec(body) {
        Ok(body) => json_response_with_status(status, &body),
        Err(source) => {
            publish_error(events, &DomainError::Serialization { operation, source });
            empty_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn access_response(
    events: &dyn ServerEvents,
    status: StatusCode,
    request_status: AccessRequestStatus,
    authorization_code: Option<String>,
) -> Response<Body> {
    let body = AccessStatusResponse {
        protocol: ACCESS_PROTOCOL.to_owned(),
        status: request_status,
        authorization_code,
    };
    match serde_json::to_vec(&body) {
        Ok(body) => json_response_with_status(status, &body),
        Err(error) => {
            publish_error(
                events,
                &DomainError::Serialization {
                    operation: "serialize access status",
                    source: error,
                },
            );
            empty_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn access_request_id(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(ACCESS_REQUEST_HEADER)?.to_str().ok()?.trim();
    if !(MIN_ACCESS_REQUEST_ID_CHARS..=MAX_ACCESS_REQUEST_ID_CHARS).contains(&value.len()) {
        return None;
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn client_id_value(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(CLIENT_ID_HEADER)?.to_str().ok()?.trim();
    if !(MIN_CLIENT_ID_CHARS..=MAX_CLIENT_ID_CHARS).contains(&value.len()) {
        return None;
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(value.to_owned())
}

/// 读取请求方上报的机器码。格式不对就当没报，服务端退回 IP 身份，
/// 这样老客户端和不发该头的普通 WebDAV 客户端仍能正常走审批流程。
fn machine_id_value(headers: &HeaderMap) -> Option<String> {
    crate::tls::normalize_fingerprint(headers.get(MACHINE_ID_HEADER)?.to_str().ok())
}

fn verified_authorization(gate: &Gate, headers: &HeaderMap) -> Option<String> {
    let identity = machine_id_value(headers)?;
    let authorizations = gate.authorizations.read().ok()?;
    authorizations
        .iter()
        .find(|entry| !entry.revoked && entry.info.machine_id.as_deref() == Some(identity.as_str()))
        .map(|entry| entry.info.id.clone())
}

fn find_authorization_entry(gate: &Gate, identity: &str) -> Option<(String, Option<String>, bool)> {
    match gate.authorizations.read() {
        Ok(authorizations) => authorizations
            .iter()
            .rev()
            .find(|entry| entry.info.machine_id.as_deref() == Some(identity))
            .map(|entry| {
                (
                    entry.info.id.clone(),
                    entry.client_id.clone(),
                    entry.revoked,
                )
            }),
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "authorization catalog lock poisoned".to_owned(),
                },
            );
            None
        }
    }
}

/// Methods that may ask an administrator for a code. Only discovery-style
/// reads qualify; mutations must first obtain a code through one of these
/// paths (or the catalog).
fn pending_eligible_method(method: &Method) -> bool {
    matches!(method.as_str(), "GET" | "PROPFIND")
}

/// Checks percent escapes in the raw request target. `DavPath` deliberately
/// treats malformed escapes as literal bytes, so this gate performs the
/// rejection before anything can be recorded as a pending request.
fn valid_percent_encoding(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let (Some(high), Some(low)) = (bytes.get(index + 1), bytes.get(index + 2)) else {
                return false;
            };
            if !high.is_ascii_hexdigit() || !low.is_ascii_hexdigit() {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

/// Renders the normalized share-relative target for a pending request, or
/// `None` when it is not a safe in-share path.
///
/// Existence is deliberately not checked: WebDAV clients routinely PROPFIND
/// a missing path before creating it, and the gate must keep the same virtual
/// directory semantics as `DavHandler`.
fn pending_target(relative: &std::path::Path) -> Option<String> {
    use std::path::Component;

    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    if relative
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|byte| *byte < 0x20 || *byte == 0x7f)
    {
        return None;
    }

    let mut rendered = relative.to_string_lossy().into_owned();
    if rendered.chars().count() > MAX_PENDING_PATH_CHARS {
        rendered = rendered.chars().take(MAX_PENDING_PATH_CHARS).collect();
    }
    Some(rendered)
}

fn next_attempt_count(attempts: u32) -> u32 {
    attempts.saturating_add(1).min(MAX_PENDING_ATTEMPTS)
}

fn record_pending(
    gate: &Gate,
    share: &ShareConfig,
    peer: SocketAddr,
    requested_path: &str,
    client_request_id: Option<&str>,
    machine_id: Option<&str>,
) {
    let now = now_unix();
    let peer_ip = canonical_ip(peer.ip()).to_string();

    let mut pending = match gate.pending.lock() {
        Ok(pending) => pending,
        Err(_) => {
            publish_error(
                gate.events.as_ref(),
                &DomainError::Internal {
                    detail: "pending request lock poisoned".to_owned(),
                },
            );
            return;
        }
    };

    if let Some(existing) = pending
        .iter_mut()
        .find(|request| same_pending_entry(request, &share.id, machine_id, &peer_ip))
    {
        existing.last_seen = now;
        existing.attempts = next_attempt_count(existing.attempts);
        existing.requested_path = requested_path.to_owned();
        // 同一台设备可能已经换到别的地址，展示用地址要跟着更新。
        existing.peer_ip = peer_ip;
        // 先前的请求没带机器码、这次带了：补上身份，之后就能按机器码认领和清理。
        if existing.machine_id.is_none() {
            existing.machine_id = machine_id.map(str::to_owned);
        }
        if let Some(client_request_id) = client_request_id {
            existing.client_request_id = Some(client_request_id.to_owned());
        }
        return;
    }

    if pending.len() >= MAX_PENDING_REQUESTS {
        log::warn!("peerdav.server pending request list is full; ignoring new peer");
        return;
    }

    let request = PendingRequestInfo {
        id: random_hex(8),
        share_id: share.id.clone(),
        share_name: share.name.clone(),
        peer_ip,
        peer_mac: None,
        machine_id: machine_id.map(str::to_owned),
        requested_path: requested_path.to_owned(),
        first_seen: now,
        last_seen: now,
        attempts: 1,
        client_request_id: client_request_id.map(str::to_owned),
    };
    pending.push(request.clone());
    drop(pending);
    gate.events.pending_request(&request);
}

/// 新请求是否该并入已排队的这条申请（同一共享下）：
/// - 双方都有机器码：相同才是同一条，不同一定是两台设备，各排各的；
/// - 任一方没报机器码（老客户端、普通 WebDAV 客户端，或某条请求路径不带该头）：
///   按地址归并，免得同一台设备重复排队。
fn same_pending_entry(
    stored: &PendingRequestInfo,
    share_id: &str,
    machine_id: Option<&str>,
    peer_ip: &str,
) -> bool {
    if stored.share_id != share_id {
        return false;
    }
    match (stored.machine_id.as_deref(), machine_id) {
        (Some(stored_machine), Some(incoming)) => stored_machine == incoming,
        _ => stored.peer_ip == peer_ip,
    }
}

/// 认领授权码/取消申请时判断是否同一个申请方：记录里存了机器码就按机器码比，
/// 没存（历史记录或普通 WebDAV 客户端）才按 IP 比。这样对端换 IP 后仍能领到码，
/// 而换了设备却用同一 IP 的访问者不会顶替前者的申请。
fn same_requester(
    stored_machine_id: Option<&str>,
    stored_ip: &str,
    machine_id: Option<&str>,
    peer_ip: &str,
) -> bool {
    match stored_machine_id {
        Some(stored) => machine_id == Some(stored),
        // 记录里只有 IP（历史数据或普通 WebDAV 客户端）：只接受同样没报机器码的请求，
        // 免得同一地址上的另一台设备顶替这条申请。
        None => machine_id.is_none() && stored_ip == peer_ip,
    }
}

fn options_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("DAV", "1, 2")
        .header(
            "Allow",
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, COPY, MOVE, LOCK, UNLOCK",
        )
        .header("Server", SERVER_AGENT)
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn auth_challenge() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(WWW_AUTHENTICATE, AUTH_CHALLENGE)
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn method_not_allowed() -> Response<Body> {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(
            "Allow",
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, COPY, MOVE, LOCK, UNLOCK",
        )
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn empty_response(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn json_response(body: &[u8]) -> Response<Body> {
    json_response_with_status(StatusCode::OK, body)
}

fn json_response_with_status(status: StatusCode, body: &[u8]) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .header(CACHE_CONTROL, "no-store")
        .header(CONTENT_LENGTH, body.len())
        .body(Body::from(bytes::Bytes::from(body.to_vec())))
        .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR))
}

/// Extracts a share authorization code from `Authorization`. Basic auth uses
/// the password as the code; when it is empty the username is used, matching
/// the share-token convention. Bearer tokens are accepted as well.
#[cfg(test)]
fn auth_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, credentials) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(credentials.trim())
            .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (username, password) = decoded.split_once(':')?;
        let token = if password.is_empty() {
            username
        } else {
            password
        };
        if token.is_empty() {
            return None;
        }
        return Some(token.to_owned());
    }
    if scheme.eq_ignore_ascii_case("bearer") {
        let token = credentials.trim();
        if token.is_empty() {
            return None;
        }
        return Some(token.to_owned());
    }
    None
}

fn hash_code(code: &str) -> [u8; 32] {
    Sha256::digest(code.as_bytes()).into()
}

#[cfg(test)]
fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    hex::encode(buffer)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct RecordingEvents {
        errors: StdMutex<Vec<AppError>>,
        pending: StdMutex<Vec<PendingRequestInfo>>,
    }

    impl ServerEvents for RecordingEvents {
        fn error(&self, error: &AppError) {
            self.errors.lock().expect("errors lock").push(error.clone());
        }

        fn pending_request(&self, request: &PendingRequestInfo) {
            self.pending
                .lock()
                .expect("pending lock")
                .push(request.clone());
        }
    }

    #[test]
    fn publish_error_uses_the_shared_structured_payload() {
        let events = RecordingEvents::default();
        publish_error(&events, &DomainError::ServerAlreadyRunning);

        let errors = events.errors.lock().expect("errors");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "SERVER_ALREADY_RUNNING");
        assert!(errors[0].message.contains("服务已在运行"));
        assert!(errors[0].message.contains("already running"));
        assert!(errors[0].detail.is_some());
    }

    #[test]
    fn token_extraction_supports_basic_bearer_and_share_tokens() {
        let basic = basic_header("peerdav", "code123");
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, basic);
        assert_eq!(auth_token(&headers).as_deref(), Some("code123"));

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            http::HeaderValue::from_static("Bearer plain-code"),
        );
        assert_eq!(auth_token(&headers).as_deref(), Some("plain-code"));

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, basic_header("username-only", ""));
        assert_eq!(auth_token(&headers).as_deref(), Some("username-only"));

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, http::HeaderValue::from_static("Digest x"));
        assert!(auth_token(&headers).is_none());
    }

    fn basic_header(username: &str, password: &str) -> http::HeaderValue {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        http::HeaderValue::from_str(&format!("Basic {encoded}")).expect("header")
    }

    #[test]
    fn authorization_hashes_compare_in_constant_time_shape() {
        let hash = hash_code("abc");
        assert!(constant_time_eq(&hash, &hash_code("abc")));
        assert!(!constant_time_eq(&hash, &hash_code("abd")));
    }

    #[test]
    fn malformed_percent_escapes_are_rejected() {
        assert!(valid_percent_encoding("/share/id/a%20b.txt"));
        assert!(valid_percent_encoding("/share/id/100%25"));
        assert!(!valid_percent_encoding("/share/id/%zz"));
        assert!(!valid_percent_encoding("/share/id/%2"));
        assert!(!valid_percent_encoding("/share/id/%"));
    }

    #[test]
    fn pending_targets_stay_inside_the_share_and_are_bounded() {
        assert_eq!(
            pending_target(std::path::Path::new("")).as_deref(),
            Some("")
        );
        assert_eq!(
            pending_target(std::path::Path::new("a/b.txt")).as_deref(),
            Some("a/b.txt")
        );
        assert!(pending_target(std::path::Path::new("../escape")).is_none());
        assert!(pending_target(std::path::Path::new("bad\nname")).is_none());

        let long = "a".repeat(MAX_PENDING_PATH_CHARS + 64);
        let rendered = pending_target(std::path::Path::new(&long)).expect("long target");
        assert_eq!(rendered.chars().count(), MAX_PENDING_PATH_CHARS);
    }

    #[test]
    fn pending_attempts_are_capped() {
        assert_eq!(next_attempt_count(0), 1);
        assert_eq!(
            next_attempt_count(MAX_PENDING_ATTEMPTS - 1),
            MAX_PENDING_ATTEMPTS
        );
        assert_eq!(
            next_attempt_count(MAX_PENDING_ATTEMPTS),
            MAX_PENDING_ATTEMPTS
        );
        assert_eq!(next_attempt_count(u32::MAX), MAX_PENDING_ATTEMPTS);
    }

    #[test]
    fn pending_entries_merge_by_machine_code_and_fall_back_to_the_address() {
        let stored = |machine: Option<&str>, ip: &str| PendingRequestInfo {
            id: "id".to_owned(),
            share_id: "share".to_owned(),
            share_name: "Docs".to_owned(),
            peer_ip: ip.to_owned(),
            peer_mac: None,
            machine_id: machine.map(str::to_owned),
            requested_path: String::new(),
            first_seen: 0,
            last_seen: 0,
            attempts: 1,
            client_request_id: None,
        };

        let machine_entry = stored(Some("abc"), "10.0.0.5");
        // 同一台设备换地址（机器码相同）仍是同一条申请。
        assert!(same_pending_entry(
            &machine_entry,
            "share",
            Some("abc"),
            "10.0.0.9"
        ));
        // 机器码不同就是另一台设备，哪怕地址相同。
        assert!(!same_pending_entry(
            &machine_entry,
            "share",
            Some("def"),
            "10.0.0.5"
        ));
        // 另一个共享是另一条申请。
        assert!(!same_pending_entry(
            &machine_entry,
            "other",
            Some("abc"),
            "10.0.0.5"
        ));
        // 任一方没报机器码时按地址归并。
        assert!(same_pending_entry(
            &machine_entry,
            "share",
            None,
            "10.0.0.5"
        ));
        assert!(!same_pending_entry(
            &machine_entry,
            "share",
            None,
            "10.0.0.9"
        ));

        let address_entry = stored(None, "10.0.0.5");
        assert!(same_pending_entry(
            &address_entry,
            "share",
            None,
            "10.0.0.5"
        ));
        assert!(same_pending_entry(
            &address_entry,
            "share",
            Some("abc"),
            "10.0.0.5"
        ));
        assert!(!same_pending_entry(
            &address_entry,
            "share",
            Some("abc"),
            "10.0.0.9"
        ));
    }

    #[test]
    fn claiming_matches_the_stored_identity_not_the_address_alone() {
        // 存了机器码：只认同一个机器码，换 IP 也能领。
        assert!(same_requester(
            Some("abc"),
            "10.0.0.5",
            Some("abc"),
            "10.0.0.9"
        ));
        // 机器码不同就是另一台设备，即使 IP 完全相同、请求 key 也相同。
        assert!(!same_requester(
            Some("abc"),
            "10.0.0.5",
            Some("def"),
            "10.0.0.5"
        ));
        assert!(!same_requester(Some("abc"), "10.0.0.5", None, "10.0.0.5"));
        // 没存机器码（历史记录）：退回 IP 比对。
        assert!(same_requester(None, "10.0.0.5", None, "10.0.0.5"));
        assert!(!same_requester(None, "10.0.0.5", None, "10.0.0.9"));
        // 存了 IP 身份但新请求带机器码时不能顶替。
        assert!(!same_requester(None, "10.0.0.5", Some("abc"), "10.0.0.5"));
    }
}
