//! Vault lifecycle: first-run initialization, explicit unlock/lock, admin
//! authentication, password rotation, and encrypted secret storage.
//!
//! Security invariants:
//! - The 32-byte vault key exists only inside [`Session`] in process memory.
//! - [`zeroize::Zeroizing`] clears the key on lock, rotation, and drop.
//! - SQLite receives only AEAD ciphertext and non-secret KDF metadata.
//! - Every record is bound to its name through AEAD associated data.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use zeroize::Zeroizing;

use crate::admin;
use crate::crypto;
use crate::db::Database;
use crate::error::DomainError;
use crate::types::{
    AdminLoginResult, AdminRecord, AuthorizationRecord, InitializeVaultResult, KDF_ALGORITHM,
    KDF_SALT_LEN, META_KDF_ALGORITHM, META_KDF_SALT, META_SCHEMA_VERSION, PEER_SECRET_PREFIX,
    PeerRecord, PeerSecret, SCHEMA_VERSION, SECRET_ADMIN, SECRET_VAULT_VERIFIER, SharePermissions,
    ShareRecord, ShareRuleRecord, VAULT_KEY_LEN, VAULT_VERIFIER_PLAINTEXT, VaultStatus,
};

#[derive(Default)]
struct Session {
    key: Option<Zeroizing<[u8; VAULT_KEY_LEN]>>,
}

/// Process-wide vault handle. The database mutex serializes sensitive
/// operations; the session mutex owns the in-memory key.
pub struct Vault {
    db: Mutex<Database>,
    session: Mutex<Session>,
}

impl Vault {
    pub fn open(path: &Path) -> Result<Self, DomainError> {
        Ok(Self {
            db: Mutex::new(Database::open(path)?),
            session: Mutex::new(Session::default()),
        })
    }

    pub fn status(&self) -> Result<VaultStatus, DomainError> {
        let initialized = self.db()?.is_initialized()?;
        let unlocked = self.session()?.key.is_some();
        Ok(VaultStatus {
            initialized,
            unlocked,
        })
    }

    /// First-run initialization. Stores the admin username and the Argon2id
    /// hash of the user-chosen admin password, and leaves the vault unlocked
    /// for the current process.
    pub fn initialize(
        &self,
        master_password: &str,
        admin_username: &str,
        admin_password: &str,
    ) -> Result<InitializeVaultResult, DomainError> {
        admin::validate_password(master_password)?;
        admin::validate_username(admin_username)?;
        admin::validate_password(admin_password)?;
        let admin_username = admin_username.trim().to_owned();

        let mut db = self.db()?;
        if db.is_initialized()? {
            return Err(DomainError::VaultAlreadyInitialized);
        }

        let salt = crypto::random_bytes::<KDF_SALT_LEN>();
        let key = crypto::derive_key(master_password, &salt)?;

        let record = admin::new_record(admin_username.clone(), admin_password)?;
        let admin_plaintext = Zeroizing::new(admin::encode(&record)?);

        let (admin_nonce, admin_ciphertext) =
            crypto::encrypt(&key, SECRET_ADMIN.as_bytes(), &admin_plaintext)?;
        let (verifier_nonce, verifier_ciphertext) = crypto::encrypt(
            &key,
            SECRET_VAULT_VERIFIER.as_bytes(),
            VAULT_VERIFIER_PLAINTEXT,
        )?;

        let meta = vec![
            (META_SCHEMA_VERSION, SCHEMA_VERSION.to_owned()),
            (META_KDF_ALGORITHM, KDF_ALGORITHM.to_owned()),
            (META_KDF_SALT, crypto::encode_salt(&salt)),
        ];
        let secrets = vec![
            (SECRET_VAULT_VERIFIER, verifier_nonce, verifier_ciphertext),
            (SECRET_ADMIN, admin_nonce, admin_ciphertext),
        ];
        db.initialize_with(&meta, &secrets)?;
        drop(db);

        self.session()?.key = Some(key);

        Ok(InitializeVaultResult {
            admin_username,
            vault_status: VaultStatus {
                initialized: true,
                unlocked: true,
            },
        })
    }

    /// Verifies the master password against the encrypted verifier and keeps
    /// the derived key in memory until [`Vault::lock`] or process exit.
    pub fn unlock(&self, master_password: &str) -> Result<VaultStatus, DomainError> {
        let db = self.db()?;
        let salt = read_kdf_salt(&db)?;
        let key = crypto::derive_key(master_password, &salt)?;
        verify_verifier(&db, &key)?;
        drop(db);

        self.session()?.key = Some(key);
        Ok(VaultStatus {
            initialized: true,
            unlocked: true,
        })
    }

    /// Drops and zeroizes the in-memory key. The database remains usable for
    /// status checks but all secret operations require unlocking again.
    pub fn lock(&self) -> Result<VaultStatus, DomainError> {
        {
            let mut session = self.session()?;
            session.key = None;
        }
        self.status()
    }

    pub fn admin_login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<AdminLoginResult, DomainError> {
        let db = self.db()?;
        let session = self.session()?;
        let key = require_key(&session)?;
        let record = load_admin_record(&db, key)?;

        if !admin::authenticate(&record, username, password)? {
            return Err(DomainError::InvalidAdminCredentials);
        }

        Ok(AdminLoginResult {
            username: record.username,
            must_change_password: record.must_change_password,
        })
    }

    /// Verifies the current admin password, replaces its Argon2id hash, and
    /// clears the forced-password-change flag.
    pub fn change_admin_password(
        &self,
        username: &str,
        current_password: &str,
        new_password: &str,
    ) -> Result<AdminLoginResult, DomainError> {
        admin::validate_password(new_password)?;

        let db = self.db()?;
        let session = self.session()?;
        let key = require_key(&session)?;
        let mut record = load_admin_record(&db, key)?;

        if !admin::authenticate(&record, username, current_password)? {
            return Err(DomainError::InvalidAdminCredentials);
        }
        if current_password == new_password {
            return Err(DomainError::PasswordUnchanged);
        }

        admin::update_password(&mut record, new_password)?;
        let plaintext = Zeroizing::new(admin::encode(&record)?);
        let (nonce, ciphertext) = crypto::encrypt(key, SECRET_ADMIN.as_bytes(), &plaintext)?;
        db.put_secret(SECRET_ADMIN, &nonce, &ciphertext)?;

        Ok(AdminLoginResult {
            username: record.username,
            must_change_password: false,
        })
    }

    /// Re-derives the vault key from a new master password and re-encrypts all
    /// secret records in one SQLite transaction.
    pub fn change_master_password(
        &self,
        current_password: &str,
        new_password: &str,
    ) -> Result<VaultStatus, DomainError> {
        admin::validate_password(new_password)?;

        let mut db = self.db()?;
        let mut session = self.session()?;
        if session.key.is_none() {
            return Err(DomainError::VaultLocked);
        }

        let old_salt = read_kdf_salt(&db)?;
        let old_key = crypto::derive_key(current_password, &old_salt)?;
        verify_verifier(&db, &old_key)?;

        if current_password == new_password {
            return Err(DomainError::PasswordUnchanged);
        }

        let new_salt = crypto::random_bytes::<KDF_SALT_LEN>();
        let new_key = crypto::derive_key(new_password, &new_salt)?;
        let new_salt_encoded = crypto::encode_salt(&new_salt);

        db.reencrypt_all(
            &[(META_KDF_SALT, new_salt_encoded)],
            |name, nonce, ciphertext| {
                let plaintext = decrypt_record(&old_key, name, nonce, ciphertext)?;
                crypto::encrypt(&new_key, name.as_bytes(), &plaintext)
            },
        )?;

        session.key = Some(new_key);
        Ok(VaultStatus {
            initialized: true,
            unlocked: true,
        })
    }

    /// Stores an encrypted secret record. Reserved names are rejected so
    /// application code cannot overwrite vault internals.
    pub fn store_secret(&self, name: &str, plaintext: &[u8]) -> Result<(), DomainError> {
        validate_secret_name(name)?;

        let db = self.db()?;
        let session = self.session()?;
        let key = require_key(&session)?;
        let (nonce, ciphertext) = crypto::encrypt(key, name.as_bytes(), plaintext)?;
        db.put_secret(name, &nonce, &ciphertext)
    }

    /// Loads and authenticates an encrypted secret record.
    pub fn load_secret(&self, name: &str) -> Result<Zeroizing<Vec<u8>>, DomainError> {
        validate_secret_name(name)?;

        let db = self.db()?;
        let session = self.session()?;
        let key = require_key(&session)?;
        let record = db
            .read_secret(name)?
            .ok_or_else(|| DomainError::SecretNotFound {
                name: name.to_owned(),
            })?;
        decrypt_record(key, name, &record.nonce, &record.ciphertext)
    }

    /// Verifies admin credentials for one command. Admin operations require an
    /// unlocked vault and an admin who has completed the forced first-login
    /// password change; no session is retained between commands.
    pub fn verify_admin(&self, username: &str, password: &str) -> Result<(), DomainError> {
        let result = self.admin_login(username, password)?;
        if result.must_change_password {
            return Err(DomainError::AdminPasswordChangeRequired);
        }
        Ok(())
    }

    /// Loads all persisted shares and their rules. Policy data is not
    /// encrypted, so this works while the vault is locked.
    pub(crate) fn load_shares(&self) -> Result<Vec<ShareRecord>, DomainError> {
        self.db()?.list_shares()
    }

    /// Persists a new share and its rules atomically.
    pub(crate) fn create_share(
        &self,
        id: &str,
        name: &str,
        root_path: &str,
        permissions: SharePermissions,
        rules: &[ShareRuleRecord],
    ) -> Result<(), DomainError> {
        let mut db = self.db()?;
        db.insert_share(id, name, root_path, permissions, rules)
    }

    pub(crate) fn update_share_permissions(
        &self,
        id: &str,
        permissions: SharePermissions,
    ) -> Result<(), DomainError> {
        self.db()?.update_share_permissions(id, permissions)
    }

    /// Replaces the entire rule set of a share in one transaction.
    pub(crate) fn set_share_rules(
        &self,
        id: &str,
        rules: &[ShareRuleRecord],
    ) -> Result<(), DomainError> {
        let mut db = self.db()?;
        db.replace_share_rules(id, rules)
    }

    pub(crate) fn delete_share(&self, id: &str) -> Result<(), DomainError> {
        self.db()?.delete_share(id)
    }

    /// Active (non-revoked) authorizations. The stored value is a SHA-256
    /// digest; the plaintext code is never persisted.
    pub(crate) fn list_authorizations(&self) -> Result<Vec<AuthorizationRecord>, DomainError> {
        self.db()?.list_active_authorizations()
    }

    pub(crate) fn add_authorization(
        &self,
        id: &str,
        label: &str,
        code_hash: &str,
    ) -> Result<AuthorizationRecord, DomainError> {
        let created_at = self.db()?.insert_authorization(id, label, code_hash)?;
        Ok(AuthorizationRecord {
            id: id.to_owned(),
            label: label.to_owned(),
            code_hash: code_hash.to_owned(),
            created_at,
        })
    }

    pub(crate) fn revoke_authorization(&self, id: &str) -> Result<(), DomainError> {
        self.db()?.revoke_authorization(id)
    }

    /// Peer metadata is non-secret and remains readable while locked.
    pub(crate) fn list_peer_records(&self) -> Result<Vec<PeerRecord>, DomainError> {
        self.db()?.list_peers()
    }

    pub(crate) fn peer_record(&self, id: &str) -> Result<Option<PeerRecord>, DomainError> {
        self.db()?.get_peer(id)
    }

    pub(crate) fn save_peer_record(&self, record: &PeerRecord) -> Result<(), DomainError> {
        self.db()?.upsert_peer(record)
    }

    pub(crate) fn delete_peer_record(&self, id: &str) -> Result<(), DomainError> {
        self.db()?.delete_peer(id)
    }

    /// Stores the peer URL and authorization code as one encrypted record.
    pub(crate) fn save_peer_code(
        &self,
        id: &str,
        url: &str,
        code: &str,
    ) -> Result<(), DomainError> {
        let secret = PeerSecret {
            url: url.to_owned(),
            code: code.to_owned(),
        };
        let plaintext =
            serde_json::to_vec(&secret).map_err(|source| DomainError::Serialization {
                operation: "encode peer secret",
                source,
            })?;
        self.store_secret(&peer_secret_name(id)?, &plaintext)
    }

    pub(crate) fn load_peer_code(&self, id: &str) -> Result<PeerSecret, DomainError> {
        let name = peer_secret_name(id)?;
        let plaintext = self.load_secret(&name)?;
        serde_json::from_slice(&plaintext).map_err(|source| DomainError::Serialization {
            operation: "decode peer secret",
            source,
        })
    }

    pub(crate) fn delete_peer_code(&self, id: &str) -> Result<(), DomainError> {
        self.db()?.delete_secret(&peer_secret_name(id)?)
    }

    fn db(&self) -> Result<MutexGuard<'_, Database>, DomainError> {
        self.db.lock().map_err(|_| DomainError::Internal {
            detail: "database mutex poisoned".to_owned(),
        })
    }

    fn session(&self) -> Result<MutexGuard<'_, Session>, DomainError> {
        self.session.lock().map_err(|_| DomainError::Internal {
            detail: "session mutex poisoned".to_owned(),
        })
    }
}

fn require_key(session: &Session) -> Result<&[u8; VAULT_KEY_LEN], DomainError> {
    session.key.as_deref().ok_or(DomainError::VaultLocked)
}

fn read_kdf_salt(db: &Database) -> Result<[u8; KDF_SALT_LEN], DomainError> {
    let encoded = db
        .read_meta(META_KDF_SALT)?
        .ok_or(DomainError::VaultNotInitialized)?;
    let algorithm = db
        .read_meta(META_KDF_ALGORITHM)?
        .ok_or_else(|| DomainError::Corrupted {
            detail: "KDF algorithm metadata is missing".to_owned(),
        })?;
    if algorithm != KDF_ALGORITHM {
        return Err(DomainError::Corrupted {
            detail: format!("unsupported KDF algorithm `{algorithm}`"),
        });
    }
    crypto::decode_salt(&encoded)
}

fn verify_verifier(db: &Database, key: &[u8; VAULT_KEY_LEN]) -> Result<(), DomainError> {
    let record = db
        .read_secret(SECRET_VAULT_VERIFIER)?
        .ok_or_else(|| DomainError::Corrupted {
            detail: "vault verifier record is missing".to_owned(),
        })?;

    let plaintext = crypto::decrypt(
        key,
        SECRET_VAULT_VERIFIER.as_bytes(),
        &record.nonce,
        &record.ciphertext,
    )
    .map_err(|error| match error {
        DomainError::Crypto { detail, .. } => DomainError::InvalidMasterPassword { detail },
        other => other,
    })?;

    if plaintext.as_slice() != VAULT_VERIFIER_PLAINTEXT {
        return Err(DomainError::InvalidMasterPassword {
            detail: "vault verifier plaintext mismatch".to_owned(),
        });
    }
    Ok(())
}

fn decrypt_record(
    key: &[u8; VAULT_KEY_LEN],
    name: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, DomainError> {
    crypto::decrypt(key, name.as_bytes(), nonce, ciphertext).map_err(|error| match error {
        DomainError::Crypto { detail, .. } => DomainError::Corrupted { detail },
        other => other,
    })
}

fn load_admin_record(db: &Database, key: &[u8; VAULT_KEY_LEN]) -> Result<AdminRecord, DomainError> {
    let record = db
        .read_secret(SECRET_ADMIN)?
        .ok_or_else(|| DomainError::Corrupted {
            detail: "admin credentials record is missing".to_owned(),
        })?;
    let plaintext = decrypt_record(key, SECRET_ADMIN, &record.nonce, &record.ciphertext)?;
    admin::decode(&plaintext)
}

fn peer_secret_name(id: &str) -> Result<String, DomainError> {
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DomainError::InvalidSecretName);
    }
    Ok(format!("{PEER_SECRET_PREFIX}{id}"))
}

fn validate_secret_name(name: &str) -> Result<(), DomainError> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(DomainError::InvalidSecretName);
    }
    if name.starts_with("__") {
        return Err(DomainError::ReservedSecretName);
    }
    Ok(())
}
