//! Vault key file: generation, validation, and deterministic storage.
//!
//! The key file is the only secret needed to decrypt the vault. It contains a
//! random 32-byte key and a fingerprint, is written with owner-only permissions
//! on Unix, and is never derived from a user password.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::crypto;
use crate::error::DomainError;
use crate::types::{KEY_FILE_NAME, KEY_FILE_TYPE, KEY_FILE_VERSION, VAULT_KEY_LEN};

#[derive(Debug, Serialize, Deserialize)]
struct KeyFile {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    id: String,
    key: String,
}

/// Validated key material loaded from a key file.
pub(crate) struct KeyMaterial {
    pub key: Zeroizing<[u8; VAULT_KEY_LEN]>,
    pub path: PathBuf,
}

/// Creates `<directory>/peerdav-vault.key` with a freshly generated key.
///
/// Existing files are never overwritten: the caller must choose another
/// directory or restore the existing key file explicitly.
pub(crate) fn generate_in_directory(directory: &str) -> Result<KeyMaterial, DomainError> {
    let directory = Path::new(directory);
    let metadata = fs::metadata(directory).map_err(|source| DomainError::Io {
        operation: "inspect key directory",
        path: Some(directory.to_path_buf()),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(DomainError::InvalidKeyDirectory {
            detail: format!("{} is not a directory", directory.display()),
        });
    }

    let canonical = fs::canonicalize(directory).map_err(|source| DomainError::Io {
        operation: "resolve key directory",
        path: Some(directory.to_path_buf()),
        source,
    })?;
    let path = canonical.join(KEY_FILE_NAME);
    if path.exists() {
        return Err(DomainError::KeyFileExists {
            path: path.display().to_string(),
        });
    }

    let key = Zeroizing::new(crypto::random_bytes::<VAULT_KEY_LEN>());
    let id = key_id(&key);
    let serialized = serde_json::to_vec_pretty(&KeyFile {
        kind: KEY_FILE_TYPE.to_owned(),
        version: KEY_FILE_VERSION,
        id: id.clone(),
        key: Zeroizing::new(hex::encode(key.as_ref())).to_string(),
    })
    .map_err(|source| DomainError::Serialization {
        operation: "encode key file",
        source,
    })?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(&path).map_err(|source| DomainError::Io {
        operation: "create key file",
        path: Some(path.clone()),
        source,
    })?;

    if let Err(source) = file
        .write_all(&serialized)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(DomainError::Io {
            operation: "write key file",
            path: Some(path),
            source,
        });
    }

    Ok(KeyMaterial { key, path })
}

/// Reads and validates a key file, including its fingerprint.
pub(crate) fn read(path: &Path) -> Result<KeyMaterial, DomainError> {
    let canonical = fs::canonicalize(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => DomainError::KeyFileMissing {
            path: path.display().to_string(),
        },
        _ => DomainError::Io {
            operation: "resolve key file",
            path: Some(path.to_path_buf()),
            source,
        },
    })?;

    let contents = fs::read(&canonical).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => DomainError::KeyFileMissing {
            path: canonical.display().to_string(),
        },
        _ => DomainError::Io {
            operation: "read key file",
            path: Some(canonical.clone()),
            source,
        },
    })?;

    let key_file: KeyFile =
        serde_json::from_slice(&contents).map_err(|source| DomainError::KeyFileInvalid {
            path: canonical.display().to_string(),
            detail: source.to_string(),
        })?;
    if key_file.kind != KEY_FILE_TYPE {
        return Err(DomainError::KeyFileInvalid {
            path: canonical.display().to_string(),
            detail: format!("unsupported key file type `{}`", key_file.kind),
        });
    }
    if key_file.version != KEY_FILE_VERSION {
        return Err(DomainError::KeyFileInvalid {
            path: canonical.display().to_string(),
            detail: format!("unsupported key file version {}", key_file.version),
        });
    }

    let bytes = Zeroizing::new(hex::decode(&key_file.key).map_err(|source| {
        DomainError::KeyFileInvalid {
            path: canonical.display().to_string(),
            detail: source.to_string(),
        }
    })?);
    let key: [u8; VAULT_KEY_LEN] =
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| DomainError::KeyFileInvalid {
                path: canonical.display().to_string(),
                detail: format!("key material has {} bytes", bytes.len()),
            })?;
    let key = Zeroizing::new(key);
    let id = key_id(&key);
    if key_file.id != id {
        return Err(DomainError::KeyFileInvalid {
            path: canonical.display().to_string(),
            detail: "key fingerprint does not match the key material".to_owned(),
        });
    }

    Ok(KeyMaterial {
        key,
        path: canonical,
    })
}

/// Stable, non-secret fingerprint of a key.
fn key_id(key: &[u8; VAULT_KEY_LEN]) -> String {
    hex::encode(Sha256::digest(key))[..32].to_owned()
}
