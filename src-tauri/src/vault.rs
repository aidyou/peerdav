//! Vault lifecycle: first-run key-file creation, automatic key loading,
//! admin authentication, key-loss reset, and encrypted secret storage.
//!
//! Security invariants:
//! - The 32-byte vault key exists only inside [`Session`] in process memory.
//! - [`zeroize::Zeroizing`] clears the key on lock, rotation, and drop.
//! - SQLite receives only AEAD ciphertext, the non-secret key file path, and
//!   a one-way recovery-code fingerprint.
//! - Every record is bound to its name through AEAD associated data.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::admin;
use crate::crypto;
use crate::db::Database;
use crate::error::DomainError;
use crate::keyfile;
use crate::types::{
    AdminLoginResult, AdminRecord, AdminSessionInfo, AuthorizationRecord, CLIENT_ID_META,
    InitializeVaultResult, META_KEY_FILE, META_RECOVERY_HASH, META_SCHEMA_VERSION,
    PEER_SECRET_PREFIX, PeerRecord, PeerSecret, ResetVaultResult, SCHEMA_VERSION, SECRET_ADMIN,
    SECRET_VAULT_VERIFIER, SharePermissions, ShareRecord, ShareRuleRecord, VAULT_KEY_LEN,
    VAULT_VERIFIER_PLAINTEXT, VaultStatus,
};

#[derive(Default)]
struct Session {
    key: Option<Zeroizing<[u8; VAULT_KEY_LEN]>>,
    /// Username of the admin who signed in during this unlock. Kept only in
    /// process memory so a webview reload keeps the user signed in; cleared on
    /// lock and logout.
    admin: Option<String>,
    interface_locked: bool,
    /// Stable reason code when the configured key file could not be loaded at
    /// startup, so the UI can show a recovery path instead of a password form.
    key_error: Option<&'static str>,
}

/// Process-wide vault handle. The database mutex serializes sensitive
/// operations; the session mutex owns the in-memory key.
pub struct Vault {
    path: PathBuf,
    db: Mutex<Database>,
    session: Mutex<Session>,
    identity: crate::types::DeviceIdentity,
}

impl Vault {
    pub fn open(path: &Path) -> Result<Self, DomainError> {
        let vault = Self {
            path: path.to_path_buf(),
            db: Mutex::new(Database::open(path)?),
            session: Mutex::new(Session::default()),
            identity: crate::tls::load_identity(path)?,
        };
        if let Err(error) = vault.auto_load_key() {
            log::warn!(
                "vault key auto-load failed: {}",
                crate::error::error_chain(&error)
            );
        }
        Ok(vault)
    }

    pub(crate) fn identity(&self) -> &crate::types::DeviceIdentity {
        &self.identity
    }

    pub(crate) fn transfer_directory(&self) -> PathBuf {
        self.path.with_extension("transfers")
    }

    pub(crate) fn current_identity(&self) -> Result<crate::types::DeviceIdentity, DomainError> {
        let path = self.path.with_extension("identity.json");
        if !path.is_file() {
            return Err(DomainError::Io {
                operation: "read device identity",
                path: Some(path),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "device identity is missing; restore its backup before reconnecting",
                ),
            });
        }
        let identity = crate::tls::load_identity(&self.path)?;
        if identity.fingerprint != self.identity.fingerprint {
            return Err(DomainError::Crypto { operation: "load device identity", detail: "device identity changed while running; restore the original identity or restart and pair again".into() });
        }
        Ok(identity)
    }

    pub fn fingerprint(&self) -> &str {
        &self.identity.fingerprint
    }

    /// A pinned, mutually authenticated client, also usable by headless callers.
    pub fn encrypted_client(
        &self,
        server_fingerprint: &str,
    ) -> Result<reqwest::Client, DomainError> {
        crate::tls::agent(self, server_fingerprint, crate::types::PEER_REQUEST_TIMEOUT)
    }

    pub fn status(&self) -> Result<VaultStatus, DomainError> {
        let (initialized, key_file) = {
            let db = self.db()?;
            (db.is_initialized()?, db.read_meta(META_KEY_FILE)?)
        };
        let session = self.session()?;
        Ok(VaultStatus {
            initialized,
            unlocked: session.key.is_some(),
            key_file,
            key_error: session.key_error.map(str::to_owned),
        })
    }

    /// Whether the process currently holds the vault key.
    pub(crate) fn is_unlocked(&self) -> bool {
        self.session()
            .map(|session| session.key.is_some())
            .unwrap_or(false)
    }

    /// First-run initialization. Generates a key file in the chosen directory,
    /// stores its path in SQLite, encrypts the admin record, and leaves the
    /// vault unlocked for the current process.
    pub fn initialize(
        &self,
        key_directory: &str,
        admin_username: &str,
        admin_password: &str,
    ) -> Result<InitializeVaultResult, DomainError> {
        admin::validate_username(admin_username)?;
        admin::validate_password(admin_password)?;
        let admin_username = admin_username.trim().to_owned();

        {
            let db = self.db()?;
            if db.is_initialized()? {
                return Err(DomainError::VaultAlreadyInitialized);
            }
        }

        let material = keyfile::generate_in_directory(key_directory, self.fingerprint())?;
        let key_path = material.path.clone();
        let key = material.key;
        let recovery_code = generate_recovery_code();
        let recovery_hash = recovery_hash_hex(&recovery_code);

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
            (META_KEY_FILE, key_path.display().to_string()),
            (META_RECOVERY_HASH, recovery_hash),
        ];
        let secrets = vec![
            (SECRET_VAULT_VERIFIER, verifier_nonce, verifier_ciphertext),
            (SECRET_ADMIN, admin_nonce, admin_ciphertext),
        ];

        let initialized = {
            let mut db = self.db()?;
            db.initialize_with(&meta, &secrets)
        };
        if let Err(error) = initialized {
            let _ = fs::remove_file(&key_path);
            return Err(error);
        }

        {
            let mut session = self.session()?;
            session.key = Some(key);
            session.admin = None;
            session.interface_locked = false;
            session.key_error = None;
        }

        Ok(InitializeVaultResult {
            admin_username,
            vault_status: self.status()?,
            recovery_code,
        })
    }

    /// Resets only the encrypted records after the user proves ownership with
    /// the one-time recovery code. Non-secret data (shares, authorizations,
    /// peers, and server settings) is deliberately preserved. The original
    /// database is archived first so a later key discovery can still recover it.
    pub fn reset_lost_key(
        &self,
        recovery_code: &str,
        key_directory: &str,
        admin_username: &str,
        admin_password: &str,
    ) -> Result<ResetVaultResult, DomainError> {
        admin::validate_username(admin_username)?;
        admin::validate_password(admin_password)?;
        let admin_username = admin_username.trim().to_owned();

        let status = self.status()?;
        if !status.initialized {
            return Err(DomainError::VaultNotInitialized);
        }
        if status.unlocked {
            return Err(DomainError::VaultMustBeLocked);
        }
        {
            let db = self.db()?;
            verify_recovery_code(&db, recovery_code)?;
        }

        let archived = self.archive_database()?;
        let material = keyfile::generate_in_directory(key_directory, self.fingerprint())?;
        let key_path = material.path.clone();
        let key = material.key;
        let next_recovery_code = generate_recovery_code();
        let next_recovery_hash = recovery_hash_hex(&next_recovery_code);

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
            (META_KEY_FILE, key_path.display().to_string()),
            (META_RECOVERY_HASH, next_recovery_hash),
        ];
        let secrets = vec![
            (SECRET_VAULT_VERIFIER, verifier_nonce, verifier_ciphertext),
            (SECRET_ADMIN, admin_nonce, admin_ciphertext),
        ];

        let reset = {
            let mut db = self.db()?;
            db.reset_sensitive(&meta, &secrets)
        };
        if let Err(error) = reset {
            let _ = fs::remove_file(&key_path);
            return Err(error);
        }

        {
            let mut session = self.session()?;
            session.key = Some(key);
            session.admin = None;
            session.interface_locked = false;
            session.key_error = None;
        }

        Ok(ResetVaultResult {
            admin_username,
            vault_status: self.status()?,
            recovery_code: next_recovery_code,
            archived_database: archived.display().to_string(),
        })
    }

    /// Reloads the key file, either from the persisted path or from a backup
    /// path selected by the user. The persisted path is updated only after the
    /// candidate key has been verified against the vault.
    pub fn reload_key(&self, path: Option<&str>) -> Result<VaultStatus, DomainError> {
        let configured = {
            let db = self.db()?;
            if !db.is_initialized()? {
                return Err(DomainError::VaultNotInitialized);
            }
            db.read_meta(META_KEY_FILE)?
        };

        let candidate = match path.map(str::trim).filter(|path| !path.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => configured
                .map(PathBuf::from)
                .ok_or(DomainError::KeyFileNotConfigured)?,
        };

        let material = self.load_key_material(&candidate)?;
        self.db()?
            .write_meta(META_KEY_FILE, &material.path.display().to_string())?;

        {
            let mut session = self.session()?;
            session.key = Some(material.key);
            session.key_error = None;
        }
        self.status()
    }

    /// Drops and zeroizes the in-memory key. The database remains usable for
    /// status checks but all secret operations require reloading the key file.
    pub fn lock(&self) -> Result<VaultStatus, DomainError> {
        {
            let mut session = self.session()?;
            session.key = None;
            session.admin = None;
            session.interface_locked = false;
            session.key_error = None;
        }
        self.status()
    }

    pub fn admin_login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<AdminLoginResult, DomainError> {
        let db = self.db()?;
        let mut session = self.session()?;
        let key = *require_key(&session)?;
        let record = load_admin_record(&db, &key)?;

        if !admin::authenticate(&record, username, password)? {
            return Err(DomainError::InvalidAdminCredentials);
        }

        // Remember the signed-in admin for the lifetime of this unlock so a
        // webview reload does not force another login.
        session.admin = Some(record.username.clone());
        session.interface_locked = false;

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
        let mut session = self.session()?;
        let key = *require_key(&session)?;
        let mut record = load_admin_record(&db, &key)?;

        if !admin::authenticate(&record, username, current_password)? {
            return Err(DomainError::InvalidAdminCredentials);
        }
        if current_password == new_password {
            return Err(DomainError::PasswordUnchanged);
        }

        admin::update_password(&mut record, new_password)?;
        let plaintext = Zeroizing::new(admin::encode(&record)?);
        let (nonce, ciphertext) = crypto::encrypt(&key, SECRET_ADMIN.as_bytes(), &plaintext)?;
        db.put_secret(SECRET_ADMIN, &nonce, &ciphertext)?;

        session.admin = Some(record.username.clone());
        session.interface_locked = false;

        Ok(AdminLoginResult {
            username: record.username,
            must_change_password: false,
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
    /// password change. When the password is omitted, a matching in-memory
    /// admin session (established by login and kept until lock/logout) is
    /// accepted so a webview reload keeps the user signed in.
    pub fn verify_admin(&self, username: &str, password: &str) -> Result<(), DomainError> {
        self.require_interface_unlocked()?;
        if password.is_empty() {
            let session = self.session()?;
            if session.admin.as_deref() == Some(username.trim()) {
                require_key(&session)?;
                return Ok(());
            }
        }

        let result = self.admin_login(username, password)?;
        if result.must_change_password {
            return Err(DomainError::AdminPasswordChangeRequired);
        }
        Ok(())
    }

    /// Returns the in-memory admin session, if any. Used by the UI to restore
    /// the signed-in state after a page reload.
    pub fn admin_session(&self) -> Result<Option<AdminSessionInfo>, DomainError> {
        let db = self.db()?;
        let session = self.session()?;
        let Some(username) = session.admin.clone() else {
            return Ok(None);
        };
        let key = require_key(&session)?;
        let record = load_admin_record(&db, key)?;
        Ok(Some(AdminSessionInfo {
            interface_locked: session.interface_locked,
            username,
            must_change_password: record.must_change_password,
        }))
    }

    pub fn lock_interface(&self) -> Result<(), DomainError> {
        let mut session = self.session()?;
        if session.admin.is_some() {
            session.interface_locked = true;
        }
        Ok(())
    }

    pub(crate) fn require_interface_unlocked(&self) -> Result<(), DomainError> {
        if self.session()?.interface_locked {
            return Err(DomainError::InterfaceLocked);
        }
        Ok(())
    }

    pub fn unlock_interface(&self, password: &str) -> Result<(), DomainError> {
        let db = self.db()?;
        let mut session = self.session()?;
        let username = session
            .admin
            .as_deref()
            .ok_or(DomainError::InvalidAdminCredentials)?;
        let record = load_admin_record(&db, require_key(&session)?)?;
        if !admin::authenticate(&record, username, password)? {
            return Err(DomainError::InvalidAdminCredentials);
        }
        session.interface_locked = false;
        Ok(())
    }

    /// Clears the in-memory admin session without locking the vault.
    pub fn admin_logout(&self) -> Result<(), DomainError> {
        let mut session = self.session()?;
        session.admin = None;
        session.interface_locked = false;
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

    /// Active and revoked authorization rows. Revoked rows are tombstones used
    /// by PeerDav clients to remove a server binding whose code no longer works.
    pub(crate) fn list_authorization_records(
        &self,
    ) -> Result<Vec<AuthorizationRecord>, DomainError> {
        self.db()?.list_all_authorizations()
    }

    pub(crate) fn add_authorization(
        &self,
        id: &str,
        label: &str,
        code_hash: &str,
        machine_id: Option<&str>,
    ) -> Result<AuthorizationRecord, DomainError> {
        let db = self.db()?;
        let created_at = db.insert_authorization(id, label, code_hash)?;
        db.set_authorization_machine(id, machine_id)?;
        Ok(AuthorizationRecord {
            id: id.to_owned(),
            label: label.to_owned(),
            code_hash: code_hash.to_owned(),
            client_id: None,
            machine_id: machine_id.map(str::to_owned),
            created_at,
            revoked_at: None,
        })
    }

    pub(crate) fn bind_authorization(
        &self,
        id: &str,
        client_id: &str,
    ) -> Result<bool, DomainError> {
        self.db()?.set_authorization_client(id, client_id)
    }

    /// 该机器码名下仍然有效的授权 id。
    pub(crate) fn authorization_ids_for_machine(
        &self,
        machine_id: &str,
    ) -> Result<Vec<String>, DomainError> {
        self.db()?.active_authorization_ids_for_machine(machine_id)
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

    pub(crate) fn set_peer_authorization_id(
        &self,
        peer_id: &str,
        authorization_id: &str,
    ) -> Result<(), DomainError> {
        self.db()?
            .set_peer_authorization_id(peer_id, authorization_id)
    }

    /// Returns the stable installation id, creating it on first use. It is not
    /// secret; it only prevents one bound client from deleting another client's
    /// server authorization.
    ///
    /// 新安装优先用硬件机器码作为安装 id，这样同一台机器重装应用后仍能被对端认成
    /// 同一台设备；旧安装保留已写入的值，避免切换后历史授权因绑定 id 变化而失效。
    pub(crate) fn ensure_client_id(&self) -> Result<String, DomainError> {
        let db = self.db()?;
        if let Some(existing) = db.read_meta(CLIENT_ID_META)?
            && existing == self.identity.fingerprint
        {
            return Ok(existing);
        }

        let client_id = self.identity.fingerprint.clone();
        db.write_meta(CLIENT_ID_META, &client_id)?;
        Ok(client_id)
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

    /// Reads a non-secret setting from the metadata table.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DomainError> {
        self.db()?.read_meta(key)
    }

    /// Persists a non-secret setting in the metadata table.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DomainError> {
        self.db()?.write_meta(key, value)
    }

    /// Application preferences are separate from vault lifecycle metadata.
    pub(crate) fn get_config(&self, key: &str) -> Result<Option<String>, DomainError> {
        self.db()?.read_config(key)
    }

    pub(crate) fn set_config(&self, key: &str, value: &str) -> Result<(), DomainError> {
        self.db()?.write_config(key, value)
    }

    fn auto_load_key(&self) -> Result<(), DomainError> {
        let configured = {
            let db = self.db()?;
            if !db.is_initialized()? {
                return Ok(());
            }
            db.read_meta(META_KEY_FILE)?
        };
        let Some(path) = configured else {
            return Ok(());
        };

        match self.load_key_material(Path::new(&path)) {
            Ok(material) => {
                let mut session = self.session()?;
                session.key = Some(material.key);
                session.key_error = None;
                Ok(())
            }
            Err(error) => {
                let reason = key_error_reason(&error);
                let mut session = self.session()?;
                session.key_error = Some(reason);
                Err(error)
            }
        }
    }

    fn load_key_material(&self, path: &Path) -> Result<keyfile::KeyMaterial, DomainError> {
        let material = keyfile::read(path)?;
        let db = self.db()?;
        verify_key_file(&db, &material.key)?;
        Ok(material)
    }

    /// Copies the database next to itself before a destructive reset. Keeping
    /// the archive allows a later key-file discovery to recover the old data.
    fn archive_database(&self) -> Result<PathBuf, DomainError> {
        let _db = self.db()?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| DomainError::Internal {
                detail: format!("system clock before unix epoch: {error}"),
            })?
            .as_nanos();
        let name = self
            .path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "peerdav".to_owned());
        let extension = self
            .path
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_else(|| "pd".to_owned());
        let archive = self
            .path
            .with_file_name(format!("{name}.reset-{stamp}.{extension}"));

        fs::copy(&self.path, &archive).map_err(|source| DomainError::Io {
            operation: "archive database before reset",
            path: Some(archive.clone()),
            source,
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(source) = fs::set_permissions(&archive, fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&archive);
                return Err(DomainError::Io {
                    operation: "restrict archived database permissions",
                    path: Some(archive),
                    source,
                });
            }
        }

        Ok(archive)
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

fn decrypted_verifier(
    db: &Database,
    key: &[u8; VAULT_KEY_LEN],
) -> Result<Zeroizing<Vec<u8>>, DomainError> {
    let record = db
        .read_secret(SECRET_VAULT_VERIFIER)?
        .ok_or_else(|| DomainError::Corrupted {
            detail: "vault verifier record is missing".to_owned(),
        })?;
    crypto::decrypt(
        key,
        SECRET_VAULT_VERIFIER.as_bytes(),
        &record.nonce,
        &record.ciphertext,
    )
}

fn verify_key_file(db: &Database, key: &[u8; VAULT_KEY_LEN]) -> Result<(), DomainError> {
    let plaintext = match decrypted_verifier(db, key) {
        Ok(plaintext) => plaintext,
        Err(DomainError::Crypto { detail, .. }) => {
            log::warn!("key file failed to decrypt the vault verifier: {detail}");
            return Err(DomainError::VaultKeyMismatch);
        }
        Err(other) => return Err(other),
    };
    if plaintext.as_slice() != VAULT_VERIFIER_PLAINTEXT {
        log::warn!("key file decrypted the verifier to unexpected plaintext");
        return Err(DomainError::VaultKeyMismatch);
    }
    Ok(())
}

fn generate_recovery_code() -> String {
    const CODE_BYTES: usize = 20;
    const GROUP_LEN: usize = 5;

    let hex = hex::encode(crypto::random_bytes::<CODE_BYTES>());
    let mut code = String::with_capacity(hex.len() + hex.len() / GROUP_LEN);
    for (index, character) in hex.chars().enumerate() {
        if index > 0 && index % GROUP_LEN == 0 {
            code.push('-');
        }
        code.push(character);
    }
    code
}

fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn recovery_hash(code: &str) -> [u8; 32] {
    let normalized = normalize_recovery_code(code);
    let mut hasher = Sha256::new();
    hasher.update(b"peerdav-recovery-code-v1:");
    hasher.update(normalized.as_bytes());
    hasher.finalize().into()
}

fn recovery_hash_hex(code: &str) -> String {
    hex::encode(recovery_hash(code))
}

fn verify_recovery_code(db: &Database, code: &str) -> Result<(), DomainError> {
    let stored = db
        .read_meta(META_RECOVERY_HASH)?
        .ok_or(DomainError::RecoveryCodeNotConfigured)?;
    let stored = hex::decode(stored).map_err(|error| DomainError::Corrupted {
        detail: format!("recovery-code fingerprint is not valid hex: {error}"),
    })?;
    let normalized = normalize_recovery_code(code);
    if stored.len() != 32 || normalized.is_empty() || normalized.len() > 256 {
        return Err(DomainError::InvalidRecoveryCode);
    }
    let actual = recovery_hash(code);
    if !bool::from(stored.as_slice().ct_eq(actual.as_slice())) {
        return Err(DomainError::InvalidRecoveryCode);
    }
    Ok(())
}

fn key_error_reason(error: &DomainError) -> &'static str {
    match error {
        DomainError::KeyFileMissing { .. } => "key_file_missing",
        DomainError::KeyFileInvalid { .. } => "invalid_key_file",
        DomainError::VaultKeyMismatch => "wrong_key_file",
        DomainError::Io { .. } => "key_file_unreadable",
        DomainError::Corrupted { .. } => "decryption_failed",
        _ => "key_file_unavailable",
    }
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
