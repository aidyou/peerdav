use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::atomic::{AtomicBool, Ordering}};

use bytes::Bytes;
use futures_util::{StreamExt, stream};

use crate::cloud::{traits::ObjectStore, types::{CloudError, PutCondition}};
use super::{ledger_types::{MAX_LEDGER_BYTES, MAX_WRITERS, RemoteCache, WriterState}, types::{CloudFile, MirrorError}};

fn cache_path(data: &Path, id: &str) -> Result<PathBuf, MirrorError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) { return Err(MirrorError::InvalidId(id.into())); }
    Ok(data.join("mirror-ledgers").join(format!("{id}.json")))
}

pub(crate) fn load(data: &Path, id: &str, scope: &str) -> Result<RemoteCache, MirrorError> {
    let path = cache_path(data, id)?;
    let bytes = match std::fs::File::open(&path) {
        Ok(file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(MAX_LEDGER_BYTES as u64 + 1).read_to_end(&mut bytes)?;
            bytes
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(RemoteCache::default()),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > MAX_LEDGER_BYTES { return Ok(RemoteCache::default()); }
    let cache = serde_json::from_slice::<RemoteCache>(&bytes).ok();
    Ok(cache.filter(|cache| cache.scope == scope).unwrap_or_default())
}

pub(crate) fn save(data: &Path, id: &str, cache: &RemoteCache) -> Result<(), MirrorError> {
    use std::io::Write;
    let path = cache_path(data, id)?;
    let bytes = serde_json::to_vec(cache).map_err(|error| MirrorError::Baseline(error.to_string()))?;
    if bytes.len() > MAX_LEDGER_BYTES { return Err(MirrorError::TooManyEntries); }
    let parent = path.parent().ok_or_else(|| MirrorError::InvalidId(id.into()))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| MirrorError::Local(error.error))?;
    Ok(())
}

/// Marker bodies are bounded and parsed: an ETag alone cannot detect corruption.
pub(crate) async fn markers(store: &dyn ObjectStore, prefix: &str, cancel: &AtomicBool) -> Result<(BTreeMap<String, String>, bool), MirrorError> {
    let mut listing = store.list(prefix);
    let mut markers = BTreeMap::new();
    let mut count = 0usize;
    let mut healthy = true;
    while let Some(item) = listing.next().await {
        if cancel.load(Ordering::Acquire) { return Err(MirrorError::Canceled); }
        let meta = item?;
        if meta.key.ends_with('/') { continue; }
        count += 1;
        if count > MAX_WRITERS { return Err(MirrorError::TooManyEntries); }
        let mut body = match store.get(&meta.key, None).await {
            Ok(body) => body,
            Err(CloudError::NotFound) => { healthy = false; continue; }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > 4096 { healthy = false; break; }
            bytes.extend_from_slice(&chunk);
        }
        let state = serde_json::from_slice::<WriterState>(&bytes).ok();
        match state {
            Some(state) if state.version == 1 && !state.token.is_empty() && !state.pending => { markers.insert(meta.key, state.token); }
            _ => { healthy = false; markers.insert(meta.key, String::new()); }
        }
    }
    Ok((markers, healthy))
}

pub(crate) async fn mark(store: &dyn ObjectStore, key: &str, token: &str, pending: bool) -> Result<(), MirrorError> {
    let bytes = serde_json::to_vec(&WriterState { version: 1, token: token.into(), pending }).map_err(|error| MirrorError::Baseline(error.to_string()))?;
    store.put(key, stream::once(async move { Ok(Bytes::from(bytes)) }).boxed(), PutCondition::Any).await?;
    Ok(())
}

/// A full remote snapshot is published per writer, not to one shared mutable head.
/// Concurrent writers cannot overwrite each other's ledger. Pending markers force
/// readers to list data instead of treating an interrupted snapshot as authoritative.
pub(crate) async fn snapshot_valid(store: &dyn ObjectStore, key: &str) -> Result<bool, MirrorError> {
    use sha2::Digest;
    let mut body = match store.get(key, None).await {
        Ok(body) => body,
        Err(CloudError::NotFound) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > MAX_LEDGER_BYTES { return Ok(false); }
        bytes.extend_from_slice(&chunk);
    }
    let Ok((digest, files)) = serde_json::from_slice::<(String, BTreeMap<String, CloudFile>)>(&bytes) else { return Ok(false); };
    let encoded = serde_json::to_vec(&files).map_err(|error| MirrorError::Baseline(error.to_string()))?;
    Ok(digest == hex::encode(sha2::Sha256::digest(encoded)))
}

pub(crate) async fn snapshot(store: &dyn ObjectStore, key: &str, files: &BTreeMap<String, CloudFile>) -> Result<(), MirrorError> {
    use sha2::Digest;
    let encoded = serde_json::to_vec(files).map_err(|error| MirrorError::Baseline(error.to_string()))?;
    let digest = hex::encode(sha2::Sha256::digest(&encoded));
    let bytes = serde_json::to_vec(&(digest, files)).map_err(|error| MirrorError::Baseline(error.to_string()))?;
    if bytes.len() > MAX_LEDGER_BYTES { return Err(CloudError::PayloadTooLarge.into()); }
    store.put(key, stream::once(async move { Ok(Bytes::from(bytes)) }).boxed(), PutCondition::Any).await?;
    Ok(())
}
