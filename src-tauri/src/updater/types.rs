use std::time::Duration;

use serde::Serialize;
use tauri_plugin_updater::Update;

/// Budget for a single update-check request. Endpoints are tried in order, so this also bounds how
/// long a blocked mirror delays the fallback that follows it.
pub(super) const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Connection budget, including DNS and the TLS handshake. A blocked route usually drops packets
/// silently until the operating system gives up minutes later; failing here keeps the failover to
/// the next endpoint quick.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Budget for a single read from the server, used to drop connections that were established but
/// never answer. Downloads are unbounded by design and are not affected by this value.
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Budget for verifying that a proxy taken from the environment is running before update requests
/// are routed through it.
pub(super) const PROXY_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// How update requests must reach the endpoints.
pub(super) enum ProxyChoice {
    /// No proxy is configured; leave the platform defaults alone.
    Platform,
    /// Route update requests through this proxy.
    Url(url::Url),
    /// A proxy is configured but unusable; connect directly instead of failing through it.
    Direct,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateStatus {
    pub configured: bool,
    pub current_version: String,
    pub phase: UpdatePhase,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub downloaded: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum UpdatePhase {
    #[default]
    Idle,
    Checking,
    Downloading,
    Ready,
    Installing,
    UpToDate,
    Failed,
}

pub(super) struct ReadyUpdate {
    pub update: Update,
    pub bytes: Vec<u8>,
}
