use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, OsRng, Payload},
};
use rand::RngCore;
use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use url::Url;
use zeroize::Zeroizing;

use super::object_types::{
    OBJECT_GUARD_SCHEMA, ObjectGuardError, ProtectedIdentity, ProtectedMapping,
    StorageIdentityAssociation, StorageIdentityBinding, StorageIdentityRegistry,
};
fn storage<E: std::error::Error + Send + Sync + 'static>(e: E) -> ObjectGuardError {
    ObjectGuardError::Storage {
        source: Box::new(e),
    }
}

#[derive(Clone)]
pub struct ObjectGuard {
    db: Arc<Mutex<Connection>>,
    key: Arc<Zeroizing<[u8; 32]>>,
    active: Arc<Mutex<BTreeMap<ProtectedIdentity, usize>>>,
    domain: Arc<Mutex<()>>,
    _lock: Arc<File>,
    _path: PathBuf,
}
impl ObjectGuard {
    pub fn open(path: impl AsRef<Path>, key: &[u8]) -> Result<Self, ObjectGuardError> {
        if key.len() != 32 {
            return Err(ObjectGuardError::KeyOrCorruption);
        }
        let path = path.as_ref().to_path_buf();
        if path.exists()
            && std::fs::symlink_metadata(&path)
                .map_err(storage)?
                .file_type()
                .is_symlink()
        {
            return Err(ObjectGuardError::Storage {
                source: "symlink database path".into(),
            });
        }
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        if lock_path.exists()
            && std::fs::symlink_metadata(&lock_path)
                .map_err(storage)?
                .file_type()
                .is_symlink()
        {
            return Err(ObjectGuardError::Storage {
                source: "symlink lock path".into(),
            });
        }
        let mut lock_options = OpenOptions::new();
        lock_options.write(true).create(true);
        #[cfg(unix)]
        lock_options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        let lock = lock_options.open(&lock_path).map_err(storage)?;
        if lock.try_lock().is_err() {
            return Err(ObjectGuardError::Busy);
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let c = Connection::open_with_flags(&path, flags).map_err(storage)?;
        init(&c)?;
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(key);
        let mut c = c;
        let transaction = c.transaction().map_err(storage)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS object_guard_keycheck (id INTEGER PRIMARY KEY CHECK(id=1), record BLOB NOT NULL)").map_err(storage)?;
        let existing: Option<Vec<u8>> = {
            use rusqlite::OptionalExtension;
            transaction
                .query_row(
                    "SELECT record FROM object_guard_keycheck WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage)?
        };
        let sentinel = ProtectedMapping {
            identity: ProtectedIdentity {
                endpoint: "https://registry.invalid".into(),
                bucket: "keycheck".into(),
                prefix: String::new(),
                alias: String::new(),
            },
            repository_id: format!("object-guard-schema-{OBJECT_GUARD_SCHEMA}"),
        };
        if let Some(record) = existing {
            if open(&k, &record)? != sentinel {
                return Err(ObjectGuardError::KeyOrCorruption);
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO object_guard_keycheck(id,record) VALUES(1,?1)",
                    params![seal(&k, &sentinel)?],
                )
                .map_err(storage)?;
        }
        let registry_table_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='object_storage_identity_v1')", [], |r| r.get(0)
        ).map_err(storage)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS object_storage_identity_v1 (id INTEGER PRIMARY KEY CHECK(id=1), record BLOB NOT NULL)").map_err(storage)?;
        let registry_record: Option<Vec<u8>> = {
            use rusqlite::OptionalExtension;
            transaction
                .query_row(
                    "SELECT record FROM object_storage_identity_v1 WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?
        };
        if let Some(record) = registry_record {
            open_registry(&k, &record)?;
        } else {
            if registry_table_exists {
                return Err(ObjectGuardError::KeyOrCorruption);
            }
            let registry = StorageIdentityRegistry {
                version: 1,
                epoch: 0,
                bindings: Vec::new(),
            };
            transaction
                .execute(
                    "INSERT INTO object_storage_identity_v1(id,record) VALUES(1,?1)",
                    params![seal_registry(&k, &registry)?],
                )
                .map_err(storage)?;
        }
        transaction.commit().map_err(storage)?;
        let guard = Self {
            db: Arc::new(Mutex::new(c)),
            key: Arc::new(k),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            domain: Arc::new(Mutex::new(())),
            _lock: Arc::new(lock),
            _path: path,
        };
        guard.rows()?;
        Ok(guard)
    }
    /// Called only after explicit local-user confirmation, never by discovery.
    /// The scope's prefix and alias do not grant trust to another bucket/endpoint.
    pub fn register_storage_identity(
        &self,
        scope: &ProtectedIdentity,
        association: StorageIdentityAssociation,
    ) -> Result<String, ObjectGuardError> {
        let scope = canonical(scope.clone())?;
        let _domain = self.guard_lock()?;
        if !self
            .active
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?
            .is_empty()
        {
            return Err(ObjectGuardError::BindingBusy);
        }
        let mut c = self
            .db
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?;
        let tx = c.transaction().map_err(storage)?;
        let record: Vec<u8> = tx
            .query_row(
                "SELECT record FROM object_storage_identity_v1 WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(storage)?;
        let mut registry = open_registry(&self.key, &record)?;
        let existing = registry
            .bindings
            .iter()
            .find(|b| b.endpoint == scope.endpoint && b.bucket == scope.bucket);
        if let Some(binding) = existing {
            return match association {
                StorageIdentityAssociation::Existing(id) if id == binding.physical_id => Ok(id),
                _ => Err(ObjectGuardError::InvalidBinding),
            };
        }
        let physical_id = match association {
            StorageIdentityAssociation::New => loop {
                let mut random = [0u8; 32];
                OsRng.try_fill_bytes(&mut random).map_err(storage)?;
                let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
                if !registry.bindings.iter().any(|b| b.physical_id == id) {
                    break id;
                }
            },
            StorageIdentityAssociation::Existing(id) => {
                if !registry.bindings.iter().any(|b| b.physical_id == id) {
                    return Err(ObjectGuardError::InvalidBinding);
                }
                id
            }
        };
        registry.bindings.push(StorageIdentityBinding {
            endpoint: scope.endpoint,
            bucket: scope.bucket,
            physical_id: physical_id.clone(),
        });
        registry.epoch = registry
            .epoch
            .checked_add(1)
            .ok_or(ObjectGuardError::KeyOrCorruption)?;
        tx.execute(
            "UPDATE object_storage_identity_v1 SET record=?1 WHERE id=1",
            params![seal_registry(&self.key, &registry)?],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(physical_id)
    }

    pub fn registry_epoch(&self) -> Result<u64, ObjectGuardError> {
        Ok(self.registry()?.epoch)
    }

    fn registry(&self) -> Result<StorageIdentityRegistry, ObjectGuardError> {
        let c = self
            .db
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?;
        let record: Vec<u8> = c
            .query_row(
                "SELECT record FROM object_storage_identity_v1 WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(storage)?;
        open_registry(&self.key, &record)
    }

    fn project(
        &self,
        id: &ProtectedIdentity,
        confirmed: bool,
    ) -> Result<ProtectedIdentity, ObjectGuardError> {
        let registry = self.registry()?;
        let mut projected = canonical(id.clone())?;
        if let Some(binding) = registry
            .bindings
            .iter()
            .find(|b| b.endpoint == projected.endpoint && b.bucket == projected.bucket)
        {
            projected.endpoint = format!("physical:{}", binding.physical_id);
        } else if confirmed {
            return Err(ObjectGuardError::IdentityUnconfirmed);
        }
        projected.alias.clear();
        Ok(projected)
    }

    fn projected_rows(&self) -> Result<Vec<ProtectedIdentity>, ObjectGuardError> {
        self.rows()?
            .into_iter()
            .map(|(m, _, _)| self.project(&m.identity, false))
            .collect()
    }

    pub fn register(&self, mut mapping: ProtectedMapping) -> Result<(), ObjectGuardError> {
        mapping.identity = canonical(mapping.identity)?;
        self.insert(mapping, 1, false)
    }
    pub fn begin_create(
        &self,
        mut mapping: ProtectedMapping,
    ) -> Result<CreationLease, ObjectGuardError> {
        mapping.identity = canonical(mapping.identity)?;
        let _g = self.guard_lock()?;
        let id = self.project(&mapping.identity, true)?;
        if self.projected_rows()?.iter().any(|m| overlaps(m, &id)) {
            return Err(ObjectGuardError::Conflict);
        }
        if self.has_active_overlap(&id)? {
            return Err(ObjectGuardError::OperationInProgress);
        }
        let blob = seal(&self.key, &mapping)?;
        self.exec_insert(&mapping, &blob, 0, false)?;
        self.activate(id.clone())?;
        Ok(CreationLease {
            active: self.active.clone(),
            identity: id,
        })
    }
    pub fn mark_discovery_complete(
        &self,
        identity: &ProtectedIdentity,
    ) -> Result<(), ObjectGuardError> {
        let id = canonical(identity.clone())?;
        let _g = self.guard_lock()?;
        let c = self
            .db
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?;
        let record = seal(
            &self.key,
            &ProtectedMapping {
                identity: id.clone(),
                repository_id: "discovery".into(),
            },
        )?;
        c.execute("INSERT INTO object_discovery(id,record) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record", params![digest(&self.key, &id), record]).map_err(storage)?;
        Ok(())
    }
    pub fn complete_discovery(&self, identity: &ProtectedIdentity) -> Result<(), ObjectGuardError> {
        self.mark_discovery_complete(identity)
    }
    pub fn begin_mutation(
        &self,
        identity: &ProtectedIdentity,
    ) -> Result<ObjectLease, ObjectGuardError> {
        self.admit(identity)
    }
    pub fn admit(&self, identity: &ProtectedIdentity) -> Result<ObjectLease, ObjectGuardError> {
        let id = canonical(identity.clone())?;
        let _g = self.guard_lock()?;
        let id = self.project(&id, true)?;
        let rows = self.projected_rows()?;
        if rows.iter().any(|m| overlaps(m, &id)) {
            return Err(ObjectGuardError::Protected);
        }
        let discovery_scopes = {
            let c = self
                .db
                .lock()
                .map_err(|_| ObjectGuardError::OperationInProgress)?;
            let mut q = c
                .prepare("SELECT record FROM object_discovery")
                .map_err(storage)?;
            let records = q
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .map_err(storage)?;
            let mut scopes = Vec::new();
            for record in records {
                scopes.push(open(&self.key, &record.map_err(storage)?)?.identity);
            }
            scopes
        };
        let mut discovered = false;
        for scope in discovery_scopes {
            if contains(&self.project(&scope, false)?, &id) {
                discovered = true;
            }
        }
        if !discovered {
            return Err(ObjectGuardError::Protected);
        }
        if self.has_active_overlap(&id)? {
            return Err(ObjectGuardError::OperationInProgress);
        }
        self.activate(id.clone())?;
        Ok(ObjectLease {
            active: self.active.clone(),
            identity: id,
        })
    }
    pub fn remove(
        &self,
        _identity: &ProtectedIdentity,
    ) -> Result<ProtectedMapping, ObjectGuardError> {
        // Protection records are append-only: synchronization safety must not be
        // weakened by a mapping removal operation.
        Err(ObjectGuardError::Protected)
    }
    pub fn mappings(&self) -> Result<Vec<ProtectedMapping>, ObjectGuardError> {
        Ok(self.rows()?.into_iter().map(|x| x.0).collect())
    }
    pub fn is_protected(&self, identity: &ProtectedIdentity) -> Result<bool, ObjectGuardError> {
        let id = canonical(identity.clone())?;
        let _g = self.guard_lock()?;
        let id = self.project(&id, false)?;
        Ok(self.projected_rows()?.iter().any(|m| overlaps(m, &id)))
    }
    fn insert(
        &self,
        m: ProtectedMapping,
        state: i32,
        discovered: bool,
    ) -> Result<(), ObjectGuardError> {
        let _g = self.guard_lock()?;
        let id = self.project(&m.identity, false)?;
        if self.projected_rows()?.iter().any(|x| overlaps(x, &id))
            || self.has_active_overlap(&id)?
        {
            return Err(ObjectGuardError::Conflict);
        }
        let b = seal(&self.key, &m)?;
        self.exec_insert(&m, &b, state, discovered)
    }
    fn exec_insert(
        &self,
        m: &ProtectedMapping,
        b: &[u8],
        state: i32,
        discovered: bool,
    ) -> Result<(), ObjectGuardError> {
        let c = self
            .db
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?;
        c.execute(
            "INSERT INTO object_guard(id,record,state,discovered,epoch) VALUES(?1,?2,?3,?4,1)",
            params![digest(&self.key, &m.identity), b, state, discovered as i32],
        )
        .map_err(storage)?;
        Ok(())
    }
    fn rows(&self) -> Result<Vec<(ProtectedMapping, i32, bool)>, ObjectGuardError> {
        let c = self
            .db
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?;
        let mut q = c
            .prepare("SELECT record,state,discovered FROM object_guard")
            .map_err(storage)?;
        let it = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, i32>(2)? != 0,
                ))
            })
            .map_err(storage)?;
        let mut out = Vec::new();
        for x in it {
            let (b, s, d) = x.map_err(storage)?;
            if s != 0 && s != 1 {
                return Err(ObjectGuardError::KeyOrCorruption);
            };
            out.push((open(&self.key, &b)?, s, d));
        }
        Ok(out)
    }
    fn guard_lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, ObjectGuardError> {
        self.domain
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)
    }
    fn has_active_overlap(&self, id: &ProtectedIdentity) -> Result<bool, ObjectGuardError> {
        Ok(self
            .active
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?
            .keys()
            .any(|x| overlaps(x, id)))
    }
    fn activate(&self, id: ProtectedIdentity) -> Result<(), ObjectGuardError> {
        *self
            .active
            .lock()
            .map_err(|_| ObjectGuardError::OperationInProgress)?
            .entry(id)
            .or_insert(0) += 1;
        Ok(())
    }
}
pub struct ObjectLease {
    active: Arc<Mutex<BTreeMap<ProtectedIdentity, usize>>>,
    identity: ProtectedIdentity,
}
pub struct CreationLease {
    active: Arc<Mutex<BTreeMap<ProtectedIdentity, usize>>>,
    identity: ProtectedIdentity,
}
impl Drop for ObjectLease {
    fn drop(&mut self) {
        release(&self.active, &self.identity)
    }
}
impl Drop for CreationLease {
    fn drop(&mut self) {
        release(&self.active, &self.identity)
    }
}
fn release(a: &Arc<Mutex<BTreeMap<ProtectedIdentity, usize>>>, id: &ProtectedIdentity) {
    if let Ok(mut a) = a.lock() {
        if let Some(n) = a.get_mut(id) {
            *n -= 1;
            if *n == 0 {
                a.remove(id);
            }
        }
    }
}
fn init(c: &Connection) -> Result<(), ObjectGuardError> {
    c.pragma_update(None, "journal_mode", "WAL")
        .map_err(storage)?;
    c.pragma_update(None, "synchronous", "FULL")
        .map_err(storage)?;
    c.execute_batch(&format!("CREATE TABLE IF NOT EXISTS object_guard_meta(schema INTEGER NOT NULL); INSERT INTO object_guard_meta(schema) SELECT {OBJECT_GUARD_SCHEMA} WHERE NOT EXISTS (SELECT 1 FROM object_guard_meta); CREATE TABLE IF NOT EXISTS object_guard(id BLOB PRIMARY KEY,record BLOB NOT NULL,state INTEGER NOT NULL,discovered INTEGER NOT NULL DEFAULT 0,epoch INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS object_discovery(id BLOB PRIMARY KEY,record BLOB NOT NULL);")).map_err(storage)?;
    let schema: i32 = c
        .query_row("SELECT schema FROM object_guard_meta LIMIT 1", [], |r| {
            r.get(0)
        })
        .map_err(storage)?;
    if schema != OBJECT_GUARD_SCHEMA {
        return Err(ObjectGuardError::KeyOrCorruption);
    }
    Ok(())
}
fn contains(a: &ProtectedIdentity, b: &ProtectedIdentity) -> bool {
    a.endpoint == b.endpoint
        && a.bucket == b.bucket
        && (a.prefix.is_empty()
            || a.prefix == b.prefix
            || b.prefix.starts_with(&(a.prefix.clone() + "/")))
}
pub(super) fn canonical(mut i: ProtectedIdentity) -> Result<ProtectedIdentity, ObjectGuardError> {
    if i.bucket.is_empty() {
        return Err(ObjectGuardError::InvalidIdentity);
    };
    let u = Url::parse(&i.endpoint).map_err(|_| ObjectGuardError::InvalidIdentity)?;
    if !matches!(u.scheme(), "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
        || u.host_str().is_none()
    {
        return Err(ObjectGuardError::InvalidIdentity);
    }
    let host = u.host_str().ok_or(ObjectGuardError::InvalidIdentity)?;
    i.endpoint = format!(
        "{}://{}{}",
        u.scheme().to_ascii_lowercase(),
        host.to_ascii_lowercase(),
        u.port().map(|p| format!(":{p}")).unwrap_or_default()
    );
    if i.prefix.contains('\\') || i.prefix.contains('\0') || i.prefix.starts_with('/') {
        return Err(ObjectGuardError::InvalidIdentity);
    }
    if i.prefix.ends_with('/') && i.prefix.len() > 1 {
        i.prefix.pop();
    }
    if !i.prefix.is_empty()
        && i.prefix
            .split('/')
            .any(|x| x.is_empty() || x == "." || x == ".." || x.contains(char::is_whitespace))
    {
        return Err(ObjectGuardError::InvalidIdentity);
    }
    Ok(i)
}
fn overlaps(a: &ProtectedIdentity, b: &ProtectedIdentity) -> bool {
    a.endpoint == b.endpoint
        && a.bucket == b.bucket
        && (a.prefix.is_empty()
            || b.prefix.is_empty()
            || a.prefix == b.prefix
            || a.prefix.starts_with(&(b.prefix.clone() + "/"))
            || b.prefix.starts_with(&(a.prefix.clone() + "/")))
}
fn digest(k: &[u8; 32], i: &ProtectedIdentity) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(k);
    h.update(i.endpoint.as_bytes());
    h.update([0]);
    h.update(i.bucket.as_bytes());
    h.update([0]);
    h.update(i.prefix.as_bytes());
    h.finalize().to_vec()
}
fn seal(k: &[u8; 32], m: &ProtectedMapping) -> Result<Vec<u8>, ObjectGuardError> {
    let c = ChaCha20Poly1305::new(k.into());
    let mut n = [0; 12];
    OsRng.fill_bytes(&mut n);
    let p = serde_json::to_vec(m).map_err(|e| storage(e))?;
    let mut o = n.to_vec();
    o.extend(
        c.encrypt(Nonce::from_slice(&n), p.as_ref())
            .map_err(|_| ObjectGuardError::KeyOrCorruption)?,
    );
    Ok(o)
}
fn open(k: &[u8; 32], b: &[u8]) -> Result<ProtectedMapping, ObjectGuardError> {
    if b.len() < 12 {
        return Err(ObjectGuardError::KeyOrCorruption);
    };
    let c = ChaCha20Poly1305::new(k.into());
    let p = c
        .decrypt(Nonce::from_slice(&b[..12]), &b[12..])
        .map_err(|_| ObjectGuardError::KeyOrCorruption)?;
    serde_json::from_slice(&p).map_err(|_| ObjectGuardError::KeyOrCorruption)
}

const IDENTITY_REGISTRY_DOMAIN: &[u8] = b"peerdav/object-storage-identity/v1";

fn seal_registry(
    k: &[u8; 32],
    registry: &StorageIdentityRegistry,
) -> Result<Vec<u8>, ObjectGuardError> {
    let cipher = ChaCha20Poly1305::new(k.into());
    let mut nonce = [0u8; 12];
    OsRng.try_fill_bytes(&mut nonce).map_err(storage)?;
    let plaintext = serde_json::to_vec(registry).map_err(storage)?;
    let mut record = nonce.to_vec();
    record.extend(
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: IDENTITY_REGISTRY_DOMAIN,
                },
            )
            .map_err(|_| ObjectGuardError::KeyOrCorruption)?,
    );
    Ok(record)
}

fn open_registry(k: &[u8; 32], record: &[u8]) -> Result<StorageIdentityRegistry, ObjectGuardError> {
    if record.len() < 28 {
        return Err(ObjectGuardError::KeyOrCorruption);
    }
    let cipher = ChaCha20Poly1305::new(k.into());
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&record[..12]),
            Payload {
                msg: &record[12..],
                aad: IDENTITY_REGISTRY_DOMAIN,
            },
        )
        .map_err(|_| ObjectGuardError::KeyOrCorruption)?;
    let registry: StorageIdentityRegistry =
        serde_json::from_slice(&plaintext).map_err(|_| ObjectGuardError::KeyOrCorruption)?;
    if registry.version != 1 || registry.epoch != registry.bindings.len() as u64 {
        return Err(ObjectGuardError::KeyOrCorruption);
    }
    let mut scopes = std::collections::BTreeSet::new();
    for binding in &registry.bindings {
        let scope = ProtectedIdentity {
            endpoint: binding.endpoint.clone(),
            bucket: binding.bucket.clone(),
            prefix: String::new(),
            alias: String::new(),
        };
        if canonical(scope.clone()).map_err(|_| ObjectGuardError::KeyOrCorruption)? != scope
            || binding.physical_id.len() != 64
            || !binding.physical_id.bytes().all(|b| b.is_ascii_hexdigit())
            || !scopes.insert((&binding.endpoint, &binding.bucket))
        {
            return Err(ObjectGuardError::KeyOrCorruption);
        }
    }
    Ok(registry)
}
