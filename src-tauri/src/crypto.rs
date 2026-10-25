//! Cryptographic primitives: XChaCha20-Poly1305 AEAD and Argon2id password
//! hashing. No secret is ever written to disk in plaintext.

use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng as PasswordOsRng,
};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use rand::rngs::OsRng as SecureRandom;
use zeroize::Zeroizing;

use crate::error::DomainError;
use crate::types::{NONCE_LEN, VAULT_KEY_LEN};

/// Generates cryptographically secure random bytes.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    let mut rng = SecureRandom;
    rng.fill_bytes(&mut bytes);
    bytes
}

/// Encrypts a plaintext with a fresh 24-byte nonce and binds the record name
/// as AEAD associated data.
pub(crate) fn encrypt(
    key: &[u8; VAULT_KEY_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), DomainError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|error| DomainError::Crypto {
        operation: "initialize cipher",
        detail: error.to_string(),
    })?;
    let nonce_bytes = random_bytes::<NONCE_LEN>();
    let nonce = XNonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|error| DomainError::Crypto {
            operation: "encrypt secret",
            detail: error.to_string(),
        })?;

    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Decrypts a secret and verifies both its nonce length and AEAD tag.
pub(crate) fn decrypt(
    key: &[u8; VAULT_KEY_LEN],
    aad: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, DomainError> {
    let nonce_bytes: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| DomainError::Corrupted {
        detail: format!("nonce has {} bytes, expected {NONCE_LEN}", nonce.len()),
    })?;
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|error| DomainError::Crypto {
        operation: "initialize cipher",
        detail: error.to_string(),
    })?;
    let plaintext = cipher
        .decrypt(
            &XNonce::from(nonce_bytes),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|error| DomainError::Crypto {
            operation: "decrypt secret",
            detail: error.to_string(),
        })?;

    Ok(Zeroizing::new(plaintext))
}

/// Hashes an admin password with Argon2id and a random salt. Only the PHC
/// string is persisted, and that string is itself stored inside the vault.
pub(crate) fn hash_password(password: &str) -> Result<String, DomainError> {
    let mut rng = PasswordOsRng;
    let salt = SaltString::generate(&mut rng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|source| DomainError::PasswordHash { source })
}

/// Verifies a password against an Argon2 PHC string.
pub(crate) fn verify_password(hash: &str, password: &str) -> Result<bool, DomainError> {
    let parsed = PasswordHash::new(hash).map_err(|source| DomainError::PasswordHash { source })?;

    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(source) => Err(DomainError::PasswordHash { source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_round_trip_binds_aad() {
        let key = [7_u8; VAULT_KEY_LEN];
        let (nonce, ciphertext) = encrypt(&key, b"record-a", b"top secret").expect("encrypt");
        let plaintext = decrypt(&key, b"record-a", &nonce, &ciphertext).expect("decrypt");
        assert_eq!(plaintext.as_slice(), b"top secret");

        assert!(decrypt(&key, b"record-b", &nonce, &ciphertext).is_err());
        assert!(decrypt(&[8_u8; VAULT_KEY_LEN], b"record-a", &nonce, &ciphertext).is_err());
    }

    #[test]
    fn password_hash_round_trip() {
        let hash = hash_password("a sufficiently long password").expect("hash");
        assert!(verify_password(&hash, "a sufficiently long password").expect("verify"));
        assert!(!verify_password(&hash, "wrong password").expect("verify"));
    }
}
