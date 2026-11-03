use super::{append_tree::validate_binding, append_types::*};
use crate::cloud::traits::ObjectStore;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use futures_util::StreamExt;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const BLOCK_WIRE_LIMIT: usize = 2 * (MAX_BLOCK_SIZE + 16) + 4096;
const METADATA_WIRE_LIMIT: usize = 2 * (MAX_METADATA_SIZE + 16) + 4096;
const ANCHOR_WIRE_LIMIT: usize = 64 * 1024;
type HmacSha256 = Hmac<Sha256>;
fn frame(label: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut out = b"peerdav-append/v2/".to_vec();
    out.extend_from_slice(label);
    out.push(0);
    for f in fields {
        out.extend_from_slice(&(f.len() as u64).to_be_bytes());
        out.extend_from_slice(f);
    }
    out
}
fn json<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, AppendError> {
    Ok(serde_json::to_vec(v)?)
}
fn hex_bytes(s: &str, n: usize) -> Result<Vec<u8>, AppendError> {
    if s.len() != n * 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(AppendError::Input("invalid hexadecimal field"));
    }
    hex::decode(s).map_err(|_| AppendError::Input("invalid hexadecimal field"))
}
fn hmac(k: &[u8], label: &[u8], fields: &[&[u8]]) -> Result<[u8; 32], AppendError> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(k).map_err(|_| AppendError::Authentication)?;
    m.update(&frame(label, fields));
    Ok(m.finalize().into_bytes().into())
}
fn derive(master: &[u8; 32], repo: &[u8; 16], purpose: &[u8]) -> Result<[u8; 32], AppendError> {
    let hk = Hkdf::<Sha256>::new(Some(repo), master);
    let mut out = [0; 32];
    let info = [b"peerdav-append/v2/".as_slice(), purpose].concat();
    hk.expand(&info, &mut out)
        .map_err(|_| AppendError::Authentication)?;
    Ok(out)
}
fn object_aad(
    repo: &[u8; 16],
    kind: &str,
    mode: &AppendMode,
    id: &[u8],
    len: u64,
) -> Result<Vec<u8>, AppendError> {
    let version = json(&2u32)?;
    let kind_json = json(&kind)?;
    let mode_json = json(mode)?;
    let length = json(&len)?;
    Ok(frame(
        b"object",
        &[&version, repo, &kind_json, &mode_json, id, &length],
    ))
}
fn params() -> KdfParams {
    KdfParams {
        algorithm: "argon2id".into(),
        version: 0x13,
        memory_kib: 65536,
        iterations: 3,
        parallelism: 1,
        output_len: 32,
    }
}
fn validate_descriptor(d: &Descriptor) -> Result<(), AppendError> {
    if d.version != APPEND_VERSION {
        return Err(AppendError::Input("descriptor version"));
    }
    let _ = hex_bytes(&d.repo_id, REPO_ID_LEN)?;
    let c = &d.chunker;
    if c.name != "fastcdc"
        || c.version != "5.0.0"
        || c.normalization != "level1"
        || c.level != 1
        || c.min != 1_048_576
        || c.average != 4_194_304
        || c.max != 8_388_608
        || c.seed != 0
    {
        return Err(AppendError::Input("unsupported chunker"));
    }
    Ok(())
}
fn repo_id(d: &Descriptor) -> Result<[u8; REPO_ID_LEN], AppendError> {
    validate_descriptor(d)?;
    let b = hex_bytes(&d.repo_id, REPO_ID_LEN)?;
    b.try_into()
        .map_err(|_| AppendError::Input("repository id"))
}

fn validate_ref(r: &ObjectRef, kind: &str) -> Result<(), AppendError> {
    if r.kind != kind || hex_bytes(&r.digest, 32).is_err() {
        return Err(AppendError::Invalid("invalid object reference"));
    }
    Ok(())
}
fn validate_commit(commit: &Commit, repo: &str) -> Result<(), AppendError> {
    if commit.version != APPEND_VERSION
        || commit.repo_id != repo
        || hex_bytes(&commit.operation_id, 16).is_err()
    {
        return Err(AppendError::Invalid("commit descriptor"));
    }
    validate_ref(&commit.root, "tree")?;
    if commit.parents.len() > 64
        || commit
            .parents
            .windows(2)
            .any(|w| (&w[0].operation_id, &w[0].digest) >= (&w[1].operation_id, &w[1].digest))
    {
        return Err(AppendError::Invalid("commit parents"));
    }
    for parent in &commit.parents {
        if hex_bytes(&parent.operation_id, 16).is_err() || hex_bytes(&parent.digest, 32).is_err() {
            return Err(AppendError::Invalid("commit parent reference"));
        }
    }
    Ok(())
}
fn validate_tree(tree: &Tree, repo: &str) -> Result<(), AppendError> {
    if tree.version != APPEND_VERSION || tree.repo_id != repo {
        return Err(AppendError::Invalid("tree descriptor"));
    }
    let mut canonical = tree.clone();
    crate::sync::append_tree::canonicalize_tree(&mut canonical)?;
    if canonical != *tree {
        return Err(AppendError::Invalid("non-canonical tree"));
    }
    for child in &tree.children {
        validate_ref(&child.object, "tree")?;
    }
    Ok(())
}

pub struct AppendRepository {
    anchor: Anchor,
    master: Zeroizing<[u8; 32]>,
    repo: [u8; 16],
}
impl AppendRepository {
    pub fn create_anchor(
        descriptor: Descriptor,
        password: &[u8],
    ) -> Result<(Anchor, Zeroizing<[u8; 32]>), AppendError> {
        let repo = repo_id(&descriptor)?;
        let mut master = Zeroizing::new([0; 32]);
        OsRng
            .try_fill_bytes(&mut *master)
            .map_err(AppendError::Random)?;
        let mut salt = [0; 16];
        OsRng
            .try_fill_bytes(&mut salt)
            .map_err(AppendError::Random)?;
        let mut nonce = [0; 24];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(AppendError::Random)?;
        let p = params();
        let pp = Params::new(p.memory_kib, p.iterations, p.parallelism, Some(32))
            .map_err(AppendError::Kdf)?;
        let mut key = Zeroizing::new([0; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, pp)
            .hash_password_into(password, &salt, &mut *key)
            .map_err(AppendError::Kdf)?;
        let aad = frame(b"wrap", &[&json(&descriptor)?, &json(&p)?, &salt]);
        let c =
            XChaCha20Poly1305::new_from_slice(&*key).map_err(|_| AppendError::Authentication)?;
        let ct = c
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &*master,
                    aad: &aad,
                },
            )
            .map_err(|_| AppendError::Authentication)?;
        let w = WrappedMaster {
            kdf: p,
            salt: hex::encode(salt),
            nonce: hex::encode(nonce),
            ciphertext: hex::encode(ct),
        };
        let ak = derive(&master, &repo, b"anchor")?;
        let auth = hmac(&ak, b"anchor", &[&json(&descriptor)?, &json(&w)?])?;
        Ok((
            Anchor {
                descriptor,
                wrapped_master: w,
                auth: hex::encode(auth),
            },
            master,
        ))
    }
    pub fn unlock(anchor: &Anchor, password: &[u8]) -> Result<Zeroizing<[u8; 32]>, AppendError> {
        let repo = repo_id(&anchor.descriptor)?;
        let w = &anchor.wrapped_master;
        let salt = hex_bytes(&w.salt, 16)?;
        let nonce = hex_bytes(&w.nonce, 24)?;
        let ct = hex_bytes(&w.ciphertext, 48)?;
        let p = &w.kdf;
        if p.algorithm != "argon2id"
            || p.version != 0x13
            || p.memory_kib != 65536
            || p.iterations != 3
            || p.parallelism != 1
            || p.output_len != 32
        {
            return Err(AppendError::Input("unsupported KDF parameters"));
        }
        let pp = Params::new(p.memory_kib, p.iterations, p.parallelism, Some(32))
            .map_err(AppendError::Kdf)?;
        let mut key = Zeroizing::new([0; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, pp)
            .hash_password_into(password, &salt, &mut *key)
            .map_err(|_| AppendError::Authentication)?;
        let c =
            XChaCha20Poly1305::new_from_slice(&*key).map_err(|_| AppendError::Authentication)?;
        let aad = frame(b"wrap", &[&json(&anchor.descriptor)?, &json(p)?, &salt]);
        let plain = c
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: &aad,
                },
            )
            .map_err(|_| AppendError::Authentication)?;
        if plain.len() != 32 {
            return Err(AppendError::Authentication);
        }
        let mut master = Zeroizing::new([0; 32]);
        master.copy_from_slice(&plain);
        let ak = derive(&master, &repo, b"anchor")?;
        let expected = hmac(&ak, b"anchor", &[&json(&anchor.descriptor)?, &json(w)?])?;
        if bool::from(
            expected
                .as_slice()
                .ct_eq(hex_bytes(&anchor.auth, 32)?.as_slice()),
        ) == false
        {
            return Err(AppendError::Authentication);
        }
        Ok(master)
    }
    pub fn prepare_anchor(
        descriptor: Descriptor,
        password: &[u8],
    ) -> Result<(PreparedObject, TrustBinding), AppendError> {
        let (anchor, _) = Self::create_anchor(descriptor.clone(), password)?;
        let bytes = json(&anchor)?;
        if bytes.len() > ANCHOR_WIRE_LIMIT {
            return Err(AppendError::Input("anchor too large"));
        }
        let key = format!(
            "{APPEND_ANCHORS}{}-{}",
            descriptor.repo_id,
            hex::encode(Sha256::digest(&bytes))
        );
        let fingerprint = hex::encode(Sha256::digest(&bytes));
        Ok((
            PreparedObject::new(key.clone(), bytes),
            TrustBinding {
                key,
                repo_id: descriptor.repo_id,
                fingerprint,
                mode: descriptor.mode,
                source: TrustSource::Unconfirmed,
            },
        ))
    }

    pub async fn discover_anchors<S: ObjectStore + ?Sized>(
        store: &S,
    ) -> Result<Vec<Candidate>, AppendError> {
        let mut listing = store.list(APPEND_ANCHORS);
        let mut candidates = Vec::new();
        while let Some(item) = listing.next().await {
            let meta = item.map_err(AppendError::Storage)?;
            let mut stream = store
                .get(&meta.key, None)
                .await
                .map_err(AppendError::Storage)?;
            let mut bytes = Vec::new();
            while let Some(part) = stream.next().await {
                bytes.extend_from_slice(&part.map_err(AppendError::Storage)?);
                if bytes.len() > ANCHOR_WIRE_LIMIT {
                    return Err(AppendError::Input("anchor too large"));
                }
            }
            let anchor: Anchor = serde_json::from_slice(&bytes)?;
            if serde_json::to_vec(&anchor)? != bytes {
                return Err(AppendError::Invalid("non-canonical anchor"));
            }
            validate_descriptor(&anchor.descriptor)?;
            let fingerprint = hex::encode(Sha256::digest(&bytes));
            candidates.push(Candidate {
                key: meta.key,
                fingerprint,
                repo_id: anchor.descriptor.repo_id.clone(),
                descriptor: anchor.descriptor,
                trust: TrustSource::Unconfirmed,
            });
        }
        Ok(candidates)
    }

    pub async fn open_selected<S: ObjectStore + ?Sized>(
        store: Arc<S>,
        binding: TrustBinding,
        password: &[u8],
    ) -> Result<(Self, TrustBinding), AppendError> {
        if !matches!(
            binding.source,
            TrustSource::TrustedFingerprint | TrustSource::AcceptedTofu
        ) {
            return Err(AppendError::TrustRejected);
        }
        let mut stream = store.get(&binding.key, None).await.map_err(|e| {
            if matches!(e, crate::cloud::types::CloudError::NotFound) {
                AppendError::PendingDependency(e)
            } else {
                AppendError::Storage(e)
            }
        })?;
        let mut bytes = Vec::new();
        while let Some(part) = stream.next().await {
            bytes.extend_from_slice(&part.map_err(AppendError::Storage)?);
            if bytes.len() > ANCHOR_WIRE_LIMIT {
                return Err(AppendError::Input("anchor too large"));
            }
        }
        let fingerprint = hex::encode(Sha256::digest(&bytes));
        if fingerprint != binding.fingerprint {
            return Err(AppendError::FingerprintMismatch);
        }
        let anchor: Anchor = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&anchor)? != bytes
            || anchor.descriptor.repo_id != binding.repo_id
            || anchor.descriptor.mode != binding.mode
        {
            return Err(AppendError::Invalid("anchor binding"));
        }
        let repo = Self::new(store, anchor, password)?;
        Ok((repo, binding))
    }

    pub fn new<S: ?Sized>(
        _store: Arc<S>,
        anchor: Anchor,
        password: &[u8],
    ) -> Result<Self, AppendError> {
        let master = Self::unlock(&anchor, password)?;
        let repo = repo_id(&anchor.descriptor)?;
        Ok(Self {
            anchor,
            master,
            repo,
        })
    }
    pub fn descriptor(&self) -> &Descriptor {
        &self.anchor.descriptor
    }

    pub fn repo_id(&self) -> &str {
        &self.anchor.descriptor.repo_id
    }
    /// Authenticate plaintext identity without allocating or sealing a new wire object.
    pub fn block_id(&self, plain: &[u8]) -> Result<String, AppendError> {
        if plain.len() > MAX_BLOCK_SIZE {
            return Err(AppendError::Input("block too large"));
        }
        Ok(hex::encode(hmac(
            &derive(&self.master, &self.repo, b"block-id")?,
            b"block-id",
            &[&self.repo, plain],
        )?))
    }

    pub fn seal_block(&self, plain: &[u8]) -> Result<(String, Vec<u8>), AppendError> {
        if plain.len() > MAX_BLOCK_SIZE {
            return Err(AppendError::Input("block too large"));
        }
        let id = hmac(
            &derive(&self.master, &self.repo, b"block-id")?,
            b"block-id",
            &[&self.repo, plain],
        )?;
        self.seal("block", &id, plain)
    }
    fn seal(
        &self,
        kind: &str,
        id: &[u8; 32],
        plain: &[u8],
    ) -> Result<(String, Vec<u8>), AppendError> {
        let mode = self.anchor.descriptor.mode.clone();
        let aad = object_aad(&self.repo, kind, &mode, id, plain.len() as u64)?;
        let (nonce, payload, auth) = match mode {
            AppendMode::Encrypted => {
                let mut n = [0; 24];
                OsRng.try_fill_bytes(&mut n).map_err(AppendError::Random)?;
                let k = derive(&self.master, &self.repo, b"encryption")?;
                let c = XChaCha20Poly1305::new_from_slice(&k)
                    .map_err(|_| AppendError::Authentication)?;
                let p = c
                    .encrypt(
                        XNonce::from_slice(&n),
                        Payload {
                            msg: plain,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| AppendError::Authentication)?;
                (Some(hex::encode(n)), hex::encode(p), None)
            }
            AppendMode::AuthenticatedPlain => {
                let k = derive(&self.master, &self.repo, b"authentication")?;
                (
                    None,
                    hex::encode(plain),
                    Some(hex::encode(hmac(&k, b"plain", &[&aad, plain])?)),
                )
            }
        };
        let e = Envelope {
            version: 2,
            repo_id: hex::encode(self.repo),
            kind: kind.into(),
            mode,
            logical_id: hex::encode(id),
            plaintext_len: plain.len() as u64,
            nonce,
            payload,
            auth,
        };
        let bytes = json(&e)?;
        let digest = hex::encode(Sha256::digest(&bytes));
        Ok((
            format!("{APPEND_ROOT}/{}/{kind}s/{digest}", self.anchor.descriptor.repo_id),
            bytes,
        ))
    }
    pub fn open_block(&self, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, AppendError> {
        self.open_block_with_ref(bytes, None)
    }
    pub fn open_block_with_ref(
        &self,
        bytes: &[u8],
        expected: Option<&BlockBinding>,
    ) -> Result<Zeroizing<Vec<u8>>, AppendError> {
        if bytes.len() > BLOCK_WIRE_LIMIT {
            return Err(AppendError::Input("object too large"));
        }
        let e: Envelope = serde_json::from_slice(bytes)?;
        if serde_json::to_vec(&e)? != bytes {
            return Err(AppendError::Invalid("non-canonical envelope"));
        }
        let wire_digest = hex::encode(Sha256::digest(bytes));
        if let Some(binding) = expected {
            validate_binding(binding)?;
            if binding.object.digest != wire_digest
                || binding.len != e.plaintext_len
                || binding.block_id != e.logical_id
            {
                return Err(AppendError::Invalid("binding mismatch"));
            }
        }
        if e.version != 2
            || e.kind != "block"
            || e.repo_id != hex::encode(self.repo)
            || e.plaintext_len as usize > MAX_BLOCK_SIZE
        {
            return Err(AppendError::Invalid("envelope"));
        }
        if e.plaintext_len > MAX_BLOCK_SIZE as u64 {
            return Err(AppendError::Invalid("envelope"));
        }
        let id = hex_bytes(&e.logical_id, BLOCK_ID_LEN)?;
        let mode = e.mode.clone();
        if mode != self.anchor.descriptor.mode {
            return Err(AppendError::Invalid("mode mismatch"));
        }
        match mode {
            AppendMode::Encrypted if e.auth.is_some() || e.nonce.is_none() => {
                return Err(AppendError::Invalid("encrypted envelope shape"));
            }
            AppendMode::AuthenticatedPlain if e.auth.is_none() || e.nonce.is_some() => {
                return Err(AppendError::Invalid("plain envelope shape"));
            }
            _ => {}
        }
        let aad = object_aad(&self.repo, &e.kind, &mode, &id, e.plaintext_len)?;
        let p = match mode {
            AppendMode::Encrypted => {
                let n = hex_bytes(e.nonce.as_deref().ok_or(AppendError::Invalid("nonce"))?, 24)?;
                let c = XChaCha20Poly1305::new_from_slice(&derive(
                    &self.master,
                    &self.repo,
                    b"encryption",
                )?)
                .map_err(|_| AppendError::Authentication)?;
                c.decrypt(
                    XNonce::from_slice(&n),
                    Payload {
                        msg: &hex_bytes(&e.payload, e.payload.len() / 2)?,
                        aad: &aad,
                    },
                )
                .map_err(|_| AppendError::Authentication)?
            }
            AppendMode::AuthenticatedPlain => {
                let p = hex_bytes(&e.payload, e.payload.len() / 2)?;
                let got = hmac(
                    &derive(&self.master, &self.repo, b"authentication")?,
                    b"plain",
                    &[&aad, &p],
                )?;
                let expected_auth =
                    hex_bytes(e.auth.as_deref().ok_or(AppendError::Invalid("auth"))?, 32)?;
                if bool::from(got.as_slice().ct_eq(expected_auth.as_slice())) == false {
                    return Err(AppendError::Authentication);
                }
                p
            }
        };
        if p.len() != e.plaintext_len as usize {
            return Err(AppendError::Invalid("plaintext length"));
        }
        let expected = hmac(
            &derive(&self.master, &self.repo, b"block-id")?,
            b"block-id",
            &[&self.repo, &p],
        )?;
        if bool::from(expected.as_slice().ct_eq(id.as_slice())) == false {
            return Err(AppendError::Authentication);
        }
        Ok(Zeroizing::new(p))
    }
    pub fn seal_tree(&self, tree: &Tree) -> Result<(ObjectRef, Vec<u8>), AppendError> {
        if tree.version != APPEND_VERSION || tree.repo_id != hex::encode(self.repo) {
            return Err(AppendError::Invalid("tree repository"));
        }
        self.seal_tree_owned(tree.clone())
    }

    pub(crate) fn seal_tree_owned(
        &self,
        mut value: Tree,
    ) -> Result<(ObjectRef, Vec<u8>), AppendError> {
        if value.version != APPEND_VERSION || value.repo_id != hex::encode(self.repo) {
            return Err(AppendError::Invalid("tree repository"));
        }
        crate::sync::append_tree::canonicalize_tree(&mut value)?;
        let plain = json(&value)?;
        if plain.len() > MAX_METADATA_SIZE {
            return Err(AppendError::Input("metadata too large"));
        }
        let id = hmac(
            &derive(&self.master, &self.repo, b"authentication")?,
            b"tree",
            &[&plain],
        )?;
        let (_, bytes) = self.seal("tree", &id, &plain)?;
        Ok((
            ObjectRef {
                kind: "tree".into(),
                digest: hex::encode(Sha256::digest(&bytes)),
            },
            bytes,
        ))
    }
    pub fn seal_commit(&self, commit: &Commit) -> Result<(CommitRef, Vec<u8>), AppendError> {
        validate_commit(commit, &hex::encode(self.repo))?;
        let plain = json(commit)?;
        if plain.len() > MAX_METADATA_SIZE {
            return Err(AppendError::Input("metadata too large"));
        }
        let id = hmac(
            &derive(&self.master, &self.repo, b"authentication")?,
            b"commit",
            &[&plain],
        )?;
        let (_, bytes) = self.seal("commit", &id, &plain)?;
        Ok((
            CommitRef {
                operation_id: commit.operation_id.clone(),
                digest: hex::encode(Sha256::digest(&bytes)),
            },
            bytes,
        ))
    }
    pub fn open_metadata<T: for<'de> serde::Deserialize<'de> + serde::Serialize>(
        &self,
        bytes: &[u8],
        expected: &ObjectRef,
        kind: &str,
    ) -> Result<T, AppendError> {
        if bytes.len() > METADATA_WIRE_LIMIT
            || expected.kind != kind
            || hex::encode(Sha256::digest(bytes)) != expected.digest
        {
            return Err(AppendError::Invalid("metadata digest"));
        }
        let e: Envelope = serde_json::from_slice(bytes)?;
        if serde_json::to_vec(&e)? != bytes || e.kind != kind {
            return Err(AppendError::Invalid("non-canonical metadata envelope"));
        }
        let p = self.open_envelope(bytes, &e)?;
        let value = serde_json::from_slice(&p)?;
        if serde_json::to_vec(&value)? != *p {
            return Err(AppendError::Invalid("non-canonical metadata"));
        }
        Ok(value)
    }
    pub fn open_tree(&self, bytes: &[u8], expected: &ObjectRef) -> Result<Tree, AppendError> {
        if expected.kind != "tree" {
            return Err(AppendError::Invalid("tree reference"));
        }
        let value: Tree = self.open_metadata(bytes, expected, "tree")?;
        validate_tree(&value, &hex::encode(self.repo))?;
        Ok(value)
    }
    pub fn open_commit(&self, bytes: &[u8], expected: &ObjectRef) -> Result<Commit, AppendError> {
        if expected.kind != "commit" {
            return Err(AppendError::Invalid("commit reference"));
        }
        let value: Commit = self.open_metadata(bytes, expected, "commit")?;
        validate_commit(&value, &hex::encode(self.repo))?;
        Ok(value)
    }
    fn open_envelope(
        &self,
        _bytes: &[u8],
        e: &Envelope,
    ) -> Result<Zeroizing<Vec<u8>>, AppendError> {
        if e.version != APPEND_VERSION
            || e.repo_id != hex::encode(self.repo)
            || e.plaintext_len > MAX_METADATA_SIZE as u64
        {
            return Err(AppendError::Invalid("metadata envelope"));
        }
        let raw_id = hex_bytes(&e.logical_id, 32)?;
        let id: [u8; 32] = raw_id
            .try_into()
            .map_err(|_| AppendError::Invalid("metadata id"))?;
        let payload_len = e
            .payload
            .len()
            .checked_div(2)
            .ok_or(AppendError::Invalid("payload encoding"))?;
        if e.payload.len() % 2 != 0
            || payload_len > MAX_METADATA_SIZE + 16
            || e.plaintext_len > MAX_METADATA_SIZE as u64
        {
            return Err(AppendError::Invalid("metadata payload length"));
        }
        let plain_len = usize::try_from(e.plaintext_len)
            .map_err(|_| AppendError::Invalid("metadata length"))?;
        let aad = object_aad(&self.repo, &e.kind, &e.mode, &id, e.plaintext_len)?;
        if e.mode != self.anchor.descriptor.mode {
            return Err(AppendError::Invalid("mode mismatch"));
        }
        let p = match e.mode {
            AppendMode::Encrypted => {
                if e.auth.is_some() || e.nonce.is_none() {
                    return Err(AppendError::Invalid("encrypted envelope shape"));
                }
                let n = hex_bytes(e.nonce.as_deref().ok_or(AppendError::Invalid("nonce"))?, 24)?;
                let payload = hex_bytes(&e.payload, payload_len)?;
                XChaCha20Poly1305::new_from_slice(&derive(&self.master, &self.repo, b"encryption")?)
                    .map_err(|_| AppendError::Authentication)?
                    .decrypt(
                        XNonce::from_slice(&n),
                        Payload {
                            msg: &payload,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| AppendError::Authentication)?
            }
            AppendMode::AuthenticatedPlain => {
                if e.auth.is_none() || e.nonce.is_some() {
                    return Err(AppendError::Invalid("plain envelope shape"));
                }
                let p = hex_bytes(&e.payload, payload_len)?;
                let a = hex_bytes(e.auth.as_deref().ok_or(AppendError::Invalid("auth"))?, 32)?;
                let got = hmac(
                    &derive(&self.master, &self.repo, b"authentication")?,
                    b"plain",
                    &[&aad, &p],
                )?;
                if !bool::from(got.as_slice().ct_eq(&a)) {
                    return Err(AppendError::Authentication);
                }
                p
            }
        };
        if p.len() != plain_len {
            return Err(AppendError::Invalid("metadata length"));
        }
        let expected = hmac(
            &derive(&self.master, &self.repo, b"authentication")?,
            e.kind.as_bytes(),
            &[&p],
        )?;
        if !bool::from(expected.as_slice().ct_eq(&id)) {
            return Err(AppendError::Authentication);
        }
        Ok(Zeroizing::new(p))
    }
    pub async fn prepare_block(&self, plain: &[u8]) -> Result<PreparedObject, AppendError> {
        let (key, bytes) = self.seal_block(plain)?;
        Ok(PreparedObject::new(key, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent Python 3 stdlib reference: RFC5869 extract/one-block expand
    // with hmac.digest(..., "sha256"), struct-equivalent u64 big-endian frame,
    // json.dumps(separators=(',', ':')). No production helpers generated goldens.
    fn vector_repository() -> AppendRepository {
        let repo = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let descriptor = Descriptor {
            version: 2,
            repo_id: hex::encode(repo),
            mode: AppendMode::AuthenticatedPlain,
            chunker: Chunker {
                name: "fastcdc".into(),
                version: "5.0.0".into(),
                normalization: "level1".into(),
                level: 1,
                min: 1_048_576,
                average: 4_194_304,
                max: 8_388_608,
                seed: 0,
            },
        };
        // Wrapping is not exercised here: inject a fixed master privately, never
        // expose a deterministic-master or deterministic-nonce production API.
        AppendRepository {
            anchor: Anchor {
                descriptor,
                wrapped_master: WrappedMaster {
                    kdf: params(),
                    salt: String::new(),
                    nonce: String::new(),
                    ciphertext: String::new(),
                },
                auth: String::new(),
            },
            master: Zeroizing::new(std::array::from_fn(|i| i as u8)),
            repo,
        }
    }

    #[test]
    fn v2_frame_hkdf_and_aad_independent_vectors() -> Result<(), AppendError> {
        let repo = vector_repository();
        assert_eq!(
            hex::encode(frame(b"vector", &[b"", b"\0x", b"abc"])),
            "706565726461762d617070656e642f76322f766563746f72000000000000000000000000000000000200780000000000000003616263"
        );
        for (purpose, expected) in [
            (
                b"anchor".as_slice(),
                "07c19682f5d08f71ab300c01358abe803caf6f6616208483f7eebc94963d47c2",
            ),
            (
                b"block-id".as_slice(),
                "e406c7e9b083bde00c5c1e336d2f1e3aec15d6f002d1ffa0ecc3c688c1c11dfc",
            ),
            (
                b"encryption".as_slice(),
                "2a82c21fc179930b5440a6fcbbbcaf988803dd88078d136880129c3eacb94f44",
            ),
            (
                b"authentication".as_slice(),
                "b5fd873ba2c5306cd8993465a93a787906af4ced51278056b260ddbba45c23e6",
            ),
        ] {
            assert_eq!(
                hex::encode(derive(&repo.master, &repo.repo, purpose)?),
                expected
            );
        }
        let id = hmac(
            &derive(&repo.master, &repo.repo, b"block-id")?,
            b"block-id",
            &[&repo.repo, b"\0append-v2\n"],
        )?;
        assert_eq!(
            hex::encode(id),
            "edd1f428c0adf21b1dcb56e7e735158db52353e5ad7fa5eebfe28e7c33c40697"
        );
        assert_eq!(
            hex::encode(object_aad(
                &repo.repo,
                "block",
                &AppendMode::AuthenticatedPlain,
                &id,
                11
            )?),
            "706565726461762d617070656e642f76322f6f626a65637400000000000000000132000000000000001000112233445566778899aabbccddeeff000000000000000722626c6f636b2200000000000000152261757468656e746963617465645f706c61696e220000000000000020edd1f428c0adf21b1dcb56e7e735158db52353e5ad7fa5eebfe28e7c33c4069700000000000000023131"
        );
        Ok(())
    }

    #[test]
    fn v2_authenticated_plain_envelope_independent_golden() -> Result<(), AppendError> {
        let repo = vector_repository();
        let golden = br#"{"version":2,"repo_id":"00112233445566778899aabbccddeeff","kind":"block","mode":"authenticated_plain","logical_id":"edd1f428c0adf21b1dcb56e7e735158db52353e5ad7fa5eebfe28e7c33c40697","plaintext_len":11,"nonce":null,"payload":"00617070656e642d76320a","auth":"ec97c1f49e963bd5667230a4d3d380f9ddd5f405a190196fa4ce7711e723d057"}"#;
        let (key, wire) = repo.seal_block(b"\0append-v2\n")?;
        assert_eq!(wire, golden);
        assert_eq!(
            key,
            "_peerdav_encrypt/00112233445566778899aabbccddeeff/blocks/c9ab99aff13090d572fe7d6c9815f36968e6c929c696b35019e0eba196281aa1"
        );
        assert_eq!(repo.open_block(golden)?.as_slice(), b"\0append-v2\n");
        Ok(())
    }

    #[test]
    fn v2_tree_canonical_bytes_independent_golden() -> Result<(), AppendError> {
        let repo = vector_repository();
        let golden = br#"{"version":2,"repo_id":"00112233445566778899aabbccddeeff","directory":"root","entries":[],"children":[{"directory":"a","object":{"kind":"tree","digest":"1111111111111111111111111111111111111111111111111111111111111111"}},{"directory":"z","object":{"kind":"tree","digest":"2222222222222222222222222222222222222222222222222222222222222222"}}],"bindings":[{"block_id":"edd1f428c0adf21b1dcb56e7e735158db52353e5ad7fa5eebfe28e7c33c40697","len":11,"object":{"kind":"block","digest":"3333333333333333333333333333333333333333333333333333333333333333"}},{"block_id":"edd1f428c0adf21b1dcb56e7e735158db52353e5ad7fa5eebfe28e7c33c40697","len":11,"object":{"kind":"block","digest":"4444444444444444444444444444444444444444444444444444444444444444"}}]}"#;
        let canonical: Tree = serde_json::from_slice(golden)?;
        let mut unsorted = canonical.clone();
        unsorted.children.reverse();
        unsorted.children.push(unsorted.children[0].clone());
        unsorted.bindings.reverse();
        unsorted.bindings.push(unsorted.bindings[0].clone());
        crate::sync::append_tree::canonicalize_tree(&mut unsorted)?;
        assert_eq!(json(&unsorted)?, golden);
        assert_eq!(
            hex::encode(Sha256::digest(golden)),
            "5c34238f180d8782ba470e4fb07da977a366251cf2415342930d7d37b11e94b6"
        );
        let (reference, wire) = repo.seal_tree(&unsorted)?;
        let envelope: Envelope = serde_json::from_slice(&wire)?;
        assert_eq!(
            envelope.logical_id,
            "4f5de3f3d1289b1cd52160fbb3e5447b83c25ff5535f9ad4db4e2f63baa7e81b"
        );
        assert_eq!(
            hex::decode(&envelope.payload).map_err(|_| AppendError::Input("golden payload"))?,
            golden
        );
        assert_eq!(repo.open_tree(&wire, &reference)?, canonical);
        Ok(())
    }
}
