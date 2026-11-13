use std::sync::{Arc, Mutex};
use peerdav_protocol_s3::{CloudError, ProbeCapabilities, S3Config, S3Store};
use peerdav_protocol_s3::traits::CapabilityObserver;
use sha2::Sha256;
use super::runtime::CloudRuntime;
use super::runtime_types::{CapabilityCache, CapabilityProgress};
use crate::sync::index::SyncIndex;
fn storage_error(source: impl std::error::Error + Send + Sync + 'static) -> CloudError {
    CloudError::Storage { source: object_store::Error::Generic { store: "capability cache", source: Box::new(source) } }
}
impl CapabilityObserver for CapabilityCache {
    fn load(&self) -> Result<ProbeCapabilities, CloudError> {
        let index = self.index.lock().map_err(|_| CloudError::InvalidConfig("capability cache lock poisoned".into()))?;
        Ok(index.load("capabilities", "v1").map_err(storage_error)?.unwrap_or_default())
    }
    fn save(&self, state: ProbeCapabilities) -> Result<(), CloudError> {
        self.index.lock().map_err(|_| CloudError::InvalidConfig("capability cache lock poisoned".into()))?
            .save("capabilities", "v1", &state).map_err(storage_error)
    }
    fn progress(&self, capability: &'static str, stage: &'static str) -> Result<(), CloudError> {
        // UI delivery must never interrupt cleanup of remote probe objects.
        if let Err(error) = self.events.capability_progress(CapabilityProgress {
            scope: self.scope.clone(), connection_id: self.connection_id.clone(),
            connection_name: self.connection_name.clone(), capability: capability.into(), stage: stage.into(),
        }) { log::error!("capability progress delivery failed: {error}"); }
        Ok(())
    }
}
impl CloudRuntime {
    pub(crate) fn capability_store(&self, connection_id: &str, config: S3Config) -> Result<S3Store, crate::AppError> {
        // Credentials and proxy settings participate only in a keyed digest;
        // neither plaintext credentials nor their unkeyed hashes are persisted.
        let key = self.config.get_or_create_index_key().map_err(super::runtime::config_error)?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&config).map_err(|error| crate::AppError::from(crate::DomainError::Internal { detail: error.to_string() }))?);
        use hmac::{Hmac, Mac};
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_ref()).map_err(|error| crate::AppError::from(crate::DomainError::Internal { detail: error.to_string() }))?;
        mac.update(b"s3-capability-cache-v1\0");
        mac.update(connection_id.as_bytes());
        mac.update(&bytes);
        let scope = hex::encode(mac.finalize().into_bytes());
        let mut stores = self.capability_stores.lock().map_err(|_| crate::AppError::from(crate::DomainError::Internal { detail: "capability stores lock poisoned".into() }))?;
        if let Some(store) = stores.get(&scope) { return Ok(store.clone()); }
        let name = self.connection(connection_id)?.name;
        let index = SyncIndex::open(&self.data_dir.join("cloud-capabilities.sqlite"), key, "s3-capabilities-v1".into(), scope.clone())
            .map_err(super::runtime::append::append_error)?;
        let store = S3Store::new(config).map_err(|error| crate::AppError::from(crate::DomainError::from(error)))?.with_capability_observer(Arc::new(CapabilityCache {
            index: Mutex::new(index), events: self.object_events.clone(), scope: scope.clone(),
            connection_id: connection_id.into(), connection_name: name,
        }));
        stores.insert(scope, store.clone());
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_cache_persists_and_isolates_storage_scopes() {
        let data = tempfile::tempdir().expect("data");
        let vault = Arc::new(crate::Vault::open(&data.path().join("vault.pd")).expect("vault"));
        vault.initialize(data.path().to_str().expect("path"), "admin", "password 123").expect("initialize");
        let runtime = CloudRuntime::new(vault, Arc::new(crate::sync::traits::NoopSyncEvents), data.path().to_owned());
        let input: super::super::config_types::SaveS3Input = serde_json::from_value(serde_json::json!({
            "id": null, "name": "MinIO", "endpoint": "http://127.0.0.1:9000", "bucket": "test", "region": "us-east-1", "prefix": "", "pathStyle": true,
            "accessKey": "test-access", "secretKey": "test-secret", "sessionToken": null,
            "clearCredentials": false, "clearSessionToken": false, "clearProxyAuth": false,
            "allowInsecureHttp": true, "proxy": { "mode": "direct" }, "schedule": null
        })).expect("input");
        let connection = runtime.config.save_connection(input).expect("connection");
        let config = runtime.connection(&connection.id).expect("config").config;
        let _first = runtime.capability_store(&connection.id, config.clone()).expect("first");
        runtime.capability_store(&connection.id, config.clone()).expect("same scope");
        assert_eq!(runtime.capability_stores.lock().expect("stores").len(), 1);
        let mut changed = config;
        changed.prefix = "other/".into();
        runtime.capability_store(&connection.id, changed.clone()).expect("new prefix");
        changed.secret_key = "rotated-secret".into();
        runtime.capability_store(&connection.id, changed).expect("new credentials");
        assert_eq!(runtime.capability_stores.lock().expect("stores").len(), 3);
        let scope = runtime.capability_stores.lock().expect("stores").keys().next().expect("scope").clone();
        let open = || CapabilityCache {
            index: Mutex::new(SyncIndex::open(&data.path().join("cloud-capabilities.sqlite"), runtime.config.get_or_create_index_key().expect("key"), "s3-capabilities-v1".into(), scope.clone()).expect("index")),
            events: runtime.object_events.clone(), scope: scope.clone(), connection_id: connection.id.clone(), connection_name: "MinIO".into(),
        };
        let cache = open();
        cache.save(ProbeCapabilities { cas: Some(true), conditional_delete: Some(false), move_create: None }).expect("save");
        drop(cache);
        let restored = open().load().expect("reopen");
        assert_eq!(restored.cas, Some(true));
        assert_eq!(restored.conditional_delete, Some(false));
    }
}

