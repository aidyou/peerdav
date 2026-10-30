use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use super::crypto::{CryptoError, RepositoryKeys};
use super::repository_types::{
    Head, MAX_BLOCK_SIZE, MAX_HEAD_SIZE, MAX_METADATA_SIZE, RepoDescriptor, RepositoryMode,
    WireError, WrappedMaster, decode, decode_head, encode, encode_head, hex_exact,
};
use crate::cloud::traits::ObjectStore;
use crate::cloud::types::{CloudError, ObjectStream, PutCondition, StorageCapabilities};
const HEAD_KEY: &str = "control/HEAD";
const PROBE_PREFIX: &str = "probes/";
const BLOCK_PREFIX: &str = "blocks/";
const TREE_PREFIX: &str = "trees/";
const COMMIT_PREFIX: &str = "commits/";
const BLOCK_WIRE_LIMIT: usize = 2 * (MAX_BLOCK_SIZE + 16) + 4096;
const METADATA_WIRE_LIMIT: usize = 2 * (MAX_METADATA_SIZE + 16) + 4096;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("cloud operation failed: {0}")]
    Cloud(#[from] CloudError),
    #[error("repository wire data is invalid: {0}")]
    Wire(#[from] WireError),
    #[error("repository cryptography failed: {0}")]
    Crypto(#[from] CryptoError),
    #[error("required storage capabilities are unavailable")]
    Unsupported,
    #[error("repository prefix is not empty")]
    PrefixNotEmpty,
    #[error("repository object was not found")]
    NotFound,
    #[error("repository data is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("repository mode is unsupported")]
    ModeUnsupported,
}

pub struct Repository {
    store: Arc<dyn ObjectStore>,
    descriptor: RepoDescriptor,
    keys: Option<RepositoryKeys>,
    wrapped_master: Option<WrappedMaster>,
}

impl Repository {
    pub async fn create(
        store: Arc<dyn ObjectStore>,
        descriptor: RepoDescriptor,
        password: &[u8],
    ) -> Result<Self, RepositoryError> {
        require_caps(store.capabilities())?;
        validate_descriptor(&descriptor)?;
        let mut existing = store.list("");
        while let Some(item) = existing.next().await {
            let object = item?;
            let owned_probe = object.key.strip_prefix(PROBE_PREFIX).is_some_and(|rest| {
                let mut p = rest.split('/');
                p.next().is_some_and(|hex| {
                    hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())
                }) && p.next().is_some_and(|x| x == "cas" || x == "cas-create")
                    && p.next().is_none()
            });
            if !owned_probe {
                return Err(RepositoryError::PrefixNotEmpty);
            }
        }
        drop(existing);
        let (keys, wrapped) = match descriptor.mode {
            RepositoryMode::Encrypted => {
                let (k, w) = RepositoryKeys::create(&descriptor, password)?;
                (Some(k), Some(w))
            }
            RepositoryMode::Plain => (None, None),
        };
        let mut head = Head {
            descriptor: descriptor.clone(),
            generation: 0,
            commit: None,
            wrapped_master: wrapped.clone(),
            auth: String::new(),
        };
        authenticate_head(&mut head, keys.as_ref())?;
        let encoded = encode_head(&head)?;
        match store
            .put(HEAD_KEY, stream(encoded), PutCondition::CreateOnly)
            .await
        {
            Ok(_) => Ok(Self {
                store,
                descriptor,
                keys,
                wrapped_master: wrapped,
            }),
            Err(CloudError::Precondition) => Err(RepositoryError::PrefixNotEmpty),
            Err(e) => Err(e.into()),
        }
    }
    pub async fn join(
        store: Arc<dyn ObjectStore>,
        password: &[u8],
    ) -> Result<Self, RepositoryError> {
        require_caps(store.capabilities())?;
        let bytes = read_object(&*store, HEAD_KEY, MAX_HEAD_SIZE)
            .await
            .map_err(|e| match e {
                RepositoryError::Cloud(CloudError::NotFound) => RepositoryError::NotFound,
                x => x,
            })?;
        let head = decode_head(&bytes)?;
        validate_descriptor(&head.descriptor)?;
        let keys = match (&head.descriptor.mode, &head.wrapped_master) {
            (RepositoryMode::Encrypted, Some(w)) => {
                Some(RepositoryKeys::unlock(&head.descriptor, w, password)?)
            }
            (RepositoryMode::Encrypted, None) => {
                return Err(RepositoryError::Corrupt("missing wrapped master"));
            }
            (RepositoryMode::Plain, None) => None,
            (RepositoryMode::Plain, Some(_)) => {
                return Err(RepositoryError::Corrupt(
                    "plain repository has wrapped master",
                ));
            }
        };
        verify_head(&head, keys.as_ref())?;
        Ok(Self {
            store,
            descriptor: head.descriptor,
            keys,
            wrapped_master: head.wrapped_master,
        })
    }
    /// Joins an encrypted repository using a master restored from the local
    /// vault cache. The remote HEAD is always authenticated before accepting
    /// the cached key.
    pub async fn join_cached(
        store: Arc<dyn ObjectStore>,
        master: Zeroizing<[u8; 32]>,
    ) -> Result<Self, RepositoryError> {
        require_caps(store.capabilities())?;
        let bytes = read_object(&*store, HEAD_KEY, MAX_HEAD_SIZE).await?;
        let head = decode_head(&bytes)?;
        validate_descriptor(&head.descriptor)?;
        if head.descriptor.mode != RepositoryMode::Encrypted {
            return Err(RepositoryError::ModeUnsupported);
        }
        let keys = RepositoryKeys::from_local_cache(&head.descriptor, master)?;
        verify_head(&head, Some(&keys))?;
        Ok(Self {
            store,
            descriptor: head.descriptor,
            keys: Some(keys),
            wrapped_master: head.wrapped_master,
        })
    }

    pub fn descriptor(&self) -> &RepoDescriptor {
        &self.descriptor
    }
    pub fn wrapped_master(&self) -> Option<&WrappedMaster> {
        self.wrapped_master.as_ref()
    }
    pub fn local_master(&self) -> Option<Zeroizing<[u8; 32]>> {
        self.keys
            .as_ref()
            .map(RepositoryKeys::export_master_for_local_cache)
    }

    /// Verify locally available plaintext against its content-addressed block id.
    /// No remote request or write is performed.
    pub fn verify_local_block(&self, id: &str, content: &[u8]) -> Result<bool, RepositoryError> {
        if content.len() > MAX_BLOCK_SIZE {
            return Err(RepositoryError::Corrupt("block too large"));
        }
        let expected =
            hex_exact::<32>(id).map_err(|_| RepositoryError::Corrupt("invalid block id"))?;
        Ok(self.block_id(content)? == hex::encode(expected))
    }
    pub async fn put_block(&self, content: &[u8]) -> Result<String, RepositoryError> {
        if content.len() > MAX_BLOCK_SIZE {
            return Err(RepositoryError::Corrupt("block too large"));
        }
        let id = self.block_id(content)?;
        let key = format!("{BLOCK_PREFIX}{id}");
        match self.store.stat(&key).await {
            Ok(_) => {
                let existing = self.get_block(&id, content.len()).await?;
                if existing.as_slice() != content {
                    return Err(RepositoryError::Corrupt("immutable collision"));
                }
                return Ok(id);
            }
            Err(CloudError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
        let body = self.encode_block(content)?;
        match self
            .store
            .put(&key, stream(body), PutCondition::CreateOnly)
            .await
        {
            Ok(_) => Ok(id),
            Err(CloudError::Precondition) => {
                let idbytes = hex_exact::<32>(&id)
                    .map_err(|_| RepositoryError::Corrupt("invalid block id"))?;
                let got = read_object(&*self.store, &key, BLOCK_WIRE_LIMIT).await?;
                let plain = self.decode_block(&got, &idbytes)?;
                if self.block_id(&plain)? == id && plain.as_slice() == content {
                    Ok(id)
                } else {
                    Err(RepositoryError::Corrupt("immutable collision"))
                }
            }
            Err(e) => Err(e.into()),
        }
    }
    pub async fn get_block(
        &self,
        id: &str,
        expected_len: usize,
    ) -> Result<Zeroizing<Vec<u8>>, RepositoryError> {
        let idbytes =
            hex_exact::<32>(id).map_err(|_| RepositoryError::Corrupt("invalid block id"))?;
        let body = read_object(
            &*self.store,
            &format!("{BLOCK_PREFIX}{id}"),
            BLOCK_WIRE_LIMIT,
        )
        .await?;
        let plain = self.decode_block(&body, &idbytes)?;
        if plain.len() != expected_len {
            return Err(RepositoryError::Corrupt("block length mismatch"));
        }
        if self.block_id(&plain)? != id {
            return Err(RepositoryError::Corrupt("block digest mismatch"));
        }
        Ok(plain)
    }
    pub async fn put_metadata(
        &self,
        kind: &str,
        content: &[u8],
    ) -> Result<String, RepositoryError> {
        if kind != "tree" && kind != "commit" {
            return Err(RepositoryError::Corrupt("metadata kind"));
        }
        if content.len() > MAX_METADATA_SIZE {
            return Err(RepositoryError::Corrupt("metadata too large"));
        }
        let body = self.encode_metadata(kind, content)?;
        let id = hex::encode(Sha256::digest(&body));
        let prefix = if kind == "tree" {
            TREE_PREFIX
        } else {
            COMMIT_PREFIX
        };
        let key = format!("{prefix}{id}");
        match self
            .store
            .put(&key, stream(body), PutCondition::CreateOnly)
            .await
        {
            Ok(_) => Ok(id),
            Err(CloudError::Precondition) => {
                let got = read_object(&*self.store, &key, METADATA_WIRE_LIMIT).await?;
                if hex::encode(Sha256::digest(&got)) != id {
                    return Err(RepositoryError::Corrupt("immutable collision"));
                }
                let plain = self.decode_metadata(kind, &got)?;
                if plain.as_slice() == content {
                    Ok(id)
                } else {
                    Err(RepositoryError::Corrupt("immutable collision"))
                }
            }
            Err(e) => Err(e.into()),
        }
    }
    pub async fn get_metadata(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Zeroizing<Vec<u8>>, RepositoryError> {
        if kind != "tree" && kind != "commit" {
            return Err(RepositoryError::Corrupt("metadata kind"));
        }
        let expected =
            hex_exact::<32>(id).map_err(|_| RepositoryError::Corrupt("invalid metadata id"))?;
        let prefix = if kind == "tree" {
            TREE_PREFIX
        } else {
            COMMIT_PREFIX
        };
        let body = read_object(&*self.store, &format!("{prefix}{id}"), METADATA_WIRE_LIMIT).await?;
        if Sha256::digest(&body).as_slice() != expected {
            return Err(RepositoryError::Corrupt("metadata digest mismatch"));
        }
        self.decode_metadata(kind, &body)
    }
    pub async fn head(&self) -> Result<Head, RepositoryError> {
        let bytes = read_object(&*self.store, HEAD_KEY, MAX_HEAD_SIZE).await?;
        let head = decode_head(&bytes)?;
        if head.descriptor != self.descriptor {
            return Err(RepositoryError::Corrupt("descriptor mismatch"));
        }
        verify_head(&head, self.keys.as_ref())?;
        Ok(head)
    }
    pub async fn read_head_revision(&self) -> Result<(Head, String), RepositoryError> {
        // Bind the body to the exact opaque revision even if another writer
        // updates HEAD between the metadata and streaming GET requests.
        for _ in 0..4 {
            let before = self.store.stat(HEAD_KEY).await?;
            let revision = before.etag.ok_or(RepositoryError::Unsupported)?;
            let head = self.head().await?;
            let after = self.store.stat(HEAD_KEY).await?;
            if after.etag.as_deref() == Some(revision.as_str()) {
                return Ok((head, revision));
            }
        }
        Err(RepositoryError::Cloud(CloudError::Precondition))
    }

    pub async fn cas_head(
        &self,
        expected_etag: &str,
        generation: u64,
        commit: &str,
    ) -> Result<Head, RepositoryError> {
        hex_exact::<32>(commit).map_err(|_| RepositoryError::Corrupt("invalid commit id"))?;
        let (mut head, current) = self.read_head_revision().await?;
        if current != expected_etag || head.generation.checked_add(1) != Some(generation) {
            return Err(RepositoryError::Cloud(CloudError::Precondition));
        }
        // The publisher authenticates and checks every dependency before calling
        // this final commit point. At least the referenced object must exist and
        // pass its ciphertext hash and type authentication here as well.
        self.get_metadata("commit", commit).await?;
        head.generation = generation;
        head.commit = Some(commit.to_owned());
        authenticate_head(&mut head, self.keys.as_ref())?;
        self.store
            .put(
                HEAD_KEY,
                stream(encode_head(&head)?),
                PutCondition::MatchEtag(expected_etag.to_owned()),
            )
            .await?;
        Ok(head)
    }

    fn block_id(&self, c: &[u8]) -> Result<String, RepositoryError> {
        match self.keys.as_ref() {
            Some(k) => Ok(hex::encode(k.block_id(c)?)),
            None => Ok(hex::encode(Sha256::digest(&domain("block", c)))),
        }
    }
    fn encode_block(&self, p: &[u8]) -> Result<Vec<u8>, RepositoryError> {
        match self.keys.as_ref() {
            Some(k) => Ok(encode(&k.seal_block(p)?, BLOCK_WIRE_LIMIT)?),
            None => Ok(p.to_vec()),
        }
    }
    fn decode_block(&self, b: &[u8], id: &[u8; 32]) -> Result<Zeroizing<Vec<u8>>, RepositoryError> {
        match self.keys.as_ref() {
            Some(k) => Ok(k.open_block(&decode(b, BLOCK_WIRE_LIMIT)?)?),
            None => {
                if b.len() > MAX_BLOCK_SIZE {
                    return Err(RepositoryError::Corrupt("block too large"));
                }
                if Sha256::digest(&domain("block", b)).as_slice() != id {
                    return Err(RepositoryError::Corrupt("block digest mismatch"));
                }
                Ok(Zeroizing::new(b.to_vec()))
            }
        }
    }
    fn encode_metadata(&self, kind: &str, p: &[u8]) -> Result<Vec<u8>, RepositoryError> {
        match self.keys.as_ref() {
            Some(k) => Ok(encode(&k.seal_metadata(kind, p)?, METADATA_WIRE_LIMIT)?),
            None => Ok(p.to_vec()),
        }
    }
    fn decode_metadata(&self, kind: &str, b: &[u8]) -> Result<Zeroizing<Vec<u8>>, RepositoryError> {
        match self.keys.as_ref() {
            Some(k) => Ok(k.open_metadata(&decode(b, METADATA_WIRE_LIMIT)?, kind)?),
            None => {
                if b.len() > MAX_METADATA_SIZE {
                    Err(RepositoryError::Corrupt("metadata too large"))
                } else {
                    Ok(Zeroizing::new(b.to_vec()))
                }
            }
        }
    }
}

fn require_caps(c: StorageCapabilities) -> Result<(), RepositoryError> {
    if c.create_only && c.conditional_replace && c.range_read && c.conditional_writes_verified {
        Ok(())
    } else {
        Err(RepositoryError::Unsupported)
    }
}
fn validate_descriptor(d: &RepoDescriptor) -> Result<(), RepositoryError> {
    if d.version != 1 || hex_exact::<16>(&d.repo_id).is_err() {
        return Err(RepositoryError::Corrupt("descriptor"));
    }
    let p = &d.chunker;
    if p.name != "fastcdc"
        || p.version != "5.0.0"
        || p.normalization != "v2020"
        || p.level != 1
        || p.min != 1_048_576
        || p.average != 4_194_304
        || p.max != 8_388_608
        || p.seed != 0
    {
        return Err(RepositoryError::ModeUnsupported);
    }
    Ok(())
}
fn stream(data: Vec<u8>) -> ObjectStream {
    Box::pin(futures_util::stream::once(
        async move { Ok(Bytes::from(data)) },
    ))
}
async fn read_object(
    store: &dyn ObjectStore,
    key: &str,
    limit: usize,
) -> Result<Vec<u8>, RepositoryError> {
    let mut s = store.get(key, None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = s.next().await {
        let b = chunk?;
        if out.len().saturating_add(b.len()) > limit {
            return Err(RepositoryError::Corrupt("object exceeds limit"));
        }
        out.extend_from_slice(&b);
    }
    Ok(out)
}
fn domain(kind: &str, data: &[u8]) -> Vec<u8> {
    let mut out = b"peerdav-s3/plain/v1\0".to_vec();
    out.extend_from_slice(kind.as_bytes());
    out.push(0);
    out.extend_from_slice(&(data.len() as u64).to_be_bytes());
    out.extend_from_slice(data);
    out
}
fn authenticate_head(h: &mut Head, keys: Option<&RepositoryKeys>) -> Result<(), RepositoryError> {
    h.auth.clear();
    let b = encode_head(h)?;
    h.auth = match keys {
        Some(k) => hex::encode(k.sign_head(&b)?),
        None => hex::encode(Sha256::digest(&domain("head", &b))),
    };
    Ok(())
}
fn verify_head(h: &Head, keys: Option<&RepositoryKeys>) -> Result<(), RepositoryError> {
    let auth = h.auth.clone();
    let mut x = h.clone();
    x.auth.clear();
    let b = encode_head(&x)?;
    match keys {
        Some(k) => {
            let tag = hex_exact::<32>(&auth).map_err(|_| RepositoryError::Corrupt("head auth"))?;
            k.verify_head(&b, &tag)?
        }
        None => {
            if auth != hex::encode(Sha256::digest(&domain("head", &b))) {
                return Err(RepositoryError::Corrupt("head auth"));
            }
        }
    }
    Ok(())
}
