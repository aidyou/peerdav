use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroize;

use super::types::{ProxyMode, S3Config};
use crate::sync::append_types::{AppendMode, TrustSource};
use crate::sync::repository_types::{RepoDescriptor, RepositoryMode};
use crate::sync::root_state::RootError;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CachedAppendRepository {
    pub connection_id: String,
    pub binding: AppendRepositoryBinding,
    pub password: String,
}
impl Drop for CachedAppendRepository {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendRepositoryRedacted {
    pub connection_id: String,
    pub protocol: StorageProtocol,
    pub binding: AppendRepositoryBinding,
}
impl CachedAppendRepository {
    pub fn redacted(&self) -> AppendRepositoryRedacted {
        AppendRepositoryRedacted {
            connection_id: self.connection_id.clone(),
            protocol: StorageProtocol::AppendV2,
            binding: self.binding.clone(),
        }
    }
}

pub const DEFAULT_SCHEDULE_INTERVAL: u64 = 120;
pub const MIN_SCHEDULE_INTERVAL: u64 = 60;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cloud configuration is not available while the vault is locked")]
    Vault(#[from] crate::DomainError),
    #[error("cloud configuration serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("root validation failed")]
    Root(#[from] RootError),
    #[error("cloud configuration validation failed: {0}")]
    Validation(String),
    #[error("mapping conflict ({kind}): requested={requested}, existing={existing}, drive={drive}, mapping={mapping_id}")]
    MappingConflict { kind: &'static str, requested: String, existing: String, drive: String, mapping_id: String },
    #[error("cloud configuration item was not found: {0}")]
    NotFound(String),
    #[error("secure random generation failed: {0}")]
    Random(#[from] rand::Error),
    #[error("connection revision overflow")]
    RevisionOverflow,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_interval")]
    pub interval: u64,
}
fn default_interval() -> u64 {
    DEFAULT_SCHEDULE_INTERVAL
}
impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: DEFAULT_SCHEDULE_INTERVAL,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CachedRepository {
    pub connection_id: String,
    pub descriptor: RepoDescriptor,
    #[serde(default)]
    pub master: Option<[u8; 32]>,
}

impl fmt::Debug for CachedRepository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedRepository")
            .field("connection_id", &self.connection_id)
            .field("descriptor", &self.descriptor)
            .field("master", &self.master.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl Drop for CachedRepository {
    fn drop(&mut self) {
        if let Some(master) = self.master.as_mut() {
            master.zeroize();
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryRedacted {
    pub id: String,
    pub connection_id: String,
    pub mode: RepositoryMode,
}

impl CachedRepository {
    pub fn redacted(&self) -> RepositoryRedacted {
        RepositoryRedacted {
            id: self.descriptor.repo_id.clone(),
            connection_id: self.connection_id.clone(),
            mode: self.descriptor.mode.clone(),
        }
    }
}
/// Transport identity is separate from the encrypted repository format.
/// Only implemented transports are accepted; legacy connections are S3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudProtocolId {
    #[default]
    S3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudConnection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub protocol_id: CloudProtocolId,
    pub config: S3Config,
    pub revision: u64,
    pub schedule: ScheduleConfig,
}

/// Missing protocol fields in legacy configurations mean CAS v1. Unknown values fail closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageProtocol {
    #[default]
    CasV1,
    AppendV2,
}

/// Persisted remote-root selection. Candidate order never implies selection.
/// This is metadata only; repository keys/passwords remain in the vault's existing secret path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppendStorage {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppendRepositoryBinding {
    #[serde(default)]
    pub storage: Option<AppendStorage>,
    pub repo_id: String,
    pub anchor_key: String,
    pub anchor_fingerprint: String,
    pub append_mode: AppendMode,
    pub trust_source: TrustSource,
    #[serde(default)]
    pub root_candidates: Vec<String>,
    #[serde(default)]
    pub selected_root: Option<String>,
}

impl AppendRepositoryBinding {
    pub fn is_trusted(&self) -> bool {
        matches!(
            self.trust_source,
            TrustSource::TrustedFingerprint | TrustSource::AcceptedTofu
        )
    }
}

/// Only the runtime that verified the anchor/explicit user confirmation should construct this.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveAppendBindingInput {
    pub mapping_id: String,
    pub binding: AppendRepositoryBinding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudMapping {
    pub id: String,
    pub connection_id: String,
    pub repository_id: String,
    pub local_root: PathBuf,
    pub logical_path: String,
    pub schedule: ScheduleConfig,
    /// Defaults provide old-to-new compatibility only (older deny_unknown_fields readers reject these).
    #[serde(default)]
    pub protocol: StorageProtocol,
    #[serde(default)]
    pub append_binding: Option<AppendRepositoryBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudConfigSnapshot {
    #[serde(default = "system_proxy")]
    pub client_proxy: ProxyMode,
    #[serde(default)]
    pub drive_proxies: std::collections::HashMap<String, bool>,
    pub connections: Vec<CloudConnection>,
    #[serde(default)]
    pub official_connections: Vec<super::official_types::OfficialConnection>,
    pub mappings: Vec<CloudMapping>,
    #[serde(default)]
    pub repositories: Vec<CachedRepository>,
    #[serde(default)]
    pub append_repositories: Vec<AppendRepositoryRedacted>,
    /// 明文镜像同步映射（路线 C）：保持目录结构的双向镜像，独立于加密同步盘。
    #[serde(default)]
    pub mirror_mappings: Vec<crate::mirror::MirrorMapping>,
}
pub fn client_proxy() -> ProxyMode { ProxyMode::Client }
pub fn system_proxy() -> ProxyMode { ProxyMode::Environment }

impl Zeroize for CloudConfigSnapshot {
    fn zeroize(&mut self) {
        fn clear_proxy(proxy: &mut ProxyMode) {
            if let ProxyMode::Custom { url, username, password } = proxy {
                url.zeroize();
                username.zeroize();
                password.zeroize();
            }
        }
        clear_proxy(&mut self.client_proxy);
        for connection in &mut self.connections {
            connection.config.access_key.zeroize();
            connection.config.secret_key.zeroize();
            connection.config.session_token.zeroize();
            clear_proxy(&mut connection.config.proxy);
        }
        for connection in &mut self.official_connections {
            connection.client_secret.zeroize();
            connection.refresh_token.zeroize();
        }
        // Repository types zeroize passwords/master keys in their Drop impls.
        self.repositories.clear();
        self.append_repositories.clear();
    }
}

impl Default for CloudConfigSnapshot {
    fn default() -> Self {
        Self {
            client_proxy: system_proxy(),
            drive_proxies: Default::default(),
            connections: Vec::new(),
            official_connections: Vec::new(),
            mappings: Vec::new(),
            repositories: Vec::new(),
            append_repositories: Vec::new(),
            mirror_mappings: Vec::new(),
        }
    }
}

/// Credentials are optional on update: None or an empty string retains the existing value.
/// Explicit clear flags are required to remove secrets.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveS3Input {
    pub id: Option<String>,
    pub name: String,
    pub endpoint: String,
    pub bucket: String,
    #[serde(default)]
    pub buckets: Vec<String>,
    pub region: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub path_style: bool,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub clear_credentials: bool,
    #[serde(default)]
    pub clear_session_token: bool,
    #[serde(default)]
    pub clear_proxy_auth: bool,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default = "client_proxy")]
    pub proxy: ProxyMode,
    pub schedule: Option<ScheduleConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCredentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRedacted {
    #[serde(default)]
    pub browser_capabilities: super::protocol_types::ConnectionBrowserCapabilities,
    pub id: String,
    pub name: String,
    pub protocol_id: CloudProtocolId,
    pub endpoint: String,
    pub bucket: String,
    pub buckets: Vec<String>,
    pub region: String,
    pub prefix: String,
    pub path_style: bool,
    pub has_credentials: bool,
    pub has_proxy_auth: bool,
    pub proxy: ProxyMode,
    pub allow_insecure_http: bool,
    pub revision: u64,
    pub schedule: ScheduleConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMappingInput {
    pub connection_id: String,
    pub repository_id: String,
    pub local_root: PathBuf,
    pub logical_path: String,
    #[serde(default)]
    pub schedule: ScheduleConfig,
}

pub type EditMappingInput = AddMappingInput;
impl std::fmt::Debug for SaveS3Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveS3Input")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("access_key", &"[redacted]")
            .field("secret_key", &"[redacted]")
            .field("session_token", &"[redacted]")
            .field("clear_credentials", &self.clear_credentials)
            .field("clear_session_token", &self.clear_session_token)
            .field("clear_proxy_auth", &self.clear_proxy_auth)
            .field("allow_insecure_http", &self.allow_insecure_http)
            .field("proxy", &self.proxy)
            .field("schedule", &self.schedule)
            .finish()
    }
}

impl CloudConnection {
    pub fn redacted(&self) -> ConnectionRedacted {
        ConnectionRedacted {
            browser_capabilities: super::protocol_types::ConnectionBrowserCapabilities { select_root: true, sync_drives: true },
            id: self.id.clone(),
            name: self.name.clone(),
            protocol_id: self.protocol_id,
            endpoint: self.config.endpoint.clone(),
            bucket: self.config.bucket.clone(),
            buckets: if self.config.buckets.is_empty() {
                vec![self.config.bucket.clone()]
            } else {
                self.config.buckets.clone()
            },
            region: self.config.region.clone(),
            prefix: self.config.prefix.clone(),
            path_style: self.config.path_style,
            has_credentials: !self.config.access_key.is_empty()
                || !self.config.secret_key.is_empty(),
            has_proxy_auth: matches!(&self.config.proxy, ProxyMode::Custom { username, password, .. } if username.is_some() || password.is_some()),
            proxy: match &self.config.proxy {
                ProxyMode::Direct => ProxyMode::Direct,
                ProxyMode::Client => ProxyMode::Client,
                ProxyMode::Environment => ProxyMode::Environment,
                ProxyMode::Custom { url, .. } => ProxyMode::Custom {
                    url: url.clone(),
                    username: None,
                    password: None,
                },
            },
            allow_insecure_http: self.config.allow_insecure_http,
            revision: self.revision,
            schedule: self.schedule.clone(),
        }
    }
}

pub(crate) fn validate_schedule(schedule: &ScheduleConfig) -> Result<(), ConfigError> {
    if schedule.interval < MIN_SCHEDULE_INTERVAL {
        return Err(ConfigError::Validation(format!(
            "schedule interval must be at least {MIN_SCHEDULE_INTERVAL}"
        )));
    }
    Ok(())
}
