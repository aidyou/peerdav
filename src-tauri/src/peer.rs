//! PeerDav client: saved peers, health checks, structured browsing, file
//! transfer, backend-controlled previews, and the external-edit round trip.
//!
//! Security invariants:
//! - Authorization codes are only read from the encrypted vault and are never
//!   serialized into `PeerInfo`.
//! - Every request re-reads the code, so revoking an authorization takes
//!   effect on the next operation.
//! - Remote paths are validated and percent-encoded per segment; `..`,
//!   backslashes, NULs, and control codes are rejected.
//! - Transfers are bounded by size limits and use a temporary file followed by
//!   an atomic rename.
//! - Previews are read by the backend and returned over IPC; no broad asset
//!   protocol scope is introduced.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use reqwest::StatusCode;
use reqwest_dav::types::list_cmd::{ListEntity, ListMultiStatus};
use reqwest_dav::{Auth, Client, ClientBuilder, Depth};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use url::Url;
use zeroize::Zeroizing;

use crate::discovery::{self, DiscoveryListener};
use crate::error::{AppError, DomainError};
use crate::ignore_rules::{IgnoreLayer, build_gitignore, is_path_ignored};
use crate::traits::PeerEvents;
use crate::types::{
    ACCESS_PROTOCOL, ACCESS_REQUEST_HEADER, ACCESS_STATUS_PATH, AUTHORIZATION_PATH,
    AUTHORIZATION_PROTOCOL, AccessRequestStatus, AccessStatusResponse,
    AuthorizationBindingResponse, AuthorizationStatus, AuthorizationStatusResponse, CATALOG_PATH,
    CATALOG_PROTOCOL, CLIENT_ID_HEADER, CatalogResponse, ConflictPolicy, DEFAULT_DISCOVERY_PORT,
    DEFAULT_SERVER_PORT, DirectoryDownloadPlan, DiscoveredPeer, DownloadCandidate,
    ExternalEditInfo, MAX_ACCESS_REQUEST_ID_CHARS, MAX_AUTHORIZATION_CODE_CHARS, MAX_DIR_ENTRIES,
    MAX_DIRECTORY_DOWNLOAD_FILES, MAX_PREVIEW_BYTES, MAX_PREVIEW_LINES, MAX_REMOTE_PATH_BYTES,
    MAX_TRANSFER_BYTES, MIN_ACCESS_REQUEST_ID_CHARS, OBSERVED_PEER_HEADER, OutboundAccessRequest,
    OutboundAccessStatus, PEER_ACCESS_POLL_INTERVAL, PEER_ACCESS_WAIT, PEER_PRESENCE_INTERVAL,
    PEER_REQUEST_TIMEOUT, PEER_SYNC_INTERVAL, PeerAccessRequest, PeerEntry, PeerHealth, PeerInfo,
    PeerListing, PeerRecord, PeerRevocationInfo, PeerShare, PeerStatus, PreviewPayload,
    SERVER_DISCOVERY_PORT_META, SHARE_URL_PREFIX, TransferResult,
};
use crate::vault::Vault;

/// Username used with Basic auth. The password carries the authorization code,
/// matching the server's share-token convention.
const CLIENT_USERNAME: &str = "peerdav";
const CATALOG_BODY_LIMIT: u64 = 1024 * 1024;
const ACCESS_BODY_LIMIT: u64 = 16 * 1024;
const AUTHORIZATION_BODY_LIMIT: u64 = 16 * 1024;
const LISTING_BODY_LIMIT: u64 = 8 * 1024 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
const READ_CHUNK: usize = 64 * 1024;
const MAX_IGNORE_FILE_BYTES: u64 = 1024 * 1024;

/// Suffixes treated as text previews. This is deliberately suffix-driven:
/// server-reported MIME types are often `application/octet-stream` for source
/// files. Keep this list aligned with `CODE_EXTENSIONS`/`TEXT_EXTENSIONS` in
/// `src/lib/fileKinds.js`.
const TEXT_EXTENSIONS: &[&str] = &[
    "1st",
    "adoc",
    "asciidoc",
    "asm",
    "astro",
    "avsc",
    "awk",
    "bash",
    "bat",
    "babelrc",
    "bashrc",
    "browserslistrc",
    "c",
    "c++",
    "cc",
    "cfg",
    "clj",
    "cljc",
    "cljs",
    "cmake",
    "cmd",
    "cnf",
    "conf",
    "copying",
    "cpp",
    "cs",
    "csx",
    "css",
    "csv",
    "cxx",
    "dart",
    "desktop",
    "dockerignore",
    "edn",
    "editorconfig",
    "elm",
    "env",
    "erl",
    "eslintrc",
    "ex",
    "exs",
    "f90",
    "f95",
    "fish",
    "fs",
    "fsx",
    "gitattributes",
    "gitconfig",
    "gitignore",
    "go",
    "gql",
    "gradle",
    "graphql",
    "groovy",
    "h",
    "hh",
    "hpp",
    "hrl",
    "hs",
    "htaccess",
    "htpasswd",
    "htm",
    "html",
    "hxx",
    "ini",
    "ipynb",
    "java",
    "jl",
    "js",
    "inputrc",
    "json",
    "json5",
    "jsonc",
    "jsx",
    "kt",
    "kts",
    "less",
    "lhs",
    "lock",
    "log",
    "lua",
    "m",
    "make",
    "map",
    "markdown",
    "md",
    "mdown",
    "mdx",
    "mjs",
    "mk",
    "mkd",
    "mli",
    "ml",
    "mm",
    "mount",
    "netrc",
    "nim",
    "nfo",
    "npmrc",
    "nvmrc",
    "org",
    "pas",
    "php",
    "php3",
    "php4",
    "php5",
    "php7",
    "phtml",
    "phps",
    "pid",
    "pl",
    "plist",
    "pm",
    "prettierrc",
    "profile",
    "properties",
    "proto",
    "ps1",
    "psm1",
    "purs",
    "py",
    "pyw",
    "r",
    "rb",
    "rc",
    "re",
    "rei",
    "rst",
    "rs",
    "rules",
    "s",
    "sass",
    "scala",
    "scss",
    "sed",
    "service",
    "sh",
    "socket",
    "sol",
    "sql",
    "srt",
    "stylelintrc",
    "sv",
    "svelte",
    "swift",
    "tcl",
    "target",
    "tex",
    "text",
    "thrift",
    "timer",
    "toml",
    "ts",
    "tsv",
    "tsx",
    "txt",
    "tx",
    "vb",
    "vhd",
    "vhdl",
    "v",
    "vimrc",
    "vtt",
    "vue",
    "wgetrc",
    "xml",
    "xsd",
    "xsl",
    "xslt",
    "yaml",
    "yarnrc",
    "yml",
    "zig",
    "zsh",
    "zshrc",
];
const TEXT_FILENAMES: &[&str] = &[
    "authors",
    "changelog",
    "copying",
    "dockerfile",
    "gemfile",
    "jenkinsfile",
    "license",
    "licence",
    "makefile",
    "notice",
    "procfile",
    "rakefile",
    "readme",
    "todo",
    "vagrantfile",
];

pub type TransferProgressCallback = Arc<dyn Fn(u64, u64) + Send + Sync>;

const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Tunable limits; tests use smaller values.
#[derive(Debug, Clone)]
pub struct PeerLimits {
    pub max_transfer_bytes: u64,
    pub max_preview_bytes: u64,
    pub request_timeout: Duration,
    pub access_wait: Duration,
    pub sync_interval: Duration,
}

impl Default for PeerLimits {
    fn default() -> Self {
        Self {
            max_transfer_bytes: MAX_TRANSFER_BYTES,
            max_preview_bytes: MAX_PREVIEW_BYTES,
            request_timeout: PEER_REQUEST_TIMEOUT,
            access_wait: PEER_ACCESS_WAIT,
            sync_interval: PEER_SYNC_INTERVAL,
        }
    }
}

#[derive(Debug, Clone)]
struct ExternalEdit {
    info: ExternalEditInfo,
    baseline_hash: [u8; 32],
    baseline_size: u64,
    baseline_modified: Option<i64>,
    baseline_etag: Option<String>,
}

/// 一条出站访问申请：对外快照 + 仅后端使用的轮询请求键。
#[derive(Debug, Clone)]
struct OutboundAccess {
    request: OutboundAccessRequest,
    request_id: Option<String>,
}

struct RunningPeerBackground {
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

// Last known reachability for one saved server. A server that goes offline is
/// not probed again: PeerDav servers announce themselves over UDP when they
/// come back, which keeps idle clients free of polling traffic.
#[derive(Debug, Clone, Copy)]
struct PeerAvailability {
    online: bool,
}

/// Result of asking a server about a previously accepted authorization code.
enum AuthorizationOutcome {
    Active(AuthorizationStatusResponse),
    Revoked,
    Removed,
    Unsupported,
}

/// Process-wide peer client. Sessions are intentionally not cached: each
/// operation re-reads the code from the vault so revocation is immediate.
pub struct PeerManager {
    vault: Arc<Vault>,
    temp_root: PathBuf,
    limits: PeerLimits,
    edits: Mutex<HashMap<String, ExternalEdit>>,
    access_cancellations: Mutex<HashSet<String>>,
    upload_cancellations: Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>,
    events: Arc<dyn PeerEvents>,
    background: std::sync::Mutex<Option<RunningPeerBackground>>,
    discovery: std::sync::Mutex<Option<Arc<DiscoveryListener>>>,
    availability: std::sync::Mutex<HashMap<String, PeerAvailability>>,
    /// 出站访问申请（按规范化后的服务器地址）。列表读取的是这份内存状态，因此
    /// 切换页面、重建视图或 webview 重载都不会让「等待批准」退回「可以申请」。
    outbound_access: std::sync::Mutex<HashMap<String, OutboundAccess>>,
    outbound_connections: std::sync::Mutex<HashMap<String, crate::types::ConnectionInfo>>,
    pairing_pins: std::sync::Mutex<HashMap<String, String>>,
}

impl Drop for PeerManager {
    fn drop(&mut self) {
        if let Some(listener) = self.discovery.get_mut().ok().and_then(|slot| slot.take()) {
            listener.request_shutdown();
        }
        if let Some(background) = self
            .background
            .get_mut()
            .ok()
            .and_then(|background| background.as_mut())
        {
            if let Some(sender) = background.shutdown.take() {
                let _ = sender.send(());
            }
            background.task.abort();
        }
    }
}

/// Default event sink for tests and headless callers. Production uses AppEvents
/// through [`PeerManager::with_events`].
#[derive(Default)]
struct LoggingPeerEvents;

impl PeerEvents for LoggingPeerEvents {
    fn error(&self, error: &AppError) {
        log::error!(
            "peerdav.peer background error [{}]: {}",
            error.code,
            error.detail.as_deref().unwrap_or(&error.message)
        );
    }

    fn peer_revoked(&self, peer: &PeerRevocationInfo) {
        log::info!(
            "peerdav.peer removed revoked server {} ({})",
            peer.label,
            peer.reason
        );
    }
}

impl PeerManager {
    pub fn new(vault: Arc<Vault>, temp_root: PathBuf) -> Result<Self, DomainError> {
        Self::with_limits(vault, temp_root, PeerLimits::default())
    }

    pub fn with_events(
        vault: Arc<Vault>,
        temp_root: PathBuf,
        events: Arc<dyn PeerEvents>,
    ) -> Result<Self, DomainError> {
        Self::with_limits_and_events(vault, temp_root, PeerLimits::default(), events)
    }

    pub fn with_limits(
        vault: Arc<Vault>,
        temp_root: PathBuf,
        limits: PeerLimits,
    ) -> Result<Self, DomainError> {
        Self::with_limits_and_events(vault, temp_root, limits, Arc::new(LoggingPeerEvents))
    }

    pub fn with_limits_and_events(
        vault: Arc<Vault>,
        temp_root: PathBuf,
        limits: PeerLimits,
        events: Arc<dyn PeerEvents>,
    ) -> Result<Self, DomainError> {
        // Copies left by a previous process are never valid edit sessions.
        // Each process uses its own run directory so a second instance cannot
        // delete another instance's active external edit.
        create_private_dir(&temp_root)?;
        cleanup_stale_runs(&temp_root);
        let run_root = temp_root.join(format!("run-{}", random_hex(8)));
        create_private_dir(&run_root)?;
        Ok(Self {
            vault,
            temp_root: run_root,
            limits,
            edits: Mutex::new(HashMap::new()),
            access_cancellations: Mutex::new(HashSet::new()),
            upload_cancellations: Mutex::new(HashMap::new()),
            events,
            background: std::sync::Mutex::new(None),
            discovery: std::sync::Mutex::new(None),
            availability: std::sync::Mutex::new(HashMap::new()),
            outbound_access: std::sync::Mutex::new(HashMap::new()),
            outbound_connections: std::sync::Mutex::new(HashMap::new()),
            pairing_pins: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Starts the periodic server-binding synchronizer and the passive LAN
    /// discovery listener. Both are idempotent and stop when this manager is
    /// dropped.
    pub fn start_background(self: &Arc<Self>) {
        // The Tauri setup hook is synchronous and has no entered Tokio
        // runtime. Discovery then starts on the first async command
        // (list_peers/resume_server/start_server), where a reactor exists.
        if tokio::runtime::Handle::try_current().is_ok() {
            self.start_discovery_listener();
        }
        let mut started = match self.background.lock() {
            Ok(started) => started,
            Err(_) => {
                log::error!("peerdav.peer background mutex poisoned");
                return;
            }
        };
        if started.is_some() {
            return;
        }

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let manager = Arc::clone(self);
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                log::error!("peerdav.peer background task has no async runtime: {error}");
                return;
            }
        };
        let task = runtime.spawn(async move {
            let mut interval = tokio::time::interval(manager.limits.sync_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut presence = tokio::time::interval(PEER_PRESENCE_INTERVAL);
            presence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Both intervals complete immediately on their first tick: saved
            // servers get a reachability result after launch, and a returning
            // server is promoted as soon as its announcement is visible.
            let mut tcp = tokio::time::interval(Duration::from_secs(120));
            tcp.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    _ = tcp.tick() => {
                        if manager.vault.is_unlocked() {
                            if let Err(error) = manager.scan_lan_tcp().await {
                                manager.events.error(&AppError::from_ref(&error));
                            }
                        }
                    }
                    _ = presence.tick() => {
                        manager.reconcile_announced_peers();
                    }
                    _ = interval.tick() => {
                        if !manager.vault.is_unlocked() {
                            continue;
                        }
                        if let Err(error) = manager.sync_saved_peers().await {
                            manager.events.error(&AppError::from_ref(&error));
                        }
                    }
                }
            }
        });
        *started = Some(RunningPeerBackground {
            shutdown: Some(shutdown_tx),
            task,
        });
    }

    /// Starts the passive listener on the persisted discovery port. The listener
    /// is independent of the local WebDAV server: other servers' announcements are
    /// received even when this device only acts as a client.
    fn start_discovery_listener(&self) {
        let port = self
            .vault
            .get_setting(SERVER_DISCOVERY_PORT_META)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .unwrap_or(DEFAULT_DISCOVERY_PORT);
        if let Err(error) = self.ensure_discovery_listener(port) {
            log::warn!("peerdav.discovery listener is unavailable: {error}");
        }
    }

    /// Rebinds the passive listener when the server's discovery port changes.
    /// A bind failure is reported through the shared background error outlet
    /// but does not prevent the WebDAV server from starting.
    pub fn ensure_discovery_listener(&self, port: u16) -> Result<(), DomainError> {
        if port == 0 {
            return Ok(());
        }

        let previous = {
            let mut slot = self.discovery.lock().map_err(|_| DomainError::Internal {
                detail: "peer discovery mutex poisoned".to_owned(),
            })?;
            if slot
                .as_ref()
                .is_some_and(|listener| listener.port() == port)
            {
                return Ok(());
            }
            slot.take()
        };
        if let Some(previous) = previous {
            previous.request_shutdown();
        }

        let listener = match DiscoveryListener::start(port, Arc::clone(&self.events)) {
            Ok(listener) => Arc::new(listener),
            Err(error) => {
                self.events.error(&AppError::from_ref(&error));
                return Err(error);
            }
        };
        let mut slot = self.discovery.lock().map_err(|_| DomainError::Internal {
            detail: "peer discovery mutex poisoned".to_owned(),
        })?;
        *slot = Some(listener);
        Ok(())
    }

    pub fn discovered_peers(&self) -> Vec<DiscoveredPeer> {
        match self.discovery.lock() {
            Ok(slot) => slot
                .as_ref()
                .map(|listener| {
                    listener
                        .peers()
                        .into_iter()
                        .filter(|peer| {
                            crate::tls::self_connection_allowed(
                                self.vault.fingerprint(),
                                &peer.server_id,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => {
                log::warn!("peer discovery mutex poisoned");
                Vec::new()
            }
        }
    }

    pub(crate) fn discovery_listener(&self) -> Option<Arc<DiscoveryListener>> {
        match self.discovery.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => {
                log::warn!("peer discovery mutex poisoned");
                None
            }
        }
    }

    fn availability_snapshot(&self) -> HashMap<String, bool> {
        match self.availability.lock() {
            Ok(map) => map
                .iter()
                .map(|(id, state)| (id.clone(), state.online))
                .collect(),
            Err(_) => {
                log::warn!("peer availability mutex poisoned");
                HashMap::new()
            }
        }
    }

    /// Records a reachability result and notifies the UI only on a transition.
    fn set_peer_online(&self, peer_id: &str, online: bool) {
        let changed = match self.availability.lock() {
            Ok(mut map) => {
                let previous = map.insert(peer_id.to_owned(), PeerAvailability { online });
                previous.is_none_or(|state| state.online != online)
            }
            Err(_) => {
                log::warn!("peer availability mutex poisoned");
                false
            }
        };
        if changed {
            self.events.peer_status(&PeerStatus {
                id: peer_id.to_owned(),
                online,
            });
        }
    }

    /// Marks saved peers that are currently announcing as online. This is the
    /// broadcast-driven path and performs no network request of its own.
    fn reconcile_announced_peers(&self) {
        if !self.vault.is_unlocked() {
            return;
        }
        let announced = self.discovered_peers();
        if announced.is_empty() {
            return;
        }
        let Ok(records) = self.vault.list_peer_records() else {
            return;
        };
        for record in records {
            if url_is_announced(&record.url, &announced) {
                self.set_peer_online(&record.id, true);
            }
        }
    }

    /// Online servers keep the normal synchronization cadence so authorization
    /// changes are noticed quickly. Once a server is offline it is no longer
    /// probed: the next UDP announcement is the signal that it is back.
    fn should_probe(&self, peer_id: &str) -> bool {
        match self.availability.lock() {
            Ok(map) => map.get(peer_id).is_none_or(|state| state.online),
            Err(_) => true,
        }
    }

    /// Scans the LAN over UDP discovery.
    pub async fn scan_lan(
        &self,
        port: Option<u16>,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<DiscoveredPeer>, DomainError> {
        let port = port.unwrap_or(DEFAULT_DISCOVERY_PORT);
        Ok(discovery::scan(port, timeout_ms)
            .await?
            .into_iter()
            .filter(|peer| {
                crate::tls::self_connection_allowed(self.vault.fingerprint(), &peer.server_id)
            })
            .collect())
    }

    /// TCP fallback for overlay networks that drop UDP broadcast. The scan is
    /// an explicit user action, only covers local private subnets, and merges
    /// results into the same registry the passive listener feeds.
    pub fn tcp_discovery_settings(
        &self,
    ) -> Result<crate::types::TcpDiscoverySettings, DomainError> {
        self.vault
            .get_setting(crate::types::TCP_DISCOVERY_META)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|source| DomainError::Serialization {
                    operation: "read TCP discovery settings",
                    source,
                })
            })
            .transpose()
            .map(|settings| settings.unwrap_or_default())
    }

    pub async fn scan_lan_tcp(&self) -> Result<Vec<DiscoveredPeer>, DomainError> {
        let settings = self.tcp_discovery_settings()?;
        if !settings.enabled || settings.networks.is_empty() {
            return Ok(Vec::new());
        }
        let identity = self.vault.current_identity()?;
        let found = discovery::scan_tcp(&identity, DEFAULT_SERVER_PORT, &settings.networks)
            .await?
            .into_iter()
            // Discovery must never list this installation, even in debug
            // builds where self-connections are allowed for loopback tests.
            .filter(|peer| peer.server_id != self.vault.fingerprint())
            .collect::<Vec<_>>();
        if let Some(listener) = self.discovery_listener() {
            listener.merge_peers(&found);
        }
        for peer in &found {
            log::info!(
                "peerdav.peer TCP discovery found {} at {} (hosts: {:?})",
                crate::tls::short_fingerprint(&peer.server_id),
                peer.url,
                peer.hosts
            );
        }
        self.rebind_scanned_peers(&found)?;
        Ok(found)
    }

    /// TCP discovery is fingerprint-verified, so a saved peer that appears on
    /// a new address can be rebound immediately. This moves a peer from a
    /// stale saved URL to the address found on the current network.
    fn rebind_scanned_peers(&self, peers: &[DiscoveredPeer]) -> Result<(), DomainError> {
        if peers.is_empty() {
            return Ok(());
        }
        for mut record in self.vault.list_peer_records()? {
            let Some(machine_id) = record.machine_id.as_deref() else {
                continue;
            };
            let Some(candidate) = peers.iter().find(|peer| peer.server_id == machine_id) else {
                continue;
            };
            if record.url == candidate.url {
                continue;
            }
            // The scan uses the default port. Do not rewrite a saved custom
            // port just because the same identity also listens on 4918.
            let record_port = Url::parse(&record.url)
                .ok()
                .and_then(|url| url.port_or_known_default());
            if record_port != Some(candidate.port) {
                continue;
            }
            log::info!(
                "peerdav.peer rebinding {} from {} to {}",
                record.id,
                record.url,
                candidate.url
            );
            record.url = candidate.url.clone();
            self.vault.save_peer_record(&record)?;
        }
        Ok(())
    }

    pub fn outbound_connections(&self) -> Result<Vec<crate::types::ConnectionInfo>, DomainError> {
        use crate::types::{ConnectionInfo, ConnectionState};
        let peers = self.list_peers()?;
        let mut registry = self
            .outbound_connections
            .lock()
            .map_err(|_| DomainError::Internal {
                detail: "outbound connection registry poisoned".into(),
            })?;
        for row in registry.values_mut() {
            row.authorized = false;
            row.state = ConnectionState::Disconnected;
        }
        for server in self.discovered_peers() {
            let row = registry
                .entry(server.server_id.clone())
                .or_insert(ConnectionInfo {
                    fingerprint: server.server_id,
                    name: server.name.clone(),
                    address: server.url.clone(),
                    state: ConnectionState::Discovered,
                    authorized: false,
                    active_connections: 0,
                    last_seen: now_unix(),
                });
            row.name = server.name;
            row.address = server.url;
            row.state = ConnectionState::Discovered;
            row.last_seen = now_unix();
        }
        for entry in self.access_requests() {
            if let Some(fingerprint) = entry.fingerprint {
                let row = registry
                    .entry(fingerprint.clone())
                    .or_insert(ConnectionInfo {
                        fingerprint,
                        name: entry.label.clone().unwrap_or_else(|| entry.url.clone()),
                        address: entry.url,
                        state: ConnectionState::Requesting,
                        authorized: false,
                        active_connections: 0,
                        last_seen: now_unix(),
                    });
                if !row.authorized {
                    row.state = match entry.status {
                        OutboundAccessStatus::Waiting => ConnectionState::Pending,
                        OutboundAccessStatus::Canceled => ConnectionState::Canceled,
                        OutboundAccessStatus::Expired => ConnectionState::Expired,
                        OutboundAccessStatus::Failed => ConnectionState::Failed,
                        OutboundAccessStatus::Accessible => ConnectionState::Approved,
                    };
                }
            }
        }
        for peer in peers {
            if let Some(fingerprint) = peer.fingerprint {
                let row = registry
                    .entry(fingerprint.clone())
                    .or_insert(ConnectionInfo {
                        fingerprint,
                        name: peer.label.clone(),
                        address: peer.url.clone(),
                        state: ConnectionState::Approved,
                        authorized: true,
                        active_connections: 0,
                        last_seen: now_unix(),
                    });
                row.name = peer.label;
                row.address = peer.url;
                row.authorized = true;
                row.state = if peer.online {
                    ConnectionState::Connected
                } else {
                    ConnectionState::Disconnected
                };
            }
        }
        let mut rows: Vec<_> = registry.values().cloned().collect();
        rows.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        Ok(rows)
    }

    pub fn list_peers(&self) -> Result<Vec<PeerInfo>, DomainError> {
        let records = self.vault.list_peer_records()?;
        let announced = self.discovered_peers();
        let availability = self.availability_snapshot();
        Ok(records
            .into_iter()
            .map(|record| PeerInfo {
                fingerprint: record.machine_id.clone(),
                online: availability.get(&record.id).copied().unwrap_or(false)
                    || url_is_announced(&record.url, &announced),
                id: record.id,
                label: record.label,
                url: record.url,
                shares: Vec::new(),
            })
            .collect())
    }

    /// Asks a server to record this client as pending. The server's policy is
    /// still enforced: a peer denied by its access rules receives
    /// `PEER_ACCESS_DENIED` with the address the server saw, and nothing is
    /// approved automatically. The returned request id is used to claim the
    /// code after an administrator approves the request.
    ///
    /// 请求同时带上本机机器码：服务端按机器码而不是 IP 识别申请方，所以换网段、
    /// 重新分配地址之后仍然是同一条申请，不会在对方控台里变成一台新设备。
    pub async fn request_access(&self, url: &str) -> Result<PeerAccessRequest, DomainError> {
        let url = normalize_peer_url(url)?;
        let request_id = random_hex(16);
        let endpoint = format!("{url}{CATALOG_PATH}");
        let response = tokio::time::timeout(
            HEALTH_TIMEOUT,
            with_machine_id(
                self.agent_for(&url)?
                    .get(endpoint)
                    .header(ACCESS_REQUEST_HEADER, &request_id),
            )
            .send(),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "request access",
            status: None,
            detail: "the request timed out".to_owned(),
        })?
        .map_err(|error| map_transport("request access", &error))?;

        let status = response.status();
        if status.is_success() {
            return Ok(PeerAccessRequest {
                requested: false,
                request_id: None,
            });
        }
        if status == StatusCode::UNAUTHORIZED {
            return Ok(PeerAccessRequest {
                requested: true,
                request_id: Some(request_id),
            });
        }
        if status == StatusCode::FORBIDDEN {
            let peer_ip = response
                .headers()
                .get(OBSERVED_PEER_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            return Err(DomainError::PeerAccessDenied { peer_ip });
        }
        Err(DomainError::PeerRequest {
            operation: "request access",
            status: Some(status.as_u16()),
            detail: status
                .canonical_reason()
                .unwrap_or("server rejected the request")
                .to_owned(),
        })
    }

    /// Cancels a pending access request. The server removes its pending or
    /// queued grant first, then the waiting command exits through
    /// [`DomainError::PeerAccessCanceled`].
    pub async fn cancel_access(&self, url: &str, request_id: &str) -> Result<(), DomainError> {
        let url = normalize_peer_url(url)?;
        let request_id = request_id.trim();
        if !valid_access_request_id(request_id) {
            return Err(DomainError::PeerRequest {
                operation: "cancel access",
                status: None,
                detail: "the access request id is invalid".to_owned(),
            });
        }

        self.access_cancellations
            .lock()
            .await
            .insert(request_id.to_owned());
        let endpoint = format!("{url}{ACCESS_STATUS_PATH}");
        let response = tokio::time::timeout(
            HEALTH_TIMEOUT,
            with_machine_id(
                self.agent_for(&url)?
                    .delete(endpoint)
                    .header(ACCESS_REQUEST_HEADER, request_id),
            )
            .send(),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "cancel access",
            status: None,
            detail: "the request timed out".to_owned(),
        })
        .and_then(|response| response.map_err(|error| map_transport("cancel access", &error)));

        let response = match response {
            Ok(response) => response,
            Err(error) => {
                self.access_cancellations.lock().await.remove(request_id);
                return Err(error);
            }
        };

        let status = response.status();
        if status.is_success()
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::NOT_FOUND
            || status == StatusCode::GONE
        {
            return Ok(());
        }
        self.access_cancellations.lock().await.remove(request_id);
        Err(DomainError::PeerRequest {
            operation: "cancel access",
            status: Some(status.as_u16()),
            detail: status
                .canonical_reason()
                .unwrap_or("server rejected the cancellation")
                .to_owned(),
        })
    }

    /// 发起或复用一次出站访问申请，并把结果记在后端内存里。
    ///
    /// 同一地址已经在等待批准时直接返回既有记录：页面重载后前端会再次调用本方法，
    /// 不能因此朝对方重复登记一条申请。申请失败也记成终态返回，让列表能显示原因，
    /// 而不是只靠一次性的 toast，刷新页面后消失。
    pub async fn begin_access(
        &self,
        url: &str,
        label: Option<&str>,
    ) -> Result<OutboundAccessRequest, DomainError> {
        let url = normalize_peer_url(url)?;
        let label = label
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned);

        if let Some(existing) = self.outbound_entry(&url)
            && existing.request.status == OutboundAccessStatus::Waiting
        {
            return Ok(existing.request);
        }

        if let Ok(expected) = self.expected_identity(&url)
            && !crate::tls::self_connection_allowed(self.vault.fingerprint(), &expected)
        {
            return Err(DomainError::SelfConnectionForbidden);
        }
        let entry = match self.request_access(&url).await {
            Ok(request) if !request.requested => OutboundAccess {
                request: OutboundAccessRequest {
                    fingerprint: self.expected_identity(&url).ok(),
                    url: url.clone(),
                    label,
                    status: OutboundAccessStatus::Accessible,
                    message: None,
                    error: None,
                },
                request_id: None,
            },
            Ok(request) => match request.request_id {
                Some(request_id) => OutboundAccess {
                    request: OutboundAccessRequest {
                        fingerprint: self.expected_identity(&url).ok(),
                        url: url.clone(),
                        label,
                        status: OutboundAccessStatus::Waiting,
                        message: None,
                        error: None,
                    },
                    request_id: Some(request_id),
                },
                // 对方要求授权却没给申请标识：接不上审批结果，只能记为失败。
                None => {
                    let error = DomainError::PeerProtocol {
                        detail: "the server did not return an access request id".to_owned(),
                    };
                    settle_outbound_entry(url, label, access_failure(&error))
                }
            },
            Err(error) => settle_outbound_entry(url, label, access_failure(&error)),
        };
        self.store_outbound(&entry);
        Ok(entry.request.clone())
    }

    /// 等待审批结果并写回共享状态；成功时保存对端并清掉这条申请。
    pub async fn await_access(
        &self,
        url: &str,
        label: Option<&str>,
    ) -> Result<PeerInfo, DomainError> {
        let url = normalize_peer_url(url)?;
        let request_id = self
            .outbound_entry(&url)
            .and_then(|entry| entry.request_id)
            .ok_or(DomainError::PeerAccessExpired)?;

        let code = match self.wait_for_access(&url, &request_id).await {
            Ok(code) => code,
            Err(error) => {
                // 金库锁着只是暂时连不上，保留「等待批准」；解锁后前端会重新接上。
                if !matches!(error, DomainError::VaultLocked) {
                    self.settle_outbound(&url, access_failure(&error));
                }
                return Err(error);
            }
        };

        let code = Zeroizing::new(code);
        match self.connect(&url, code.as_str(), label).await {
            Ok(peer) => {
                self.clear_outbound(&url);
                Ok(peer)
            }
            Err(error) => {
                self.settle_outbound(&url, access_failure(&error));
                Err(error)
            }
        }
    }

    /// 取消等待中的申请，并把状态记为「已取消」。
    pub async fn cancel_access_request(
        &self,
        url: &str,
    ) -> Result<OutboundAccessRequest, DomainError> {
        let url = normalize_peer_url(url)?;
        let request_id = self.outbound_entry(&url).and_then(|entry| entry.request_id);
        if let Some(request_id) = request_id {
            self.cancel_access(&url, &request_id).await?;
        }

        let entry = match self.outbound_entry(&url) {
            Some(mut entry) => {
                entry.request.status = OutboundAccessStatus::Canceled;
                entry.request.message = None;
                entry.request_id = None;
                entry
            }
            None => OutboundAccess {
                request: OutboundAccessRequest {
                    fingerprint: self.expected_identity(&url).ok(),
                    url: url.clone(),
                    label: None,
                    status: OutboundAccessStatus::Canceled,
                    message: None,
                    error: None,
                },
                request_id: None,
            },
        };
        self.store_outbound(&entry);
        Ok(entry.request.clone())
    }

    /// 所有出站访问申请的快照。按地址排序，列表顺序因此不会随哈希顺序抖动。
    pub fn access_requests(&self) -> Vec<OutboundAccessRequest> {
        let mut entries: Vec<OutboundAccessRequest> = match self.outbound_access.lock() {
            Ok(registry) => registry
                .values()
                .map(|entry| entry.request.clone())
                .collect(),
            Err(_) => {
                log::warn!("outbound access registry lock poisoned");
                Vec::new()
            }
        };
        entries.sort_by(|left, right| left.url.cmp(&right.url));
        entries
    }

    fn outbound_entry(&self, url: &str) -> Option<OutboundAccess> {
        match self.outbound_access.lock() {
            Ok(registry) => registry.get(url).cloned(),
            Err(_) => {
                log::warn!("outbound access registry lock poisoned");
                None
            }
        }
    }

    fn store_outbound(&self, entry: &OutboundAccess) {
        match self.outbound_access.lock() {
            Ok(mut registry) => {
                registry.insert(entry.request.url.clone(), entry.clone());
            }
            Err(_) => log::warn!("outbound access registry lock poisoned while recording"),
        }
    }

    fn settle_outbound(&self, url: &str, settled: (OutboundAccessStatus, Option<AppError>)) {
        let entry = match self.outbound_entry(url) {
            Some(mut entry) => {
                entry.request.status = settled.0;
                entry.request.message = settled.1.as_ref().map(|error| error.message.clone());
                entry.request.error = settled.1;
                entry.request_id = None;
                entry
            }
            None => return,
        };
        self.store_outbound(&entry);
    }

    fn clear_outbound(&self, url: &str) {
        match self.outbound_access.lock() {
            Ok(mut registry) => {
                registry.remove(url);
            }
            Err(_) => log::warn!("outbound access registry lock poisoned while clearing"),
        }
    }

    /// Waits for an administrator to approve a request and returns the
    /// automatically delivered authorization code. Polling runs in the
    /// backend, so the frontend can show the pending state without keeping a
    /// long-lived HTTP connection or asking the user to copy a code.
    pub async fn wait_for_access(
        &self,
        url: &str,
        request_id: &str,
    ) -> Result<String, DomainError> {
        let request_id = request_id.trim();
        let result = self.wait_for_access_inner(url, request_id).await;
        self.access_cancellations.lock().await.remove(request_id);
        result
    }

    async fn wait_for_access_inner(
        &self,
        url: &str,
        request_id: &str,
    ) -> Result<String, DomainError> {
        let url = normalize_peer_url(url)?;
        if !valid_access_request_id(request_id) {
            return Err(DomainError::PeerRequest {
                operation: "wait for approval",
                status: None,
                detail: "the access request id is invalid".to_owned(),
            });
        }

        let endpoint = format!("{url}{ACCESS_STATUS_PATH}");
        let deadline = Instant::now() + self.limits.access_wait;
        loop {
            if self.access_cancellations.lock().await.contains(request_id) {
                return Err(DomainError::PeerAccessCanceled);
            }
            let response = tokio::time::timeout(
                HEALTH_TIMEOUT,
                with_machine_id(
                    self.agent_for(&url)?
                        .get(&endpoint)
                        .header(ACCESS_REQUEST_HEADER, request_id),
                )
                .send(),
            )
            .await
            .map_err(|_| DomainError::PeerRequest {
                operation: "wait for approval",
                status: None,
                detail: "the status request timed out".to_owned(),
            })?
            .map_err(|error| map_transport("wait for approval", &error))?;

            let status = response.status();
            if status == StatusCode::OK {
                let bytes =
                    read_response_limited(response, ACCESS_BODY_LIMIT, "read access status")
                        .await?;
                let parsed: AccessStatusResponse =
                    serde_json::from_slice(&bytes).map_err(|source| {
                        DomainError::Serialization {
                            operation: "decode access status",
                            source,
                        }
                    })?;
                if parsed.protocol != ACCESS_PROTOCOL {
                    return Err(DomainError::PeerProtocol {
                        detail: format!("unsupported access protocol `{}`", parsed.protocol),
                    });
                }
                match parsed.status {
                    AccessRequestStatus::Approved => {
                        let code = parsed.authorization_code.unwrap_or_default();
                        if code.is_empty() || code.chars().count() > MAX_AUTHORIZATION_CODE_CHARS {
                            return Err(DomainError::PeerProtocol {
                                detail: "the server delivered an invalid authorization code"
                                    .to_owned(),
                            });
                        }
                        if self.access_cancellations.lock().await.contains(request_id) {
                            if let Err(error) = self.revoke_authorization(&url, &code).await {
                                log::warn!(
                                    "failed to revoke an authorization after canceling the request: {error}"
                                );
                            }
                            return Err(DomainError::PeerAccessCanceled);
                        }
                        return Ok(code);
                    }
                    AccessRequestStatus::Pending => {}
                    AccessRequestStatus::Expired => return Err(DomainError::PeerAccessExpired),
                }
            } else if status == StatusCode::ACCEPTED {
                // The administrator has not approved the request yet.
            } else if status == StatusCode::NOT_FOUND {
                if self.access_cancellations.lock().await.contains(request_id) {
                    return Err(DomainError::PeerAccessCanceled);
                }
                return Err(DomainError::PeerAccessExpired);
            } else {
                if self.access_cancellations.lock().await.contains(request_id) {
                    return Err(DomainError::PeerAccessCanceled);
                }
                return Err(DomainError::PeerRequest {
                    operation: "wait for approval",
                    status: Some(status.as_u16()),
                    detail: status
                        .canonical_reason()
                        .unwrap_or("server rejected the status request")
                        .to_owned(),
                });
            }

            let now = Instant::now();
            if now >= deadline {
                if self.access_cancellations.lock().await.contains(request_id) {
                    return Err(DomainError::PeerAccessCanceled);
                }
                return Err(DomainError::PeerAccessTimeout);
            }
            let remaining = deadline.saturating_duration_since(now);
            tokio::time::sleep(PEER_ACCESS_POLL_INTERVAL.min(remaining)).await;
        }
    }

    /// Verifies the code against the catalog and saves the peer. The code is
    /// only persisted after the server accepts it.
    pub async fn connect(
        &self,
        url: &str,
        code: &str,
        label: Option<&str>,
    ) -> Result<PeerInfo, DomainError> {
        let url = normalize_peer_url(url)?;
        let code = code.trim();
        if code.is_empty() || code.chars().count() > MAX_AUTHORIZATION_CODE_CHARS {
            return Err(DomainError::PeerAuthorizationRejected);
        }

        let client = build_client(
            &self.agent_for(&url)?,
            &url,
            Auth::Basic(CLIENT_USERNAME.to_owned(), code.to_owned()),
        )?;
        let catalog = tokio::time::timeout(CONNECT_TIMEOUT, catalog(&client))
            .await
            .map_err(|_| DomainError::PeerRequest {
                operation: "read catalog",
                status: None,
                detail: "the connection timed out".to_owned(),
            })??;

        let existing =
            self.vault.list_peer_records()?.into_iter().find(|record| {
                record.machine_id == catalog.machine_id && record.machine_id.is_some()
            });
        let now = now_unix();
        let id = existing
            .as_ref()
            .map(|record| record.id.clone())
            .unwrap_or_else(|| random_hex(8));
        // 标签优先反映「对方设备的名称」：调用方给的名字（发现时的广播名）> 已保存的
        // 非默认标签 > 服务器在目录里自报的 server_name > URL 主机。地址只是兜底，
        // 否则手动输入 IP 连接成功后会显示成 `10.1.1.10:4918` 这类地址串。
        let label = match label.map(str::trim).filter(|label| !label.is_empty()) {
            Some(label) => validate_label(label)?,
            None => {
                let auto_label = default_label(&url);
                existing
                    .as_ref()
                    .map(|record| record.label.clone())
                    .filter(|label| *label != auto_label)
                    .or_else(|| validate_label(catalog.server_name.trim()).ok())
                    .unwrap_or(auto_label)
            }
        };
        let authorization_id = self
            .bind_authorization(&url, code)
            .await
            .unwrap_or_else(|error| {
                log::debug!("failed to bind a new peer authorization: {error}");
                None
            });
        let record = PeerRecord {
            id: id.clone(),
            label: label.clone(),
            url: url.clone(),
            authorization_id,
            // 记住对端机器码：解绑这台设备时用它清掉本机为它开过的入站授权。
            machine_id: catalog.machine_id.clone().or_else(|| {
                existing
                    .as_ref()
                    .and_then(|record| record.machine_id.clone())
            }),
            created_at: existing.as_ref().map_or(now, |record| record.created_at),
            last_connected_at: Some(now),
        };

        self.vault.save_peer_code(&id, &url, code)?;
        if let Err(error) = self.vault.save_peer_record(&record) {
            if let Err(cleanup) = self.vault.delete_peer_code(&id) {
                log::warn!("failed to roll back a peer secret: {cleanup}");
            }
            return Err(error);
        }
        self.set_peer_online(&id, true);

        Ok(PeerInfo {
            fingerprint: record.machine_id.clone(),
            id,
            label,
            url,
            online: true,
            shares: catalog.shares,
        })
    }

    /// Checks reachability and whether the stored code is still accepted.
    pub async fn check_health(&self, peer_id: &str) -> Result<PeerHealth, DomainError> {
        let (record, client) = self.peer_client(peer_id)?;
        let started = Instant::now();
        let result = tokio::time::timeout(HEALTH_TIMEOUT, catalog(&client)).await;
        let latency_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));

        let health = match result {
            Ok(Ok(_)) => PeerHealth {
                online: true,
                authorized: true,
                latency_ms,
                code: None,
                message: None,
                error: None,
                detail: None,
            },
            Ok(Err(error)) => {
                let app_error = AppError::from_ref(&error);
                let online = error_is_reachable(&error);
                PeerHealth {
                    online,
                    authorized: false,
                    latency_ms,
                    error: Some(app_error.clone()),
                    code: Some(app_error.code),
                    message: Some(app_error.message),
                    detail: app_error.detail,
                }
            }
            Err(_) => PeerHealth {
                error: Some(AppError::from_ref(&DomainError::PeerRequest {
                    operation: "check connection",
                    status: None,
                    detail: "connection timed out".into(),
                })),
                online: false,
                authorized: false,
                latency_ms,
                code: Some("PEER_UNREACHABLE".to_owned()),
                message: Some("服务器无响应 / The server did not respond.".to_owned()),
                detail: Some(format!(
                    "catalog request to {} timed out after {}s",
                    record.url,
                    HEALTH_TIMEOUT.as_secs()
                )),
            },
        };
        self.set_peer_online(peer_id, health.online);
        Ok(health)
    }

    /// Reloads the catalog for a saved peer, so the UI can browse after a restart
    /// without asking for the code again.
    pub async fn shares(&self, peer_id: &str) -> Result<Vec<PeerShare>, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let catalog = tokio::time::timeout(CONNECT_TIMEOUT, catalog(&client))
            .await
            .map_err(|_| DomainError::PeerRequest {
                operation: "read catalog",
                status: None,
                detail: "the connection timed out".to_owned(),
            })??;
        Ok(catalog.shares)
    }

    /// Removes a saved peer after asking its server to revoke the binding. The
    /// local record is removed when the server confirms removal or reports that
    /// the authorization no longer exists. A transport failure leaves the record
    /// intact so the user can retry.
    ///
    /// 返回被解绑设备的机器码，供调用方清掉本机为同一台设备开过的入站授权；
    /// 老记录没有机器码时返回 `None`。
    pub async fn forget(&self, peer_id: &str) -> Result<Option<String>, DomainError> {
        let record = self
            .vault
            .peer_record(peer_id)?
            .ok_or_else(|| DomainError::PeerNotFound {
                id: peer_id.to_owned(),
            })?;
        let code = Zeroizing::new(self.vault.load_peer_code(peer_id)?.code);
        match self.revoke_authorization(&record.url, code.as_str()).await {
            Ok(()) | Err(DomainError::PeerAuthorizationRejected) => {}
            Err(error) => return Err(error),
        }
        self.forget_local(peer_id).await?;
        Ok(record.machine_id)
    }

    /// Removes a peer only from this installation. This is used after the
    /// server has already revoked an authorization.
    pub async fn forget_local(&self, peer_id: &str) -> Result<(), DomainError> {
        if self.vault.peer_record(peer_id)?.is_none() {
            return Err(DomainError::PeerNotFound {
                id: peer_id.to_owned(),
            });
        }
        self.vault.delete_peer_code(peer_id)?;
        self.vault.delete_peer_record(peer_id)?;
        if let Ok(mut map) = self.availability.lock() {
            map.remove(peer_id);
        }
        self.edits
            .lock()
            .await
            .retain(|_, edit| edit.info.peer_id != peer_id);
        remove_dir_best_effort(&self.temp_root.join(peer_id));
        Ok(())
    }

    /// Reconciles every saved server with its authorization status. A revoked
    /// or displaced binding is removed locally and reported through the shared
    /// background event outlet.
    pub async fn sync_saved_peers(&self) -> Result<usize, DomainError> {
        if !self.vault.is_unlocked() {
            return Ok(0);
        }
        let records = self.vault.list_peer_records()?;
        let client_id = self.vault.ensure_client_id()?;
        let announced = self.discovered_peers();
        let mut removed = 0;

        for mut record in records {
            if let Some(candidate) = announced
                .iter()
                .find(|server| record.machine_id.as_deref() == Some(server.server_id.as_str()))
                && record.url != candidate.url
            {
                let agent = crate::tls::agent(
                    &self.vault,
                    candidate.server_id.as_str(),
                    self.limits.request_timeout,
                )?;
                let code = self.vault.load_peer_code(&record.id)?;
                let probe = build_client(
                    &agent,
                    &candidate.url,
                    Auth::Basic(CLIENT_USERNAME.into(), code.code),
                )?;
                let reachable = match catalog(&probe).await {
                    Ok(_) => true,
                    Err(error) => {
                        // 401/403 still proves the new address reaches the
                        // same TLS identity. Keeping the stale URL would make
                        // the next request fail with a transport error instead
                        // of showing the real authorization/policy reason.
                        let reachable = error_is_reachable(&error);
                        if !reachable {
                            log::debug!(
                                "peerdav.peer candidate {} is not reachable: {error}",
                                candidate.url
                            );
                        }
                        reachable
                    }
                };
                if reachable {
                    record.url = candidate.url.clone();
                    self.vault.save_peer_record(&record)?;
                }
            }
            let recently_announced = url_is_announced(&record.url, &announced);
            // Broadcasts are the primary heartbeat. A known-offline server is
            // skipped entirely and waits for its next announcement; unknown or
            // online servers use this sync as the lightweight HTTP probe.
            if !recently_announced && !self.should_probe(&record.id) {
                continue;
            }
            let Ok(secret) = self.vault.load_peer_code(&record.id) else {
                self.set_peer_online(&record.id, false);
                continue;
            };
            let code = Zeroizing::new(secret.code);
            match self
                .fetch_authorization_status(&record.url, code.as_str())
                .await
            {
                Ok(AuthorizationOutcome::Active(status)) => {
                    self.set_peer_online(&record.id, true);
                    let expected = record.authorization_id.as_deref();
                    if expected != Some(status.authorization_id.as_str()) {
                        self.vault
                            .set_peer_authorization_id(&record.id, &status.authorization_id)?;
                    }
                    if let Some(bound) = status.client_id.as_deref()
                        && bound != client_id
                    {
                        self.remove_saved_peer(record, "bound-elsewhere").await?;
                        removed += 1;
                        continue;
                    }
                    if status.client_id.is_none() {
                        self.bind_saved_peer(&record.id, &record.url, code.as_str(), &client_id)
                            .await;
                    }
                }
                Ok(AuthorizationOutcome::Revoked)
                | Ok(AuthorizationOutcome::Removed)
                | Err(DomainError::PeerAuthorizationRejected) => {
                    self.remove_saved_peer(record, "revoked").await?;
                    removed += 1;
                }
                Ok(AuthorizationOutcome::Unsupported) => {
                    self.set_peer_online(&record.id, true);
                }
                Err(error) => {
                    // A fresh broadcast still proves the server is reachable
                    // even if this particular request failed.
                    self.set_peer_online(
                        &record.id,
                        error_is_reachable(&error) || recently_announced,
                    );
                    // An offline server is a normal transient state. Only
                    // unexpected protocol or local failures are surfaced.
                    if !matches!(
                        error,
                        DomainError::PeerRequest { .. } | DomainError::PeerProtocol { .. }
                    ) {
                        return Err(error);
                    }
                    log::debug!(
                        "peerdav.peer synchronization skipped {}: {error}",
                        record.url
                    );
                }
            }
        }

        Ok(removed)
    }

    async fn bind_saved_peer(&self, peer_id: &str, url: &str, code: &str, client_id: &str) {
        match self.post_authorization_binding(url, code, client_id).await {
            Ok(Some(binding)) => {
                if let Err(error) = self
                    .vault
                    .set_peer_authorization_id(peer_id, &binding.authorization_id)
                {
                    log::warn!("failed to save a peer authorization binding: {error}");
                }
            }
            Ok(None) => {}
            Err(error) => log::debug!("peerdav.peer authorization bind failed: {error}"),
        }
    }

    async fn remove_saved_peer(&self, record: PeerRecord, reason: &str) -> Result<(), DomainError> {
        self.forget_local(&record.id).await?;
        self.events.peer_revoked(&PeerRevocationInfo {
            id: record.id,
            label: record.label,
            url: record.url,
            reason: reason.to_owned(),
        });
        Ok(())
    }

    /// Drops temporary external-edit copies, e.g. when the vault locks.
    pub async fn clear_temp(&self) {
        self.edits.lock().await.clear();
        remove_dir_best_effort(&self.temp_root);
        if let Err(error) = create_private_dir(&self.temp_root) {
            log::error!("failed to recreate the peer temp directory: {error}");
        }
    }

    /// Best-effort synchronous cleanup for application exit.
    pub fn clear_temp_now(&self) {
        if let Ok(mut edits) = self.edits.try_lock() {
            edits.clear();
        } else {
            log::warn!("peer edit map was busy during shutdown cleanup");
        }
        remove_dir_best_effort(&self.temp_root);
    }

    async fn bind_authorization(
        &self,
        url: &str,
        code: &str,
    ) -> Result<Option<String>, DomainError> {
        let client_id = self.vault.ensure_client_id()?;
        Ok(self
            .post_authorization_binding(url, code, &client_id)
            .await?
            .map(|binding| binding.authorization_id))
    }

    async fn post_authorization_binding(
        &self,
        url: &str,
        code: &str,
        client_id: &str,
    ) -> Result<Option<AuthorizationBindingResponse>, DomainError> {
        let endpoint = format!("{}{AUTHORIZATION_PATH}", normalize_peer_url(url)?);
        let response = tokio::time::timeout(
            HEALTH_TIMEOUT,
            with_machine_id(
                self.agent_for(url)?
                    .post(endpoint)
                    .basic_auth(CLIENT_USERNAME, Some(code))
                    .header(CLIENT_ID_HEADER, client_id),
            )
            .send(),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "bind authorization",
            status: None,
            detail: "the request timed out".to_owned(),
        })?
        .map_err(|error| map_transport("bind authorization", &error))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(None);
        }
        if !status.is_success() {
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::CONFLICT {
                return Ok(None);
            }
            return Err(DomainError::PeerRequest {
                operation: "bind authorization",
                status: Some(status.as_u16()),
                detail: status
                    .canonical_reason()
                    .unwrap_or("server rejected the authorization binding")
                    .to_owned(),
            });
        }

        let bytes =
            read_response_limited(response, AUTHORIZATION_BODY_LIMIT, "decode binding").await?;
        let binding: AuthorizationBindingResponse =
            serde_json::from_slice(&bytes).map_err(|source| DomainError::Serialization {
                operation: "decode authorization binding",
                source,
            })?;
        if binding.protocol != AUTHORIZATION_PROTOCOL {
            return Err(DomainError::PeerProtocol {
                detail: format!("unsupported authorization protocol `{}`", binding.protocol),
            });
        }
        Ok(Some(binding))
    }

    async fn revoke_authorization(&self, url: &str, code: &str) -> Result<(), DomainError> {
        let endpoint = format!("{}{AUTHORIZATION_PATH}", normalize_peer_url(url)?);
        let response = tokio::time::timeout(
            HEALTH_TIMEOUT,
            self.agent_for(url)?
                .delete(endpoint)
                .basic_auth(CLIENT_USERNAME, Some(code))
                .send(),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "revoke authorization",
            status: None,
            detail: "the request timed out".to_owned(),
        })?
        .map_err(|error| map_transport("revoke authorization", &error))?;

        let status = response.status();
        if status.is_success()
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::NOT_FOUND
            || status == StatusCode::GONE
        {
            return Ok(());
        }
        Err(DomainError::PeerRequest {
            operation: "revoke authorization",
            status: Some(status.as_u16()),
            detail: status
                .canonical_reason()
                .unwrap_or("server rejected the authorization revocation")
                .to_owned(),
        })
    }

    async fn fetch_authorization_status(
        &self,
        url: &str,
        code: &str,
    ) -> Result<AuthorizationOutcome, DomainError> {
        let endpoint = format!("{}{AUTHORIZATION_PATH}", normalize_peer_url(url)?);
        let response = tokio::time::timeout(
            HEALTH_TIMEOUT,
            self.agent_for(url)?
                .get(endpoint)
                .basic_auth(CLIENT_USERNAME, Some(code))
                .send(),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "check authorization",
            status: None,
            detail: "the request timed out".to_owned(),
        })?
        .map_err(|error| map_transport("check authorization", &error))?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Ok(AuthorizationOutcome::Removed);
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(AuthorizationOutcome::Unsupported);
        }
        if status != StatusCode::OK && status != StatusCode::GONE {
            return Err(DomainError::PeerRequest {
                operation: "check authorization",
                status: Some(status.as_u16()),
                detail: status
                    .canonical_reason()
                    .unwrap_or("server rejected the authorization status request")
                    .to_owned(),
            });
        }

        let bytes =
            read_response_limited(response, AUTHORIZATION_BODY_LIMIT, "decode status").await?;
        let parsed: AuthorizationStatusResponse =
            serde_json::from_slice(&bytes).map_err(|source| DomainError::Serialization {
                operation: "decode authorization status",
                source,
            })?;
        if parsed.protocol != AUTHORIZATION_PROTOCOL {
            return Err(DomainError::PeerProtocol {
                detail: format!("unsupported authorization protocol `{}`", parsed.protocol),
            });
        }
        match parsed.status {
            AuthorizationStatus::Active => Ok(AuthorizationOutcome::Active(parsed)),
            AuthorizationStatus::Revoked => Ok(AuthorizationOutcome::Revoked),
        }
    }

    pub async fn list_dir(
        &self,
        peer_id: &str,
        share_id: &str,
        path: &str,
    ) -> Result<PeerListing, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let display = normalize_display_path(path)?;
        let remote = encode_share_path(share_id, &display)?;

        let entities = list_entities(
            &client,
            &remote,
            Depth::Number(1),
            "list directory",
            &display,
        )
        .await?;

        let mut entries = Vec::new();
        for entity in entities {
            let Some(entry) = listing_entry(entity, share_id, &display) else {
                continue;
            };
            entries.push(entry);
        }
        entries.sort_by(|left, right| {
            right
                .is_dir
                .cmp(&left.is_dir)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        let truncated = entries.len() > MAX_DIR_ENTRIES;
        entries.truncate(MAX_DIR_ENTRIES);

        Ok(PeerListing {
            share_id: share_id.to_owned(),
            path: display,
            entries,
            truncated,
        })
    }

    /// Recursively scans a remote directory and creates the matching local
    /// directory tree. The returned files are ordered by relative path so the
    /// UI can download them sequentially with visible queue progress.
    pub async fn prepare_directory_download(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
        local_parent: &str,
        conflict: ConflictPolicy,
    ) -> Result<DirectoryDownloadPlan, DomainError> {
        let (display, _, folder_name) = remote_file_target(share_id, remote_path)?;
        let parent = canonical_local_dir(local_parent).await?;
        let target_root = parent.join(&folder_name);

        let root_exists = match tokio::fs::symlink_metadata(&target_root).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io_error(
                    "inspect download directory",
                    &target_root,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "refusing to use a symbolic link as the download directory",
                    ),
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(DomainError::TransferConflict {
                    path: target_root.display().to_string(),
                });
            }
            Ok(_) if conflict != ConflictPolicy::Overwrite => {
                return Err(DomainError::TransferConflict {
                    path: target_root.display().to_string(),
                });
            }
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(io_error("inspect download directory", &target_root, error));
            }
        };

        let mut directories = Vec::new();
        let mut remote_files = Vec::new();
        let mut pending = vec![(display, String::new(), Vec::<IgnoreLayer>::new())];
        while let Some((directory, relative_parent, mut layers)) = pending.pop() {
            let listing = self.list_dir(peer_id, share_id, &directory).await?;
            if listing.truncated {
                return Err(DomainError::DownloadQueueTooLarge {
                    count: MAX_DIRECTORY_DOWNLOAD_FILES + 1,
                    limit: MAX_DIRECTORY_DOWNLOAD_FILES,
                });
            }
            if let Some(ignore_entry) = listing
                .entries
                .iter()
                .find(|entry| !entry.is_dir && entry.name == ".gitignore")
                && let Some(content) = self
                    .read_remote_ignore_file(peer_id, share_id, &ignore_entry.path)
                    .await?
                && let Some(matcher) = build_gitignore(&content)
            {
                layers.push(IgnoreLayer {
                    base: directory.clone(),
                    matcher,
                });
            }
            for entry in listing.entries {
                let name = sanitize_file_name(&entry.name)?;
                if is_path_ignored(&entry.path, &name, entry.is_dir, &layers) {
                    continue;
                }
                let relative = if relative_parent.is_empty() {
                    name
                } else {
                    format!("{relative_parent}/{name}")
                };
                if directories.len() + remote_files.len() >= MAX_DIRECTORY_DOWNLOAD_FILES {
                    return Err(DomainError::DownloadQueueTooLarge {
                        count: directories.len() + remote_files.len() + 1,
                        limit: MAX_DIRECTORY_DOWNLOAD_FILES,
                    });
                }
                if entry.is_dir {
                    directories.push(relative.clone());
                    pending.push((entry.path, relative, layers.clone()));
                } else {
                    remote_files.push((entry.path, relative, entry.size));
                }
            }
        }

        if !root_exists {
            tokio::fs::create_dir(&target_root)
                .await
                .map_err(|error| io_error("create download directory", &target_root, error))?;
        }
        let target_root = canonical_local_dir(target_root.to_str().ok_or_else(|| {
            DomainError::RemotePathInvalid {
                detail: "the download directory is not valid UTF-8".to_owned(),
            }
        })?)
        .await?;

        directories.sort();
        remote_files.sort_by(|left, right| left.1.cmp(&right.1));
        for relative in &directories {
            ensure_local_download_dir(&target_root, relative).await?;
        }

        let mut files = Vec::with_capacity(remote_files.len());
        for (remote_path, relative_path, size) in remote_files {
            files.push(DownloadCandidate {
                remote_path,
                local_path: local_download_path(&target_root, &relative_path)?
                    .display()
                    .to_string(),
                relative_path,
                size,
            });
        }

        Ok(DirectoryDownloadPlan {
            local_root: target_root.display().to_string(),
            directory_count: directories.len(),
            files,
        })
    }

    async fn read_remote_ignore_file(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
    ) -> Result<Option<String>, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let (display, remote, _) = remote_file_target(share_id, remote_path)?;
        let response = client
            .get_raw(&remote)
            .await
            .map_err(|error| map_dav_error("read .gitignore", error, &display))?;
        ensure_status(response.status(), "read .gitignore", &display)?;
        let bytes =
            read_response_limited(response, MAX_IGNORE_FILE_BYTES, "read .gitignore").await?;
        match String::from_utf8(bytes) {
            Ok(content) => Ok(Some(content)),
            Err(error) => {
                log::warn!("ignoring a non-UTF-8 .gitignore at {display}: {error}");
                Ok(None)
            }
        }
    }

    pub async fn delete_entry(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
    ) -> Result<(), DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let display = normalize_display_path(remote_path)?;
        if display == "/" {
            return Err(DomainError::RemotePathInvalid {
                detail: "the shared root cannot be deleted".to_owned(),
            });
        }
        let remote = encode_share_path(share_id, &display)?;
        client
            .delete(&remote)
            .await
            .map_err(|error| map_dav_error("delete entry", error, &display))?;
        Ok(())
    }

    pub async fn download(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
        local_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferResult, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let (display, remote, file_name) = remote_file_target(share_id, remote_path)?;
        let dir = canonical_local_dir(local_dir).await?;
        let destination = resolve_local_destination(&dir, &file_name, conflict).await?;

        let written = self
            .resumable_download(&client, &remote, &display, &destination, conflict, None)
            .await?;

        Ok(TransferResult {
            transferred: true,
            remote_path: display,
            local_path: Some(destination.display().to_string()),
            bytes: written,
            skipped_reason: None,
        })
    }

    /// Downloads one file to the exact path chosen in a native save dialog.
    /// The save dialog owns the overwrite confirmation, so the destination is
    /// replaced atomically after the complete download succeeds.
    pub async fn download_to(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<TransferResult, DomainError> {
        self.download_to_with_progress(peer_id, share_id, remote_path, local_path, None)
            .await
    }

    pub async fn download_to_with_progress(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
        local_path: &str,
        progress: Option<TransferProgressCallback>,
    ) -> Result<TransferResult, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let (display, remote, _) = remote_file_target(share_id, remote_path)?;
        let (_, destination) = resolve_local_file_destination(local_path).await?;
        let written = self
            .resumable_download(
                &client,
                &remote,
                &display,
                &destination,
                ConflictPolicy::Overwrite,
                progress,
            )
            .await?;

        Ok(TransferResult {
            transferred: true,
            remote_path: display,
            local_path: Some(destination.display().to_string()),
            bytes: written,
            skipped_reason: None,
        })
    }

    async fn resumable_download(
        &self,
        client: &Client,
        remote: &str,
        display: &str,
        destination: &Path,
        conflict: ConflictPolicy,
        progress: Option<TransferProgressCallback>,
    ) -> Result<u64, DomainError> {
        crate::transfer::download(
            client,
            remote,
            display,
            destination,
            conflict,
            self.limits.max_transfer_bytes,
            progress,
        )
        .await
    }

    pub async fn begin_upload(&self) -> String {
        let id = random_hex(16);
        let (sender, _) = tokio::sync::watch::channel(false);
        self.upload_cancellations
            .lock()
            .await
            .insert(id.clone(), sender);
        id
    }

    pub async fn cancel_upload(&self, transfer_id: &str) {
        if let Some(sender) = self.upload_cancellations.lock().await.get(transfer_id) {
            sender.send_replace(true);
        }
    }

    pub async fn upload_with_progress(
        &self,
        peer_id: &str,
        share_id: &str,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
        transfer_id: &str,
        progress: crate::types::UploadProgressCallback,
    ) -> Result<TransferResult, DomainError> {
        let mut cancel = self
            .upload_cancellations
            .lock()
            .await
            .get(transfer_id)
            .map(|sender| sender.subscribe())
            .ok_or_else(|| DomainError::PeerProtocol {
                detail: "unknown upload transfer".into(),
            })?;
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_for_progress = Arc::clone(&started);
        let tracked_progress: crate::types::UploadProgressCallback =
            Arc::new(move |phase, bytes, total, sent| {
                started_for_progress.store(true, std::sync::atomic::Ordering::SeqCst);
                progress(phase, bytes, total, sent);
            });
        let result = if *cancel.borrow() {
            Err(DomainError::TransferCanceled)
        } else {
            let uploading = self.upload_inner(
                peer_id,
                share_id,
                local_path,
                remote_dir,
                conflict,
                Some(tracked_progress),
                Some((transfer_id.to_owned(), cancel.clone())),
            );
            tokio::pin!(uploading);
            tokio::select! {
                biased;
                _ = cancel.changed() => {
                    if started.load(std::sync::atomic::Ordering::SeqCst) {
                        // The transfer owns remote cancellation and cleanup once
                        // it starts. Do not drop its cancellation handshake.
                        uploading.await
                    } else {
                        Err(DomainError::TransferCanceled)
                    }
                },
                result = &mut uploading => result,
            }
        };
        self.upload_cancellations.lock().await.remove(transfer_id);
        result
    }

    pub async fn upload(
        &self,
        peer_id: &str,
        share_id: &str,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferResult, DomainError> {
        self.upload_inner(
            peer_id, share_id, local_path, remote_dir, conflict, None, None,
        )
        .await
    }

    async fn upload_inner(
        &self,
        peer_id: &str,
        share_id: &str,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
        progress: Option<crate::types::UploadProgressCallback>,
        cancellation: Option<(String, tokio::sync::watch::Receiver<bool>)>,
    ) -> Result<TransferResult, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let local = canonical_local_file(local_path).await?;
        let size = tokio::fs::metadata(&local)
            .await
            .map_err(|error| io_error("inspect local file", &local, error))?
            .len();
        if size > self.limits.max_transfer_bytes {
            return Err(DomainError::TransferTooLarge {
                size,
                limit: self.limits.max_transfer_bytes,
            });
        }
        let file_name = local
            .file_name()
            .and_then(|name| name.to_str())
            .map(sanitize_file_name)
            .transpose()?
            .ok_or_else(|| DomainError::RemotePathInvalid {
                detail: "the local file has no usable name".to_owned(),
            })?;

        let directory = normalize_display_path(remote_dir)?;
        let directory_encoded = encode_share_path(share_id, &directory)?;
        ensure_remote_directories(&client, share_id, &directory).await?;
        let (remote, display) = resolve_remote_destination(
            &client,
            &directory_encoded,
            &directory,
            &file_name,
            conflict,
        )
        .await?;

        crate::transfer::upload(&client, &remote, &local, size, progress, cancellation).await?;

        Ok(TransferResult {
            transferred: true,
            remote_path: display,
            local_path: Some(local.display().to_string()),
            bytes: size,
            skipped_reason: None,
        })
    }

    /// Reads a file through the backend and returns it over IPC. Images and
    /// PDFs are base64-encoded; text-like files are UTF-8 and must stay within
    /// the configured line limit.
    pub async fn preview(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
    ) -> Result<PreviewPayload, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let (display, remote, file_name) = remote_file_target(share_id, remote_path)?;

        let response = client
            .get_raw(&remote)
            .await
            .map_err(|error| map_dav_error("preview file", error, &display))?;
        ensure_status(response.status(), "preview file", &display)?;
        let mut bytes =
            read_response_limited(response, self.limits.max_preview_bytes, "preview file")
                .await
                .map_err(|error| match error {
                    DomainError::TransferTooLarge { size, limit } => {
                        DomainError::PreviewTooLarge { size, limit }
                    }
                    other => other,
                })?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mime = mime_for(&file_name).to_owned();
        let text_preview = is_text_file(&file_name);

        if text_preview {
            match String::from_utf8(bytes) {
                Ok(text) => {
                    let lines = text.lines().count();
                    if lines > MAX_PREVIEW_LINES {
                        return Err(DomainError::PreviewTooManyLines {
                            lines,
                            limit: MAX_PREVIEW_LINES,
                        });
                    }
                    return Ok(PreviewPayload {
                        file_name,
                        mime,
                        encoding: "utf8".to_owned(),
                        data: text,
                        size,
                    });
                }
                Err(error) => bytes = error.into_bytes(),
            }
        }

        Ok(PreviewPayload {
            file_name,
            mime: if text_preview {
                "application/octet-stream".to_owned()
            } else {
                mime
            },
            encoding: "base64".to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            size,
        })
    }

    /// Downloads to a controlled temporary directory and records a baseline.
    pub async fn prepare_external_edit(
        &self,
        peer_id: &str,
        share_id: &str,
        remote_path: &str,
    ) -> Result<ExternalEditInfo, DomainError> {
        let (_, client) = self.peer_client(peer_id)?;
        let (display, remote, file_name) = remote_file_target(share_id, remote_path)?;
        let meta = remote_meta(&client, &remote, &display).await?;
        if meta.size > self.limits.max_transfer_bytes {
            return Err(DomainError::TransferTooLarge {
                size: meta.size,
                limit: self.limits.max_transfer_bytes,
            });
        }

        let edit_id = random_hex(8);
        let directory = self.temp_root.join(peer_id).join(&edit_id);
        create_private_dir(&directory)?;
        let local_path = directory.join(&file_name);

        let result = async {
            let response = client
                .get_raw(&remote)
                .await
                .map_err(|error| map_dav_error("prepare external edit", error, &display))?;
            ensure_status(response.status(), "prepare external edit", &display)?;
            write_response_to_path(
                response,
                &local_path,
                self.limits.max_transfer_bytes,
                "prepare external edit",
                None,
            )
            .await
        }
        .await;

        let (size, baseline_hash) = match result {
            Ok(value) => value,
            Err(error) => {
                remove_dir_best_effort(&directory);
                return Err(error);
            }
        };

        let info = ExternalEditInfo {
            edit_id,
            peer_id: peer_id.to_owned(),
            share_id: share_id.to_owned(),
            remote_path: display,
            local_path: local_path.display().to_string(),
            file_name,
            size,
            opened_at: now_unix(),
        };
        self.edits.lock().await.insert(
            info.edit_id.clone(),
            ExternalEdit {
                info: info.clone(),
                baseline_hash,
                baseline_size: size,
                baseline_modified: meta.modified,
                baseline_etag: meta.etag,
            },
        );
        Ok(info)
    }

    /// Uploads an edited temporary file back after re-authorizing the request.
    pub async fn commit_external_edit(
        &self,
        edit_id: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferResult, DomainError> {
        let edit = self.edits.lock().await.remove(edit_id).ok_or_else(|| {
            DomainError::ExternalEditNotFound {
                id: edit_id.to_owned(),
            }
        })?;

        let result = self.commit_inner(&edit, conflict).await;
        match result {
            Ok(outcome) => {
                if outcome.transferred {
                    if let Some(parent) = Path::new(&edit.info.local_path).parent() {
                        remove_dir_best_effort(parent);
                    }
                } else {
                    self.edits.lock().await.insert(edit_id.to_owned(), edit);
                }
                Ok(outcome)
            }
            Err(error) => {
                self.edits.lock().await.insert(edit_id.to_owned(), edit);
                Err(error)
            }
        }
    }

    async fn commit_inner(
        &self,
        edit: &ExternalEdit,
        conflict: ConflictPolicy,
    ) -> Result<TransferResult, DomainError> {
        let local = PathBuf::from(&edit.info.local_path);
        let metadata = tokio::fs::symlink_metadata(&local)
            .await
            .map_err(|error| io_error("inspect external edit", &local, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(DomainError::Io {
                operation: "inspect external edit",
                path: Some(local.clone()),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the edit path is not a regular file",
                ),
            });
        }
        if metadata.len() > self.limits.max_transfer_bytes {
            return Err(DomainError::TransferTooLarge {
                size: metadata.len(),
                limit: self.limits.max_transfer_bytes,
            });
        }

        let hash = hash_file(&local).await?;
        if hash == edit.baseline_hash {
            return Ok(TransferResult {
                transferred: false,
                remote_path: edit.info.remote_path.clone(),
                local_path: Some(edit.info.local_path.clone()),
                bytes: 0,
                skipped_reason: Some("unchanged".to_owned()),
            });
        }

        let (_, client) = self.peer_client(&edit.info.peer_id)?;
        let remote = encode_share_path(&edit.info.share_id, &edit.info.remote_path)?;
        match remote_meta(&client, &remote, &edit.info.remote_path).await {
            Ok(meta) => {
                let changed = match (&meta.etag, &edit.baseline_etag) {
                    (Some(current), Some(baseline)) => current != baseline,
                    _ => meta.size != edit.baseline_size || meta.modified != edit.baseline_modified,
                };
                if changed && conflict != ConflictPolicy::Overwrite {
                    return Err(DomainError::TransferConflict {
                        path: edit.info.remote_path.clone(),
                    });
                }
            }
            Err(DomainError::RemoteNotFound { .. }) => {}
            Err(error) => return Err(error),
        }

        let file = tokio::fs::File::open(&local)
            .await
            .map_err(|error| io_error("open external edit", &local, error))?;
        tokio::time::timeout(
            TRANSFER_TIMEOUT,
            client.put(&remote, reqwest::Body::from(file)),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "commit external edit",
            status: None,
            detail: "the upload timed out".to_owned(),
        })?
        .map_err(|error| map_dav_error("commit external edit", error, &edit.info.remote_path))?;

        Ok(TransferResult {
            transferred: true,
            remote_path: edit.info.remote_path.clone(),
            local_path: Some(edit.info.local_path.clone()),
            bytes: metadata.len(),
            skipped_reason: None,
        })
    }

    pub async fn discard_external_edit(&self, edit_id: &str) -> Result<(), DomainError> {
        let edit = self.edits.lock().await.remove(edit_id).ok_or_else(|| {
            DomainError::ExternalEditNotFound {
                id: edit_id.to_owned(),
            }
        })?;
        if let Some(parent) = Path::new(&edit.info.local_path).parent() {
            remove_dir_best_effort(parent);
        }
        Ok(())
    }

    pub fn set_pairing_fingerprint(&self, url: &str, fingerprint: &str) -> Result<(), DomainError> {
        let url = normalize_peer_url(url)?;
        let fingerprint =
            crate::tls::normalize_fingerprint(Some(fingerprint)).ok_or_else(|| {
                DomainError::PeerProtocol {
                    detail: "The device fingerprint must contain 64 hexadecimal characters.".into(),
                }
            })?;
        if !crate::tls::self_connection_allowed(self.vault.fingerprint(), &fingerprint) {
            return Err(DomainError::SelfConnectionForbidden);
        }
        if let Some(saved) = self
            .vault
            .list_peer_records()?
            .iter()
            .find(|peer| peer.url == url)
            && saved.machine_id.as_deref() != Some(fingerprint.as_str())
        {
            return Err(DomainError::PeerProtocol { detail: "The fingerprint differs from the paired device; unbind it before pairing a new identity.".into() });
        }
        self.pairing_pins
            .lock()
            .map_err(|_| DomainError::Internal {
                detail: "pairing registry lock poisoned".into(),
            })?
            .insert(url, fingerprint);
        Ok(())
    }

    fn agent_for(&self, url: &str) -> Result<reqwest::Client, DomainError> {
        crate::tls::agent(
            &self.vault,
            &self.expected_identity(url)?,
            self.limits.request_timeout,
        )
    }

    fn expected_identity(&self, url: &str) -> Result<String, DomainError> {
        let expected = self.vault.list_peer_records()?.into_iter()
            .find(|peer| peer.url == url).and_then(|peer| peer.machine_id)
            .or_else(|| self.pairing_pins.lock().ok().and_then(|pins| pins.get(url).cloned()))
            .or_else(|| self.discovered_peers().into_iter()
                .find(|peer| peer.url == url).map(|peer| peer.server_id))
            .or_else(|| url::Url::parse(url).ok().filter(|url| {
                matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
            }).map(|_| self.vault.identity().fingerprint.clone()))
            .ok_or_else(|| DomainError::PeerProtocol {
                detail: "No verified server fingerprint is available. Select a discovered server or enter its fingerprint before pairing.".into(),
            })?;
        Ok(expected)
    }

    fn peer_client(&self, peer_id: &str) -> Result<(PeerRecord, Client), DomainError> {
        let record = self
            .vault
            .peer_record(peer_id)?
            .ok_or_else(|| DomainError::PeerNotFound {
                id: peer_id.to_owned(),
            })?;
        let secret = self
            .vault
            .load_peer_code(peer_id)
            .map_err(|error| match error {
                DomainError::SecretNotFound { .. } | DomainError::InvalidSecretName => {
                    DomainError::PeerNotFound {
                        id: peer_id.to_owned(),
                    }
                }
                other => other,
            })?;
        let client = build_client(
            &self.agent_for(&record.url)?,
            &record.url,
            Auth::Basic(CLIENT_USERNAME.to_owned(), secret.code),
        )?;
        Ok((record, client))
    }
}

fn build_client(agent: &reqwest::Client, host: &str, auth: Auth) -> Result<Client, DomainError> {
    ClientBuilder::new()
        .set_agent(agent.clone())
        .set_host(host.to_owned())
        .set_auth(auth)
        .build()
        .map_err(|error| DomainError::PeerProtocol {
            detail: format!("failed to build a WebDAV client: {error}"),
        })
}

/// 给 PeerDav 协议请求带上本机机器码。取不到机器码时不加这个头：服务端会把
/// 申请身份退回 IP，老行为保持不变，而不是拒绝请求。
fn with_machine_id(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder
}

async fn catalog(client: &Client) -> Result<CatalogResponse, DomainError> {
    let response = client
        .start_request(reqwest::Method::GET, CATALOG_PATH)
        .await
        .map_err(|error| map_dav_error("read catalog", error, ""))?
        .send()
        .await
        .map_err(|error| map_transport("read catalog", &error))?;

    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return Err(DomainError::PeerAuthorizationRejected);
    }
    if !status.is_success() {
        return Err(DomainError::PeerRequest {
            operation: "read catalog",
            status: Some(status.as_u16()),
            detail: status
                .canonical_reason()
                .unwrap_or("server rejected")
                .to_owned(),
        });
    }

    let bytes = read_response_limited(response, CATALOG_BODY_LIMIT, "read catalog").await?;
    let catalog: CatalogResponse =
        serde_json::from_slice(&bytes).map_err(|error| DomainError::PeerProtocol {
            detail: format!("invalid catalog JSON: {error}"),
        })?;
    if catalog.protocol != CATALOG_PROTOCOL {
        return Err(DomainError::PeerProtocol {
            detail: format!(
                "unexpected catalog protocol `{}`; expected `{CATALOG_PROTOCOL}`",
                catalog.protocol
            ),
        });
    }
    Ok(catalog)
}

fn normalize_peer_url(raw: &str) -> Result<String, DomainError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::InvalidPeerUrl {
            detail: "the address is empty".to_owned(),
        });
    }
    let url = Url::parse(trimmed).map_err(|error| DomainError::InvalidPeerUrl {
        detail: format!("`{trimmed}` is not a valid URL: {error}"),
    })?;
    if url.scheme() != "https" {
        return Err(DomainError::InvalidPeerUrl {
            detail: "PeerDav requires an encrypted https:// connection".to_owned(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DomainError::InvalidPeerUrl {
            detail: "credentials must not be embedded in the URL".to_owned(),
        });
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(DomainError::InvalidPeerUrl {
            detail: "query strings and fragments are not supported".to_owned(),
        });
    }
    if !url.path().is_empty() && url.path() != "/" {
        return Err(DomainError::InvalidPeerUrl {
            detail: "the address must point at the server root".to_owned(),
        });
    }
    let host = url.host_str().ok_or_else(|| DomainError::InvalidPeerUrl {
        detail: "the address has no host".to_owned(),
    })?;
    if host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_unspecified())
    {
        return Err(DomainError::InvalidPeerUrl {
            detail: "the address must not be a wildcard address".to_owned(),
        });
    }
    if url.port() == Some(0) {
        return Err(DomainError::InvalidPeerUrl {
            detail: "port 0 is not usable".to_owned(),
        });
    }
    Ok(url.origin().ascii_serialization())
}

/// 申请失败对应到列表上的终态，并保留一句面向用户的原因（单层文案，来自统一的
/// 错误映射，不再拼接上下文）。用户主动取消除外：那不是错误。
fn access_failure(error: &DomainError) -> (OutboundAccessStatus, Option<AppError>) {
    let status = match error {
        DomainError::PeerAccessCanceled => OutboundAccessStatus::Canceled,
        DomainError::PeerAccessTimeout | DomainError::PeerAccessExpired => {
            OutboundAccessStatus::Expired
        }
        _ => OutboundAccessStatus::Failed,
    };
    let message = match status {
        OutboundAccessStatus::Canceled => None,
        _ => Some(AppError::from_ref(error)),
    };
    (status, message)
}

/// 申请阶段（还没拿到申请标识）就直接失败的条目。
fn settle_outbound_entry(
    url: String,
    label: Option<String>,
    settled: (OutboundAccessStatus, Option<AppError>),
) -> OutboundAccess {
    OutboundAccess {
        request: OutboundAccessRequest {
            fingerprint: None,
            url,
            label,
            status: settled.0,
            message: settled.1.as_ref().map(|error| error.message.clone()),
            error: settled.1,
        },
        request_id: None,
    }
}

fn default_label(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?.to_owned();
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host,
            })
        })
        .unwrap_or_else(|| url.to_owned())
}

fn valid_access_request_id(request_id: &str) -> bool {
    (MIN_ACCESS_REQUEST_ID_CHARS..=MAX_ACCESS_REQUEST_ID_CHARS).contains(&request_id.len())
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_label(label: &str) -> Result<String, DomainError> {
    if label.is_empty() || label.chars().count() > 128 || label.chars().any(char::is_control) {
        return Err(DomainError::RemotePathInvalid {
            detail: "the peer label must be 1-128 characters without control codes".to_owned(),
        });
    }
    Ok(label.to_owned())
}

/// Normalizes a share-relative path for display: `/` for the root, otherwise
/// one leading slash and no trailing slash.
fn normalize_display_path(path: &str) -> Result<String, DomainError> {
    let trimmed = path;
    if trimmed.len() > MAX_REMOTE_PATH_BYTES {
        return Err(DomainError::RemotePathInvalid {
            detail: "the remote path is too long".to_owned(),
        });
    }
    if trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(DomainError::RemotePathInvalid {
            detail: "the remote path contains a forbidden character".to_owned(),
        });
    }
    let mut segments = Vec::new();
    for segment in trimmed.trim_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.chars().any(char::is_control) {
            return Err(DomainError::RemotePathInvalid {
                detail: format!("`{segment}` is not a valid path segment"),
            });
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        Ok("/".to_owned())
    } else {
        Ok(format!("/{}", segments.join("/")))
    }
}

fn encode_share_path(share_id: &str, display: &str) -> Result<String, DomainError> {
    validate_share_id(share_id)?;
    let mut encoded = String::from(SHARE_URL_PREFIX);
    encoded.push_str(share_id);
    encoded.push('/');
    for segment in display.trim_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        encoded.push_str(&utf8_percent_encode(segment, PATH_SEGMENT).to_string());
        encoded.push('/');
    }
    Ok(encoded)
}

fn validate_share_id(share_id: &str) -> Result<(), DomainError> {
    if share_id.is_empty()
        || share_id.len() > 64
        || !share_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(DomainError::RemotePathInvalid {
            detail: "the share id is invalid".to_owned(),
        });
    }
    Ok(())
}

fn remote_file_target(
    share_id: &str,
    remote_path: &str,
) -> Result<(String, String, String), DomainError> {
    let display = normalize_display_path(remote_path)?;
    if display == "/" {
        return Err(DomainError::RemotePathInvalid {
            detail: "a file path is required".to_owned(),
        });
    }
    let file_name = display
        .rsplit('/')
        .next()
        .map(sanitize_file_name)
        .transpose()?
        .ok_or_else(|| DomainError::RemotePathInvalid {
            detail: "the remote file has no name".to_owned(),
        })?;
    let remote = encode_share_path(share_id, &display)?;
    Ok((display, remote, file_name))
}

fn sanitize_file_name(name: &str) -> Result<String, DomainError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(char::is_control)
    {
        return Err(DomainError::RemotePathInvalid {
            detail: format!("`{name}` is not a safe file name"),
        });
    }
    Ok(name.to_owned())
}

fn local_download_path(root: &Path, relative: &str) -> Result<PathBuf, DomainError> {
    let mut path = root.to_path_buf();
    for segment in relative.split('/') {
        path.push(sanitize_file_name(segment)?);
    }
    if path == root {
        return Err(DomainError::RemotePathInvalid {
            detail: "the downloaded file has no relative path".to_owned(),
        });
    }
    Ok(path)
}

async fn ensure_local_download_dir(root: &Path, relative: &str) -> Result<(), DomainError> {
    let mut path = root.to_path_buf();
    for segment in relative.split('/') {
        path.push(sanitize_file_name(segment)?);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io_error(
                    "inspect download directory",
                    &path,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "refusing to traverse a symbolic link",
                    ),
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io_error(
                    "inspect download directory",
                    &path,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "the path is not a directory",
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&path)
                    .await
                    .map_err(|error| io_error("create download directory", &path, error))?;
            }
            Err(error) => {
                return Err(io_error("inspect download directory", &path, error));
            }
        }
    }
    Ok(())
}

async fn canonical_local_dir(path: &str) -> Result<PathBuf, DomainError> {
    if path.trim().is_empty() {
        return Err(DomainError::Io {
            operation: "resolve local directory",
            path: None,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the local directory is empty",
            ),
        });
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| io_error("resolve local directory", Path::new(path), error))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|error| io_error("inspect local directory", &canonical, error))?;
    if !metadata.is_dir() {
        return Err(DomainError::Io {
            operation: "inspect local directory",
            path: Some(canonical),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the local path is not a directory",
            ),
        });
    }
    Ok(canonical)
}

async fn canonical_local_file(path: &str) -> Result<PathBuf, DomainError> {
    let input = Path::new(path);
    let metadata = tokio::fs::symlink_metadata(input)
        .await
        .map_err(|error| io_error("inspect local file", input, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DomainError::Io {
            operation: "inspect local file",
            path: Some(input.to_path_buf()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the local path is not a regular file",
            ),
        });
    }
    tokio::fs::canonicalize(input)
        .await
        .map_err(|error| io_error("resolve local file", input, error))
}

/// Resolves the file name chosen in a native save dialog without requiring the
/// target file to exist. The parent directory must already exist and the
/// target must not be a symbolic link or directory.
async fn resolve_local_file_destination(path: &str) -> Result<(PathBuf, PathBuf), DomainError> {
    if path.trim().is_empty() {
        return Err(DomainError::Io {
            operation: "resolve download destination",
            path: None,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the download destination is empty",
            ),
        });
    }
    let input = Path::new(path);
    let file_name = input
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| DomainError::Io {
            operation: "resolve download destination",
            path: Some(input.to_path_buf()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the download destination has no file name",
            ),
        })?;
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let directory =
        canonical_local_dir(
            parent
                .to_str()
                .ok_or_else(|| DomainError::RemotePathInvalid {
                    detail: "the download destination is not valid UTF-8".to_owned(),
                })?,
        )
        .await?;
    let destination = directory.join(file_name);

    match tokio::fs::symlink_metadata(&destination).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io_error(
                "inspect download destination",
                &destination,
                error,
            ));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(DomainError::Io {
                operation: "inspect download destination",
                path: Some(destination),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "refusing to overwrite a symbolic link",
                ),
            });
        }
        Ok(metadata) if metadata.is_dir() => {
            return Err(DomainError::Io {
                operation: "inspect download destination",
                path: Some(destination),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the download destination is a directory",
                ),
            });
        }
        Ok(_) => {}
    }

    Ok((directory, destination))
}

async fn resolve_local_destination(
    directory: &Path,
    file_name: &str,
    conflict: ConflictPolicy,
) -> Result<PathBuf, DomainError> {
    let candidate = directory.join(file_name);
    match tokio::fs::symlink_metadata(&candidate).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(candidate),
        Err(error) => Err(io_error("inspect destination", &candidate, error)),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::Io {
            operation: "inspect destination",
            path: Some(candidate),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to overwrite a symbolic link",
            ),
        }),
        Ok(_) => match conflict {
            ConflictPolicy::Fail => Err(DomainError::TransferConflict {
                path: candidate.display().to_string(),
            }),
            ConflictPolicy::Overwrite => Ok(candidate),
            ConflictPolicy::Rename => rename_local_destination(directory, file_name).await,
        },
    }
}

async fn rename_local_destination(
    directory: &Path,
    file_name: &str,
) -> Result<PathBuf, DomainError> {
    let (base, extension) = split_file_name(file_name);
    for index in 1..=999 {
        let candidate = directory.join(format!("{base} ({index}){extension}"));
        match tokio::fs::symlink_metadata(&candidate).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(io_error("inspect destination", &candidate, error)),
            Ok(_) => {}
        }
    }
    Err(DomainError::TransferConflict {
        path: directory.join(file_name).display().to_string(),
    })
}

fn split_file_name(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(index) if index > 0 => (&file_name[..index], &file_name[index..]),
        _ => (file_name, ""),
    }
}

async fn resolve_remote_destination(
    client: &Client,
    directory_encoded: &str,
    directory_display: &str,
    file_name: &str,
    conflict: ConflictPolicy,
) -> Result<(String, String), DomainError> {
    let candidate = format!("{directory_encoded}{}", encode_segment(file_name));
    let exists = remote_exists(client, &candidate).await?;
    let display = join_display(directory_display, file_name);

    if !exists {
        return Ok((candidate, display));
    }
    match conflict {
        ConflictPolicy::Fail => Err(DomainError::TransferConflict { path: display }),
        ConflictPolicy::Overwrite => Ok((candidate, display)),
        ConflictPolicy::Rename => {
            let (base, extension) = split_file_name(file_name);
            for index in 1..=999 {
                let renamed = format!("{base} ({index}){extension}");
                let encoded = format!("{directory_encoded}{}", encode_segment(&renamed));
                if !remote_exists(client, &encoded).await? {
                    return Ok((encoded, join_display(directory_display, &renamed)));
                }
            }
            Err(DomainError::TransferConflict { path: display })
        }
    }
}

fn encode_segment(segment: &str) -> String {
    utf8_percent_encode(segment, PATH_SEGMENT).to_string()
}

/// Creates each missing collection on the way to `directory`. WebDAV returns
/// 405 when the collection already exists, which is the normal case for all
/// but the newest directory in an upload queue.
async fn ensure_remote_directories(
    client: &Client,
    share_id: &str,
    directory: &str,
) -> Result<(), DomainError> {
    let normalized = normalize_display_path(directory)?;
    if normalized == "/" {
        return Ok(());
    }
    let mut accumulated = String::new();
    for segment in normalized.trim_matches('/').split('/') {
        accumulated.push('/');
        accumulated.push_str(segment);
        let encoded = encode_share_path(share_id, &accumulated)?;
        if remote_exists(client, &encoded).await? {
            continue;
        }
        match client.mkcol(&encoded).await {
            Ok(()) => {}
            Err(reqwest_dav::Error::Decode(reqwest_dav::DecodeError::StatusMismatched(status)))
                if status.response_code == StatusCode::METHOD_NOT_ALLOWED.as_u16() => {}
            Err(reqwest_dav::Error::Decode(reqwest_dav::DecodeError::Server(server)))
                if server.response_code == StatusCode::METHOD_NOT_ALLOWED.as_u16() => {}
            Err(error) => {
                return Err(map_dav_error("create remote folder", error, &accumulated));
            }
        }
    }
    Ok(())
}

fn join_display(directory: &str, file_name: &str) -> String {
    if directory == "/" {
        format!("/{file_name}")
    } else {
        format!("{}/{file_name}", directory.trim_end_matches('/'))
    }
}

async fn remote_exists(client: &Client, path: &str) -> Result<bool, DomainError> {
    let response = client
        .list_raw(path, Depth::Number(0))
        .await
        .map_err(|error| map_dav_error("inspect remote path", error, path))?;
    match response.status() {
        status if status.is_success() => Ok(true),
        StatusCode::NOT_FOUND => Ok(false),
        StatusCode::UNAUTHORIZED => Err(DomainError::PeerAuthorizationRejected),
        status => Err(DomainError::PeerRequest {
            operation: "inspect remote path",
            status: Some(status.as_u16()),
            detail: status
                .canonical_reason()
                .unwrap_or("server rejected")
                .to_owned(),
        }),
    }
}

async fn list_entities(
    client: &Client,
    remote: &str,
    depth: Depth,
    operation: &'static str,
    display: &str,
) -> Result<Vec<ListEntity>, DomainError> {
    let response = tokio::time::timeout(TRANSFER_TIMEOUT, client.list_raw(remote, depth))
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation,
            status: None,
            detail: "the request timed out".to_owned(),
        })?
        .map_err(|error| map_dav_error(operation, error, display))?;
    ensure_status(response.status(), operation, display)?;

    // Bound the XML body ourselves: `reqwest_dav::list` reads it without a
    // limit, which would let a hostile server exhaust memory.
    let bytes = read_response_limited(response, LISTING_BODY_LIMIT, operation).await?;
    let text = std::str::from_utf8(&bytes).map_err(|error| DomainError::PeerProtocol {
        detail: format!("the PROPFIND response is not valid UTF-8: {error}"),
    })?;
    let parsed: ListMultiStatus =
        reqwest_dav::re_exports::serde_xml_rs::from_str(text).map_err(|error| {
            DomainError::PeerProtocol {
                detail: format!("invalid PROPFIND XML: {error}"),
            }
        })?;

    parsed
        .responses
        .into_iter()
        .map(|response| {
            ListEntity::try_from(response).map_err(|error| DomainError::PeerProtocol {
                detail: format!("unusable PROPFIND entry: {error}"),
            })
        })
        .collect()
}

async fn remote_meta(
    client: &Client,
    remote: &str,
    display: &str,
) -> Result<RemoteMeta, DomainError> {
    let entities = list_entities(
        client,
        remote,
        Depth::Number(0),
        "read remote metadata",
        display,
    )
    .await?;
    match entities.into_iter().next() {
        Some(ListEntity::File(file)) => Ok(RemoteMeta {
            size: u64::try_from(file.content_length).unwrap_or(0),
            modified: Some(file.last_modified.timestamp()),
            etag: file.tag,
        }),
        Some(ListEntity::Folder(_)) => Err(DomainError::RemotePathInvalid {
            detail: format!("`{display}` is a directory"),
        }),
        None => Err(DomainError::RemoteNotFound {
            path: display.to_owned(),
        }),
    }
}

struct RemoteMeta {
    size: u64,
    modified: Option<i64>,
    etag: Option<String>,
}

fn listing_entry(entity: ListEntity, share_id: &str, requested: &str) -> Option<PeerEntry> {
    let (href, is_dir, size, modified, content_type) = match entity {
        ListEntity::Folder(folder) => (
            folder.href,
            true,
            0,
            Some(folder.last_modified.timestamp()),
            None,
        ),
        ListEntity::File(file) => (
            file.href,
            false,
            u64::try_from(file.content_length).unwrap_or(0),
            Some(file.last_modified.timestamp()),
            Some(file.content_type),
        ),
    };

    let path = href_to_relative(&href, share_id)?;
    if path == "/" || path == requested {
        return None;
    }
    let name = path.rsplit('/').next()?.to_owned();
    if name.is_empty() {
        return None;
    }

    Some(PeerEntry {
        path,
        name,
        is_dir,
        size,
        modified,
        content_type,
    })
}

fn href_to_relative(href: &str, share_id: &str) -> Option<String> {
    let decoded = percent_decode_str(href).decode_utf8_lossy();
    let path = if decoded.starts_with("http://") || decoded.starts_with("https://") {
        Url::parse(&decoded).ok()?.path().to_owned()
    } else {
        decoded.into_owned()
    };

    let prefix = format!("{SHARE_URL_PREFIX}{share_id}");
    let rest = path.strip_prefix(&prefix)?;
    let rest = rest.trim_matches('/');
    if rest.is_empty() {
        Some("/".to_owned())
    } else {
        Some(format!("/{rest}"))
    }
}

fn ensure_status(
    status: StatusCode,
    operation: &'static str,
    path: &str,
) -> Result<(), DomainError> {
    if status.is_success() {
        return Ok(());
    }
    match status {
        StatusCode::UNAUTHORIZED => Err(DomainError::PeerAuthorizationRejected),
        StatusCode::NOT_FOUND => Err(DomainError::RemoteNotFound {
            path: path.to_owned(),
        }),
        _ => Err(DomainError::PeerRequest {
            operation,
            status: Some(status.as_u16()),
            detail: status
                .canonical_reason()
                .unwrap_or("server rejected")
                .to_owned(),
        }),
    }
}

fn map_transport(operation: &'static str, error: &reqwest::Error) -> DomainError {
    DomainError::PeerRequest {
        operation,
        status: error.status().map(|status| status.as_u16()),
        // Keep the whole reqwest source chain: the outer "error sending
        // request" text does not distinguish a refused connection, a TLS
        // fingerprint mismatch or a timeout.
        detail: crate::error::error_chain(error),
    }
}

fn map_dav_error(operation: &'static str, error: reqwest_dav::Error, path: &str) -> DomainError {
    match error {
        reqwest_dav::Error::Decode(reqwest_dav::DecodeError::StatusMismatched(status)) => {
            match status.response_code {
                401 => DomainError::PeerAuthorizationRejected,
                404 => DomainError::RemoteNotFound {
                    path: path.to_owned(),
                },
                code => DomainError::PeerRequest {
                    operation,
                    status: Some(code),
                    detail: format!("server returned HTTP {code}"),
                },
            }
        }
        reqwest_dav::Error::Decode(reqwest_dav::DecodeError::Server(server)) => {
            match server.response_code {
                401 => DomainError::PeerAuthorizationRejected,
                404 => DomainError::RemoteNotFound {
                    path: path.to_owned(),
                },
                code => DomainError::PeerRequest {
                    operation,
                    status: Some(code),
                    detail: format!("server returned HTTP {code}: {}", server.message.trim()),
                },
            }
        }
        reqwest_dav::Error::Reqwest(source) => map_transport(operation, &source),
        other => DomainError::PeerRequest {
            operation,
            status: None,
            detail: other.to_string(),
        },
    }
}

/// Whether a saved server URL is present in the passive discovery snapshot.
/// The preferred announcement URL and the saved URL can differ (a server
/// advertises every interface plus loopback), so compare the host and port as
/// well as the exact URL.
fn url_is_announced(url: &str, announced: &[DiscoveredPeer]) -> bool {
    announced
        .iter()
        .any(|peer| announced_peer_matches(url, peer))
}

fn announced_peer_matches(url: &str, peer: &DiscoveredPeer) -> bool {
    if url == peer.url {
        return true;
    }
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if parsed.port_or_known_default() != Some(peer.port) {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    peer.hosts.iter().any(|candidate| candidate == host)
}

fn error_is_reachable(error: &DomainError) -> bool {
    match error {
        DomainError::PeerRequest { status, .. } => status.is_some(),
        DomainError::PeerAuthorizationRejected | DomainError::PeerProtocol { .. } => true,
        _ => false,
    }
}

async fn read_response_limited(
    mut response: reqwest::Response,
    limit: u64,
    operation: &'static str,
) -> Result<Vec<u8>, DomainError> {
    if let Some(length) = response.content_length()
        && length > limit
    {
        return Err(DomainError::TransferTooLarge {
            size: length,
            limit,
        });
    }

    let mut data = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let next = u64::try_from(data.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                if next > limit {
                    return Err(DomainError::TransferTooLarge { size: next, limit });
                }
                data.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => return Err(map_transport(operation, &error)),
        }
    }
    Ok(data)
}

async fn write_response_to_path(
    mut response: reqwest::Response,
    target: &Path,
    limit: u64,
    operation: &'static str,
    progress: Option<TransferProgressCallback>,
) -> Result<(u64, [u8; 32]), DomainError> {
    let total = response.content_length().unwrap_or(0);
    if let Some(length) = response.content_length()
        && length > limit
    {
        return Err(DomainError::TransferTooLarge {
            size: length,
            limit,
        });
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(target)
        .await
        .map_err(|error| io_error(operation, target, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            tokio::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600)).await
        {
            log::warn!(
                "failed to restrict permissions on {}: {error}",
                target.display()
            );
        }
    }

    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                drop(file);
                remove_file_best_effort(target);
                return Err(map_transport(operation, &error));
            }
        };
        written = written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if written > limit {
            drop(file);
            remove_file_best_effort(target);
            return Err(DomainError::TransferTooLarge {
                size: written,
                limit,
            });
        }
        hasher.update(&chunk);
        if let Err(error) = file.write_all(&chunk).await {
            drop(file);
            remove_file_best_effort(target);
            return Err(io_error(operation, target, error));
        }
        if let Some(progress) = progress.as_ref() {
            progress(written, total);
        }
    }

    if let Err(error) = file.flush().await {
        drop(file);
        remove_file_best_effort(target);
        return Err(io_error(operation, target, error));
    }
    drop(file);
    Ok((written, hasher.finalize().into()))
}

async fn hash_file(path: &Path) -> Result<[u8; 32], DomainError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| io_error("hash external edit", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; READ_CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| io_error("hash external edit", path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn mime_for(file_name: &str) -> &'static str {
    let extension = file_extension(file_name);
    match extension.as_str() {
        "md" | "markdown" | "mdown" | "mkd" | "mdx" => "text/markdown",
        "csv" | "tsv" => "text/csv",
        "json" | "jsonc" | "json5" | "ipynb" | "avsc" | "map" => "application/json",
        "xml" | "xsd" | "xsl" | "xslt" | "plist" => "application/xml",
        "yaml" | "yml" => "text/yaml",
        "html" | "htm" | "vue" | "svelte" | "astro" => "text/html",
        "css" | "scss" | "sass" | "less" => "text/css",
        "js" | "mjs" | "cjs" | "jsx" => "text/javascript",
        "png" => "image/png",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "weba" => "audio/webm",
        "aif" | "aiff" => "audio/aiff",
        "wma" => "audio/x-ms-wma",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "3gp" => "video/3gpp",
        "zip" => "application/zip",
        _ if is_text_file(file_name) => "text/plain",
        _ => "application/octet-stream",
    }
}

fn file_extension(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Text previews are selected by suffix (or a known extensionless filename),
/// never by a server-reported MIME type.
fn is_text_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    if lower == ".env" || lower.starts_with(".env.") {
        return true;
    }
    let extension = file_extension(&lower);
    TEXT_EXTENSIONS.contains(&extension.as_str()) || TEXT_FILENAMES.contains(&lower.as_str())
}

/// Removes run directories left behind by crashed processes. A seven-day age
/// threshold avoids touching a live second instance.
fn cleanup_stale_runs(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let cutoff = SystemTime::now().checked_sub(Duration::from_secs(7 * 24 * 60 * 60));
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("run-") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if cutoff.is_some_and(|cutoff| modified < cutoff) {
            remove_dir_best_effort(&entry.path());
        }
    }
}

fn create_private_dir(path: &Path) -> Result<(), DomainError> {
    std::fs::create_dir_all(path)
        .map_err(|error| io_error("create private directory", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
            log::warn!(
                "failed to restrict permissions on {}: {error}",
                path.display()
            );
        }
    }
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> DomainError {
    DomainError::Io {
        operation,
        path: Some(path.to_path_buf()),
        source,
    }
}

fn remove_file_best_effort(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("failed to remove {}: {error}", path.display());
    }
}

fn remove_dir_best_effort(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("failed to remove {}: {error}", path.display());
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut buffer);
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

    fn announced() -> DiscoveredPeer {
        DiscoveredPeer {
            server_id: "server-1".to_owned(),
            name: "Lab".to_owned(),
            url: "http://192.168.1.20:4918".to_owned(),
            hosts: vec!["192.168.1.20".to_owned(), "127.0.0.1".to_owned()],
            port: 4918,
            share_count: 2,
        }
    }

    #[test]
    fn saved_url_matches_any_announced_host() {
        let announced = announced();
        assert!(announced_peer_matches(
            "http://192.168.1.20:4918/",
            &announced
        ));
        assert!(announced_peer_matches("http://127.0.0.1:4918", &announced));
    }

    #[test]
    fn saved_url_requires_the_announced_port() {
        let announced = announced();
        assert!(!announced_peer_matches(
            "http://192.168.1.20:4919",
            &announced
        ));
        assert!(!announced_peer_matches("not a url", &announced));
    }

    #[test]
    fn a_scanned_address_rebinds_the_saved_peer() {
        let directory = tempfile::tempdir().expect("directory");
        let vault =
            Arc::new(Vault::open(&directory.path().join("peerdav.pd")).expect("open vault"));
        let key_dir = directory.path().join("keys");
        std::fs::create_dir_all(&key_dir).expect("key directory");
        vault
            .initialize(
                key_dir.to_str().expect("utf8 key dir"),
                "peerdav-admin",
                "admin password peer 2026",
            )
            .expect("initialize");
        vault
            .save_peer_record(&crate::types::PeerRecord {
                id: "peer-1".to_owned(),
                label: "Mac".to_owned(),
                url: "https://10.1.1.88:4918".to_owned(),
                authorization_id: None,
                machine_id: Some("fingerprint".to_owned()),
                created_at: 0,
                last_connected_at: None,
            })
            .expect("save peer");
        vault
            .save_peer_record(&crate::types::PeerRecord {
                id: "peer-2".to_owned(),
                label: "Custom port".to_owned(),
                url: "https://10.1.1.88:9443".to_owned(),
                authorization_id: None,
                machine_id: Some("fingerprint".to_owned()),
                created_at: 0,
                last_connected_at: None,
            })
            .expect("save custom peer");
        let manager =
            PeerManager::new(Arc::clone(&vault), directory.path().join("temp")).expect("manager");
        let discovered = DiscoveredPeer {
            server_id: "fingerprint".to_owned(),
            name: "PeerDav (10.2.2.8)".to_owned(),
            url: "https://10.2.2.8:4918".to_owned(),
            hosts: vec!["10.2.2.8".to_owned()],
            port: 4918,
            share_count: 0,
        };

        manager
            .rebind_scanned_peers(std::slice::from_ref(&discovered))
            .expect("rebind");

        assert_eq!(
            vault
                .peer_record("peer-1")
                .expect("read")
                .expect("peer")
                .url,
            "https://10.2.2.8:4918"
        );
        assert_eq!(
            vault
                .peer_record("peer-2")
                .expect("read")
                .expect("peer")
                .url,
            "https://10.1.1.88:9443",
            "a custom-port peer must not be moved to the scan's default port"
        );
    }
}
