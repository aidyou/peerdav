use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid repository descriptor: {0}")]
    Descriptor(&'static str),
    #[error("invalid cryptographic input: {0}")]
    Input(&'static str),
    #[error("key derivation failed")]
    Kdf(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("repository authentication failed")]
    Auth,
    #[error("invalid repository encoding")]
    Wire(#[from] WireError),
    #[error("secure random generation failed")]
    Random(#[source] rand::Error),
}

/// Never serializable or Debug: a repository master is independent of device keys.
pub struct RepositoryKeys {
    pub(super) master: Zeroizing<[u8; 32]>,
    pub(super) repo_id: [u8; 16],
}

pub const MAX_HEAD_SIZE: usize = 64 * 1024;
pub const MAX_METADATA_SIZE: usize = 32 * 1024 * 1024;
pub const MAX_BLOCK_SIZE: usize = 8 * 1024 * 1024;
pub const OBJECT_WIRE_MAGIC: &[u8; 8] = b"PDAVOBJ\0";
pub const OBJECT_HEADER_LIMIT: usize = 1024;
pub const OBJECT_WIRE_OVERHEAD: usize = 12 + OBJECT_HEADER_LIMIT + 16;

#[derive(Debug, Error)]
pub enum WireError {
    #[error("wire serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("wire frame exceeds limit")]
    TooLarge,
    #[error("wire frame is not canonical")]
    NonCanonical,
    #[error("invalid hexadecimal field")]
    Hex(#[from] hex::FromHexError),
    #[error("invalid field: {0}")]
    Invalid(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkerProfile {
    pub name: String,
    pub version: String,
    pub normalization: String,
    pub level: u8,
    pub min: u64,
    pub average: u64,
    pub max: u64,
    pub seed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepositoryMode {
    Encrypted,
    Plain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDescriptor {
    pub version: u32,
    pub repo_id: String,
    pub mode: RepositoryMode,
    pub chunker: ChunkerProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KdfParams {
    pub algorithm: String,
    pub version: u32,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub output_len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrappedMaster {
    pub kdf: KdfParams,
    pub salt: String,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub descriptor: RepoDescriptor,
    pub generation: u64,
    pub commit: Option<String>,
    pub wrapped_master: Option<WrappedMaster>,
    pub auth: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub repo_id: String,
    pub kind: String,
    pub block_id: Option<String>,
    pub nonce: String,
    #[serde(skip)]
    pub ciphertext: Vec<u8>,
}

pub fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > limit {
        return Err(WireError::TooLarge);
    }
    Ok(bytes)
}
pub fn decode<T>(bytes: &[u8], limit: usize) -> Result<T, WireError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if bytes.len() > limit {
        return Err(WireError::TooLarge);
    }
    let value: T = serde_json::from_slice(bytes)?;
    if serde_json::to_vec(&value)? != bytes {
        return Err(WireError::NonCanonical);
    }
    Ok(value)
}
pub fn encode_head(head: &Head) -> Result<Vec<u8>, WireError> {
    encode(head, MAX_HEAD_SIZE)
}
pub fn decode_head(bytes: &[u8]) -> Result<Head, WireError> {
    decode(bytes, MAX_HEAD_SIZE)
}
pub fn encode_metadata<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    encode(value, MAX_METADATA_SIZE)
}
pub fn decode_metadata<T>(bytes: &[u8]) -> Result<T, WireError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    decode(bytes, MAX_METADATA_SIZE)
}

pub fn hex_exact<const N: usize>(s: &str) -> Result<[u8; N], WireError> {
    if s.len() != N * 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(WireError::Invalid("hex length or encoding"));
    }
    let v = hex::decode(s)?;
    v.try_into().map_err(|_| WireError::Invalid("hex length"))
}
