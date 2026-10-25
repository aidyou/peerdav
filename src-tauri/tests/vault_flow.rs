//! Focused U-1 integration tests: first-run bootstrap, encrypted storage,
//! lock/unlock, admin credential rotation, and encryption-password rotation.

use std::path::{Path, PathBuf};

use peerdav_lib::{AppError, DomainError, Vault, VaultStatus};
use rusqlite::params;

mod common;

use common::{DevTestDir, dev_test_dir};

const MASTER_PASSWORD: &str = "correct horse battery 2026";
const NEW_MASTER_PASSWORD: &str = "new correct horse 2026";
const ADMIN_USERNAME: &str = "peerdav-admin";
const ADMIN_PASSWORD: &str = "admin password 2026";
const NEW_ADMIN_PASSWORD: &str = "new admin password 2026";
const SECRET_NAME: &str = "service.token";
const SECRET_VALUE: &[u8] = b"8c3f0a11-4b27-4ad3-bfc5-1b6e9b0a23ea";

fn open_vault() -> (DevTestDir, PathBuf, Vault) {
    let directory = dev_test_dir("vault_flow");
    let path = directory.path().join("peerdav.sqlite3");
    let vault = Vault::open(&path).expect("open vault");
    (directory, path, vault)
}

fn initialized_vault() -> (DevTestDir, PathBuf, Vault, String, String) {
    let (directory, path, vault) = open_vault();
    let result = vault
        .initialize(MASTER_PASSWORD, ADMIN_USERNAME, ADMIN_PASSWORD)
        .expect("initialize vault");
    (
        directory,
        path,
        vault,
        result.admin_username,
        ADMIN_PASSWORD.to_owned(),
    )
}

#[test]
fn first_run_creates_encrypted_vault() {
    let (directory, path, vault) = open_vault();

    assert_eq!(
        vault.status().expect("status"),
        VaultStatus {
            initialized: false,
            unlocked: false,
        }
    );

    let result = vault
        .initialize(MASTER_PASSWORD, "owner", ADMIN_PASSWORD)
        .expect("initialize");
    assert_eq!(result.admin_username, "owner");
    assert_eq!(
        result.vault_status,
        VaultStatus {
            initialized: true,
            unlocked: true,
        }
    );
    assert_eq!(vault.status().expect("status"), result.vault_status);

    assert!(matches!(
        vault.initialize(MASTER_PASSWORD, "owner", ADMIN_PASSWORD),
        Err(DomainError::VaultAlreadyInitialized)
    ));

    // The default SQLite file stays inside the app data directory and the
    // vault metadata is non-secret.
    let connection = rusqlite::Connection::open(&path).expect("open sqlite");
    let algorithm: String = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'kdf_algorithm'",
            [],
            |row| row.get(0),
        )
        .expect("algorithm");
    assert_eq!(algorithm, "argon2id-v1");
    let salt: String = connection
        .query_row("SELECT value FROM meta WHERE key = 'kdf_salt'", [], |row| {
            row.get(0)
        })
        .expect("salt");
    assert!(!salt.is_empty());

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

    // No credential or encryption password may exist in plaintext on disk.
    let database_bytes = std::fs::read(&path).expect("read database");
    assert!(!contains(&database_bytes, MASTER_PASSWORD.as_bytes()));
    assert!(!contains(&database_bytes, ADMIN_PASSWORD.as_bytes()));
    assert!(!contains(&database_bytes, result.admin_username.as_bytes()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    drop(directory);
}

#[test]
fn lock_and_unlock_lifecycle() {
    let (_directory, path, vault, username, admin_password) = initialized_vault();

    let login = vault
        .admin_login(&username, &admin_password)
        .expect("first login");
    assert!(!login.must_change_password);

    let locked = vault.lock().expect("lock");
    assert!(!locked.unlocked);
    assert!(matches!(
        vault.admin_login(&username, &admin_password),
        Err(DomainError::VaultLocked)
    ));
    assert!(matches!(
        vault.store_secret(SECRET_NAME, SECRET_VALUE),
        Err(DomainError::VaultLocked)
    ));

    assert!(matches!(
        vault.unlock("definitely the wrong master password"),
        Err(DomainError::InvalidMasterPassword { .. })
    ));
    assert!(!vault.status().expect("status").unlocked);

    let unlocked = vault.unlock(MASTER_PASSWORD).expect("unlock");
    assert!(unlocked.initialized);
    assert!(unlocked.unlocked);
    assert!(
        !vault
            .admin_login(&username, &admin_password)
            .expect("login after unlock")
            .must_change_password
    );

    // A fresh process handle starts locked and can unlock with the same
    // encryption password.
    let restarted = Vault::open(&path).expect("reopen");
    assert_eq!(
        restarted.status().expect("status"),
        VaultStatus {
            initialized: true,
            unlocked: false,
        }
    );
    restarted.unlock(MASTER_PASSWORD).expect("unlock reopened");
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
    let login = vault
        .admin_login(&username, NEW_ADMIN_PASSWORD)
        .expect("login after change");
    assert!(!login.must_change_password);

    vault.lock().expect("lock");
    vault.unlock(MASTER_PASSWORD).expect("unlock");
    assert!(
        !vault
            .admin_login(&username, NEW_ADMIN_PASSWORD)
            .expect("login after restart")
            .must_change_password
    );
}

#[test]
fn master_password_change_reencrypts_all_secrets() {
    let (_directory, path, vault, username, admin_password) = initialized_vault();

    vault
        .store_secret(SECRET_NAME, SECRET_VALUE)
        .expect("store secret");
    assert_eq!(
        vault.load_secret(SECRET_NAME).expect("load").as_slice(),
        SECRET_VALUE
    );

    let salt_before = read_meta(&path, "kdf_salt");

    assert!(matches!(
        vault.change_master_password("wrong current password", NEW_MASTER_PASSWORD),
        Err(DomainError::InvalidMasterPassword { .. })
    ));
    assert!(matches!(
        vault.change_master_password(MASTER_PASSWORD, "short"),
        Err(DomainError::WeakPassword { .. })
    ));
    assert!(matches!(
        vault.change_master_password(MASTER_PASSWORD, MASTER_PASSWORD),
        Err(DomainError::PasswordUnchanged)
    ));

    let status = vault
        .change_master_password(MASTER_PASSWORD, NEW_MASTER_PASSWORD)
        .expect("change master password");
    assert!(status.unlocked);
    assert_eq!(
        vault
            .load_secret(SECRET_NAME)
            .expect("load after rekey")
            .as_slice(),
        SECRET_VALUE
    );
    let salt_after = read_meta(&path, "kdf_salt");
    assert_ne!(salt_before, salt_after);

    // The admin record was re-encrypted too, so the user-chosen password still
    // authenticates without a forced change.
    assert!(
        !vault
            .admin_login(&username, &admin_password)
            .expect("admin login after rekey")
            .must_change_password
    );

    vault.lock().expect("lock");
    assert!(matches!(
        vault.unlock(MASTER_PASSWORD),
        Err(DomainError::InvalidMasterPassword { .. })
    ));
    vault
        .unlock(NEW_MASTER_PASSWORD)
        .expect("unlock new password");
    assert_eq!(
        vault
            .load_secret(SECRET_NAME)
            .expect("load after unlock")
            .as_slice(),
        SECRET_VALUE
    );

    let database_bytes = std::fs::read(&path).expect("read database");
    assert!(!contains(&database_bytes, SECRET_VALUE));
    assert!(!contains(&database_bytes, NEW_MASTER_PASSWORD.as_bytes()));
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
        let connection = rusqlite::Connection::open(&path).expect("open sqlite");
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

    let error = AppError::from(DomainError::WeakPassword {
        reason: "too_short",
    });
    assert_eq!(error.code, "VALIDATION_WEAK_PASSWORD");
    assert!(error.message.contains("至少为 6"));
    assert!(error.message.contains("at least 6"));
}

fn read_meta(path: &Path, key: &str) -> String {
    let connection = rusqlite::Connection::open(path).expect("open sqlite");
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .expect("read meta")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
