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
        snapshot.mappings.retain(|m| m.connection_id != id);
        snapshot.repositories.retain(|r| r.connection_id != id);
        snapshot
            .append_repositories
            .retain(|r| r.connection_id != id);
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
        let _guard = self.guard()?;
        let mut snapshot = self.load_locked()?;
        validate_schedule(&input.schedule)?;
        if !snapshot
            .connections
            .iter()
            .any(|c| c.id == input.connection_id)
        {
            return Err(ConfigError::Validation("connection does not exist".into()));
        }
        if id.is_some() && !snapshot.mappings.iter().any(|m| Some(m.id.as_str()) == id) {
            return Err(ConfigError::NotFound("mapping".into()));
        }
        let binding = crate::sync::root_state::bind_initial(input.local_root.clone())?;
        let root = binding.root_path;
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
        self.save_locked(&snapshot)?;
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
        if !snapshot.connections.iter().any(|c| c.id == input.connection_id) {
            return Err(ConfigError::NotFound("connection".into()));
        }
        let cloud_prefix = crate::mirror::normalize_cloud_prefix(&input.cloud_prefix)
            .map_err(|e| ConfigError::Validation(e.to_string()))?;
        let local_root = input.local_root.clone();
        if !local_root.is_absolute() {
            return Err(ConfigError::Validation(
                "本地目录必须是绝对路径 / Local root must be absolute".into(),
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
            local_root,
            cloud_prefix,
            schedule: input.schedule,
        };
        // 同一连接下不允许两条镜像映射指向同一个本地目录或同一个云端目录：
        // 它们会互相删除对方的文件。
        for existing in &snapshot.mirror_mappings {
            if existing.id == id || existing.connection_id != mapping.connection_id {
                continue;
            }
            if existing.local_root == mapping.local_root
                || (existing.cloud_prefix.is_empty() && mapping.cloud_prefix.is_empty())
                || existing.cloud_prefix == mapping.cloud_prefix
            {
                return Err(ConfigError::Validation(
                    "本地目录或云端目录与已有镜像映射冲突 / Local or cloud directory conflicts with an existing mirror mapping"
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
