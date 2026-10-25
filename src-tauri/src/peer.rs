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

use crate::discovery;
use crate::error::{AppError, DomainError};
use crate::traits::PeerEvents;
use crate::types::{
    ACCESS_PROTOCOL, ACCESS_REQUEST_HEADER, ACCESS_STATUS_PATH, AUTHORIZATION_PATH,
    AUTHORIZATION_PROTOCOL, AccessRequestStatus, AccessStatusResponse,
    AuthorizationBindingResponse, AuthorizationStatus, AuthorizationStatusResponse, CATALOG_PATH,
    CATALOG_PROTOCOL, CLIENT_ID_HEADER, CatalogResponse, ConflictPolicy, DiscoveredPeer,
    ExternalEditInfo, MAX_ACCESS_REQUEST_ID_CHARS, MAX_AUTHORIZATION_CODE_CHARS, MAX_DIR_ENTRIES,
    MAX_PREVIEW_BYTES, MAX_REMOTE_PATH_BYTES, MAX_TEXT_PREVIEW_BYTES, MAX_TRANSFER_BYTES,
    MIN_ACCESS_REQUEST_ID_CHARS, PEER_ACCESS_POLL_INTERVAL, PEER_ACCESS_WAIT, PEER_REQUEST_TIMEOUT,
    PEER_SYNC_INTERVAL, PeerAccessRequest, PeerEntry, PeerHealth, PeerInfo, PeerListing,
    PeerRecord, PeerRevocationInfo, PeerShare, PreviewPayload, SHARE_URL_PREFIX, TransferResult,
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

struct RunningPeerBackground {
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
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
    agent: reqwest::Client,
    temp_root: PathBuf,
    limits: PeerLimits,
    edits: Mutex<HashMap<String, ExternalEdit>>,
    access_cancellations: Mutex<HashSet<String>>,
    events: Arc<dyn PeerEvents>,
    background: std::sync::Mutex<Option<RunningPeerBackground>>,
}

impl Drop for PeerManager {
    fn drop(&mut self) {
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
        let agent = reqwest::Client::builder()
            .timeout(limits.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| DomainError::Internal {
                detail: format!("failed to build the peer HTTP client: {error}"),
            })?;
        Ok(Self {
            vault,
            agent,
            temp_root: run_root,
            limits,
            edits: Mutex::new(HashMap::new()),
            access_cancellations: Mutex::new(HashSet::new()),
            events,
            background: std::sync::Mutex::new(None),
        })
    }

    /// Starts the periodic server-binding synchronizer. The task is idempotent
    /// and is shut down when this manager is dropped.
    pub fn start_background(self: &Arc<Self>) {
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
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
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

    /// Scans the LAN over UDP discovery.
    pub async fn scan_lan(
        &self,
        port: Option<u16>,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<DiscoveredPeer>, DomainError> {
        let port = port.unwrap_or(crate::types::DEFAULT_DISCOVERY_PORT);
        discovery::scan(port, timeout_ms).await
    }

    pub fn list_peers(&self) -> Result<Vec<PeerInfo>, DomainError> {
        let records = self.vault.list_peer_records()?;
        Ok(records
            .into_iter()
            .map(|record| PeerInfo {
                id: record.id,
                label: record.label,
                url: record.url,
                online: false,
                shares: Vec::new(),
            })
            .collect())
    }

    /// Asks a server to record this client as pending. The server's policy is
    /// still enforced: a denied peer receives `PEER_REQUEST_FAILED`, and
    /// nothing is approved automatically. The returned request id is used to
    /// claim the code after an administrator approves the request.
    pub async fn request_access(&self, url: &str) -> Result<PeerAccessRequest, DomainError> {
        let url = normalize_peer_url(url)?;
        let request_id = random_hex(16);
        let endpoint = format!("{url}{CATALOG_PATH}");
        let response = tokio::time::timeout(
            HEALTH_TIMEOUT,
            self.agent
                .get(endpoint)
                .header(ACCESS_REQUEST_HEADER, &request_id)
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
            self.agent
                .delete(endpoint)
                .header(ACCESS_REQUEST_HEADER, request_id)
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
                self.agent
                    .get(&endpoint)
                    .header(ACCESS_REQUEST_HEADER, request_id)
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
            &self.agent,
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

        let existing = self
            .vault
            .list_peer_records()?
            .into_iter()
            .find(|record| record.url == url);
        let now = now_unix();
        let id = existing
            .as_ref()
            .map(|record| record.id.clone())
            .unwrap_or_else(|| random_hex(8));
        let label = match label.map(str::trim).filter(|label| !label.is_empty()) {
            Some(label) => validate_label(label)?,
            None => existing
                .as_ref()
                .map(|record| record.label.clone())
                .unwrap_or_else(|| default_label(&url)),
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

        Ok(PeerInfo {
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

        match result {
            Ok(Ok(_)) => Ok(PeerHealth {
                online: true,
                authorized: true,
                latency_ms,
                code: None,
                message: None,
                detail: None,
            }),
            Ok(Err(error)) => {
                let app_error = AppError::from_ref(&error);
                let online = error_is_reachable(&error);
                Ok(PeerHealth {
                    online,
                    authorized: false,
                    latency_ms,
                    code: Some(app_error.code),
                    message: Some(app_error.message),
                    detail: app_error.detail,
                })
            }
            Err(_) => Ok(PeerHealth {
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
            }),
        }
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
    pub async fn forget(&self, peer_id: &str) -> Result<(), DomainError> {
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
        self.forget_local(peer_id).await
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
        let mut removed = 0;

        for record in records {
            let Ok(secret) = self.vault.load_peer_code(&record.id) else {
                continue;
            };
            let code = Zeroizing::new(secret.code);
            match self
                .fetch_authorization_status(&record.url, code.as_str())
                .await
            {
                Ok(AuthorizationOutcome::Active(status)) => {
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
                Ok(AuthorizationOutcome::Unsupported) => {}
                Err(error) => {
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
            self.agent
                .post(endpoint)
                .basic_auth(CLIENT_USERNAME, Some(code))
                .header(CLIENT_ID_HEADER, client_id)
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
            self.agent
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
            self.agent
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

        let response = client
            .get_raw(&remote)
            .await
            .map_err(|error| map_dav_error("download file", error, &display))?;
        ensure_status(response.status(), "download file", &display)?;

        let written = write_response_to_file(
            response,
            &dir,
            &destination,
            self.limits.max_transfer_bytes,
            conflict,
            None,
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
        let (directory, destination) = resolve_local_file_destination(local_path).await?;

        let response = client
            .get_raw(&remote)
            .await
            .map_err(|error| map_dav_error("download file", error, &display))?;
        ensure_status(response.status(), "download file", &display)?;

        let written = write_response_to_file(
            response,
            &directory,
            &destination,
            self.limits.max_transfer_bytes,
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

    pub async fn upload(
        &self,
        peer_id: &str,
        share_id: &str,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
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

        let file = tokio::fs::File::open(&local)
            .await
            .map_err(|error| io_error("open local file", &local, error))?;
        tokio::time::timeout(
            TRANSFER_TIMEOUT,
            client.put(&remote, reqwest::Body::from(file)),
        )
        .await
        .map_err(|_| DomainError::PeerRequest {
            operation: "upload file",
            status: None,
            detail: "the upload timed out".to_owned(),
        })?
        .map_err(|error| map_dav_error("upload file", error, &display))?;

        Ok(TransferResult {
            transferred: true,
            remote_path: display,
            local_path: Some(local.display().to_string()),
            bytes: size,
            skipped_reason: None,
        })
    }

    /// Reads a file through the backend and returns it over IPC. Images and
    /// PDFs are base64-encoded; text-like files are UTF-8 (truncated).
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
            read_response_limited(response, self.limits.max_preview_bytes, "preview file").await?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mime = mime_for(&file_name).to_owned();

        if is_text_mime(&mime) {
            match String::from_utf8(bytes) {
                Ok(text) => {
                    let (data, truncated) = truncate_utf8(text, MAX_TEXT_PREVIEW_BYTES);
                    return Ok(PreviewPayload {
                        file_name,
                        mime,
                        encoding: "utf8".to_owned(),
                        data,
                        size,
                        truncated,
                    });
                }
                Err(error) => bytes = error.into_bytes(),
            }
        }

        Ok(PreviewPayload {
            file_name,
            mime: if is_text_mime(&mime) {
                "application/octet-stream".to_owned()
            } else {
                mime
            },
            encoding: "base64".to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            size,
            truncated: false,
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
            &self.agent,
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
    if url.scheme() != "http" {
        return Err(DomainError::InvalidPeerUrl {
            detail: format!(
                "scheme `{}` is not supported; PeerDav currently uses plain http:// on the LAN",
                url.scheme()
            ),
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
        detail: error.to_string(),
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
        reqwest_dav::Error::Reqwest(source) => map_transport(operation, &source),
        other => DomainError::PeerRequest {
            operation,
            status: None,
            detail: other.to_string(),
        },
    }
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

async fn write_response_to_file(
    response: reqwest::Response,
    directory: &Path,
    destination: &Path,
    limit: u64,
    conflict: ConflictPolicy,
    progress: Option<TransferProgressCallback>,
) -> Result<u64, DomainError> {
    let temp = directory.join(format!(".peerdav-{}.part", random_hex(8)));
    let (written, _) =
        write_response_to_path(response, &temp, limit, "download file", progress).await?;

    if conflict == ConflictPolicy::Fail && destination.exists() {
        remove_file_best_effort(&temp);
        return Err(DomainError::TransferConflict {
            path: destination.display().to_string(),
        });
    }

    if let Err(error) = tokio::fs::rename(&temp, destination).await {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            remove_file_best_effort(destination);
            if let Err(retry) = tokio::fs::rename(&temp, destination).await {
                remove_file_best_effort(&temp);
                return Err(io_error("commit download", destination, retry));
            }
        } else {
            remove_file_best_effort(&temp);
            return Err(io_error("commit download", destination, error));
        }
    }
    Ok(written)
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
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "txt" | "log" | "md" | "markdown" | "rst" => "text/plain",
        "csv" | "tsv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "text/yaml",
        "toml" | "ini" | "conf" | "cfg" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "ts" | "tsx" | "jsx" | "rs" | "go" | "py" | "java" | "c" | "h" | "cpp" | "hpp" | "sh"
        | "bat" | "ps1" | "sql" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/xml" | "application/javascript"
        )
}

fn truncate_utf8(mut text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
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
