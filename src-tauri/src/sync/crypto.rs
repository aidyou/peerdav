pub use super::repository_types::{CryptoError, RepositoryKeys};
use super::repository_types::{
    Envelope, KdfParams, MAX_BLOCK_SIZE, MAX_METADATA_SIZE, RepoDescriptor, RepositoryMode,
    WireError, WrappedMaster, hex_exact,
};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;
use std::sync::{Mutex, OnceLock};
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;
static KDF_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn boxed<E: std::error::Error + Send + Sync + 'static>(e: E) -> CryptoError {
    CryptoError::Kdf(Box::new(e))
}
fn descriptor_id(d: &RepoDescriptor) -> Result<[u8; 16], CryptoError> {
    if d.version != 1 || d.mode != RepositoryMode::Encrypted {
        return Err(CryptoError::Descriptor("unsupported descriptor"));
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
        return Err(CryptoError::Descriptor("unsupported chunker"));
    }
    hex_exact(&d.repo_id).map_err(|_| CryptoError::Descriptor("repo_id must be 16-byte hex"))
}
fn rand_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut x = [0; N];
    OsRng.try_fill_bytes(&mut x).map_err(CryptoError::Random)?;
    Ok(x)
}
fn kdf_params() -> KdfParams {
    KdfParams {
        algorithm: "argon2id".into(),
        version: 0x13,
        memory_kib: 65536,
        iterations: 3,
        parallelism: 1,
        output_len: 32,
    }
}
fn validate_kdf(p: &KdfParams) -> Result<(), CryptoError> {
    if p.algorithm != "argon2id"
        || p.version != 0x13
        || p.memory_kib != 65536
        || p.iterations != 3
        || p.parallelism != 1
        || p.output_len != 32
    {
        Err(CryptoError::Input("unsupported KDF parameters"))
    } else {
        Ok(())
    }
}
fn derive(password: &[u8], salt: &[u8], p: &KdfParams) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    validate_kdf(p)?;
    if salt.len() != 16 {
        return Err(CryptoError::Input("salt length"));
    }
    let _guard = KDF_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| CryptoError::Input("KDF mutex poisoned"))?;
    let params = Params::new(p.memory_kib, p.iterations, p.parallelism, Some(32)).map_err(boxed)?;
    let mut out = Zeroizing::new([0; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, &mut *out)
        .map_err(boxed)?;
    Ok(out)
}
fn frame(label: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(b"peerdav-s3\0");
    o.extend_from_slice(&1u32.to_be_bytes());
    o.extend_from_slice(&(label.len() as u32).to_be_bytes());
    o.extend_from_slice(label);
    o.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    for f in fields {
        o.extend_from_slice(&(f.len() as u64).to_be_bytes());
        o.extend_from_slice(f);
    }
    o
}
fn expand(
    master: &[u8; 32],
    repo: &[u8; 16],
    purpose: &[u8],
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let hk = Hkdf::<Sha256>::new(Some(&frame(b"repo-extract", &[repo])), master);
    let mut out = Zeroizing::new([0; 32]);
    hk.expand(&frame(b"repo-key", &[repo, purpose]), &mut *out)
        .map_err(|_| CryptoError::Input("HKDF expand failed"))?;
    Ok(out)
}
fn canonical<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, CryptoError> {
    serde_json::to_vec(v).map_err(|e| CryptoError::Wire(WireError::Serialize(e)))
}

impl RepositoryKeys {
    /// Exports the repository master for encrypted local vault caching.
    /// Plain repositories intentionally have no key to export.
    pub fn export_master_for_local_cache(&self) -> Zeroizing<[u8; 32]> {
        self.master.clone()
    }

    /// Reconstructs keys from a vault-cached master after validating the
    /// descriptor shape. The remote HEAD authentication is performed by the
    /// repository join operation, never by this constructor.
    pub fn from_local_cache(
        d: &RepoDescriptor,
        master: Zeroizing<[u8; 32]>,
    ) -> Result<Self, CryptoError> {
        let repo_id = descriptor_id(d)?;
        Ok(Self { master, repo_id })
    }

    pub fn is_encrypted(&self) -> bool {
        true
    }

    pub fn create(
        d: &RepoDescriptor,
        password: &[u8],
    ) -> Result<(Self, WrappedMaster), CryptoError> {
        let repo = descriptor_id(d)?;
        let master = Zeroizing::new(rand_bytes::<32>()?);
        let p = kdf_params();
        let salt = rand_bytes::<16>()?;
        let key = derive(password, &salt, &p)?;
        let nonce = rand_bytes::<24>()?;
        let c = XChaCha20Poly1305::new_from_slice(&*key).map_err(|_| CryptoError::Auth)?;
        let aad = frame(b"master-wrap", &[&canonical(d)?, &canonical(&p)?]);
        let ct = c
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &*master,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Auth)?;
        Ok((
            Self {
                master,
                repo_id: repo,
            },
            WrappedMaster {
                kdf: p,
                salt: hex::encode(salt),
                nonce: hex::encode(nonce),
                ciphertext: hex::encode(ct),
            },
        ))
    }
    pub fn unlock(
        d: &RepoDescriptor,
        w: &WrappedMaster,
        password: &[u8],
    ) -> Result<Self, CryptoError> {
        let repo = descriptor_id(d)?;
        let salt = hex_exact::<16>(&w.salt).map_err(|_| CryptoError::Input("salt"))?;
        let nonce = hex_exact::<24>(&w.nonce).map_err(|_| CryptoError::Input("nonce"))?;
        let ct = hex_exact::<48>(&w.ciphertext)
            .map_err(|_| CryptoError::Input("ciphertext length or encoding"))?;
        let key = derive(password, &salt, &w.kdf)?;
        let c = XChaCha20Poly1305::new_from_slice(&*key).map_err(|_| CryptoError::Auth)?;
        let aad = frame(b"master-wrap", &[&canonical(d)?, &canonical(&w.kdf)?]);
        let plain = Zeroizing::new(
            c.decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &ct,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Auth)?,
        );
        let mut master = Zeroizing::new([0_u8; 32]);
        if plain.len() != master.len() {
            return Err(CryptoError::Auth);
        }
        master.copy_from_slice(&plain);
        Ok(Self {
            master,
            repo_id: repo,
        })
    }
    pub fn block_id(&self, p: &[u8]) -> Result<[u8; 32], CryptoError> {
        if p.len() > MAX_BLOCK_SIZE {
            return Err(CryptoError::Input("block too large"));
        }
        let k = expand(&self.master, &self.repo_id, b"block-id")?;
        let mut m = <HmacSha256 as Mac>::new_from_slice(&*k).map_err(|_| CryptoError::Auth)?;
        m.update(&frame(
            b"block-id",
            &[&self.repo_id, &(p.len() as u64).to_be_bytes(), p],
        ));
        Ok(m.finalize().into_bytes().into())
    }
    pub fn seal_block(&self, p: &[u8]) -> Result<Envelope, CryptoError> {
        let id = self.block_id(p)?;
        self.seal_kind("block", Some(&id), p)
    }
    pub fn open_block(&self, e: &Envelope) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        if e.kind != "block" {
            return Err(CryptoError::Input("envelope kind"));
        }
        let id: [u8; 32] = hex_exact(
            e.block_id
                .as_deref()
                .ok_or(CryptoError::Input("block id"))?,
        )
        .map_err(|_| CryptoError::Input("block id"))?;
        let p = Zeroizing::new(self.open_kind(e, &id)?);
        let expected = self.block_id(&p)?;
        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            expected.as_slice(),
            id.as_slice(),
        )) {
            return Err(CryptoError::Auth);
        }
        Ok(p)
    }
    pub fn seal_metadata(&self, kind: &str, p: &[u8]) -> Result<Envelope, CryptoError> {
        if kind != "tree" && kind != "commit" {
            return Err(CryptoError::Input("metadata kind"));
        }
        self.seal_kind(kind, None, p)
    }
    pub fn open_metadata(
        &self,
        e: &Envelope,
        expected_kind: &str,
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        if expected_kind != "tree" && expected_kind != "commit"
            || e.kind != expected_kind
            || e.block_id.is_some()
        {
            return Err(CryptoError::Input("metadata kind"));
        }
        Ok(Zeroizing::new(self.open_kind(e, &[0; 32])?))
    }
    fn seal_kind(
        &self,
        kind: &str,
        id: Option<&[u8; 32]>,
        p: &[u8],
    ) -> Result<Envelope, CryptoError> {
        let limit = if kind == "block" {
            MAX_BLOCK_SIZE
        } else {
            MAX_METADATA_SIZE
        };
        if p.len() > limit {
            return Err(CryptoError::Input("object too large"));
        }
        let purpose: &[u8] = match kind {
            "block" => b"block-aead",
            "tree" => b"tree-aead",
            "commit" => b"commit-aead",
            _ => return Err(CryptoError::Input("object kind")),
        };
        let k = expand(&self.master, &self.repo_id, purpose)?;
        let n = rand_bytes::<24>()?;
        let c = XChaCha20Poly1305::new_from_slice(&*k).map_err(|_| CryptoError::Auth)?;
        let aad = if kind == "block" {
            frame(
                b"block-aad",
                &[
                    &self.repo_id,
                    id.ok_or(CryptoError::Input("missing block id"))?,
                    &(p.len() as u64).to_be_bytes(),
                ],
            )
        } else {
            frame(b"object-aad", &[&self.repo_id, kind.as_bytes()])
        };
        let ct = c
            .encrypt(&XNonce::from(n), Payload { msg: p, aad: &aad })
            .map_err(|_| CryptoError::Auth)?;
        Ok(Envelope {
            version: 1,
            repo_id: hex::encode(self.repo_id),
            kind: kind.into(),
            block_id: id.map(hex::encode),
            nonce: hex::encode(n),
            ciphertext: ct,
        })
    }
    fn open_kind(&self, e: &Envelope, id: &[u8; 32]) -> Result<Vec<u8>, CryptoError> {
        if e.version != 1 || e.repo_id != hex::encode(self.repo_id) {
            return Err(CryptoError::Input("envelope"));
        }
        let n = hex_exact::<24>(&e.nonce).map_err(|_| CryptoError::Input("nonce"))?;
        let (limit, purpose): (usize, &[u8]) = match e.kind.as_str() {
            "block" => (MAX_BLOCK_SIZE, b"block-aead"),
            "tree" => (MAX_METADATA_SIZE, b"tree-aead"),
            "commit" => (MAX_METADATA_SIZE, b"commit-aead"),
            _ => return Err(CryptoError::Input("object kind")),
        };
        if e.ciphertext.len() < 16 || e.ciphertext.len() > limit + 16 {
            return Err(CryptoError::Input("ciphertext length"));
        }
        let ct = &e.ciphertext;
        let k = expand(&self.master, &self.repo_id, purpose)?;
        let c = XChaCha20Poly1305::new_from_slice(&*k).map_err(|_| CryptoError::Auth)?;
        let aad = if e.kind == "block" {
            frame(
                b"block-aad",
                &[&self.repo_id, id, &((ct.len() - 16) as u64).to_be_bytes()],
            )
        } else {
            frame(b"object-aad", &[&self.repo_id, e.kind.as_bytes()])
        };
        c.decrypt(
            &XNonce::from(n),
            Payload {
                msg: &ct,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::Auth)
    }
    pub fn sign_head(&self, b: &[u8]) -> Result<[u8; 32], CryptoError> {
        if b.len() > 64 * 1024 {
            return Err(CryptoError::Input("head too large"));
        }
        let k = expand(&self.master, &self.repo_id, b"head-mac")?;
        let mut m = <HmacSha256 as Mac>::new_from_slice(&*k).map_err(|_| CryptoError::Auth)?;
        m.update(&frame(b"head-mac", &[b]));
        Ok(m.finalize().into_bytes().into())
    }
    pub fn verify_head(&self, b: &[u8], tag: &[u8]) -> Result<(), CryptoError> {
        let got = self.sign_head(b)?;
        if tag.len() != 32 || !bool::from(subtle::ConstantTimeEq::ct_eq(got.as_slice(), tag)) {
            return Err(CryptoError::Auth);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_hkdf_and_hmac_match_independent_python_vectors() {
        // Independently computed using Python hashlib/hmac/struct, not these helpers.
        let repo = std::array::from_fn(|i| i as u8);
        let master = std::array::from_fn(|i| i as u8);
        assert_eq!(
            hex::encode(frame(b"test", &[b"a", b"bc"])),
            "706565726461762d7333000000000100000004746573740000000200000000000000016100000000000000026263"
        );
        assert_eq!(
            hex::encode(&*expand(&master, &repo, b"block-id").expect("derive")),
            "bc9340205b52a46032c1eb45371e4cb2ce7922a7d382bcad3340d49d1e701de1"
        );
        let keys = RepositoryKeys {
            master: Zeroizing::new(master),
            repo_id: repo,
        };
        assert_eq!(
            hex::encode(keys.block_id(b"hello").expect("block id")),
            "4f3504276a511dd696dbaf00db9d764f718ec0a7e9f924d8fa47fe571d802b8d"
        );
        let block_key = expand(&keys.master, &repo, b"block-aead").expect("key");
        let tree_key = expand(&keys.master, &repo, b"tree-aead").expect("key");
        assert_ne!(*block_key, *tree_key);
    }
}
