use super::append_types::{
    DiscoveryBudget, DiscoveryState, ObjectRef, OperationState, PreparedObject, PreparedOperation,
    VerificationContext,
};
use super::index_types::IndexError;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::Path;
use zeroize::Zeroizing;

#[cfg(test)]
mod run_budget_tests {
    use super::*;

    #[test]
    fn metadata_preview_slot_does_not_reuse_exhausted_payload_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("index");
        let mut index = AppendIndex::open(&path, Zeroizing::new([7; 32]), "preview-test".into())?;
        let legacy = index.begin_run("preview", "legacy-full-discovery")?;
        let exhausted = DiscoveryBudget {
            allocation_bytes: super::super::append_types::MAX_DISCOVERY_ALLOCATION,
            ..Default::default()
        };
        index.save_discovery_budget(&legacy, &exhausted)?;
        drop(index);
        let mut index = AppendIndex::open(&path, Zeroizing::new([7; 32]), "preview-test".into())?;
        let metadata = index.begin_run("preview-metadata-v1", "metadata-only")?;
        assert_ne!(metadata, legacy);
        assert_eq!(index.discovery_budget(&metadata)?.allocation_bytes, 0);
        index.save_discovery_budget(&metadata, &exhausted)?;
        assert_eq!(index.begin_run("preview-metadata-v1", "retry")?, metadata);
        assert_eq!(index.discovery_budget(&metadata)?.allocation_bytes, exhausted.allocation_bytes);
        assert_eq!(index.discovery_budget(&legacy)?.allocation_bytes, exhausted.allocation_bytes);
        Ok(())
    }

    #[test]
    fn legacy_budget_migration_retains_work_and_does_not_reset_on_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("index");
        let mut index = AppendIndex::open(&path, Zeroizing::new([7; 32]), "migration".into())?;
        let old = serde_json::json!({
            "requests": 23, "pages": 2, "bytes_read": 123456,
            "raw_entries": 7, "tree_plaintext": 4567,
            "allocation_bytes": super::super::append_types::MAX_DISCOVERY_ALLOCATION
        });
        let tx = Transaction::new_unchecked(&index.connection, TransactionBehavior::Immediate)?;
        index.write_raw_tx(&tx, "discovery-budget", "operation", &serde_json::to_vec(&old)?)?;
        tx.execute("INSERT INTO append_ledger(scope,name,used,reserved) VALUES(?1,?2,?3,?3)",
            params![&index.scope, "discover:9:operation:staging", super::super::append_types::MAX_DISCOVERY_ALLOCATION as i64])?;
        tx.commit()?;
        assert_eq!(index.begin_run("engine", "operation")?, "operation");
        let mut migrated = index.discovery_budget("operation")?;
        assert_eq!(migrated.accounting_version, 2);
        assert_eq!(migrated.allocation_bytes, 0);
        assert_eq!(migrated.legacy_allocation_bytes, super::super::append_types::MAX_DISCOVERY_ALLOCATION);
        assert_eq!(migrated.requests, 23);
        assert_eq!(migrated.bytes_read, 123456);
        assert_eq!(migrated.tree_plaintext, 4567);
        migrated.allocation_bytes = 4096;
        index.save_discovery_budget("operation", &migrated)?;
        drop(index);
        let mut index = AppendIndex::open(&path, Zeroizing::new([7; 32]), "migration".into())?;
        let mut retried = index.discovery_budget("operation")?;
        assert_eq!(index.begin_run("engine", "new-attempt")?, "operation");
        let legacy_ledger: i64 = index.connection.query_row(
            "SELECT used FROM append_ledger WHERE scope=?1 AND name=?2",
            params![&index.scope, "discover:9:operation:staging"], |row| row.get(0))?;
        assert_eq!(legacy_ledger, super::super::append_types::MAX_DISCOVERY_ALLOCATION as i64);
        assert_eq!(retried.allocation_bytes, 4096);
        assert_eq!(retried.legacy_allocation_bytes, migrated.legacy_allocation_bytes);
        assert_eq!(retried.requests, 23);
        retried.allocation_bytes = 4095;
        assert!(index.save_discovery_budget("operation", &retried).is_err());
        Ok(())
    }

    #[test]
    fn unfinished_run_survives_outer_ids_and_completed_run_releases_slot()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("index");
        let mut index = AppendIndex::open(&path, Zeroizing::new([7; 32]), "run-test".into())?;
        assert_eq!(index.begin_run("engine", "first")?, "first");
        let before = DiscoveryBudget {
            requests: 17,
            bytes_read: 65536,
            ..Default::default()
        };
        index.save_discovery_budget("first", &before)?;
        drop(index);
        let mut index = AppendIndex::open(&path, Zeroizing::new([7; 32]), "run-test".into())?;
        assert_eq!(index.begin_run("engine", "automatic-retry")?, "first");
        assert_eq!(index.begin_run("engine", "explicit-continue")?, "first");
        assert_eq!(index.discovery_budget("first")?.requests, 17);
        index.complete_run("engine", "first")?;
        assert_eq!(index.begin_run("engine", "second")?, "second");
        assert_eq!(index.discovery_budget("second")?.requests, 0);
        index.complete_run("engine", "second")?;
        assert_ne!(index.begin_run("engine", "first")?, "first");
        assert_eq!(index.discovery_budget("first")?.bytes_read, 65536);
        Ok(())
    }
}

const MAX_RECORD: usize = 32 * 1024 * 1024;
const MAX_CHUNK: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownRecord {
    pub key: String,
    pub state: DiscoveryState,
    pub fingerprint: Option<String>,
    pub error: Option<String>,
}

pub struct AppendIndex {
    connection: Connection,
    key: Zeroizing<[u8; 32]>,
    scope: Vec<u8>,
    repo_id: Vec<u8>,
}

fn frame(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = b"peerdav-append-index-v1".to_vec();
    for p in parts {
        out.extend_from_slice(&(p.len() as u64).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}
impl AppendIndex {
    pub fn open(path: &Path, key: Zeroizing<[u8; 32]>, repo: String) -> Result<Self, IndexError> {
        if repo.is_empty() {
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
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
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
        connection.execute_batch("CREATE TABLE IF NOT EXISTS append_records (scope BLOB NOT NULL, token BLOB NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL, PRIMARY KEY(scope,token)); CREATE INDEX IF NOT EXISTS append_records_name ON append_records(scope,kind,name); CREATE TABLE IF NOT EXISTS append_meta (scope BLOB PRIMARY KEY, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL); CREATE TABLE IF NOT EXISTS append_ledger (scope BLOB NOT NULL, name TEXT NOT NULL, used INTEGER NOT NULL, reserved INTEGER NOT NULL, PRIMARY KEY(scope,name));")?;
        let mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&*key).map_err(|_| IndexError::Authentication);
        let mut mac = mac?;
        mac.update(&frame(&[b"scope", repo.as_bytes()]));
        let scope = mac.finalize().into_bytes().to_vec();
        let idx = Self {
            connection,
            key,
            scope,
            repo_id: repo.into_bytes(),
        };
        let tx = Transaction::new_unchecked(&idx.connection, TransactionBehavior::Immediate)?;
        // This local index belongs to one key/repository, not multiple namespaces.
        let foreign: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM append_meta WHERE scope<>?1 UNION ALL SELECT 1 FROM append_records WHERE scope<>?1 UNION ALL SELECT 1 FROM append_ledger WHERE scope<>?1)",
            params![&idx.scope],
            |r| r.get(0),
        )?;
        if foreign {
            return Err(IndexError::Authentication);
        }
        let meta: Option<(Vec<u8>, Vec<u8>)> = tx
            .query_row(
                "SELECT nonce,ciphertext FROM append_meta WHERE scope=?1",
                params![&idx.scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((nonce, ciphertext)) = meta {
            if idx.decrypt_raw("meta", "sentinel", &nonce, &ciphertext)?
                != b"append-index-authenticated"
            {
                return Err(IndexError::Authentication);
            }
        } else {
            // Older versions accidentally stored the sentinel in append_records.
            if let Some(legacy) = idx.read_raw("meta", "sentinel")? {
                if legacy != b"append-index-authenticated" {
                    return Err(IndexError::Authentication);
                }
            } else {
                let populated: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM append_records UNION ALL SELECT 1 FROM append_ledger)",
                    [],
                    |r| r.get(0),
                )?;
                if populated {
                    return Err(IndexError::Authentication);
                }
            }
            idx.write_meta(&tx)?;
        }
        tx.commit()?;
        Ok(idx)
    }
    fn token(&self, kind: &str, name: &str) -> Result<Vec<u8>, IndexError> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&*self.key)
            .map_err(|_| IndexError::Authentication)?;
        m.update(&frame(&[&self.scope, kind.as_bytes(), name.as_bytes()]));
        Ok(m.finalize().into_bytes().to_vec())
    }
    fn crypt(
        &self,
        kind: &str,
        name: &str,
        plain: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), IndexError> {
        if plain.len() > MAX_RECORD {
            return Err(IndexError::InvalidRecord);
        }
        let mut n = [0u8; 24];
        rand::rngs::OsRng
            .try_fill_bytes(&mut n)
            .map_err(IndexError::Random)?;
        let t = self.token(kind, name)?;
        let aad = frame(&[
            &self.scope,
            &self.repo_id,
            &t,
            kind.as_bytes(),
            name.as_bytes(),
        ]);
        let c = XChaCha20Poly1305::new_from_slice(&*self.key)
            .map_err(|_| IndexError::Authentication)?
            .encrypt(
                &XNonce::from(n),
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .map_err(|_| IndexError::Authentication)?;
        Ok((n.to_vec(), c))
    }
    fn write_raw_tx(
        &self,
        tx: &Transaction<'_>,
        kind: &str,
        name: &str,
        plain: &[u8],
    ) -> Result<(), IndexError> {
        let (n, c) = self.crypt(kind, name, plain)?;
        let t = self.token(kind, name)?;
        tx.execute("INSERT INTO append_records(scope,token,kind,name,nonce,ciphertext) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(scope,token) DO UPDATE SET nonce=excluded.nonce,ciphertext=excluded.ciphertext",params![&self.scope,t,kind,name,n,c])?;
        Ok(())
    }
    fn read_raw(&self, kind: &str, name: &str) -> Result<Option<Vec<u8>>, IndexError> {
        let t = self.token(kind, name)?;
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT nonce,ciphertext FROM append_records WHERE scope=?1 AND token=?2",
                params![&self.scope, t],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(n, c)| self.decrypt_raw(kind, name, &n, &c))
            .transpose()
    }
    fn decrypt_raw(
        &self,
        kind: &str,
        name: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, IndexError> {
        let n: [u8; 24] = nonce.try_into().map_err(|_| IndexError::Authentication)?;
        let t = self.token(kind, name)?;
        let aad = frame(&[
            &self.scope,
            &self.repo_id,
            &t,
            kind.as_bytes(),
            name.as_bytes(),
        ]);
        XChaCha20Poly1305::new_from_slice(&*self.key)
            .map_err(|_| IndexError::Authentication)?
            .decrypt(
                &XNonce::from(n),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| IndexError::Authentication)
    }
    fn write_meta(&self, tx: &Transaction<'_>) -> Result<(), IndexError> {
        let (nonce, ciphertext) = self.crypt("meta", "sentinel", b"append-index-authenticated")?;
        tx.execute(
            "INSERT INTO append_meta(scope,nonce,ciphertext) VALUES(?1,?2,?3)",
            params![&self.scope, nonce, ciphertext],
        )?;
        Ok(())
    }
    pub fn save(&mut self, record: &KnownRecord) -> Result<(), IndexError> {
        if record.key.is_empty() {
            return Err(IndexError::InvalidRecord);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut incoming = record.clone();
        if let Some(old) = self.load(&record.key)? {
            // Fingerprints are opaque conflict markers, not validation context.
            if let (Some(previous), Some(next)) = (&old.fingerprint, &record.fingerprint) {
                if !previous.is_empty() && previous != next {
                    return Err(IndexError::InvalidRecord);
                }
            }
            if old.state == DiscoveryState::Validated && incoming.state != DiscoveryState::Validated
            {
                tx.commit()?;
                return Ok(());
            }
            if incoming.fingerprint.is_none() {
                incoming.fingerprint = old.fingerprint;
            }
        }
        self.write_raw_tx(&tx, "known", &record.key, &serde_json::to_vec(&incoming)?)?;
        tx.commit()?;
        Ok(())
    }
    pub fn load(&self, key: &str) -> Result<Option<KnownRecord>, IndexError> {
        self.read_raw("known", key)?
            .map(|b| serde_json::from_slice(&b).map_err(IndexError::Serialization))
            .transpose()
    }
    pub fn records(&self) -> Result<Vec<KnownRecord>, IndexError> {
        let mut q = self.connection.prepare(
            "SELECT name FROM append_records WHERE scope=?1 AND kind='known' ORDER BY name",
        )?;
        let names = q.query_map(params![&self.scope], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for n in names {
            out.push(
                self.load(&n?)
                    .and_then(|x| x.ok_or(IndexError::InvalidRecord))?,
            );
        }
        Ok(out)
    }
    /// One record at a time; callers can meter before requesting the next key.
    pub fn next_known_key_len(&self, after: &str) -> Result<Option<usize>, IndexError> {
        let length: Option<i64> = self.connection.query_row(
            "SELECT length(CAST(name AS BLOB)) FROM append_records WHERE scope=?1 AND kind='known' AND name>?2 ORDER BY name LIMIT 1",
            params![&self.scope, after], |r| r.get(0),
        ).optional()?;
        length
            .map(|n| usize::try_from(n).map_err(|_| IndexError::InvalidRecord))
            .transpose()
    }

    pub fn next_known_key(&self, after: &str) -> Result<Option<String>, IndexError> {
        self.connection.query_row(
            "SELECT name FROM append_records WHERE scope=?1 AND kind='known' AND name>?2 ORDER BY name LIMIT 1",
            params![&self.scope, after], |r| r.get(0),
        ).optional().map_err(IndexError::from)
    }

    pub fn repository_id(&self) -> &[u8] {
        &self.repo_id
    }

    /// A validated state without this exact separate context is not proof.
    pub fn verification(&self, key: &str) -> Result<Option<VerificationContext>, IndexError> {
        self.read_raw("verification", key)?
            .map(|b| serde_json::from_slice(&b).map_err(IndexError::Serialization))
            .transpose()
    }

    pub(crate) fn save_verification(
        &mut self,
        key: &str,
        context: &VerificationContext,
    ) -> Result<(), IndexError> {
        if context.repo_id.as_bytes() != self.repo_id {
            return Err(IndexError::InvalidRecord);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if let Some(old) = self.verification(key)? {
            if old.object_digest != context.object_digest {
                return Err(IndexError::InvalidRecord);
            }
        }
        self.write_raw_tx(&tx, "verification", key, &serde_json::to_vec(context)?)?;
        tx.commit()?;
        Ok(())
    }

    /// Unfinished pre-epoch work belongs to a durable operation, not the outer
    /// worker attempt. Completion alone releases the slot; budgets are retained.
    pub(crate) fn begin_run(&mut self, slot: &str, proposed: &str) -> Result<String, IndexError> {
        if proposed.is_empty()
            || proposed.len() > 128
            || !proposed.is_ascii()
            || proposed.contains(':')
        {
            return Err(IndexError::InvalidRecord);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if let Some(bytes) = self.read_raw("active-run", slot)? {
            let id = serde_json::from_slice(&bytes)?;
            tx.commit()?;
            return Ok(id);
        }
        let id = if self.read_raw("completed-run", proposed)?.is_some() {
            hex::encode(rand::random::<[u8; 16]>())
        } else {
            proposed.to_owned()
        };
        self.write_raw_tx(&tx, "active-run", slot, &serde_json::to_vec(&id)?)?;
        tx.commit()?;
        Ok(id)
    }

    fn complete_run_tx(
        &self,
        tx: &Transaction<'_>,
        slot: &str,
        id: &str,
    ) -> Result<(), IndexError> {
        if let Some(bytes) = self.read_raw("active-run", slot)? {
            if serde_json::from_slice::<String>(&bytes)? != id {
                return Err(IndexError::InvalidRecord);
            }
            tx.execute(
                "DELETE FROM append_records WHERE scope=?1 AND token=?2",
                params![&self.scope, self.token("active-run", slot)?],
            )?;
        }
        self.write_raw_tx(tx, "completed-run", id, b"true")
    }

    pub(crate) fn complete_run(&mut self, slot: &str, id: &str) -> Result<(), IndexError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.complete_run_tx(&tx, slot, id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn discovery_budget(&self, operation: &str) -> Result<DiscoveryBudget, IndexError> {
        let mut budget: DiscoveryBudget = self.read_raw("discovery-budget", operation)?
            .map(|b| serde_json::from_slice(&b).map_err(IndexError::Serialization))
            .transpose()?
            .unwrap_or_default();
        if budget.accounting_version == 0 {
            // One-time accounting migration, not a retry reset. Keep all network
            // work and the old allocation total, including frozen-run budgets.
            budget.legacy_allocation_bytes = budget.allocation_bytes;
            budget.allocation_bytes = 0;
            budget.accounting_version = 2;
        }
        if budget.accounting_version != 2 {
            return Err(IndexError::InvalidRecord);
        }
        Ok(budget)
    }

    pub(crate) fn save_discovery_budget(
        &mut self,
        operation: &str,
        budget: &DiscoveryBudget,
    ) -> Result<(), IndexError> {
        if operation.is_empty() || budget.accounting_version != 2 {
            return Err(IndexError::InvalidRecord);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for (metric, amount, limit) in [
            (
                "legacy-allocation-v1",
                budget.legacy_allocation_bytes,
                super::append_types::MAX_DISCOVERY_ALLOCATION,
            ),
            (
                "requests",
                budget.requests,
                super::append_types::MAX_DISCOVERY_REQUESTS,
            ),
            (
                "pages",
                budget.pages,
                super::append_types::MAX_DISCOVERY_PAGES,
            ),
            (
                "raw_entries",
                budget.raw_entries,
                super::append_types::MAX_DISCOVERY_NODES,
            ),
            (
                "read",
                budget.bytes_read,
                super::append_types::MAX_DISCOVERY_READ_BYTES,
            ),
            (
                "metadata-allocation-v2",
                budget.allocation_bytes,
                super::append_types::MAX_DISCOVERY_ALLOCATION,
            ),
            (
                "tree",
                budget.tree_plaintext,
                super::append_types::MAX_TREE_SIZE,
            ),
        ] {
            if amount > limit {
                return Err(IndexError::InvalidRecord);
            }
            let amount = i64::try_from(amount).map_err(|_| IndexError::InvalidRecord)?;
            let name = format!("discover:{}:{operation}:{metric}", operation.len());
            let previous: Option<i64> = tx
                .query_row(
                    "SELECT used FROM append_ledger WHERE scope=?1 AND name=?2",
                    params![&self.scope, &name],
                    |r| r.get(0),
                )
                .optional()?;
            if previous.is_some_and(|previous| previous < 0 || previous > amount) {
                return Err(IndexError::InvalidRecord);
            }
            tx.execute("INSERT INTO append_ledger(scope,name,used,reserved) VALUES(?1,?2,?3,?3) ON CONFLICT(scope,name) DO UPDATE SET used=excluded.used,reserved=excluded.reserved", params![&self.scope, name, amount])?;
        }
        self.write_raw_tx(
            &tx,
            "discovery-budget",
            operation,
            &serde_json::to_vec(budget)?,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn prepare_operation(
        &mut self,
        operation_id: &str,
        parents_frontier: &[String],
        root: ObjectRef,
        objects: &[PreparedObject],
        bytes_digest: &str,
    ) -> Result<(), IndexError> {
        if operation_id.is_empty() || objects.iter().any(|o| o.bytes().len() > MAX_CHUNK) {
            return Err(IndexError::InvalidRecord);
        }
        let incoming = PreparedOperation {
            operation_id: operation_id.into(),
            parents_frontier: parents_frontier.to_vec(),
            root,
            objects: objects.to_vec(),
            bytes_digest: bytes_digest.into(),
            state: OperationState::Prepared,
        };
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if let Some(old) = self.load_operation(operation_id)? {
            if old.parents_frontier != incoming.parents_frontier
                || old.root != incoming.root
                || old.bytes_digest != incoming.bytes_digest
                || old.objects != incoming.objects
            {
                return Err(IndexError::InvalidRecord);
            }
            tx.commit()?;
            return Ok(());
        }
        self.write_raw_tx(
            &tx,
            "operation",
            operation_id,
            &serde_json::to_vec(&incoming)?,
        )?;
        for (i, o) in objects.iter().enumerate() {
            self.write_raw_tx(&tx, "object", &format!("{operation_id}:{i}"), o.bytes())?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn load_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<PreparedOperation>, IndexError> {
        self.read_raw("operation", operation_id)?
            .map(|b| serde_json::from_slice(&b).map_err(IndexError::Serialization))
            .transpose()
    }
    pub fn set_operation_state(
        &mut self,
        id: &str,
        state: OperationState,
    ) -> Result<(), IndexError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let Some(mut op) = self.load_operation(id)? else {
            return Err(IndexError::InvalidRecord);
        };
        if op.state == state {
            tx.commit()?;
            return Ok(());
        }
        if !matches!(
            (&op.state, &state),
            (OperationState::Prepared, OperationState::Publishing)
                | (OperationState::Publishing, OperationState::PublishedUnknown)
                | (OperationState::PublishedUnknown, OperationState::Published)
                | (OperationState::Published, OperationState::ApplyPending)
                | (OperationState::ApplyPending, OperationState::Applied)
        ) {
            return Err(IndexError::InvalidRecord);
        }
        op.state = state;
        self.write_raw_tx(&tx, "operation", id, &serde_json::to_vec(&op)?)?;
        tx.commit()?;
        Ok(())
    }
    /// Freeze metadata and independently encrypted chunks atomically. Never copy the
    /// complete wire closure into a second in-memory vector or JSON record.
    #[cfg(test)]
    pub(crate) fn freeze_publication(
        &mut self,
        frozen: &super::append_types::FrozenOperation,
        objects: &[PreparedObject],
    ) -> Result<(), IndexError> {
        self.freeze_publication_source(frozen, |_, i| {
            objects.get(i).cloned().ok_or(IndexError::InvalidRecord)
        })
    }

    pub(crate) fn freeze_publication_source<F>(
        &mut self,
        frozen: &super::append_types::FrozenOperation,
        mut object_at: F,
    ) -> Result<(), IndexError>
    where
        F: FnMut(&AppendIndex, usize) -> Result<PreparedObject, IndexError>,
    {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if let Some(old) = self.frozen_operation(&frozen.operation_id)? {
            if old.closure_validation_version != frozen.closure_validation_version
                || old.operation_id != frozen.operation_id
                || old.parents_frontier != frozen.parents_frontier
                || old.root != frozen.root
                || old.objects != frozen.objects
            {
                return Err(IndexError::InvalidRecord);
            }
            for i in 0..frozen.objects.len() {
                let object = object_at(self, i)?;
                if self.frozen_bytes(&old, i)? != object.bytes() {
                    return Err(IndexError::InvalidRecord);
                }
            }
            tx.commit()?;
            return Ok(());
        }
        for i in 0..frozen.objects.len() {
            let object = object_at(self, i)?;
            for (chunk, bytes) in object.bytes().chunks(MAX_CHUNK).enumerate() {
                self.write_raw_tx(
                    &tx,
                    "publish-chunk",
                    &format!("{}:{i}:{chunk}", frozen.operation_id),
                    bytes,
                )?;
            }
        }
        self.write_raw_tx(
            &tx,
            "publication",
            &frozen.operation_id,
            &serde_json::to_vec(frozen)?,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn frozen_operation(
        &self,
        id: &str,
    ) -> Result<Option<super::append_types::FrozenOperation>, IndexError> {
        self.read_raw("publication", id)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(IndexError::Serialization))
            .transpose()
    }

    pub(crate) fn frozen_bytes(
        &self,
        op: &super::append_types::FrozenOperation,
        ordinal: usize,
    ) -> Result<Vec<u8>, IndexError> {
        use sha2::Digest;
        let object = op.objects.get(ordinal).ok_or(IndexError::InvalidRecord)?;
        if object.len > 2 * (super::append_types::MAX_METADATA_SIZE + 16) + 4096
            || object.chunks != object.len.div_ceil(MAX_CHUNK)
        {
            return Err(IndexError::InvalidRecord);
        }
        let mut bytes = Vec::with_capacity(object.len);
        for chunk in 0..object.chunks {
            let part = self
                .read_raw(
                    "publish-chunk",
                    &format!("{}:{ordinal}:{chunk}", op.operation_id),
                )?
                .ok_or(IndexError::InvalidRecord)?;
            if part.len() > MAX_CHUNK || bytes.len() + part.len() > object.len {
                return Err(IndexError::InvalidRecord);
            }
            bytes.extend_from_slice(&part);
        }
        if bytes.len() != object.len || hex::encode(sha2::Sha256::digest(&bytes)) != object.digest {
            return Err(IndexError::InvalidRecord);
        }
        Ok(bytes)
    }

    pub(crate) fn save_publication(
        &mut self,
        op: &super::append_types::FrozenOperation,
    ) -> Result<(), IndexError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if op.requests > super::append_types::MAX_PUBLISH_REQUESTS
            || op.bytes_written > super::append_types::MAX_PUBLISH_WRITE
        {
            return Err(IndexError::InvalidRecord);
        }
        if let Some(old) = self.frozen_operation(&op.operation_id)? {
            if old.closure_validation_version != op.closure_validation_version
                || old.objects != op.objects
                || old.parents_frontier != op.parents_frontier
                || old.root != op.root
                || old.next_object > op.next_object
                || old.requests > op.requests
                || old.bytes_written > op.bytes_written
                || (matches!(old.state, OperationState::Applied) && old.state != op.state)
            {
                return Err(IndexError::InvalidRecord);
            }
        } else {
            return Err(IndexError::InvalidRecord);
        }
        for (metric, amount) in [("requests", op.requests), ("write", op.bytes_written)] {
            let name = format!(
                "publish:{}:{}:{metric}",
                op.operation_id.len(),
                op.operation_id
            );
            let previous: Option<i64> = tx
                .query_row(
                    "SELECT used FROM append_ledger WHERE scope=?1 AND name=?2",
                    params![&self.scope, &name],
                    |r| r.get(0),
                )
                .optional()?;
            let amount = i64::try_from(amount).map_err(|_| IndexError::InvalidRecord)?;
            if previous.is_some_and(|previous| previous < 0 || previous > amount) {
                return Err(IndexError::InvalidRecord);
            }
            tx.execute("INSERT INTO append_ledger(scope,name,used,reserved) VALUES(?1,?2,?3,?3) ON CONFLICT(scope,name) DO UPDATE SET used=excluded.used,reserved=excluded.reserved", params![&self.scope, name, amount])?;
        }
        self.write_raw_tx(
            &tx,
            "publication",
            &op.operation_id,
            &serde_json::to_vec(op)?,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Engine metadata and revisions are separate authenticated records. No full
    /// history is serialized into an epoch pointer or duplicated wire vector.
    pub fn active_epoch(
        &self,
    ) -> Result<Option<super::append_engine_types::LocalEpoch>, IndexError> {
        self.engine_record("active", "epoch")
    }

    pub(crate) fn pending_epoch(
        &self,
    ) -> Result<Option<super::append_engine_types::LocalEpoch>, IndexError> {
        self.engine_record("pending", "epoch")
    }

    fn engine_record<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<Option<T>, IndexError> {
        self.read_raw(&format!("engine-{kind}"), name)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(IndexError::Serialization))
            .transpose()
    }

    pub(crate) fn epoch_revisions(
        &self,
        epoch: &super::append_engine_types::LocalEpoch,
    ) -> Result<super::merge_types::RevisionMap, IndexError> {
        if epoch.revision_count > super::merge_types::MAX_REVISIONS {
            return Err(IndexError::InvalidRecord);
        }
        let mut out = super::merge_types::RevisionMap::new();
        for i in 0..epoch.revision_count {
            let revision: super::merge_types::Revision = self
                .engine_record("revision", &format!("{}:{i}", epoch.id))?
                .ok_or(IndexError::InvalidRecord)?;
            if out.insert(revision.revision_id.clone(), revision).is_some() {
                return Err(IndexError::InvalidRecord);
            }
        }
        Ok(out)
    }

    pub(crate) fn epoch_binding(
        &self,
        epoch: &super::append_engine_types::LocalEpoch,
        ordinal: usize,
    ) -> Result<super::append_types::BlockBinding, IndexError> {
        if ordinal >= epoch.binding_count
            || epoch.binding_count > super::append_types::MAX_DISCOVERY_NODES
        {
            return Err(IndexError::InvalidRecord);
        }
        self.engine_record("binding", &format!("{}:{ordinal}", epoch.id))?
            .ok_or(IndexError::InvalidRecord)
    }

    pub(crate) fn stage_epoch(
        &mut self,
        epoch: &super::append_engine_types::LocalEpoch,
        revisions: &super::merge_types::RevisionMap,
        bindings: &[super::append_types::BlockBinding],
    ) -> Result<(), IndexError> {
        if epoch.id.is_empty()
            || epoch.revision_count != revisions.len()
            || epoch.binding_count != bindings.len()
            || self.pending_epoch()?.is_some()
            || self
                .active_epoch()?
                .is_some_and(|active| active.id == epoch.id)
        {
            return Err(IndexError::InvalidRecord);
        }
        let prefix = format!("{}:", epoch.id);
        let reused: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM append_records WHERE scope=?1 AND (kind='engine-apply' OR kind='engine-apply-completed') AND substr(name,1,?2)=?3)",
            params![&self.scope, prefix.len(), prefix], |row| row.get(0),
        )?;
        if reused {
            return Err(IndexError::InvalidRecord);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for (i, revision) in revisions.values().enumerate() {
            self.write_raw_tx(
                &tx,
                "engine-revision",
                &format!("{}:{i}", epoch.id),
                &serde_json::to_vec(revision)?,
            )?;
        }
        for (i, binding) in bindings.iter().enumerate() {
            self.write_raw_tx(
                &tx,
                "engine-binding",
                &format!("{}:{i}", epoch.id),
                &serde_json::to_vec(binding)?,
            )?;
        }
        self.write_raw_tx(&tx, "engine-pending", "epoch", &serde_json::to_vec(epoch)?)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn record_engine_apply(
        &mut self,
        operation: &str,
        intent: &super::apply_types::ApplyIntent,
    ) -> Result<(), IndexError> {
        use sha2::Digest;
        let pending = self.pending_epoch()?.ok_or(IndexError::InvalidRecord)?;
        if pending.id != operation {
            return Err(IndexError::InvalidRecord);
        }
        let suffix = hex::encode(sha2::Sha256::digest(serde_json::to_vec(
            &intent.relative_target,
        )?));
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.write_raw_tx(
            &tx,
            "engine-apply",
            &format!("{operation}:{suffix}"),
            &serde_json::to_vec(intent)?,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Lists durable apply record IDs, including completed records. IDs are
    /// stable and bounded so callers can page without loading the payload.
    pub fn recovery_record_ids(
        &self,
        after: &str,
        limit: usize,
    ) -> Result<Vec<String>, IndexError> {
        if limit == 0 || limit > super::merge_types::MAX_ENTITIES {
            return Err(IndexError::InvalidRecord);
        }
        let mut query = self.connection.prepare("SELECT name FROM append_records WHERE scope=?1 AND (kind='engine-apply' OR kind='engine-apply-completed') AND name>?2 ORDER BY name LIMIT ?3")?;
        let rows = query.query_map(
            params![
                &self.scope,
                after,
                i64::try_from(limit).map_err(|_| IndexError::InvalidRecord)?
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(IndexError::from)
    }

    /// Reads one pending or completed apply record by its stable ID.
    pub fn recovery_record(
        &self,
        id: &str,
    ) -> Result<Option<super::apply_types::ApplyIntent>, IndexError> {
        for kind in ["engine-apply", "engine-apply-completed"] {
            if let Some(bytes) = self.read_raw(kind, id)? {
                return serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(IndexError::Serialization);
            }
        }
        Ok(None)
    }

    /// Marks all records for an acknowledged epoch completed without deleting
    /// their authenticated payloads. This is intentionally never garbage-collected.
    fn complete_engine_apply(
        &self,
        tx: &Transaction<'_>,
        operation: &str,
    ) -> Result<(), IndexError> {
        let prefix = format!("{operation}:");
        let mut query = tx.prepare("SELECT name FROM append_records WHERE scope=?1 AND kind='engine-apply' AND substr(name,1,?2)=?3")?;
        let names = query.query_map(params![&self.scope, prefix.len(), prefix], |row| {
            row.get::<_, String>(0)
        })?;
        let names = names.collect::<Result<Vec<_>, _>>()?;
        for name in names {
            let intent = self
                .read_raw("engine-apply", &name)?
                .ok_or(IndexError::InvalidRecord)?;
            self.write_raw_tx(tx, "engine-apply-completed", &name, &intent)?;
            tx.execute(
                "DELETE FROM append_records WHERE scope=?1 AND kind='engine-apply' AND name=?2",
                params![&self.scope, name],
            )?;
        }
        Ok(())
    }

    pub(crate) fn engine_apply_records(
        &self,
        operation: &str,
    ) -> Result<Vec<(String, super::apply_types::ApplyIntent)>, IndexError> {
        let mut query = self.connection.prepare("SELECT name FROM append_records WHERE scope=?1 AND kind='engine-apply' AND substr(name,1,?2)=?3 ORDER BY name")?;
        let prefix = format!("{operation}:");
        let rows = query.query_map(params![&self.scope, prefix.len(), prefix], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            if out.len() >= super::merge_types::MAX_ENTITIES {
                return Err(IndexError::InvalidRecord);
            }
            let id = row?;
            let intent = self
                .engine_record("apply", &id)?
                .ok_or(IndexError::InvalidRecord)?;
            out.push((id, intent));
        }
        Ok(out)
    }

    /// Only invoked after Strict filesystem verification. Filesystem changes are
    /// recoverable journaled work, NOT part of this SQLite transaction.
    pub(crate) fn acknowledge_epoch(
        &mut self,
        epoch: &super::append_engine_types::LocalEpoch,
    ) -> Result<(), IndexError> {
        let pending = self.pending_epoch()?.ok_or(IndexError::InvalidRecord)?;
        if pending.id != epoch.id
            || pending.sequence != epoch.sequence
            || pending.frontier != epoch.frontier
        {
            return Err(IndexError::InvalidRecord);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if let Some(id) = &epoch.publication {
            let mut publication = self
                .frozen_operation(id)?
                .ok_or(IndexError::InvalidRecord)?;
            if !matches!(
                publication.state,
                OperationState::Published | OperationState::ApplyPending
            ) {
                return Err(IndexError::InvalidRecord);
            }
            publication.state = OperationState::Applied;
            self.write_raw_tx(&tx, "publication", id, &serde_json::to_vec(&publication)?)?;
        }
        self.write_raw_tx(&tx, "engine-active", "epoch", &serde_json::to_vec(epoch)?)?;
        tx.execute(
            "DELETE FROM append_records WHERE scope=?1 AND token=?2",
            params![&self.scope, self.token("engine-pending", "epoch")?],
        )?;
        self.complete_engine_apply(&tx, &epoch.id)?;
        self.complete_run_tx(&tx, "engine", &epoch.id)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn cache_engine_object(
        &mut self,
        run: &str,
        ordinal: usize,
        object: &PreparedObject,
    ) -> Result<(), IndexError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.write_raw_tx(
            &tx,
            "engine-object",
            &format!("{run}:{ordinal}"),
            &serde_json::to_vec(object)?,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn engine_object(
        &self,
        run: &str,
        ordinal: usize,
    ) -> Result<PreparedObject, IndexError> {
        self.engine_record("object", &format!("{run}:{ordinal}"))?
            .ok_or(IndexError::InvalidRecord)
    }

    pub fn reserve(&mut self, name: &str, amount: u64, limit: u64) -> Result<(), IndexError> {
        let amount = i64::try_from(amount).map_err(|_| IndexError::InvalidRecord)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let (used, reserved): (i64, i64) = tx
            .query_row(
                "SELECT used,reserved FROM append_ledger WHERE scope=?1 AND name=?2",
                params![&self.scope, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0));
        if used < 0 || reserved < 0 {
            return Err(IndexError::InvalidRecord);
        }
        let next_used = used.checked_add(amount).ok_or(IndexError::InvalidRecord)?;
        let next_reserved = reserved
            .checked_add(amount)
            .ok_or(IndexError::InvalidRecord)?;
        if u64::try_from(next_used).map_err(|_| IndexError::InvalidRecord)? > limit {
            return Err(IndexError::InvalidRecord);
        }
        tx.execute(
            "INSERT INTO append_ledger(scope,name,used,reserved) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,name) DO UPDATE SET used=excluded.used,reserved=excluded.reserved",
            params![&self.scope, name, next_used, next_reserved],
        )?;
        tx.commit()?;
        Ok(())
    }
}
