//! Shared DTOs, constants, and vault data types.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Minimum accepted password length, in Unicode scalar values.
pub const MIN_PASSWORD_CHARS: usize = 6;
/// Upper bound to avoid unbounded KDF work from oversized IPC payloads.
pub const MAX_PASSWORD_CHARS: usize = 1024;
/// Upper bound for the administrator username chosen during setup.
pub const MAX_USERNAME_CHARS: usize = 64;

/// Argon2id salt length in bytes.
pub const KDF_SALT_LEN: usize = 16;
/// XChaCha20-Poly1305 key length in bytes.
pub const VAULT_KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce length in bytes.
pub const NONCE_LEN: usize = 24;

/// Fixed KDF parameters. These are never read from the database, so a local
/// attacker cannot weaken the work factor by editing SQLite metadata.
pub const KDF_ALGORITHM: &str = "argon2id-v1";
/// Memory cost in KiB (64 MiB).
pub const KDF_M_COST: u32 = 65_536;
/// Time cost.
pub const KDF_T_COST: u32 = 3;
/// Parallelism.
pub const KDF_P_COST: u32 = 1;

/// SQLite database file name inside the application data directory.
pub const DB_FILE_NAME: &str = "peerdav.sqlite3";
/// Current vault schema version.
pub const SCHEMA_VERSION: &str = "1";

pub const META_SCHEMA_VERSION: &str = "schema_version";
pub const META_KDF_ALGORITHM: &str = "kdf_algorithm";
pub const META_KDF_SALT: &str = "kdf_salt";

/// Reserved secret record holding the encrypted unlock verifier.
pub const SECRET_VAULT_VERIFIER: &str = "__vault_verifier__";
/// Reserved secret record holding the encrypted admin credentials.
pub const SECRET_ADMIN: &str = "__admin__";
/// Known plaintext used to verify that a derived key is correct.
pub const VAULT_VERIFIER_PLAINTEXT: &[u8] = b"peerdav-vault-verifier-v1";

/// Public vault state returned to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub initialized: bool,
    pub unlocked: bool,
}

/// Result of first-run initialization. Echoes the normalized admin username;
/// no password material is returned to the frontend.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeVaultResult {
    pub admin_username: String,
    pub vault_status: VaultStatus,
}

/// Result of an admin login or password change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLoginResult {
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
    pub created_at: i64,
}

/// A client waiting for the administrator to approve access.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestInfo {
    pub id: String,
    pub share_id: String,
    pub share_name: String,
    pub peer_ip: String,
    pub peer_mac: Option<String>,
    pub requested_path: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub attempts: u32,
}

/// The one and only time the plaintext authorization code is returned.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovePendingResult {
    pub authorization_code: String,
    pub authorization: AuthorizationInfo,
}

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
    pub created_at: i64,
}

/// Prefix of the vault secret that stores a peer's authorization code.
pub(crate) const PEER_SECRET_PREFIX: &str = "peer.";
/// Maximum size of a single download or upload.
pub const MAX_TRANSFER_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum size of a backend preview payload.
pub const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;
/// Text previews are further truncated to keep IPC payloads small.
pub const MAX_TEXT_PREVIEW_BYTES: usize = 256 * 1024;
/// Upper bound for a share-relative remote path.
pub const MAX_REMOTE_PATH_BYTES: usize = 4096;
/// Upper bound for one directory listing.
pub const MAX_DIR_ENTRIES: usize = 10_000;
/// Upper bound for advertised discovery hosts.
pub const MAX_DISCOVERY_HOSTS: usize = 16;
pub const DISCOVERY_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(3);
pub const DISCOVERY_SCAN_TIMEOUT: Duration = Duration::from_millis(1200);
pub const DISCOVERY_RECV_BUFFER: usize = 8 * 1024;
/// Timeout applied to every peer HTTP request.
pub const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for an authorization code supplied by the UI.
pub const MAX_AUTHORIZATION_CODE_CHARS: usize = 512;

/// One server seen on the LAN through UDP discovery.
#[derive(Debug, Clone, Serialize)]
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
    pub label: String,
    pub url: String,
    pub online: bool,
    pub shares: Vec<PeerShare>,
}

/// Result of asking a server to record this client as pending.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerAccessRequest {
    pub requested: bool,
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
    pub truncated: bool,
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
    pub created_at: i64,
    pub last_connected_at: Option<i64>,
}

/// Decrypted peer secret payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PeerSecret {
    pub url: String,
    pub code: String,
}
