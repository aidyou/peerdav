use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroize;

use super::types::{ProxyMode, S3Config};
use crate::sync::repository_types::{RepoDescriptor, RepositoryMode};
use crate::sync::root_state::RootError;

pub const DEFAULT_SCHEDULE_INTERVAL: u64 = 900;
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudConnection {
    pub id: String,
    pub name: String,
    pub config: S3Config,
    pub revision: u64,
    pub schedule: ScheduleConfig,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudConfigSnapshot {
    pub connections: Vec<CloudConnection>,
    pub mappings: Vec<CloudMapping>,
    #[serde(default)]
    pub repositories: Vec<CachedRepository>,
}
impl Default for CloudConfigSnapshot {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            mappings: Vec::new(),
            repositories: Vec::new(),
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
    #[serde(default)]
    pub proxy: ProxyMode,
    pub schedule: Option<ScheduleConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRedacted {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub bucket: String,
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
            id: self.id.clone(),
            name: self.name.clone(),
            endpoint: self.config.endpoint.clone(),
            bucket: self.config.bucket.clone(),
            region: self.config.region.clone(),
            prefix: self.config.prefix.clone(),
            path_style: self.config.path_style,
            has_credentials: !self.config.access_key.is_empty()
                || !self.config.secret_key.is_empty(),
            has_proxy_auth: matches!(&self.config.proxy, ProxyMode::Custom { username, password, .. } if username.is_some() || password.is_some()),
            proxy: match &self.config.proxy {
                ProxyMode::Direct => ProxyMode::Direct,
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
