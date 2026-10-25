//! SQLite persistence. Only non-secret metadata and AEAD ciphertext are
//! stored; the vault key never touches this layer.

use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::DomainError;
use crate::types::{
    AuthorizationRecord, META_SCHEMA_VERSION, PeerRecord, RuleKind, RuleMatcher, SecretRecord,
    SharePermissions, ShareRecord, ShareRuleRecord,
};

/// Thin SQLite wrapper used by the vault.
pub(crate) struct Database {
    conn: Connection,
}

impl Database {
    /// Opens (or creates) the database, restricts file permissions on Unix,
    /// and ensures the schema exists.
    pub(crate) fn open(path: &Path) -> Result<Self, DomainError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| DomainError::Io {
                operation: "create database directory",
                path: Some(parent.to_path_buf()),
                source,
            })?;
        }

        let conn = Connection::open(path).map_err(|source| DomainError::DatabaseOpen {
            path: path.to_path_buf(),
            source,
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
                DomainError::Io {
                    operation: "restrict database permissions",
                    path: Some(path.to_path_buf()),
                    source,
                }
            })?;
        }

        conn.busy_timeout(Duration::from_secs(5))
            .map_err(database_error("set busy timeout"))?;
        conn.execute_batch("PRAGMA secure_delete = ON; PRAGMA foreign_keys = ON;")
            .map_err(database_error("configure connection"))?;
        Self::initialize_schema(&conn)?;

        Ok(Self { conn })
    }

    fn initialize_schema(conn: &Connection) -> Result<(), DomainError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS secrets (
                 name       TEXT PRIMARY KEY NOT NULL,
                 nonce      BLOB NOT NULL,
                 ciphertext BLOB NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS shares (
                 id         TEXT PRIMARY KEY NOT NULL,
                 name       TEXT NOT NULL,
                 root_path  TEXT NOT NULL,
                 can_read   INTEGER NOT NULL,
                 can_write  INTEGER NOT NULL,
                 can_delete INTEGER NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS share_rules (
                 share_id      TEXT NOT NULL REFERENCES shares(id) ON DELETE CASCADE,
                 kind          TEXT NOT NULL,
                 matcher_type  TEXT NOT NULL,
                 matcher_value TEXT NOT NULL,
                 created_at    INTEGER NOT NULL,
                 PRIMARY KEY (share_id, kind, matcher_type, matcher_value)
             );
             CREATE TABLE IF NOT EXISTS authorizations (
                 id         TEXT PRIMARY KEY NOT NULL,
                 label      TEXT NOT NULL,
                 code_hash  TEXT NOT NULL UNIQUE,
                 created_at INTEGER NOT NULL,
                 revoked_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS peers (
                 id                TEXT PRIMARY KEY NOT NULL,
                 label             TEXT NOT NULL,
                 url               TEXT NOT NULL,
                 created_at        INTEGER NOT NULL,
                 last_connected_at INTEGER
             );",
        )
        .map_err(database_error("initialize schema"))
    }

    pub(crate) fn is_initialized(&self) -> Result<bool, DomainError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key = ?1)",
                params![META_SCHEMA_VERSION],
                |row| row.get(0),
            )
            .map_err(database_error("check initialization"))
    }

    pub(crate) fn read_meta(&self, key: &str) -> Result<Option<String>, DomainError> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error("read metadata"))
    }

    pub(crate) fn write_meta(&self, key: &str, value: &str) -> Result<(), DomainError> {
        self.conn
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(database_error("write metadata"))?;
        Ok(())
    }

    pub(crate) fn read_secret(&self, name: &str) -> Result<Option<SecretRecord>, DomainError> {
        self.conn
            .query_row(
                "SELECT nonce, ciphertext FROM secrets WHERE name = ?1",
                params![name],
                |row| {
                    Ok(SecretRecord {
                        nonce: row.get(0)?,
                        ciphertext: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(database_error("read secret"))
    }

    pub(crate) fn put_secret(
        &self,
        name: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<(), DomainError> {
        self.conn
            .execute(
                "INSERT INTO secrets (name, nonce, ciphertext, updated_at)
                 VALUES (?1, ?2, ?3, CAST(strftime('%s', 'now') AS INTEGER))
                 ON CONFLICT(name) DO UPDATE SET
                     nonce = excluded.nonce,
                     ciphertext = excluded.ciphertext,
                     updated_at = excluded.updated_at",
                params![name, nonce, ciphertext],
            )
            .map(|_| ())
            .map_err(database_error("write secret"))
    }

    /// Creates the initial vault records atomically. Re-checks initialization
    /// inside the transaction so two concurrent first-run calls cannot both win.
    pub(crate) fn initialize_with(
        &mut self,
        meta: &[(&str, String)],
        secrets: &[(&str, Vec<u8>, Vec<u8>)],
    ) -> Result<(), DomainError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(database_error("begin initialization transaction"))?;

        let already_initialized: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key = ?1)",
                params![META_SCHEMA_VERSION],
                |row| row.get(0),
            )
            .map_err(database_error("check initialization in transaction"))?;
        if already_initialized {
            return Err(DomainError::VaultAlreadyInitialized);
        }

        for (key, value) in meta {
            transaction
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    params![*key, value],
                )
                .map_err(database_error("write initial metadata"))?;
        }

        for (name, nonce, ciphertext) in secrets {
            transaction
                .execute(
                    "INSERT INTO secrets (name, nonce, ciphertext, updated_at)
                     VALUES (?1, ?2, ?3, CAST(strftime('%s', 'now') AS INTEGER))",
                    params![*name, nonce, ciphertext],
                )
                .map_err(database_error("write initial secret"))?;
        }

        transaction
            .commit()
            .map_err(database_error("commit initialization"))
    }

    /// Replaces only the encrypted secret catalog and updates vault metadata.
    /// Shares, share rules, authorizations, peers, and server settings are
    /// intentionally left untouched for a key-loss reset.
    pub(crate) fn reset_sensitive(
        &mut self,
        meta_updates: &[(&str, String)],
        secrets: &[(&str, Vec<u8>, Vec<u8>)],
    ) -> Result<(), DomainError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(database_error("begin vault reset transaction"))?;

        transaction
            .execute("DELETE FROM secrets", [])
            .map_err(database_error("clear encrypted secrets"))?;

        for (key, value) in meta_updates {
            transaction
                .execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![*key, value],
                )
                .map_err(database_error("update reset metadata"))?;
        }

        for (name, nonce, ciphertext) in secrets {
            transaction
                .execute(
                    "INSERT INTO secrets (name, nonce, ciphertext, updated_at)
                     VALUES (?1, ?2, ?3, CAST(strftime('%s', 'now') AS INTEGER))",
                    params![*name, nonce, ciphertext],
                )
                .map_err(database_error("write reset secret"))?;
        }

        transaction
            .commit()
            .map_err(database_error("commit vault reset"))
    }

    pub(crate) fn insert_share(
        &mut self,
        id: &str,
        name: &str,
        root_path: &str,
        permissions: SharePermissions,
        rules: &[ShareRuleRecord],
    ) -> Result<(), DomainError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(database_error("begin share transaction"))?;

        transaction
            .execute(
                "INSERT INTO shares
                     (id, name, root_path, can_read, can_write, can_delete, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    name,
                    root_path,
                    permissions.can_read,
                    permissions.can_write,
                    permissions.can_delete,
                    now_unix(),
                ],
            )
            .map_err(database_error("insert share"))?;

        for rule in rules {
            let (matcher_type, matcher_value) = matcher_columns(&rule.matcher);
            transaction
                .execute(
                    "INSERT INTO share_rules
                         (share_id, kind, matcher_type, matcher_value, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id,
                        rule_kind_text(rule.kind),
                        matcher_type,
                        matcher_value,
                        now_unix(),
                    ],
                )
                .map_err(database_error("insert share rule"))?;
        }

        transaction.commit().map_err(database_error("commit share"))
    }

    pub(crate) fn update_share_permissions(
        &self,
        id: &str,
        permissions: SharePermissions,
    ) -> Result<(), DomainError> {
        let changed = self
            .conn
            .execute(
                "UPDATE shares SET can_read = ?1, can_write = ?2, can_delete = ?3 WHERE id = ?4",
                params![
                    permissions.can_read,
                    permissions.can_write,
                    permissions.can_delete,
                    id,
                ],
            )
            .map_err(database_error("update share permissions"))?;
        if changed == 0 {
            return Err(DomainError::ShareNotFound { id: id.to_owned() });
        }
        Ok(())
    }

    pub(crate) fn delete_share(&self, id: &str) -> Result<(), DomainError> {
        let changed = self
            .conn
            .execute("DELETE FROM shares WHERE id = ?1", params![id])
            .map_err(database_error("delete share"))?;
        if changed == 0 {
            return Err(DomainError::ShareNotFound { id: id.to_owned() });
        }
        Ok(())
    }

    pub(crate) fn replace_share_rules(
        &mut self,
        share_id: &str,
        rules: &[ShareRuleRecord],
    ) -> Result<(), DomainError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(database_error("begin share rule transaction"))?;

        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM shares WHERE id = ?1)",
                params![share_id],
                |row| row.get(0),
            )
            .map_err(database_error("check share existence"))?;
        if !exists {
            return Err(DomainError::ShareNotFound {
                id: share_id.to_owned(),
            });
        }

        transaction
            .execute(
                "DELETE FROM share_rules WHERE share_id = ?1",
                params![share_id],
            )
            .map_err(database_error("clear share rules"))?;

        for rule in rules {
            let (matcher_type, matcher_value) = matcher_columns(&rule.matcher);
            transaction
                .execute(
                    "INSERT INTO share_rules
                         (share_id, kind, matcher_type, matcher_value, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        share_id,
                        rule_kind_text(rule.kind),
                        matcher_type,
                        matcher_value,
                        now_unix(),
                    ],
                )
                .map_err(database_error("insert share rule"))?;
        }

        transaction
            .commit()
            .map_err(database_error("commit share rules"))
    }

    pub(crate) fn list_shares(&self) -> Result<Vec<ShareRecord>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, name, root_path, can_read, can_write, can_delete
                 FROM shares ORDER BY created_at, id",
            )
            .map_err(database_error("prepare share scan"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(ShareRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    root_path: row.get(2)?,
                    permissions: SharePermissions {
                        can_read: row.get(3)?,
                        can_write: row.get(4)?,
                        can_delete: row.get(5)?,
                    },
                    rules: Vec::new(),
                })
            })
            .map_err(database_error("scan shares"))?;

        let mut shares = Vec::new();
        for row in rows {
            let mut share = row.map_err(database_error("read share row"))?;
            share.rules = self.list_share_rules(&share.id)?;
            shares.push(share);
        }
        Ok(shares)
    }

    fn list_share_rules(&self, share_id: &str) -> Result<Vec<ShareRuleRecord>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT kind, matcher_type, matcher_value
                 FROM share_rules WHERE share_id = ?1 ORDER BY kind, matcher_type, matcher_value",
            )
            .map_err(database_error("prepare share rule scan"))?;
        let rows = statement
            .query_map(params![share_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(database_error("scan share rules"))?;

        let mut rules = Vec::new();
        for row in rows {
            let (kind, matcher_type, matcher_value) =
                row.map_err(database_error("read share rule row"))?;
            let kind = match kind.as_str() {
                "allow" => RuleKind::Allow,
                "deny" => RuleKind::Deny,
                other => {
                    return Err(DomainError::Corrupted {
                        detail: format!("unknown share rule kind `{other}`"),
                    });
                }
            };
            let matcher = match matcher_type.as_str() {
                "ip" => RuleMatcher::Ip(matcher_value),
                "network" => RuleMatcher::Network(matcher_value),
                "mac" => RuleMatcher::Mac(matcher_value),
                other => {
                    return Err(DomainError::Corrupted {
                        detail: format!("unknown share rule matcher `{other}`"),
                    });
                }
            };
            rules.push(ShareRuleRecord { kind, matcher });
        }
        Ok(rules)
    }

    /// Removes a secret record. Missing records are not an error so peer
    /// cleanup stays idempotent.
    pub(crate) fn delete_secret(&self, name: &str) -> Result<(), DomainError> {
        self.conn
            .execute("DELETE FROM secrets WHERE name = ?1", params![name])
            .map(|_| ())
            .map_err(database_error("delete secret"))
    }

    /// Creates or updates peer metadata. The authorization code is stored
    /// separately in the encrypted vault.
    pub(crate) fn upsert_peer(&self, peer: &PeerRecord) -> Result<(), DomainError> {
        self.conn
            .execute(
                "INSERT INTO peers (id, label, url, created_at, last_connected_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                     label = excluded.label,
                     url = excluded.url,
                     last_connected_at = excluded.last_connected_at",
                params![
                    peer.id,
                    peer.label,
                    peer.url,
                    peer.created_at,
                    peer.last_connected_at,
                ],
            )
            .map(|_| ())
            .map_err(database_error("write peer record"))
    }

    pub(crate) fn get_peer(&self, id: &str) -> Result<Option<PeerRecord>, DomainError> {
        self.conn
            .query_row(
                "SELECT id, label, url, created_at, last_connected_at
                 FROM peers WHERE id = ?1",
                params![id],
                peer_from_row,
            )
            .optional()
            .map_err(database_error("read peer record"))
    }

    pub(crate) fn list_peers(&self) -> Result<Vec<PeerRecord>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, label, url, created_at, last_connected_at
                 FROM peers ORDER BY label, id",
            )
            .map_err(database_error("prepare peer scan"))?;
        let rows = statement
            .query_map([], peer_from_row)
            .map_err(database_error("scan peers"))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(database_error("read peer row"))
    }

    pub(crate) fn delete_peer(&self, id: &str) -> Result<(), DomainError> {
        self.conn
            .execute("DELETE FROM peers WHERE id = ?1", params![id])
            .map(|_| ())
            .map_err(database_error("delete peer record"))
    }

    pub(crate) fn insert_authorization(
        &self,
        id: &str,
        label: &str,
        code_hash: &str,
    ) -> Result<i64, DomainError> {
        let created_at = now_unix();
        self.conn
            .execute(
                "INSERT INTO authorizations (id, label, code_hash, created_at, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![id, label, code_hash, created_at],
            )
            .map_err(database_error("insert authorization"))?;
        Ok(created_at)
    }

    pub(crate) fn revoke_authorization(&self, id: &str) -> Result<(), DomainError> {
        let changed = self
            .conn
            .execute(
                "UPDATE authorizations SET revoked_at = ?1
                 WHERE id = ?2 AND revoked_at IS NULL",
                params![now_unix(), id],
            )
            .map_err(database_error("revoke authorization"))?;
        if changed == 0 {
            return Err(DomainError::AuthorizationNotFound { id: id.to_owned() });
        }
        Ok(())
    }

    pub(crate) fn list_active_authorizations(
        &self,
    ) -> Result<Vec<AuthorizationRecord>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, label, code_hash, created_at
                 FROM authorizations WHERE revoked_at IS NULL ORDER BY created_at, id",
            )
            .map_err(database_error("prepare authorization scan"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(AuthorizationRecord {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    code_hash: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(database_error("scan authorizations"))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(database_error("read authorization row"))
    }
}

fn peer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerRecord> {
    Ok(PeerRecord {
        id: row.get(0)?,
        label: row.get(1)?,
        url: row.get(2)?,
        created_at: row.get(3)?,
        last_connected_at: row.get(4)?,
    })
}

fn matcher_columns(matcher: &RuleMatcher) -> (&'static str, &str) {
    match matcher {
        RuleMatcher::Ip(value) => ("ip", value),
        RuleMatcher::Network(value) => ("network", value),
        RuleMatcher::Mac(value) => ("mac", value),
    }
}

fn rule_kind_text(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Allow => "allow",
        RuleKind::Deny => "deny",
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn database_error(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> DomainError {
    move |source| DomainError::Database { operation, source }
}
