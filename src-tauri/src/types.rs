//! Shared DTOs, constants, and vault data types.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Minimum accepted password length, in Unicode scalar values.
pub const MIN_PASSWORD_CHARS: usize = 6;
/// Upper bound to avoid unbounded password work from oversized IPC payloads.
pub const MAX_PASSWORD_CHARS: usize = 1024;
/// Upper bound for the administrator username chosen during setup.
pub const MAX_USERNAME_CHARS: usize = 64;

/// XChaCha20-Poly1305 key length in bytes.
pub const VAULT_KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce length in bytes.
pub const NONCE_LEN: usize = 24;

/// SQLite database file name inside the application data directory.
pub const DB_FILE_NAME: &str = "peerdav.pd";
/// Current vault schema version.
pub const SCHEMA_VERSION: &str = "2";

pub const META_SCHEMA_VERSION: &str = "schema_version";
/// Absolute path of the user-owned key file.
pub const META_KEY_FILE: &str = "vault.keyFile";
/// SHA-256 fingerprint of the one-time recovery code. The code itself is
/// shown once and never persisted.
pub const META_RECOVERY_HASH: &str = "vault.recoveryHash";

/// Key file format shared with the desktop UI.
pub const KEY_FILE_TYPE: &str = "peerdav-vault";
pub const KEY_FILE_VERSION: u32 = 1;
pub const KEY_FILE_NAME: &str = "peerdav-vault.key";

/// Persisted WebDAV server lifecycle and configuration. The server is restored
/// on every app start when `SERVER_ENABLED_META` is `"1"`, including while the
/// vault is locked: it serves from the non-secret share and authorization
/// catalog and never needs the vault key.
pub const SERVER_ENABLED_META: &str = "server.enabled";
pub const SERVER_BIND_META: &str = "server.bindAddress";
pub const SERVER_PORT_META: &str = "server.port";
pub const SERVER_DISCOVERY_PORT_META: &str = "server.discoveryPort";
pub const SERVER_NAME_META: &str = "server.name";
pub const TCP_DISCOVERY_META: &str = "discovery.tcp";

#[derive(Debug, Clone, Default, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpDiscoverySettings {
    pub enabled: bool,
    pub networks: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSettings {
    pub name: String,
    pub bind_address: String,
    pub port: u16,
    pub discovery_port: u16,
    pub tcp: TcpDiscoverySettings,
}
/// Stable installation identifier used to bind a server authorization to the
/// PeerDav client that owns it.
pub const CLIENT_ID_META: &str = "client.id";

/// Reserved secret record holding the encrypted unlock verifier.
pub const SECRET_VAULT_VERIFIER: &str = "__vault_verifier__";
/// Reserved secret record holding the encrypted admin credentials.
pub const SECRET_ADMIN: &str = "__admin__";
/// Known plaintext used to verify that a derived key is correct.
pub const VAULT_VERIFIER_PLAINTEXT: &[u8] = b"peerdav-vault-verifier-v1";

/// Public vault state returned to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub initialized: bool,
    pub unlocked: bool,
    /// Configured key file path, if any.
    pub key_file: Option<String>,
    /// Stable reason code when automatic key loading failed.
    pub key_error: Option<String>,
}

/// Result of first-run initialization. The recovery code is returned exactly
/// once; only its hash is persisted.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeVaultResult {
    pub admin_username: String,
    pub vault_status: VaultStatus,
    pub recovery_code: String,
}

/// Result of a key-loss reset. Returns the new recovery code exactly once and
/// the archived database path for operator visibility.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetVaultResult {
    pub admin_username: String,
    pub vault_status: VaultStatus,
    pub recovery_code: String,
    pub archived_database: String,
}

/// Result of an admin login or password change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLoginResult {
    pub username: String,
    pub must_change_password: bool,
}

/// In-memory admin session restored by the UI after a webview reload. It lives
/// only in process memory and is cleared when the vault is locked or the admin
/// logs out.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSessionInfo {
    pub username: String,
    pub must_change_password: bool,
}

/// Decrypted admin credentials. This value is serialized to JSON and then
/// encrypted before it touches SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AdminRecord {
    pub username: String,
    pub password_hash: String,
    pub must_change_password: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Encrypted secret record as stored in SQLite.
#[derive(Debug, Clone)]
pub(crate) struct SecretRecord {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Default WebDAV port and URL layout. Every share is mounted below
/// `/share/<share id>/`, which the request gate and the scoped filesystem
/// both parse the same way.
pub const DEFAULT_SERVER_PORT: u16 = 4918;
pub const SHARE_URL_PREFIX: &str = "/share/";
/// Catalog endpoint used by the client to discover the shares a code can see.
pub const CATALOG_PATH: &str = "/__peerdav/catalog";
pub const CATALOG_PROTOCOL: &str = "peerdav-catalog/1";
/// Endpoint where a waiting client claims its approved authorization code.
pub const ACCESS_STATUS_PATH: &str = "/__peerdav/access";
pub const ACCESS_PROTOCOL: &str = "peerdav-access/1";
/// Client-generated key that binds one approval to the requesting client.
pub const ACCESS_REQUEST_HEADER: &str = "x-peerdav-access-request";
/// Authorization lifecycle endpoint. `GET` reports whether the submitted code
/// is still active or has been revoked; `POST` binds the authorization to this
/// installation; `DELETE` revokes it on behalf of the owning client.
pub const AUTHORIZATION_PATH: &str = "/__peerdav/authorization";
pub const AUTHORIZATION_PROTOCOL: &str = "peerdav-authorization/1";
pub const CLIENT_ID_HEADER: &str = "x-peerdav-client-id";
/// 设备机器码。随每个 PeerDav 请求发送，服务端据此把「同一台设备」的申请、授权
/// 和已保存对端关联起来；IP 会变，机器码不会。
pub const MACHINE_ID_HEADER: &str = "x-peerdav-machine-id";
/// 服务端在按访问规则拒绝请求时回传的来源地址。客户端把它展示给用户，
/// 这样虚拟局域网里的地址不匹配能直接看到，而不是只剩一句“请求失败”。
pub const OBSERVED_PEER_HEADER: &str = "x-peerdav-observed-ip";
pub const MIN_CLIENT_ID_CHARS: usize = 16;
pub const MAX_CLIENT_ID_CHARS: usize = 64;
/// Default interval for checking saved peers against their server bindings.
/// The same request doubles as the lightweight reachability probe for servers
/// that are online or have just announced themselves over UDP.
pub const PEER_SYNC_INTERVAL: Duration = Duration::from_secs(5);
/// How often saved peers are matched against passive announcements. This only
/// reads the in-memory discovery snapshot and the saved-peer rows; it never
/// performs network requests.
pub const PEER_PRESENCE_INTERVAL: Duration = Duration::from_secs(2);
pub const MIN_ACCESS_REQUEST_ID_CHARS: usize = 16;
pub const MAX_ACCESS_REQUEST_ID_CHARS: usize = 128;
/// How long an approved code waits for the client to claim it.
pub const ACCESS_GRANT_TTL: Duration = Duration::from_secs(15 * 60);
/// Upper bound for in-memory approved grants.
pub const MAX_ACCESS_GRANTS: usize = 64;
/// The client stops waiting for an administrator after this duration.
pub const PEER_ACCESS_WAIT: Duration = Duration::from_secs(10 * 60);
/// Poll interval while waiting for approval; the UI remains responsive because
/// the wait runs in the backend.
pub const PEER_ACCESS_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Default UDP port used for LAN discovery.
pub const DEFAULT_DISCOVERY_PORT: u16 = 4919;
pub const DISCOVERY_PROTOCOL: &str = "peerdav-discovery/1";
pub const DISCOVERY_PROBE_KIND: &str = "probe";
pub const DISCOVERY_ANNOUNCE_KIND: &str = "announce";
/// Upper bound for in-memory pending approval requests.
pub const MAX_PENDING_REQUESTS: usize = 64;
/// Upper bound for the share-relative path stored on a pending request. The
/// value is display/debug data, so it is truncated instead of allowing an
/// unauthenticated peer to grow the pending queue with path strings.
pub const MAX_PENDING_PATH_CHARS: usize = 256;
/// Upper bound for the attempt counter shown on a pending request. Repeats
/// keep refreshing the entry, but the counter cannot grow without limit.
pub const MAX_PENDING_ATTEMPTS: u32 = 100;
/// Event names for the shared background error outlet.
pub const ERROR_EVENT: &str = "app://error";
pub const PENDING_REQUEST_EVENT: &str = "app://pending-request";
/// Event emitted when a client withdraws its pending access request.
pub const PENDING_CANCELED_EVENT: &str = "app://pending-canceled";
/// Event emitted while a peer transfer is streaming.
pub const TRANSFER_PROGRESS_EVENT: &str = "app://transfer-progress";
/// Event emitted when a saved peer is removed because its server revoked it.
pub const PEER_REVOKED_EVENT: &str = "app://peer-revoked";
/// Event emitted when a saved server's reachable state changes.
pub const PEER_STATUS_EVENT: &str = "app://peer-status";
/// Event emitted with the current passive LAN discovery snapshot.
pub const DISCOVERED_PEERS_EVENT: &str = "app://discovered-peers";
/// Event emitted when the user asks to quit from the tray menu.
pub const QUIT_REQUEST_EVENT: &str = "app://quit-request";
/// Label of the main application window.
pub const MAIN_WINDOW_LABEL: &str = "main";
/// Tray menu identifiers.
pub const TRAY_SHOW_MENU_ID: &str = "tray-show";
pub const TRAY_QUIT_MENU_ID: &str = "tray-quit";

/// Read/write/delete permissions of one shared directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharePermissions {
    pub can_read: bool,
    pub can_write: bool,
    pub can_delete: bool,
}

/// Whether an access rule allows or denies a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleKind {
    Allow,
    Deny,
}

/// Network matcher for one share. Values are validated before persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum RuleMatcher {
    Ip(String),
    Network(String),
    Mac(String),
}

/// One allow/deny rule attached to a share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessRule {
    pub kind: RuleKind,
    pub matcher: RuleMatcher,
}

/// Public description of a shared directory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareInfo {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub permissions: SharePermissions,
    pub rules: Vec<AccessRule>,
}

/// Public description of an approved authorization.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationInfo {
    pub id: String,
    pub label: String,
    /// 申请方上报的机器码。它是授权身份，IP 只是方便识别的展示信息：
    /// 换网段或重新分配 DHCP 地址不会让同一台设备变成新设备。
    pub machine_id: Option<String>,
    pub created_at: i64,
}

/// 出站访问申请的结果。它是后端内存状态而不是前端组件状态：切换页面或 webview
/// 重载后，列表必须还能看到「已发送，等待对方批准」，而不是退回「可以申请」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutboundAccessStatus {
    /// 申请已在对方登记，等待对方管理员批准。
    Waiting,
    /// 对方没有开启授权要求，当前版本无法自动加入。
    Accessible,
    /// 用户自己取消了申请。
    Canceled,
    /// 对方已经不认这条申请（超时或已过期）。
    Expired,
    /// 申请或等待过程失败，原因在 `message` 里。
    Failed,
}

/// 一台服务器的出站访问申请快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundAccessRequest {
    pub url: String,
    pub fingerprint: Option<String>,
    pub label: Option<String>,
    pub status: OutboundAccessStatus,
    /// 失败/过期时面向用户的一句话（后端错误映射出的单层文案）；其他状态为空。
    pub message: Option<String>,
    pub error: Option<crate::error::AppError>,
}

/// A client waiting for the administrator to approve access.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestInfo {
    pub id: String,
    pub share_id: String,
    pub share_name: String,
    /// 展示用地址：会随网络变化，因此不参与身份判定。
    pub peer_ip: String,
    pub peer_mac: Option<String>,
    /// 申请方上报的机器码，缺失时退回 `peer_ip` 作为身份。
    pub machine_id: Option<String>,
    pub requested_path: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub attempts: u32,
    /// Opaque key supplied by a PeerDav client. It is omitted from the admin
    /// DTO because only the requesting client needs it to claim the code.
    #[serde(skip_serializing)]
    pub client_request_id: Option<String>,
}

/// The one and only time the plaintext authorization code is returned.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovePendingResult {
    pub authorization_code: String,
    pub authorization: AuthorizationInfo,
    /// True when the code was queued for automatic delivery to the client.
    pub auto_delivery: bool,
}

/// Installation identity is independent of hardware and network addresses.
pub(crate) struct DeviceIdentity {
    pub certificate: rustls::pki_types::CertificateDer<'static>,
    pub key: rustls::pki_types::PrivatePkcs8KeyDer<'static>,
    pub fingerprint: String,
}

/// Logical dimensions survive changes to monitor scale factors; positions
/// are physical and are validated against the currently connected work areas.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowPreference {
    pub width: f64,
    pub height: f64,
    pub x: i32,
    pub y: i32,
    pub monitor: Option<String>,
    pub maximized: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScreenArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

pub(crate) struct InboundConnectionGuard {
    pub registry:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, ConnectionInfo>>>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub fingerprint: String,
    pub name: String,
    pub address: String,
    pub state: ConnectionState,
    pub authorized: bool,
    pub active_connections: usize,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    Discovered,
    Requesting,
    Pending,
    Approved,
    Connected,
    Disconnected,
    Canceled,
    Rejected,
    Expired,
    Failed,
}

/// Transfer workers operate on 10 MiB parts, capped per file.
pub const TRANSFER_PART_BYTES: u64 = 10 * 1024 * 1024;
pub const TRANSFER_MAX_WORKERS: usize = 5;

/// Runtime server state exposed to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub bind_address: Option<String>,
    pub discovery_running: bool,
    pub discovery_port: Option<u16>,
    pub share_count: usize,
    pub pending_count: usize,
}

/// Persisted share row; rules are stored separately.
#[derive(Debug, Clone)]
pub(crate) struct ShareRecord {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub permissions: SharePermissions,
    pub rules: Vec<ShareRuleRecord>,
}

/// Persisted allow/deny row.
#[derive(Debug, Clone)]
pub(crate) struct ShareRuleRecord {
    pub kind: RuleKind,
    pub matcher: RuleMatcher,
}

/// Persisted authorization row. `code_hash` is the SHA-256 hex digest and is
/// the only representation of the code that ever reaches storage.
#[derive(Debug, Clone)]
pub(crate) struct AuthorizationRecord {
    pub id: String,
    pub label: String,
    pub code_hash: String,
    pub client_id: Option<String>,
    /// 申请方上报的机器码；历史数据为 None，此时仍按 IP 匹配。
    pub machine_id: Option<String>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// Prefix of the vault secret that stores a peer's authorization code.
pub(crate) const PEER_SECRET_PREFIX: &str = "peer.";
/// Maximum size of a single download or upload (10 GiB).
pub const MAX_TRANSFER_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Maximum size of a backend preview payload.
pub const MAX_PREVIEW_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum physical line count accepted for an online text preview.
pub const MAX_PREVIEW_LINES: usize = 5000;
/// Upper bound for a share-relative remote path.
pub const MAX_REMOTE_PATH_BYTES: usize = 4096;
/// Upper bound for one directory listing.
pub const MAX_DIR_ENTRIES: usize = 10_000;
/// Upper bound for the files discovered by one recursive directory download.
pub const MAX_DIRECTORY_DOWNLOAD_FILES: usize = 10_000;
/// Upper bound for advertised discovery hosts.
pub const MAX_DISCOVERY_HOSTS: usize = 16;
pub const DISCOVERY_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(3);
/// A server that stops announcing disappears from the passive discovery list
/// after this grace period. Three missed announcements are tolerated before a
/// server is treated as offline.
pub const DISCOVERY_PEER_TTL: Duration = Duration::from_secs(10);
/// How often the passive discovery registry drops expired servers.
pub const DISCOVERY_PRUNE_INTERVAL: Duration = Duration::from_secs(1);
pub const DISCOVERY_SCAN_TIMEOUT: Duration = Duration::from_millis(1200);
pub const DISCOVERY_RECV_BUFFER: usize = 8 * 1024;
/// Default timeout for ordinary peer HTTP requests.
pub const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-request timeout for file parts, independent of the whole file size.
pub const PEER_TRANSFER_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Upload commits also hash, assemble and durably copy the entire file.
pub const PEER_UPLOAD_COMMIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Bound cancellation confirmation when the remote peer becomes unreachable.
pub const PEER_UPLOAD_CANCEL_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound for an authorization code accepted from the backend or a peer.
pub const MAX_AUTHORIZATION_CODE_CHARS: usize = 512;

/// One server seen on the LAN through UDP discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPeer {
    pub server_id: String,
    pub name: String,
    pub url: String,
    pub hosts: Vec<String>,
    pub port: u16,
    pub share_count: usize,
}

/// Shares returned by the authenticated catalog endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResponse {
    pub protocol: String,
    pub server_name: String,
    /// 服务端自己的机器码：客户端保存对端记录时用它建立「同一台设备」的对应关系，
    /// 这样解绑出站设备时能连带清掉本机为它开过的入站授权。未知时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    pub shares: Vec<PeerShare>,
}

/// One share as the client sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerShare {
    pub id: String,
    pub name: String,
    pub permissions: SharePermissions,
}

/// A saved peer connection. The authorization code is not part of this type;
/// it only ever lives in the encrypted vault.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub id: String,
    pub fingerprint: Option<String>,
    pub label: String,
    pub url: String,
    pub online: bool,
    pub shares: Vec<PeerShare>,
}

/// Reachability change for one saved server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatus {
    pub id: String,
    pub online: bool,
}

/// Result of asking a server to record this client as pending.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerAccessRequest {
    pub requested: bool,
    /// Opaque key used to claim the approval code; `None` when no approval is
    /// needed.
    pub request_id: Option<String>,
}

/// State of one client access request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessRequestStatus {
    Pending,
    Approved,
    Expired,
}

/// Response from the one-time access status endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessStatusResponse {
    pub protocol: String,
    pub status: AccessRequestStatus,
    pub authorization_code: Option<String>,
}

/// State reported by the authorization lifecycle endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorizationStatus {
    Active,
    Revoked,
}

/// Response from `/__peerdav/authorization`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationStatusResponse {
    pub protocol: String,
    pub status: AuthorizationStatus,
    pub authorization_id: String,
    pub client_id: Option<String>,
}

/// Response from binding an authorization to this PeerDav installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationBindingResponse {
    pub protocol: String,
    pub authorization_id: String,
    pub client_id: String,
}

/// Payload sent to the UI after a saved peer is automatically removed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerRevocationInfo {
    pub id: String,
    pub label: String,
    pub url: String,
    pub reason: String,
}

/// Lightweight connection health, including denial reasons.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerHealth {
    pub online: bool,
    pub authorized: bool,
    pub latency_ms: Option<u64>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub detail: Option<String>,
    pub error: Option<crate::error::AppError>,
}

/// One structured directory entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerEntry {
    /// Share-relative path with a leading slash; `/` is the share root.
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub content_type: Option<String>,
}

/// Structured result of one PROPFIND request.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerListing {
    pub share_id: String,
    pub path: String,
    pub entries: Vec<PeerEntry>,
    pub truncated: bool,
}

/// How an existing destination is handled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictPolicy {
    #[default]
    Fail,
    Overwrite,
    Rename,
}

/// Outcome of a download or upload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferResult {
    pub transferred: bool,
    pub remote_path: String,
    pub local_path: Option<String>,
    pub bytes: u64,
    /// Set when nothing was transferred, e.g. an unchanged external edit.
    pub skipped_reason: Option<String>,
}

/// Streaming progress for a download or upload initiated by the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub transfer_id: String,
    pub direction: String,
    pub file_name: String,
    pub transferred: u64,
    /// Zero when the server did not provide a content length.
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<UploadPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_per_second: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadOutcome {
    Active,
    Canceled,
    Completed,
}

pub(crate) struct UploadSession {
    pub cancel: tokio::sync::watch::Sender<bool>,
    pub active: std::sync::Arc<tokio::sync::RwLock<()>>,
    pub outcome: std::sync::Mutex<UploadOutcome>,
}

pub(crate) type UploadSessions =
    tokio::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<UploadSession>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UploadPhase {
    Hashing,
    Uploading,
    Committing,
    Completed,
}

pub type UploadProgressCallback = std::sync::Arc<dyn Fn(UploadPhase, u64, u64, u64) + Send + Sync>;

/// One local file discovered for an upload queue. `relative_path` keeps the
/// selected directory hierarchy, using `/` separators.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCandidate {
    pub local_path: String,
    pub relative_path: String,
    pub size: u64,
}

/// One remote file discovered for a directory-download queue.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadCandidate {
    pub remote_path: String,
    pub relative_path: String,
    pub local_path: String,
    pub size: u64,
}

/// A prepared directory download. The backend has already scanned the remote
/// tree and created the local directory hierarchy, so every candidate's parent
/// directory exists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryDownloadPlan {
    pub local_root: String,
    pub directory_count: usize,
    pub files: Vec<DownloadCandidate>,
}

/// Backend-read preview payload. Never served through a broad asset scope.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPayload {
    pub file_name: String,
    pub mime: String,
    /// `utf8` or `base64`.
    pub encoding: String,
    pub data: String,
    pub size: u64,
}

/// A controlled temporary copy opened in an external application.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEditInfo {
    pub edit_id: String,
    pub peer_id: String,
    pub share_id: String,
    pub remote_path: String,
    pub local_path: String,
    pub file_name: String,
    pub size: u64,
    pub opened_at: i64,
}

/// Persisted peer metadata. Authorization codes are stored separately.
#[derive(Debug, Clone)]
pub(crate) struct PeerRecord {
    pub id: String,
    pub label: String,
    pub url: String,
    pub authorization_id: Option<String>,
    /// 对端设备上报的机器码（连接时从其目录里读到）。解绑时用它找到本机为同一台
    /// 设备开过的入站授权，一并撤销，避免对方还留在「其他设备访问此设备」里。
    pub machine_id: Option<String>,
    pub created_at: i64,
    pub last_connected_at: Option<i64>,
}

/// Decrypted peer secret payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PeerSecret {
    pub url: String,
    pub code: String,
}
