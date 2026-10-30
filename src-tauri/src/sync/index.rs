use std::path::Path;

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::Sha256;
use zeroize::Zeroizing;

pub use super::index_types::{BaselineRecord, IndexError, SyncIndex};
use super::index_types::{INDEX_MAX_RECORD, INDEX_SCHEMA};

fn frame(fields: &[&[u8]]) -> Vec<u8> {
    let mut out = b"peerdav-local-sync-index\0v1\0".to_vec();
    for field in fields {
        out.extend_from_slice(&(field.len() as u64).to_be_bytes());
        out.extend_from_slice(field);
    }
    out
}

impl SyncIndex {
    /// The random local index key must be loaded from a vault secret, never from
    /// a device fingerprint or from the repository password.
    pub fn open(
        path: &Path,
        key: Zeroizing<[u8; 32]>,
        repo: String,
        mapping: String,
    ) -> Result<Self, IndexError> {
        if repo.is_empty() || mapping.is_empty() {
            return Err(IndexError::InvalidRecord);
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let handle = options.open(path)?;
        if !handle.metadata()?.is_file() {
            return Err(IndexError::InvalidRecord);
        }
        let connection = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != 0 && version != INDEX_SCHEMA {
            return Err(IndexError::UnsupportedSchema);
        }
        connection.execute_batch("CREATE TABLE IF NOT EXISTS records (scope BLOB NOT NULL, token BLOB NOT NULL, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL, PRIMARY KEY(scope,token));")?;
        connection.pragma_update(None, "user_version", INDEX_SCHEMA)?;
        Ok(Self {
            connection,
            key,
            repo,
            mapping,
        })
    }

    fn token(&self, category: &str, name: &str) -> Result<[u8; 32], IndexError> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&*self.key)
            .map_err(|_| IndexError::Authentication)?;
        mac.update(&frame(&[
            b"row-token",
            self.repo.as_bytes(),
            self.mapping.as_bytes(),
            category.as_bytes(),
            name.as_bytes(),
        ]));
        Ok(mac.finalize().into_bytes().into())
    }

    fn scope(&self) -> Result<[u8; 32], IndexError> {
        self.token("scope", "mapping")
    }

    pub fn save<T: Serialize>(
        &mut self,
        category: &str,
        name: &str,
        value: &T,
    ) -> Result<(), IndexError> {
        self.write_record(category, name, value)
    }

    fn write_record<T: Serialize>(
        &self,
        category: &str,
        name: &str,
        value: &T,
    ) -> Result<(), IndexError> {
        if category.is_empty() || name.is_empty() {
            return Err(IndexError::InvalidRecord);
        }
        let plain = Zeroizing::new(serde_json::to_vec(value)?);
        if plain.len() > INDEX_MAX_RECORD {
            return Err(IndexError::InvalidRecord);
        }
        let token = self.token(category, name)?;
        let scope = self.scope()?;
        let aad = frame(&[b"records", &scope, &token]);
        let mut nonce = [0_u8; 24];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(IndexError::Random)?;
        let cipher = XChaCha20Poly1305::new_from_slice(&*self.key)
            .map_err(|_| IndexError::Authentication)?;
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .map_err(|_| IndexError::Authentication)?;
        self.connection.execute("INSERT INTO records(scope,token,nonce,ciphertext) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,token) DO UPDATE SET nonce=excluded.nonce,ciphertext=excluded.ciphertext", params![scope.as_slice(),token.as_slice(),nonce.as_slice(),ciphertext])?;
        Ok(())
    }

    pub fn load<T: DeserializeOwned>(
        &self,
        category: &str,
        name: &str,
    ) -> Result<Option<T>, IndexError> {
        let scope = self.scope()?;
        let token = self.token(category, name)?;
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT nonce,ciphertext FROM records WHERE scope=?1 AND token=?2",
                params![scope.as_slice(), token.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((nonce, ciphertext)) = row else {
            return Ok(None);
        };
        let nonce: [u8; 24] = nonce
            .as_slice()
            .try_into()
            .map_err(|_| IndexError::Authentication)?;
        if ciphertext.len() < 16 || ciphertext.len() > INDEX_MAX_RECORD + 16 {
            return Err(IndexError::InvalidRecord);
        }
        let cipher = XChaCha20Poly1305::new_from_slice(&*self.key)
            .map_err(|_| IndexError::Authentication)?;
        let aad = frame(&[b"records", &scope, &token]);
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| IndexError::Authentication)?,
        );
        Ok(Some(serde_json::from_slice(&plain)?))
    }

    /// Atomically keep a discoverable encrypted catalog beside each apply
    /// intent. A crash never leaves a destructive operation known only in RAM.
    pub fn record_apply<T: Serialize>(
        &mut self,
        operation_id: &str,
        intent: &T,
    ) -> Result<(), IndexError> {
        let mut operations: Vec<String> = self.load("catalog", "apply")?.unwrap_or_default();
        if !operations.iter().any(|id| id == operation_id) {
            operations.push(operation_id.to_owned());
        }
        if operations.len() > 100_000 {
            return Err(IndexError::InvalidRecord);
        }
        operations.sort();
        let tx = self.connection.unchecked_transaction()?;
        self.write_record("apply", operation_id, intent)?;
        self.write_record("catalog", "apply", &operations)?;
        tx.commit()?;
        Ok(())
    }

    pub fn apply_records<T: DeserializeOwned>(&self) -> Result<Vec<(String, T)>, IndexError> {
        let operations: Vec<String> = self.load("catalog", "apply")?.unwrap_or_default();
        let mut records = Vec::new();
        for operation in operations {
            let intent = self
                .load("apply", &operation)?
                .ok_or(IndexError::InvalidRecord)?;
            records.push((operation, intent));
        }
        Ok(records)
    }

    /// The apply intent and its baseline are confirmed in one SQLite commit.
    /// Recovery records remain discoverable; confirmation never authorizes GC.
    pub fn confirm_apply<T: Serialize>(
        &mut self,
        operation_id: &str,
        intent: &T,
        relative_path: &str,
        baseline: &BaselineRecord,
    ) -> Result<(), IndexError> {
        if matches!(baseline, BaselineRecord::Unknown) {
            return Err(IndexError::InvalidRecord);
        }
        let tx = self.connection.unchecked_transaction()?;
        self.write_record("apply", operation_id, intent)?;
        self.write_record("baseline", relative_path, baseline)?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically confirm the engine coordinator state after every target was verified.
    pub fn confirm_engine<T: Serialize, U: Serialize>(
        &mut self,
        operation_id: &str,
        journal: &T,
        snapshot: &U,
        baseline_entries: &[(String, BaselineRecord)],
    ) -> Result<(), IndexError> {
        let tx = self.connection.unchecked_transaction()?;
        self.write_record("run", operation_id, journal)?;
        self.write_record("engine", "materialized", snapshot)?;
        self.write_record("engine", "pending", &Option::<()>::None)?;
        self.write_record("engine", "active_operation", &Option::<String>::None)?;
        self.write_record("engine", "active_intent", &Option::<()>::None)?;
        let completed: Vec<String> = self.load("catalog", "apply")?.unwrap_or_default();
        self.write_record("engine", "completed_apply_operations", &completed)?;
        for (path, baseline) in baseline_entries {
            if matches!(baseline, BaselineRecord::Unknown) {
                return Err(IndexError::InvalidRecord);
            }
            self.write_record("baseline", path, baseline)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn baseline(&self, relative_path: &str) -> Result<BaselineRecord, IndexError> {
        Ok(self
            .load("baseline", relative_path)?
            .unwrap_or(BaselineRecord::Unknown))
    }

    /// Only the apply/publisher transaction coordinator may call this after
    /// materialization and any necessary local version publication are durable.
    pub fn confirm_materialized(
        &mut self,
        relative_path: &str,
        baseline: &BaselineRecord,
    ) -> Result<(), IndexError> {
        if matches!(baseline, BaselineRecord::Unknown) {
            return Err(IndexError::InvalidRecord);
        }
        self.save("baseline", relative_path, baseline)
    }
}
