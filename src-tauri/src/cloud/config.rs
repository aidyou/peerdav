use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rand::RngCore;
use zeroize::Zeroizing;

use super::config_types::*;
use super::types::{ProxyMode, S3Config};
use crate::sync::append_types::APPEND_ANCHORS;
use crate::sync::repository_types::RepoDescriptor;
use crate::vault::Vault;

const SECRET_NAME: &str = "cloud.peerdav-s3.config";
const INDEX_KEY_SECRET: &str = "cloud.local-index-key";

/// Short-lived in-process guard. It never crosses an await or performs network I/O.
pub struct CloudConfigStore {
    vault: Arc<Vault>,
    lock: Mutex<()>,
}

impl CloudConfigStore {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self {
            vault,
            lock: Mutex::new(()),
        }
    }

    fn guard(&self) -> Result<MutexGuard<'_, ()>, ConfigError> {
        self.lock
            .lock()
            .map_err(|_| ConfigError::Validation("cloud config lock poisoned".into()))
    }

    fn load_locked(&self) -> Result<CloudConfigSnapshot, ConfigError> {
        match self.vault.load_secret(SECRET_NAME) {
            Ok(bytes) => {
                let snapshot: CloudConfigSnapshot = serde_json::from_slice(&bytes)?;
                for mapping in &snapshot.mappings {
                    validate_mapping_protocol(mapping)?;
                }
                let mut identities = std::collections::HashSet::new();
                for connection in &snapshot.connections { identities.insert(connection.id.as_str()); }
                for connection in &snapshot.official_connections {
                    connection.validate()?;
                    if connection.id.is_empty() || connection.revision == 0 || !identities.insert(connection.id.as_str()) {
                        return Err(ConfigError::Validation("invalid official connection identity".into()));
                    }
                }
                Ok(snapshot)
            }
            Err(crate::DomainError::SecretNotFound { .. }) => Ok(CloudConfigSnapshot::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn save_locked(&self, snapshot: &CloudConfigSnapshot) -> Result<(), ConfigError> {
        let bytes = Zeroizing::new(serde_json::to_vec(snapshot)?);
        self.vault.store_secret(SECRET_NAME, &bytes)?;
        Ok(())
    }

    /// Official credentials are encrypted by the same Vault transaction as S3.
    pub fn save_official_connection(
        &self,
        connection: super::official_types::OfficialConnection,
    ) -> Result<super::official_types::OfficialConnectionRedacted, ConfigError> {
        self.save_official_connection_with_proxy(connection, None)
    }

    pub fn save_official_connection_with_proxy(
        &self,
        mut connection: super::official_types::OfficialConnection,
        direct: Option<bool>,
    ) -> Result<super::official_types::OfficialConnectionRedacted, ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        connection.validate()?;
        if connection.id.is_empty() {
            connection.id = crate::local_upload::token().map_err(|_| ConfigError::Validation("connection identity unavailable".into()))?;
            connection.revision = 1;
            snapshot.official_connections.push(connection.clone());
        } else {
            let existing = snapshot.official_connections.iter_mut().find(|entry| entry.id == connection.id)
                .ok_or_else(|| ConfigError::NotFound("official connection".into()))?;
            if existing.provider != connection.provider || existing.revision != connection.revision {
                return Err(ConfigError::Validation("official connection changed".into()));
            }
            connection.revision = existing.revision.checked_add(1)
                .ok_or_else(|| ConfigError::Validation("connection revision exhausted".into()))?;
            *existing = connection.clone();
        }
        if let Some(direct) = direct { snapshot.drive_proxies.insert(connection.id.clone(), direct); }
        self.save_locked(&snapshot)?;
        Ok(connection.redacted())
    }

    pub fn update_official_connection(&self, input: super::official_types::UpdateOfficialConnectionInput) -> Result<super::official_types::OfficialConnectionRedacted, ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        let connection = snapshot.official_connections.iter_mut().find(|entry| entry.id == input.id)
            .ok_or_else(|| ConfigError::NotFound("official connection".into()))?;
        if connection.revision != input.revision { return Err(ConfigError::Validation("official connection changed".into())); }
        let old_secret = connection.client_secret.clone();
        let old_direct = snapshot.drive_proxies.get(&input.id).copied().unwrap_or(false);
        connection.name = input.name.trim().to_owned();
        if input.clear_client_secret { connection.client_secret = None; }
        else if let Some(secret) = input.client_secret.filter(|value| !value.is_empty()) { connection.client_secret = Some(secret); }
        connection.validate()?;
        // Display names do not change the admitted account, root or transport.
        // Keep existing sync stores and access tokens valid for metadata-only edits.
        if connection.client_secret != old_secret || input.direct != old_direct {
            connection.revision = connection.revision.checked_add(1).ok_or(ConfigError::RevisionOverflow)?;
        }
        let redacted = connection.redacted();
        snapshot.drive_proxies.insert(input.id, input.direct);
        self.save_locked(&snapshot)?;
        Ok(redacted)
    }

    /// Token rotation does not change the account/root generation of active sessions.
    pub fn rotate_official_token(&self, id: &str, revision: u64, token: &str) -> Result<(), ConfigError> {
        if token.trim().is_empty() || token.len() > 16384 {
            return Err(ConfigError::Validation("invalid refresh token".into()));
        }
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        let connection = snapshot.official_connections.iter_mut().find(|entry| entry.id == id && entry.revision == revision)
            .ok_or_else(|| ConfigError::NotFound("official connection revision".into()))?;
        connection.refresh_token = token.to_owned();
        self.save_locked(&snapshot)
    }

    pub fn save_client_proxy(&self, mut proxy: ProxyMode) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        match &mut proxy {
            ProxyMode::Client => return Err(ConfigError::Validation("client proxy cannot inherit itself".into())),
            ProxyMode::Custom { url, username, password } => {
                let parsed = url::Url::parse(url).map_err(|_| ConfigError::Validation("invalid HTTP proxy address".into()))?;
                if parsed.scheme() != "http" || parsed.host_str().is_none() || !parsed.username().is_empty()
                    || parsed.password().is_some() || parsed.query().is_some() || parsed.fragment().is_some()
                    || !matches!(parsed.path(), "" | "/") {
                    return Err(ConfigError::Validation("HTTP proxy requires an http://host:port address without credentials or path".into()));
                }
                if username.as_ref().is_some_and(|v| v.len() > 2048) || password.as_ref().is_some_and(|v| v.len() > 4096) {
                    return Err(ConfigError::Validation("proxy credentials too long".into()));
                }
                if password.is_none() {
                    if let ProxyMode::Custom { url: old_url, username: old_username, password: old_password } = &snapshot.client_proxy {
                        if old_url == url && old_username == username { *password = old_password.clone(); }
                    }
                }
            }
            _ => {}
        }
        for connection in snapshot.connections.iter_mut().filter(|entry| matches!(entry.config.proxy, ProxyMode::Client)) {
            connection.revision = connection.revision.checked_add(1).ok_or(ConfigError::RevisionOverflow)?;
        }
        for connection in &mut snapshot.official_connections {
            connection.revision = connection.revision.checked_add(1).ok_or(ConfigError::RevisionOverflow)?;
        }
        snapshot.client_proxy = proxy;
        self.save_locked(&snapshot)
    }

    pub fn save_drive_proxy(&self, id: &str, direct: bool) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        if let Some(connection) = snapshot.connections.iter_mut().find(|entry| entry.id == id) {
            connection.config.proxy = if direct { ProxyMode::Direct } else { ProxyMode::Client };
            connection.revision = connection.revision.checked_add(1).ok_or(ConfigError::RevisionOverflow)?;
        } else if let Some(connection) = snapshot.official_connections.iter_mut().find(|entry| entry.id == id) {
            connection.revision = connection.revision.checked_add(1).ok_or(ConfigError::RevisionOverflow)?;
        } else {
            return Err(ConfigError::NotFound("connection".into()));
        }
        snapshot.drive_proxies.insert(id.to_owned(), direct);
        self.save_locked(&snapshot)
    }

    pub fn snapshot(&self) -> Result<CloudConfigSnapshot, ConfigError> {
        let _guard = self.guard()?;
        self.load_locked()
    }

    pub fn list_redacted(&self) -> Result<Vec<ConnectionRedacted>, ConfigError> {
        Ok(self
            .snapshot()?
            .connections
            .iter()
            .map(CloudConnection::redacted)
            .collect())
    }

    pub fn save_connection(&self, input: SaveS3Input) -> Result<ConnectionRedacted, ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        let existing = input
            .id
            .as_ref()
            .and_then(|id| snapshot.connections.iter().find(|c| &c.id == id).cloned());
        if input.id.is_some() && existing.is_none() {
            return Err(ConfigError::NotFound("connection".into()));
        }
        let schedule = match input.schedule {
            Some(schedule) => {
                validate_schedule(&schedule)?;
                schedule
            }
            None => existing
                .as_ref()
                .map(|c| c.schedule.clone())
                .unwrap_or_default(),
        };
        let (id, revision, old) = match existing {
            Some(c) => (
                c.id.clone(),
                c.revision
                    .checked_add(1)
                    .ok_or(ConfigError::RevisionOverflow)?,
                Some(c),
            ),
            None => (random_id()?, 1, None),
        };
        let access_key = if input.clear_credentials {
            String::new()
        } else {
            input
                .access_key
                .filter(|v| !v.is_empty())
                .or_else(|| old.as_ref().map(|c| c.config.access_key.clone()))
                .unwrap_or_default()
        };
        let secret_key = if input.clear_credentials {
            String::new()
        } else {
            input
                .secret_key
                .filter(|v| !v.is_empty())
                .or_else(|| old.as_ref().map(|c| c.config.secret_key.clone()))
                .unwrap_or_default()
        };
        let session_token = if input.clear_session_token {
            None
        } else {
            input
                .session_token
                .filter(|v| !v.is_empty())
                .or_else(|| old.as_ref().and_then(|c| c.config.session_token.clone()))
        };
        let proxy = if input.clear_proxy_auth {
            match input.proxy {
                ProxyMode::Custom { url, .. } => ProxyMode::Custom {
                    url,
                    username: None,
                    password: None,
                },
                other => other,
            }
        } else if let ProxyMode::Custom {
            url,
            username,
            password,
        } = input.proxy
        {
            let old_auth = old.as_ref().and_then(|c| match &c.config.proxy {
                ProxyMode::Custom {
                    username, password, ..
                } => Some((username.clone(), password.clone())),
                _ => None,
            });
            ProxyMode::Custom {
                url,
                username: username
                    .filter(|v| !v.is_empty())
                    .or_else(|| old_auth.as_ref().and_then(|v| v.0.clone())),
                password: password
                    .filter(|v| !v.is_empty())
                    .or_else(|| old_auth.and_then(|v| v.1)),
            }
        } else {
            input.proxy
        };
        let mut config = S3Config {
            endpoint: input.endpoint,
            bucket: input.bucket,
            region: input.region,
            prefix: input.prefix,
            path_style: input.path_style,
            access_key,
            secret_key,
            session_token,
            buckets: input.buckets,
            allow_insecure_http: input.allow_insecure_http,
            proxy,
        };
        // Users only provide an address; infer the region from it when omitted.
        if config.region.trim().is_empty() {
            let host = url::Url::parse(&config.endpoint)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default();
            config.region = super::service::derive_region(&host);
        }
        // Denormalize the multi-bucket selection: `bucket` stays primary for
        // legacy sync, while `buckets` is the ordered, de-duplicated list.
        let mut buckets = Vec::new();
        for bucket in config.buckets.drain(..) {
            let bucket = bucket.trim().to_owned();
            if !bucket.is_empty() && !buckets.contains(&bucket) {
                buckets.push(bucket);
            }
        }
        if buckets.is_empty() && !config.bucket.trim().is_empty() {
            buckets.push(config.bucket.trim().to_owned());
        }
        if let Some(primary) = buckets.first() {
            config.bucket = primary.clone();
        }
        config.buckets = buckets;
        validate_config(&config)?;
        let connection = CloudConnection {
            id: id.clone(),
            name: input.name,
            protocol_id: CloudProtocolId::S3,
            config,
            revision,
            schedule,
        };
        if let Some(slot) = snapshot.connections.iter_mut().find(|c| c.id == id) {
            *slot = connection.clone();
        } else {
            snapshot.connections.push(connection.clone());
        }
        self.save_locked(&snapshot)?;
        Ok(connection.redacted())
    }

    pub fn delete_connection(&self, id: &str) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        snapshot.connections.retain(|c| c.id != id);
        snapshot.official_connections.retain(|c| c.id != id);
        snapshot.mappings.retain(|m| m.connection_id != id);
        snapshot.repositories.retain(|r| r.connection_id != id);
        snapshot
            .append_repositories
            .retain(|r| r.connection_id != id);
        snapshot.mirror_mappings.retain(|m| m.connection_id != id);
        self.save_locked(&snapshot)
    }

    pub fn add_mapping(
        &self,
        input: AddMappingInput,
        protected: &[&Path],
    ) -> Result<CloudMapping, ConfigError> {
        self.save_mapping(None, input, protected, None)
    }

    pub fn edit_mapping(
        &self,
        id: &str,
        input: EditMappingInput,
        protected: &[&Path],
    ) -> Result<CloudMapping, ConfigError> {
        self.save_mapping(Some(id), input, protected, None)
    }

    pub fn save_append_mapping(
        &self,
        id: Option<&str>,
        input: AddMappingInput,
        protected: &[&Path],
        binding: AppendRepositoryBinding,
    ) -> Result<CloudMapping, ConfigError> {
        self.save_mapping(id, input, protected, Some(binding))
    }

    fn save_mapping(
        &self,
        id: Option<&str>,
        input: AddMappingInput,
        protected: &[&Path],
        append: Option<AppendRepositoryBinding>,
    ) -> Result<CloudMapping, ConfigError> {
        self.mapping_inner(id, input, protected, append, true, false)
    }

    pub(crate) fn prepare_mapping(
        &self, input: AddMappingInput, protected: &[&Path], append: Option<AppendRepositoryBinding>,
    ) -> Result<CloudMapping, ConfigError> {
        let mut mapping = self.mapping_inner(None, input, protected, append, false, false)?;
        // Stable prebinding identity preserves failed discovery reservations across
        // cancel/retry/restart; it is never the executable mapping identity.
        use sha2::{Digest, Sha256};
        let identity = serde_json::to_vec(&(&mapping.connection_id, &mapping.repository_id, &mapping.local_root, &mapping.logical_path, &mapping.append_binding))?;
        mapping.id = format!("prepare-{}", hex::encode(Sha256::digest(identity)));
        Ok(mapping)
    }

    pub(crate) fn commit_prepared_mapping(&self, mapping: &CloudMapping, protected: &[&Path]) -> Result<CloudMapping, ConfigError> {
        let input = AddMappingInput { connection_id: mapping.connection_id.clone(), repository_id: mapping.repository_id.clone(), local_root: mapping.local_root.clone(), logical_path: mapping.logical_path.clone(), schedule: mapping.schedule.clone() };
        // Revalidate overlaps and the physical root immediately before publishing.
        let id = random_id()?;
        self.mapping_inner(Some(&id), input, protected, mapping.append_binding.clone(), true, true)
    }

    fn mapping_inner(
        &self, id: Option<&str>, input: AddMappingInput, protected: &[&Path],
        append: Option<AppendRepositoryBinding>, persist: bool, prepared: bool,
    ) -> Result<CloudMapping, ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        validate_schedule(&input.schedule)?;
        if !snapshot
            .connections
            .iter()
            .any(|c| c.id == input.connection_id)
            && !snapshot.official_connections.iter().any(|c| c.id == input.connection_id)
        {
            return Err(ConfigError::Validation("connection does not exist".into()));
        }
        if prepared && snapshot.mappings.iter().any(|m| Some(m.id.as_str()) == id) {
            return Err(ConfigError::Validation("prepared mapping already exists".into()));
        }
        if id.is_some() && !prepared && !snapshot.mappings.iter().any(|m| Some(m.id.as_str()) == id) {
            return Err(ConfigError::NotFound("mapping".into()));
        }
        let binding = crate::sync::root_state::bind_initial(input.local_root.clone())?;
        let root = std::fs::canonicalize(&binding.root_path)
            .map_err(crate::sync::root_types::RootError::Io)?;
        crate::sync::paths::validate_root_protection(&root, protected)
            .map_err(|e| ConfigError::Validation(e.to_string()))?;
        let logical = input.logical_path.trim_matches('/').to_owned();
        if logical.is_empty() || logical.split('/').any(|part| part == "." || part == "..") {
            return Err(ConfigError::Validation("logical path is invalid".into()));
        }
        if snapshot.mappings.iter().any(|m| {
            Some(m.id.as_str()) != id
                && ((m.local_root.starts_with(&root) || root.starts_with(&m.local_root))
                    || (m.repository_id == input.repository_id
                        && logical_overlap(&m.logical_path, &logical)))
        }) {
            return Err(ConfigError::Validation(
                "mapping overlaps an existing mapping".into(),
            ));
        }
        if snapshot
            .mirror_mappings
            .iter()
            .any(|m| m.local_root.starts_with(&root) || root.starts_with(&m.local_root))
        {
            return Err(ConfigError::Validation(
                "本地目录与已有镜像映射重叠 / Local root overlaps an existing mirror mapping"
                    .into(),
            ));
        }
        let existing = snapshot.mappings.iter().find(|m| Some(m.id.as_str()) == id);
        if let Some(mapping) = existing {
            if mapping.protocol == StorageProtocol::AppendV2
                && (mapping.connection_id != input.connection_id
                    || mapping.repository_id != input.repository_id)
            {
                return Err(ConfigError::Validation(
                    "append binding must be removed before changing its connection or repository"
                        .into(),
                ));
            }
        }
        let mapping = CloudMapping {
            protocol: if append.is_some() {
                StorageProtocol::AppendV2
            } else {
                existing.map(|m| m.protocol).unwrap_or_default()
            },
            append_binding: append.or_else(|| existing.and_then(|m| m.append_binding.clone())),
            id: id.map(str::to_owned).unwrap_or(random_id()?),
            connection_id: input.connection_id,
            repository_id: input.repository_id,
            local_root: root,
            logical_path: logical,
            schedule: input.schedule,
        };
        validate_mapping_protocol(&mapping)?;
        if let Some(slot) = snapshot
            .mappings
            .iter_mut()
            .find(|m| Some(m.id.as_str()) == id)
        {
            *slot = mapping.clone();
        } else {
            snapshot.mappings.push(mapping.clone());
        }
        if persist { self.save_locked(&snapshot)?; }
        Ok(mapping)
    }

    /// Store verified discovery metadata or a pending selection via the existing encrypted vault.
    /// This does not confirm TOFU or choose a root on the caller's behalf.
    pub fn save_append_binding(
        &self,
        input: SaveAppendBindingInput,
    ) -> Result<CloudMapping, ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        let mapping = snapshot
            .mappings
            .iter_mut()
            .find(|m| m.id == input.mapping_id)
            .ok_or_else(|| ConfigError::NotFound("mapping".into()))?;
        mapping.protocol = StorageProtocol::AppendV2;
        mapping.append_binding = Some(input.binding);
        validate_mapping_protocol(mapping)?;
        let saved = mapping.clone();
        self.save_locked(&snapshot)?;
        Ok(saved)
    }

    /// Returns pending metadata unchanged, including unconfirmed trust and no root selection.
    pub fn load_append_binding(
        &self,
        mapping_id: &str,
    ) -> Result<AppendRepositoryBinding, ConfigError> {
        let snapshot = self.snapshot()?;
        let mapping = snapshot
            .mappings
            .into_iter()
            .find(|m| m.id == mapping_id)
            .ok_or_else(|| ConfigError::NotFound("mapping".into()))?;
        if mapping.protocol != StorageProtocol::AppendV2 {
            return Err(ConfigError::Validation(
                "mapping does not use append v2".into(),
            ));
        }
        mapping
            .append_binding
            .ok_or_else(|| ConfigError::Validation("append binding is missing".into()))
    }

    /// Runtime must use this gate before opening a repository. It never upgrades trust or selects roots.
    pub fn load_trusted_append_binding(
        &self,
        mapping_id: &str,
    ) -> Result<AppendRepositoryBinding, ConfigError> {
        let binding = self.load_append_binding(mapping_id)?;
        if !binding.is_trusted() {
            return Err(ConfigError::Validation(
                "append anchor trust is unconfirmed".into(),
            ));
        }
        if binding.selected_root.is_none() {
            return Err(ConfigError::Validation(
                "append root must be explicitly selected".into(),
            ));
        }
        Ok(binding)
    }

    pub fn delete_mapping(&self, id: &str) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        snapshot.mappings.retain(|m| m.id != id);
        self.save_locked(&snapshot)
    }

    /// 保存（新建或更新）一条镜像同步映射。云端目录会被规范化，
    /// 同一连接 + 本地目录 + 云端目录的组合必须唯一，避免两条映射互相覆盖。
    pub fn save_mirror_mapping(
        &self,
        input: crate::mirror::SaveMirrorMappingInput,
    ) -> Result<crate::mirror::MirrorMapping, ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        let connection = snapshot.connections.iter().find(|c| c.id == input.connection_id);
        let official = snapshot.official_connections.iter().find(|c| c.id == input.connection_id && c.provider == super::oauth_types::OfficialProvider::GoogleDrive);
        let default_bucket = connection.map(|c| c.config.bucket.as_str()).or_else(|| official.map(|c| c.root_id.as_str())).ok_or_else(|| ConfigError::NotFound("connection".into()))?;
        let bucket = input.bucket.as_deref()
            .or_else(|| input.id.as_ref().and_then(|id| snapshot.mirror_mappings.iter().find(|m| &m.id == id).and_then(|m| m.bucket.as_deref())))
            .unwrap_or(default_bucket).to_owned();
        if connection.is_some_and(|c| bucket != c.config.bucket && !c.config.buckets.contains(&bucket)) || official.is_some_and(|c| bucket != c.root_id) {
            return Err(ConfigError::Validation("请选择已配置的存储范围 / Select a configured storage scope".into()));
        }
        let cloud_prefix = crate::mirror::normalize_cloud_prefix(&input.cloud_prefix)
            .map_err(|e| ConfigError::Validation(e.to_string()))?;
        validate_schedule(&input.schedule)?;
        let local_root = input.local_root.clone();
        if !local_root.is_absolute() {
            return Err(ConfigError::Validation(
                "本地目录必须是绝对路径 / Local root must be absolute".into(),
            ));
        }
        crate::sync::root_state::bind_initial(local_root.clone())?;
        let local_root =
            std::fs::canonicalize(&local_root).map_err(crate::sync::root_types::RootError::Io)?;
        if snapshot
            .mappings
            .iter()
            .any(|m| m.local_root.starts_with(&local_root) || local_root.starts_with(&m.local_root))
        {
            return Err(ConfigError::Validation(
                "本地目录与已有同步映射重叠 / Local root overlaps an existing sync mapping".into(),
            ));
        }
        let id = match input.id.as_deref() {
            Some(id) => {
                if !snapshot
                    .mirror_mappings
                    .iter()
                    .any(|m| m.id == id && m.connection_id == input.connection_id)
                {
                    return Err(ConfigError::NotFound("mirror mapping".into()));
                }
                id.to_owned()
            }
            None => random_id()?,
        };
        let mapping = crate::mirror::MirrorMapping {
            id: id.clone(),
            connection_id: input.connection_id.clone(),
            bucket: Some(bucket.clone()),
            local_root,
            cloud_prefix,
            schedule: input.schedule,
        };
        // 镜像映射会互相接管对方的文件，因此本地根不能在任意映射之间相等或嵌套
        // （即使连接不同，同一批本地文件也不能被两个远端同时镜像）；
        // 云端前缀只与同一连接（同一桶）下的映射比较，空前缀占满整个镜像根。
        for existing in &snapshot.mirror_mappings {
            if existing.id == id {
                continue;
            }
            if existing.local_root.starts_with(&mapping.local_root)
                || mapping.local_root.starts_with(&existing.local_root)
            {
                return Err(ConfigError::Validation(
                    "本地目录与已有镜像映射重叠 / Local root overlaps an existing mirror mapping"
                        .into(),
                ));
            }
            if existing.connection_id == mapping.connection_id
                && existing.bucket.as_deref().unwrap_or(default_bucket) == bucket
                && mirror_prefix_overlap(&existing.cloud_prefix, &mapping.cloud_prefix)
            {
                return Err(ConfigError::Validation(
                    "云端目录与已有镜像映射重叠 / Cloud prefix overlaps an existing mirror mapping"
                        .into(),
                ));
            }
        }
        snapshot.mirror_mappings.retain(|m| m.id != id);
        snapshot.mirror_mappings.push(mapping.clone());
        self.save_locked(&snapshot)?;
        Ok(mapping)
    }

    pub fn delete_mirror_mapping(&self, id: &str) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        snapshot.mirror_mappings.retain(|m| m.id != id);
        self.save_locked(&snapshot)
    }

    pub fn cache_repository(
        &self,
        connection_id: &str,
        descriptor: RepoDescriptor,
        master: Option<Zeroizing<[u8; 32]>>,
    ) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        let cached = CachedRepository {
            connection_id: connection_id.to_owned(),
            descriptor,
            master: master.map(|key| *key),
        };
        if let Some(existing) = snapshot.repositories.iter_mut().find(|item| {
            item.connection_id == connection_id
                && item.descriptor.repo_id == cached.descriptor.repo_id
        }) {
            *existing = cached;
        } else {
            snapshot.repositories.push(cached);
        }
        self.save_locked(&snapshot)
    }

    pub fn cache_append_repository(
        &self,
        cached: CachedAppendRepository,
    ) -> Result<(), ConfigError> {
        let _guard = self.guard()?;
        let name = format!(
            "cloud.append.{}.{}",
            cached.connection_id, cached.binding.repo_id
        );
        self.vault
            .store_secret(&name, &Zeroizing::new(serde_json::to_vec(&cached)?))?;
        let mut snapshot = self.load_locked()?;
        snapshot.append_repositories.retain(|item| {
            item.connection_id != cached.connection_id
                || item.binding.repo_id != cached.binding.repo_id
        });
        snapshot.append_repositories.push(cached.redacted());
        self.save_locked(&snapshot)
    }

    pub fn load_append_repository(
        &self,
        connection: &str,
        repository: &str,
    ) -> Result<CachedAppendRepository, ConfigError> {
        let _guard = self.guard()?;
        let name = format!("cloud.append.{connection}.{repository}");
        Ok(serde_json::from_slice(&self.vault.load_secret(&name)?)?)
    }

    pub fn load_repository(&self, connection_id: &str) -> Result<CachedRepository, ConfigError> {
        let _guard = self.guard()?;
        self.load_locked()?
            .repositories
            .into_iter()
            .find(|item| item.connection_id == connection_id)
            .ok_or_else(|| ConfigError::NotFound("repository".into()))
    }

    pub fn get_or_create_index_key(&self) -> Result<Zeroizing<[u8; 32]>, ConfigError> {
        let _guard = self.guard()?;
        match self.vault.load_secret(INDEX_KEY_SECRET) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut key = [0_u8; 32];
                key.copy_from_slice(&bytes);
                Ok(Zeroizing::new(key))
            }
            Ok(_) => Err(ConfigError::Validation(
                "local index key is corrupted".into(),
            )),
            Err(crate::DomainError::SecretNotFound { .. }) => {
                let mut key = [0_u8; 32];
                rand::rngs::OsRng.try_fill_bytes(&mut key)?;
                let key = Zeroizing::new(key);
                self.vault.store_secret(INDEX_KEY_SECRET, key.as_slice())?;
                Ok(key)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn load_secret_snapshot(&self) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
        self.vault.load_secret(SECRET_NAME).map_err(Into::into)
    }
}

fn validate_mapping_protocol(mapping: &CloudMapping) -> Result<(), ConfigError> {
    match (mapping.protocol, mapping.append_binding.as_ref()) {
        (StorageProtocol::CasV1, None) => Ok(()),
        (StorageProtocol::AppendV2, Some(binding)) => {
            if binding.repo_id != mapping.repository_id
                || binding.repo_id.trim().is_empty()
                || binding.repo_id.contains('/')
                || binding.anchor_fingerprint.len() != 64
                || !binding
                    .anchor_fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || binding.anchor_key
                    != format!(
                        "{APPEND_ANCHORS}{}-{}",
                        binding.repo_id, binding.anchor_fingerprint
                    )
                || binding
                    .root_candidates
                    .iter()
                    .any(|root| root.trim().is_empty())
                || binding.selected_root.as_ref().is_some_and(|root| {
                    root.trim().is_empty() || !binding.root_candidates.contains(root)
                })
            {
                return Err(ConfigError::Validation(
                    "invalid append repository/root binding".into(),
                ));
            }
            Ok(())
        }
        _ => Err(ConfigError::Validation(
            "storage protocol and binding disagree".into(),
        )),
    }
}

fn logical_overlap(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// 镜像云端前缀的重叠判定：空串表示 `_peerdav_mirror/` 整根，覆盖所有前缀；
/// 其余情况按路径分段判断祖先关系，避免 `docs` 与 `docs2` 被误判。
fn mirror_prefix_overlap(a: &str, b: &str) -> bool {
    a.is_empty() || b.is_empty() || logical_overlap(a, b)
}
fn random_id() -> Result<String, ConfigError> {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes)?;
    Ok(hex::encode(bytes))
}
fn validate_config(config: &S3Config) -> Result<(), ConfigError> {
    if config.endpoint.trim().is_empty()
        || config.bucket.trim().is_empty()
        || config.region.trim().is_empty()
    {
        return Err(ConfigError::Validation(
            "endpoint, bucket and region are required".into(),
        ));
    }
    if config.access_key.is_empty() != config.secret_key.is_empty() {
        return Err(ConfigError::Validation(
            "access key and secret key must be supplied together".into(),
        ));
    }
    if let ProxyMode::Custom {
        url,
        username,
        password,
    } = &config.proxy
    {
        if url.trim().is_empty() || username.is_some() != password.is_some() {
            return Err(ConfigError::Validation(
                "proxy authentication must be complete".into(),
            ));
        }
        if url.contains('@') || url.contains('?') || url.contains('#') {
            return Err(ConfigError::Validation(
                "proxy URL must not contain credentials, query, or fragment".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::SaveMirrorMappingInput;

    #[test]
    fn client_proxy_defaults_encrypts_credentials_and_preserves_password_on_edit() {
        let (_dir, store) = store();
        assert!(matches!(store.snapshot().expect("snapshot").client_proxy, ProxyMode::Environment));
        store.save_client_proxy(ProxyMode::Custom { url: "http://127.0.0.1:7890".into(), username: Some("user".into()), password: Some("private".into()) }).expect("save");
        store.save_client_proxy(ProxyMode::Custom { url: "http://127.0.0.1:7890".into(), username: Some("user".into()), password: None }).expect("retain");
        let snapshot = store.snapshot().expect("snapshot");
        assert!(matches!(&snapshot.client_proxy, ProxyMode::Custom { password: Some(value), .. } if value == "private"));
        assert!(!format!("{snapshot:?}").contains("private"));
        assert!(store.save_client_proxy(ProxyMode::Client).is_err());
        for url in ["socks5://127.0.0.1:7890", "http://user:secret@127.0.0.1", "http://127.0.0.1/path", "http://127.0.0.1?secret=1"] {
            assert!(store.save_client_proxy(ProxyMode::Custom { url: url.into(), username: None, password: None }).is_err());
        }
        let connection = store.save_connection(connection_input()).expect("connection");
        store.save_drive_proxy(&connection.id, false).expect("inherit");
        assert!(matches!(store.snapshot().expect("snapshot").connections[0].config.proxy, ProxyMode::Client));
        store.save_drive_proxy(&connection.id, true).expect("direct");
        assert!(matches!(store.snapshot().expect("snapshot").connections[0].config.proxy, ProxyMode::Direct));
        assert!(store.save_drive_proxy("forged", false).is_err());
    }

    fn store() -> (tempfile::TempDir, CloudConfigStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = Arc::new(Vault::open(&dir.path().join("vault.pd")).expect("open"));
        vault
            .initialize(dir.path().to_str().expect("path"), "admin", "password 123")
            .expect("init");
        (dir, CloudConfigStore::new(vault))
    }

    fn connection_input() -> SaveS3Input {
        SaveS3Input {
            id: None,
            name: "main".into(),
            endpoint: "https://s3.example".into(),
            bucket: "bucket".into(),
            buckets: Vec::new(),
            region: "us-east-1".into(),
            prefix: String::new(),
            path_style: false,
            access_key: Some("access".into()),
            secret_key: Some("secret".into()),
            session_token: None,
            clear_credentials: false,
            clear_session_token: false,
            clear_proxy_auth: false,
            allow_insecure_http: false,
            proxy: ProxyMode::Direct,
            schedule: None,
        }
    }

    fn mirror_input(
        connection_id: &str,
        local_root: &std::path::Path,
        cloud_prefix: &str,
    ) -> SaveMirrorMappingInput {
        SaveMirrorMappingInput {
            id: None,
            connection_id: connection_id.to_owned(),
            bucket: None,
            local_root: local_root.to_path_buf(),
            cloud_prefix: cloud_prefix.to_owned(),
            schedule: ScheduleConfig {
                enabled: false,
                interval: 900,
            },
        }
    }

    #[test]
    fn preparing_mapping_does_not_publish_binding_or_schedule() {
        let (dir, store) = store();
        let connection = store.save_connection(connection_input()).unwrap();
        let root = dir.path().join("pending");
        std::fs::create_dir(&root).unwrap();
        let input = AddMappingInput {
            connection_id: connection.id, repository_id: "repository".into(),
            local_root: root, logical_path: "docs".into(),
            schedule: ScheduleConfig { enabled: true, interval: 900 },
        };
        let prepared = store.prepare_mapping(input.clone(), &[], None).unwrap();
        let first_id = prepared.id.clone();
        assert_eq!(store.prepare_mapping(input.clone(), &[], None).unwrap().id, first_id, "repreparing the same binding must reuse its durable budget scope");
        assert!(prepared.schedule.enabled);
        assert!(store.snapshot().unwrap().mappings.is_empty());
        store.add_mapping(input.clone(), &[]).unwrap();
        assert!(store.prepare_mapping(input, &[], None).is_err());
        assert_eq!(store.snapshot().unwrap().mappings.len(), 1);
    }

    #[test]
    fn mirror_bucket_selection_persists_and_scopes_overlap_checks() {
        let (dir, store) = store();
        let mut input = connection_input();
        input.buckets = vec!["bucket".into(), "photos".into()];
        let connection = store.save_connection(input).unwrap();
        let save = |name: &str, bucket: Option<&str>| {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
            let mut value = serde_json::to_value(mirror_input(&connection.id, &dir.path().join(name), "docs")).unwrap();
            if let Some(bucket) = bucket { value["bucket"] = bucket.into(); }
            store.save_mirror_mapping(serde_json::from_value(value).unwrap())
        };
        let first = save("a", None).unwrap();
        let second = save("b", Some("photos")).unwrap();
        assert_eq!(serde_json::to_value(&second).unwrap()["bucket"], "photos");
        assert!(save("c", Some("photos")).is_err());
        assert!(save("d", Some("unknown")).is_err());
        let mut edit = mirror_input(&connection.id, &dir.path().join("b"), "docs");
        edit.id = Some(second.id.clone());
        let updated = store.save_mirror_mapping(edit).unwrap();
        assert_eq!(serde_json::to_value(updated).unwrap()["bucket"], "photos");
        let saved = store.snapshot().unwrap();
        assert_eq!(saved.mirror_mappings.len(), 2);
        assert_eq!(serde_json::to_value(&saved.mirror_mappings[0]).unwrap()["bucket"], "bucket");
        assert_eq!(saved.mirror_mappings[0].id, first.id);
    }

    #[test]
    fn mirror_mappings_reject_overlap_and_invalid_schedule() {
        let (dir, store) = store();
        let connection = store
            .save_connection(connection_input())
            .expect("connection");
        let root_a = dir.path().join("a");
        let nested = root_a.join("nested");
        let root_b = dir.path().join("b");
        std::fs::create_dir_all(&nested).expect("nested root");
        std::fs::create_dir(&root_b).expect("second root");
        store
            .save_mirror_mapping(mirror_input(&connection.id, &root_a, "docs"))
            .expect("first mapping");

        // 前缀相等、前缀嵌套、空前缀占满整根，都必须拒绝。
        for prefix in ["docs", "docs/sub", ""] {
            assert!(
                store
                    .save_mirror_mapping(mirror_input(&connection.id, &root_b, prefix))
                    .is_err(),
                "prefix {prefix:?} must overlap"
            );
        }
        // 本地根嵌套同样会互相接管文件。
        assert!(
            store
                .save_mirror_mapping(mirror_input(&connection.id, &nested, "other"))
                .is_err()
        );
        // 前缀只是同名前缀（docs2）时不算重叠。
        let saved = store
            .save_mirror_mapping(mirror_input(&connection.id, &root_b, "docs2"))
            .expect("disjoint mapping");
        assert_eq!(saved.cloud_prefix, "docs2");

        let mut invalid = mirror_input(&connection.id, &root_b, "third");
        invalid.schedule.interval = 30;
        assert!(store.save_mirror_mapping(invalid).is_err());

        store.delete_connection(&connection.id).expect("delete");
        assert!(
            store
                .snapshot()
                .expect("snapshot")
                .mirror_mappings
                .is_empty()
        );
    }
}
