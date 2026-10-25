//! Focused U-1 integration tests: key-file bootstrap, encrypted storage,
//! automatic loading, recovery, admin credential rotation, and tamper checks.

use std::fs;
use std::path::{Path, PathBuf};

use peerdav_lib::{AppError, DomainError, Vault, VaultStatus};
use rusqlite::{Connection, OptionalExtension, params};

mod common;

use common::{DevTestDir, dev_test_dir, key_dir};

const ADMIN_USERNAME: &str = "peerdav-admin";
const ADMIN_PASSWORD: &str = "admin password 2026";
const NEW_ADMIN_PASSWORD: &str = "new admin password 2026";
const SECRET_NAME: &str = "service.token";
const SECRET_VALUE: &[u8] = b"8c3f0a11-4b27-4ad3-bfc5-1b6e9b0a23ea";

fn open_vault() -> (DevTestDir, PathBuf, Vault) {
    let directory = dev_test_dir("vault_flow");
    let path = directory.path().join("peerdav.pd");
    let vault = Vault::open(&path).expect("open vault");
    (directory, path, vault)
}

fn initialized_vault() -> (DevTestDir, PathBuf, Vault, String, String) {
    let (directory, path, vault) = open_vault();
    let result = vault
        .initialize(&key_dir(directory.path()), ADMIN_USERNAME, ADMIN_PASSWORD)
        .expect("initialize vault");
    (
        directory,
        path,
        vault,
        result.admin_username,
        ADMIN_PASSWORD.to_owned(),
    )
}

fn status(initialized: bool, unlocked: bool, key_file: Option<String>) -> VaultStatus {
    VaultStatus {
        initialized,
        unlocked,
        key_file,
        key_error: None,
    }
}

#[test]
fn first_run_creates_key_file_and_encrypted_vault() {
    let (directory, path, vault) = open_vault();

    assert_eq!(vault.status().expect("status"), status(false, false, None));

    let key_directory = key_dir(directory.path());
    let result = vault
        .initialize(&key_directory, "owner", ADMIN_PASSWORD)
        .expect("initialize");
    let key_path = Path::new(&key_directory).join("peerdav-vault.key");
    let canonical_key_path = fs::canonicalize(&key_path).expect("canonical key path");
    let key_file = canonical_key_path.display().to_string();

    assert_eq!(result.admin_username, "owner");
    assert_eq!(
        result.vault_status,
        status(true, true, Some(key_file.clone()))
    );
    assert_eq!(vault.status().expect("status"), result.vault_status);
    assert_eq!(result.recovery_code.len(), 47, "20 bytes as grouped hex");
    assert_eq!(
        result
            .recovery_code
            .chars()
            .filter(|character| character.is_ascii_hexdigit())
            .count(),
        40
    );

    assert!(matches!(
        vault.initialize(&key_directory, "owner", ADMIN_PASSWORD),
        Err(DomainError::VaultAlreadyInitialized)
    ));

    assert!(key_path.is_file(), "key file must be created");
    let key_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&key_path).expect("read key file"))
            .expect("parse key file");
    assert_eq!(key_json["type"], "peerdav-vault");
    assert_eq!(key_json["version"], 1);
    assert_eq!(
        key_json["key"].as_str().expect("key hex").len(),
        64,
        "key material is 32 bytes hex encoded"
    );
    assert_eq!(
        key_json["id"].as_str().expect("key id").len(),
        32,
        "fingerprint is the first 16 bytes of SHA-256"
    );

    let connection = Connection::open(&path).expect("open sqlite");
    let schema: String = read_meta_connection(&connection, "schema_version").expect("schema");
    assert_eq!(schema, "2");
    assert_eq!(
        read_meta_connection(&connection, "vault.keyFile"),
        Some(key_file)
    );
    assert_eq!(
        read_meta_connection(&connection, "kdf_salt"),
        None,
        "new vaults do not persist password-derived KDF metadata"
    );
    let recovery_hash =
        read_meta_connection(&connection, "vault.recoveryHash").expect("recovery hash");
    assert_eq!(recovery_hash.len(), 64);
    assert_ne!(recovery_hash, result.recovery_code);

    let mut statement = connection
        .prepare("SELECT nonce, ciphertext FROM secrets")
        .expect("prepare secrets");
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .expect("query secrets");
    let mut record_count = 0;
    for row in rows {
        let (nonce, ciphertext) = row.expect("secret row");
        assert_eq!(nonce.len(), 24);
        assert!(!ciphertext.is_empty());
        record_count += 1;
    }
    assert_eq!(record_count, 2, "verifier and admin records");
    drop(statement);
    drop(connection);

    // No credential or key material may exist in plaintext in the database.
    let database_bytes = fs::read(&path).expect("read database");
    assert!(!contains(&database_bytes, ADMIN_PASSWORD.as_bytes()));
    assert!(!contains(&database_bytes, result.admin_username.as_bytes()));
    assert!(!contains(
        &database_bytes,
        key_json["key"].as_str().expect("key hex").as_bytes()
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&key_path)
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "key file must be owner-only");
    }

    drop(directory);
}

#[test]
fn key_file_is_auto_loaded_and_lock_can_be_recovered_without_password() {
    let (_directory, path, vault, username, admin_password) = initialized_vault();

    let login = vault
        .admin_login(&username, &admin_password)
        .expect("first login");
    assert!(!login.must_change_password);

    vault
        .store_secret(SECRET_NAME, SECRET_VALUE)
        .expect("store secret");

    let locked = vault.lock().expect("lock");
    assert!(!locked.unlocked);
    assert_eq!(locked.key_error, None);
    assert!(locked.key_file.is_some());
    assert!(matches!(
        vault.admin_login(&username, &admin_password),
        Err(DomainError::VaultLocked)
    ));
    assert!(matches!(
        vault.store_secret(SECRET_NAME, SECRET_VALUE),
        Err(DomainError::VaultLocked)
    ));

    let reloaded = vault.reload_key(None).expect("reload configured key");
    assert!(reloaded.unlocked);
    assert_eq!(reloaded.key_error, None);
    assert_eq!(
        vault.load_secret(SECRET_NAME).expect("load").as_slice(),
        SECRET_VALUE
    );
    assert!(
        !vault
            .admin_login(&username, &admin_password)
            .expect("login after reload")
            .must_change_password
    );

    // A fresh process handle loads the persisted key file automatically.
    let restarted = Vault::open(&path).expect("reopen");
    let restarted_status = restarted.status().expect("status");
    assert!(restarted_status.unlocked);
    assert_eq!(restarted_status.key_error, None);
    assert_eq!(
        restarted
            .load_secret(SECRET_NAME)
            .expect("load after restart")
            .as_slice(),
        SECRET_VALUE
    );
    restarted
        .admin_login(&username, &admin_password)
        .expect("admin login after restart");
}

#[test]
fn missing_or_wrong_key_file_requires_explicit_recovery() {
    let (directory, path, _vault, _username, _admin_password) = initialized_vault();
    let configured_key = read_meta(&path, "vault.keyFile").expect("key file meta");
    let backup = directory.path().join("backup.key");
    fs::copy(&configured_key, &backup).expect("copy key file");
    fs::remove_file(&configured_key).expect("remove original key file");

    let restarted = Vault::open(&path).expect("reopen without key file");
    let missing = restarted.status().expect("status");
    assert!(missing.initialized);
    assert!(!missing.unlocked);
    assert_eq!(missing.key_error.as_deref(), Some("key_file_missing"));
    assert_eq!(missing.key_file.as_deref(), Some(configured_key.as_str()));

    let recovered = restarted
        .reload_key(Some(backup.to_str().expect("utf8 backup")))
        .expect("recover from backup");
    assert!(recovered.unlocked);
    assert_eq!(recovered.key_error, None);
    assert_eq!(
        recovered.key_file.as_deref(),
        Some(
            fs::canonicalize(&backup)
                .expect("canonical backup")
                .to_str()
                .expect("utf8 canonical")
        )
    );

    // A valid key file from another vault must not be adopted or persisted.
    let other = dev_test_dir("vault_flow_other");
    let other_key_directory = key_dir(other.path());
    let other_vault = Vault::open(&other.path().join("peerdav.pd")).expect("other vault");
    other_vault
        .initialize(&other_key_directory, "owner", ADMIN_PASSWORD)
        .expect("other init");
    let other_key = Path::new(&other_key_directory).join("peerdav-vault.key");
    let before = restarted.status().expect("status").key_file;
    assert!(matches!(
        restarted.reload_key(Some(other_key.to_str().expect("utf8 other key"))),
        Err(DomainError::VaultKeyMismatch)
    ));
    assert_eq!(restarted.status().expect("status").key_file, before);

    let invalid = directory.path().join("invalid.key");
    fs::write(&invalid, b"{not a key file").expect("write invalid key");
    assert!(matches!(
        restarted.reload_key(Some(invalid.to_str().expect("utf8 invalid"))),
        Err(DomainError::KeyFileInvalid { .. })
    ));

    let absent = directory.path().join("absent.key");
    assert!(matches!(
        restarted.reload_key(Some(absent.to_str().expect("utf8 absent"))),
        Err(DomainError::KeyFileMissing { .. })
    ));

    drop(other);
    drop(directory);
}

#[test]
fn key_loss_reset_requires_recovery_code_and_preserves_non_secret_data() {
    const NEW_ADMIN_USERNAME: &str = "recovered-admin";
    const NEW_ADMIN_PASSWORD: &str = "recovered admin password 2026";

    let (directory, path, vault) = open_vault();
    let result = vault
        .initialize(&key_dir(directory.path()), ADMIN_USERNAME, ADMIN_PASSWORD)
        .expect("initialize");
    let recovery_code = result.recovery_code;
    vault
        .store_secret("peer.credential", b"old-encrypted-value")
        .expect("store old secret");

    {
        let connection = Connection::open(&path).expect("open sqlite");
        connection
            .execute_batch(
                "INSERT INTO shares
                     (id, name, root_path, can_read, can_write, can_delete, created_at)
                 VALUES ('share-1', 'Docs', '/tmp/docs', 1, 1, 1, 0);
                 INSERT INTO share_rules
                     (share_id, kind, matcher_type, matcher_value, created_at)
                 VALUES ('share-1', 'allow', 'ip', '127.0.0.1', 0);
                 INSERT INTO authorizations
                     (id, label, code_hash, created_at, revoked_at)
                 VALUES ('auth-1', 'client',
                         'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                         0, NULL);
                 INSERT INTO peers
                     (id, label, url, created_at, last_connected_at)
                 VALUES ('peer-1', 'peer', 'http://127.0.0.1:4918', 0, NULL);
                 INSERT INTO meta (key, value) VALUES ('server.enabled', '1');",
            )
            .expect("insert non-secret data");
    }
    let old_hash = read_meta(&path, "vault.recoveryHash").expect("old recovery hash");

    let new_keys = directory.path().join("new-keys");
    fs::create_dir_all(&new_keys).expect("new key directory");
    let new_keys = new_keys.to_string_lossy().into_owned();

    assert!(matches!(
        vault.reset_lost_key(
            &recovery_code,
            &new_keys,
            NEW_ADMIN_USERNAME,
            NEW_ADMIN_PASSWORD
        ),
        Err(DomainError::VaultMustBeLocked)
    ));

    vault.lock().expect("lock");
    assert!(matches!(
        vault.reset_lost_key(
            "not-the-recovery-code",
            &new_keys,
            NEW_ADMIN_USERNAME,
            NEW_ADMIN_PASSWORD
        ),
        Err(DomainError::InvalidRecoveryCode)
    ));

    let reset = vault
        .reset_lost_key(
            &recovery_code,
            &new_keys,
            NEW_ADMIN_USERNAME,
            NEW_ADMIN_PASSWORD,
        )
        .expect("reset with recovery code");
    assert!(reset.vault_status.unlocked);
    assert_eq!(reset.admin_username, NEW_ADMIN_USERNAME);
    assert_ne!(reset.recovery_code, recovery_code);
    assert!(Path::new(&reset.archived_database).is_file());
    assert!(reset.archived_database.ends_with(".pd"));

    assert!(
        !vault
            .admin_login(NEW_ADMIN_USERNAME, NEW_ADMIN_PASSWORD)
            .expect("new admin login")
            .must_change_password
    );
    assert!(matches!(
        vault.admin_login(ADMIN_USERNAME, ADMIN_PASSWORD),
        Err(DomainError::InvalidAdminCredentials)
    ));
    assert!(matches!(
        vault.load_secret("peer.credential"),
        Err(DomainError::SecretNotFound { .. })
    ));

    let connection = Connection::open(&path).expect("open reset database");
    assert_eq!(count_rows(&connection, "shares"), 1);
    assert_eq!(count_rows(&connection, "share_rules"), 1);
    assert_eq!(count_rows(&connection, "authorizations"), 1);
    assert_eq!(count_rows(&connection, "peers"), 1);
    assert_eq!(
        read_meta_connection(&connection, "server.enabled").as_deref(),
        Some("1")
    );
    assert_eq!(count_rows(&connection, "secrets"), 2);
    assert_ne!(
        read_meta_connection(&connection, "vault.recoveryHash").expect("new hash"),
        old_hash
    );

    let archived = Connection::open(&reset.archived_database).expect("open archive");
    assert_eq!(count_rows(&archived, "shares"), 1);
    assert_eq!(count_rows(&archived, "authorizations"), 1);
    assert_eq!(count_rows(&archived, "secrets"), 3);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&reset.archived_database)
            .expect("archive metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "archived database must be owner-only");
    }

    vault.lock().expect("lock after reset");
    assert!(matches!(
        vault.reset_lost_key(
            &recovery_code,
            &new_keys,
            NEW_ADMIN_USERNAME,
            NEW_ADMIN_PASSWORD
        ),
        Err(DomainError::InvalidRecoveryCode)
    ));
}

#[test]
fn admin_password_change_and_reverification() {
    let (_directory, _path, vault, username, admin_password) = initialized_vault();

    let login = vault
        .admin_login(&username, &admin_password)
        .expect("first login");
    assert!(!login.must_change_password);

    assert!(matches!(
        vault.change_admin_password(&username, "wrong current password", NEW_ADMIN_PASSWORD),
        Err(DomainError::InvalidAdminCredentials)
    ));
    assert!(matches!(
        vault.change_admin_password(&username, &admin_password, "short"),
        Err(DomainError::WeakPassword { .. })
    ));
    assert!(matches!(
        vault.change_admin_password(&username, &admin_password, &admin_password),
        Err(DomainError::PasswordUnchanged)
    ));

    let changed = vault
        .change_admin_password(&username, &admin_password, NEW_ADMIN_PASSWORD)
        .expect("change admin password");
    assert!(!changed.must_change_password);

    assert!(matches!(
        vault.admin_login(&username, &admin_password),
        Err(DomainError::InvalidAdminCredentials)
    ));
    assert!(
        !vault
            .admin_login(&username, NEW_ADMIN_PASSWORD)
            .expect("login after change")
            .must_change_password
    );

    vault.lock().expect("lock");
    vault.reload_key(None).expect("reload key");
    assert!(
        !vault
            .admin_login(&username, NEW_ADMIN_PASSWORD)
            .expect("login after lock/reload")
            .must_change_password
    );
}

#[test]
fn admin_session_persists_until_lock_or_logout() {
    let (_directory, _path, vault, username, admin_password) = initialized_vault();

    assert!(vault.admin_session().expect("session").is_none());

    vault
        .admin_login(&username, &admin_password)
        .expect("admin login");
    let session = vault
        .admin_session()
        .expect("session")
        .expect("session after login");
    assert_eq!(session.username, username);
    assert!(!session.must_change_password);
    assert!(vault.verify_admin(&username, "").is_ok());

    assert!(matches!(
        vault.verify_admin("someone-else", ""),
        Err(DomainError::InvalidAdminCredentials)
    ));
    assert!(matches!(
        vault.verify_admin(&username, "wrong password"),
        Err(DomainError::InvalidAdminCredentials)
    ));

    vault.admin_logout().expect("logout");
    assert!(vault.admin_session().expect("session").is_none());
    assert!(vault.status().expect("status").unlocked);
    assert!(matches!(
        vault.verify_admin(&username, ""),
        Err(DomainError::InvalidAdminCredentials)
    ));

    vault
        .admin_login(&username, &admin_password)
        .expect("login again");
    assert!(vault.admin_session().expect("session").is_some());
    vault.lock().expect("lock");
    assert!(vault.admin_session().expect("session").is_none());
    vault.reload_key(None).expect("reload key");
    assert!(vault.admin_session().expect("session").is_none());
}

#[test]
fn secret_records_validate_names_and_detect_tampering() {
    let (_directory, path, vault, _username, _admin_password) = initialized_vault();

    vault
        .store_secret(SECRET_NAME, SECRET_VALUE)
        .expect("store secret");
    assert_eq!(
        vault.load_secret(SECRET_NAME).expect("load").as_slice(),
        SECRET_VALUE
    );

    assert!(matches!(
        vault.store_secret("__admin__", b"overwrite attempt"),
        Err(DomainError::ReservedSecretName)
    ));
    assert!(matches!(
        vault.store_secret("", b"empty"),
        Err(DomainError::InvalidSecretName)
    ));
    assert!(matches!(
        vault.load_secret("missing.secret"),
        Err(DomainError::SecretNotFound { .. })
    ));

    {
        let connection = Connection::open(&path).expect("open sqlite");
        connection
            .execute(
                "UPDATE secrets SET ciphertext = ?1 WHERE name = ?2",
                params![vec![0_u8; 32], SECRET_NAME],
            )
            .expect("tamper ciphertext");
    }

    assert!(matches!(
        vault.load_secret(SECRET_NAME),
        Err(DomainError::Corrupted { .. })
    ));
}

#[test]
fn app_error_mapping_is_structured_and_bilingual() {
    let error = AppError::from(DomainError::VaultLocked);
    assert_eq!(error.code, "VAULT_LOCKED");
    assert!(error.message.contains("保险箱已锁定"));
    assert!(error.message.contains("vault is locked"));
    assert!(error.detail.is_some());

    let error = AppError::from(DomainError::KeyFileMissing {
        path: "/tmp/missing.key".to_owned(),
    });
    assert_eq!(error.code, "VAULT_KEY_FILE_MISSING");
    assert!(error.message.contains("密钥文件"));
    assert!(error.message.contains("key file"));
    assert_eq!(error.context.expect("context")["path"], "/tmp/missing.key");

    let error = AppError::from(DomainError::WeakPassword {
        reason: "too_short",
    });
    assert_eq!(error.code, "VALIDATION_WEAK_PASSWORD");
    assert!(error.message.contains("至少为 6"));
    assert!(error.message.contains("at least 6"));
}

fn read_meta(path: &Path, key: &str) -> Option<String> {
    let connection = Connection::open(path).expect("open sqlite");
    read_meta_connection(&connection, key)
}

fn read_meta_connection(connection: &Connection, key: &str) -> Option<String> {
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .expect("read meta")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn count_rows(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count rows")
}
