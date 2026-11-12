pub(crate) struct GoogleObjectStore {
    pub parents: tokio::sync::Mutex<()>,
    pub runtime: std::sync::Weak<super::runtime::CloudRuntime>,
    pub connection: String,
    pub revision: u64,
    pub root: String,
}

use super::oauth_types::OfficialProvider;
use serde::{Deserialize, Serialize};

/// Kept only inside the encrypted Vault snapshot. Never return this DTO to UI.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficialConnection {
    pub id: String,
    pub name: String,
    pub provider: OfficialProvider,
    pub config_version: u32,
    pub revision: u64,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub refresh_token: String,
    pub root_id: String,
}

impl std::fmt::Debug for OfficialConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfficialConnection").field("id", &self.id)
            .field("provider", &self.provider).field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateOfficialConnectionInput {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub client_secret: Option<String>,
    #[serde(default)]
    pub clear_client_secret: bool,
    pub direct: bool,
}

impl std::fmt::Debug for UpdateOfficialConnectionInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateOfficialConnectionInput").field("id", &self.id).field("revision", &self.revision).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialConnectionRedacted {
    pub browser_capabilities: super::protocol_types::ConnectionBrowserCapabilities,
    pub id: String,
    pub name: String,
    pub protocol_id: OfficialProvider,
    pub config_version: u32,
    pub revision: u64,
    pub root_id: String,
    pub has_credentials: bool,
}

impl OfficialConnection {
    pub(crate) fn validate(&self) -> Result<(), super::config_types::ConfigError> {
        if self.config_version != 1 || self.name.trim().is_empty() || self.name.len() > 256
            || self.client_id.trim().is_empty() || self.client_id.len() > 2048
            || self.refresh_token.trim().is_empty() || self.refresh_token.len() > 16384
            || self.root_id.trim().is_empty() || self.root_id.len() > 4096
            || self.client_secret.as_ref().is_some_and(|value| value.len() > 16384)
            || (self.provider == OfficialProvider::BaiduNetdisk && (!self.root_id.starts_with('/')
                || self.root_id.split('/').any(|part| matches!(part, "." | ".."))))
            || (self.provider == OfficialProvider::Dropbox && !self.root_id.starts_with('/'))
            || (self.provider == OfficialProvider::YandexDisk && !self.root_id.starts_with("disk:/"))
            || self.root_id.contains('\0') {
            return Err(super::config_types::ConfigError::Validation("invalid official connection configuration".into()));
        }
        Ok(())
    }

    pub fn redacted(&self) -> OfficialConnectionRedacted {
        OfficialConnectionRedacted {
            browser_capabilities: super::protocol_types::ConnectionBrowserCapabilities { select_root: false, sync_drives: self.provider != OfficialProvider::YandexDisk },
            id: self.id.clone(), name: self.name.clone(), protocol_id: self.provider,
            config_version: self.config_version, revision: self.revision,
            root_id: self.root_id.clone(), has_credentials: !self.refresh_token.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(provider: OfficialProvider) -> OfficialConnection {
        OfficialConnection {
            id: "id".into(), name: "drive".into(), provider,
            config_version: 1, revision: 1, client_id: "client".into(), client_secret: None,
            refresh_token: "refresh".into(),
            root_id: match provider {
                OfficialProvider::BaiduNetdisk | OfficialProvider::Dropbox => "/".into(),
                OfficialProvider::YandexDisk => "disk:/".into(),
                _ => "root".into(),
            },
        }
    }

    #[test]
    fn sync_capabilities_follow_the_wired_object_store_adapters() {
        for provider in [OfficialProvider::GoogleDrive, OfficialProvider::OneDrive, OfficialProvider::BaiduNetdisk, OfficialProvider::Dropbox] {
            assert!(connection(provider).redacted().browser_capabilities.sync_drives, "{provider:?}");
        }
        assert!(!connection(OfficialProvider::YandexDisk).redacted().browser_capabilities.sync_drives);
    }
}
