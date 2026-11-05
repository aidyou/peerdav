use bytes::Bytes;
use cap_fs_ext::OpenOptionsExt;
use cap_std::fs::OpenOptions as CapOpenOptions;
use futures_util::{StreamExt, stream};
use rand::RngCore;
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

use super::traits::ObjectStore;
use super::types::{CloudError, ObjectMetadata, ObjectStream, PutCondition};
use super::{
    ObjectGuard, ObjectGuardError, ProtectedIdentity,
    object_types::{BrowsePage, DeletePlan, ObjectRef},
};
use crate::sync::append_types::APPEND_ANCHORS;
use crate::sync::repository_types::decode_head;

const MAX_PAGES: usize = 1000;
const MAX_ENTRIES: usize = 100_000;
const MAX_PLAN_TTL: u64 = 24 * 60 * 60;

#[derive(Debug, Error)]
pub enum ObjectServiceError {
    #[error(transparent)]
    Cloud(#[from] CloudError),
    #[error(transparent)]
    Guard(#[from] ObjectGuardError),
    #[error("object store scope is unknown")]
    UnknownStoreScope,
    #[error("object store scope does not match the protected identity")]
    StoreScopeMismatch,
    #[error("invalid object key")]
    InvalidKey,
    #[error("repository discovery is incomplete or has an unknown format")]
    DiscoveryIncomplete,
    #[error("delete plan is expired, stale, unknown, or has been used")]
    StaleDeletePlan,
    #[error("delete plan confirmation is required")]
    ConfirmationRequired,
    #[error("deleting the bucket root is forbidden; delete entries individually")]
    RootDeleteForbidden,
    #[error("local download capability is invalid")]
    InvalidLocalCapability,
    #[error("local filesystem operation failed")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("download cancelled")]
    Cancelled,
    #[error("destination already exists")]
    DestinationExists,
    #[error("destination is a symlink")]
    DestinationSymlink,
    #[error("object stream exceeded its declared size")]
    SizeMismatch,
    #[error("download cleanup failed after: {original}")]
    Cleanup {
        original: Box<ObjectServiceError>,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct PhysicalRoot {
    dir: cap_std::fs::Dir,
    path: PathBuf,
}
impl PhysicalRoot {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ObjectServiceError> {
        let path: PathBuf = path.as_ref().components().collect();
        if !path.is_absolute() {
            return Err(ObjectServiceError::InvalidLocalCapability);
        }
        if std::fs::symlink_metadata(&path)
            .map_err(|source| ObjectServiceError::Io { source })?
            .file_type()
            .is_symlink()
        {
            return Err(ObjectServiceError::InvalidLocalCapability);
        }
        #[cfg(unix)]
        let dir = {
            // Open the selected directory itself without following its final component.
            // The resulting capability remains bound even if its ambient path is replaced.
            let fd = rustix::fs::open(
                &path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|source| ObjectServiceError::Io {
                source: source.into(),
            })?;
            cap_std::fs::Dir::from_std_file(std::fs::File::from(fd))
        };
        #[cfg(not(unix))]
        let dir = cap_std::fs::Dir::open_ambient_dir(&path, cap_std::ambient_authority())
            .map_err(|source| ObjectServiceError::Io { source })?;
        let metadata = dir
            .dir_metadata()
            .map_err(|source| ObjectServiceError::Io { source })?;
        if !metadata.is_dir() {
            return Err(ObjectServiceError::InvalidLocalCapability);
        }
        Ok(Self { dir, path })
    }
    pub(crate) fn relative_target_identity(
        &self,
        relative: &str,
    ) -> Result<Option<String>, ObjectServiceError> {
        if !self.relative_target_exists(relative)? {
            return Ok(None);
        }
        let parts: Vec<_> = relative.split('/').collect();
        let (name, parents) = parts.split_last().ok_or(ObjectServiceError::InvalidKey)?;
        let mut dir = self.upload_directory_handle()?;
        for part in parents {
            use cap_fs_ext::DirExt;
            dir = dir
                .open_dir_nofollow(part)
                .map_err(|source| ObjectServiceError::Io { source })?;
        }
        let meta = dir
            .symlink_metadata(name)
            .map_err(|source| ObjectServiceError::Io { source })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(ObjectServiceError::InvalidLocalCapability);
        }
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            Ok(Some(format!(
                "{}:{}:{}:{}:{}:{}",
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime_nsec()
            )))
        }
        #[cfg(not(unix))]
        Ok(Some(format!("{}:{:?}", meta.len(), meta.modified())))
    }
    pub(crate) fn download_parent(
        &self,
        relative: &str,
    ) -> Result<(cap_std::fs::Dir, String), ObjectServiceError> {
        valid_relative_path(relative)?;
        let parts: Vec<_> = relative.split('/').collect();
        let (name, parents) = parts.split_last().ok_or(ObjectServiceError::InvalidKey)?;
        Ok((open_download_parent(self, parents)?, (*name).to_owned()))
    }
    /// Reveal must never follow an ambient path which replaced the selected directory.
    pub(crate) fn reveal_path(&self) -> Result<&Path, ObjectServiceError> {
        let reopened = Self::open(&self.path)?;
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            let a = self
                .dir
                .dir_metadata()
                .map_err(|source| ObjectServiceError::Io { source })?;
            let b = reopened
                .dir
                .dir_metadata()
                .map_err(|source| ObjectServiceError::Io { source })?;
            if a.dev() != b.dev() || a.ino() != b.ino() {
                return Err(ObjectServiceError::InvalidLocalCapability);
            }
        }
        #[cfg(not(unix))]
        let _ = reopened;
        Ok(&self.path)
    }
    pub(crate) fn install_download(
        &self,
        relative: &str,
        source: &Path,
        overwrite: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), ObjectServiceError> {
        let (dir, name) = self.download_parent(relative)?;
        let temporary = format!(".peerdav-{}", hex::encode(rand::random::<[u8; 16]>()));
        let mut options = CapOpenOptions::new();
        options.write(true).create_new(true);
        let mut output = dir
            .open_with(&temporary, &options)
            .map_err(|source| ObjectServiceError::Io { source })?
            .into_std();
        let result = (|| {
            let mut input =
                std::fs::File::open(source).map_err(|source| ObjectServiceError::Io { source })?;
            let identity = self.relative_target_identity(relative)?;
            use std::io::{Read, Write};
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                if cancel.is_cancelled() {
                    return Err(ObjectServiceError::Cancelled);
                }
                let count = input
                    .read(&mut buffer)
                    .map_err(|source| ObjectServiceError::Io { source })?;
                if count == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..count])
                    .map_err(|source| ObjectServiceError::Io { source })?;
            }
            output
                .sync_all()
                .map_err(|source| ObjectServiceError::Io { source })?;
            if cancel.is_cancelled() {
                return Err(ObjectServiceError::Cancelled);
            }
            if overwrite && self.relative_target_identity(relative)? != identity {
                return Err(ObjectServiceError::DestinationExists);
            }
            // Identity comparison and rename are separate filesystem operations,
            // not an atomic compare-and-swap on all supported platforms.
            if overwrite {
                match dir.symlink_metadata(&name) {
                    Ok(m) if m.file_type().is_symlink() || !m.is_file() => {
                        return Err(ObjectServiceError::InvalidLocalCapability);
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => return Err(ObjectServiceError::Io { source }),
                }
                dir.rename(&temporary, &dir, &name)
                    .map_err(|source| ObjectServiceError::Io { source })?;
            } else {
                dir.hard_link(&temporary, &dir, &name)
                    .map_err(|source| ObjectServiceError::Io { source })?;
                dir.remove_file(&temporary)
                    .map_err(|source| ObjectServiceError::Io { source })?;
            }
            Ok(())
        })();
        drop(output);
        match result {
            Ok(()) => Ok(()),
            Err(e) => cleanup_staging(&dir, &temporary, e),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// A batch download hands every task its own capability handle while all of
    /// them keep writing below the single directory the user selected.
    pub fn try_clone(&self) -> Result<Self, ObjectServiceError> {
        Ok(Self {
            dir: self
                .dir
                .try_clone()
                .map_err(|source| ObjectServiceError::Io { source })?,
            path: self.path.clone(),
        })
    }
    pub(crate) fn upload_directory_handle(&self) -> Result<cap_std::fs::Dir, ObjectServiceError> {
        self.dir
            .try_clone()
            .map_err(|source| ObjectServiceError::Io { source })
    }
    /// Whether a relative download target already exists under this capability.
    pub fn relative_target_exists(&self, relative_path: &str) -> Result<bool, ObjectServiceError> {
        valid_relative_path(relative_path)?;
        let parts: Vec<&str> = relative_path.split('/').collect();
        let (name, parents) = parts.split_last().ok_or(ObjectServiceError::InvalidKey)?;
        let mut dir = self
            .dir
            .try_clone()
            .map_err(|source| ObjectServiceError::Io { source })?;
        for part in parents {
            match dir.symlink_metadata(part) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                    return Err(ObjectServiceError::InvalidLocalCapability);
                }
                Ok(_) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(source) => return Err(ObjectServiceError::Io { source }),
            }
            dir = {
                use cap_fs_ext::DirExt;
                dir.open_dir_nofollow(part)
            }
            .map_err(|source| ObjectServiceError::Io { source })?;
        }
        match dir.symlink_metadata(name) {
            Ok(meta) if meta.file_type().is_symlink() => {
                Err(ObjectServiceError::DestinationSymlink)
            }
            Ok(meta) if !meta.is_file() => Err(ObjectServiceError::InvalidLocalCapability),
            Ok(_) => Ok(true),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(ObjectServiceError::Io { source }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigRevision(pub String);
#[derive(Clone, Debug, Default)]
pub struct BudgetLedger {
    raw_entries: usize,
    requests: usize,
}
impl BudgetLedger {
    /// Failed requests also consume this operation's request budget.
    pub fn remaining_requests(&self) -> usize {
        MAX_PAGES.saturating_sub(self.requests)
    }
    /// Raw entries are counted before filtering or de-duplication.
    pub fn remaining_entries(&self) -> usize {
        MAX_ENTRIES.saturating_sub(self.raw_entries)
    }
    fn reserve_page(&mut self) -> Result<(), ObjectServiceError> {
        if self.requests >= MAX_PAGES || self.raw_entries >= MAX_ENTRIES {
            return Err(ObjectServiceError::DiscoveryIncomplete);
        }
        self.requests += 1;
        Ok(())
    }
    fn record_entries(&mut self, count: usize) -> Result<(), ObjectServiceError> {
        self.raw_entries = self
            .raw_entries
            .checked_add(count)
            .ok_or(ObjectServiceError::DiscoveryIncomplete)?;
        if self.raw_entries > MAX_ENTRIES {
            return Err(ObjectServiceError::DiscoveryIncomplete);
        }
        Ok(())
    }
}
#[derive(Default)]
struct BrowseTraversal {
    ledger: BudgetLedger,
    next: Option<String>,
    complete: bool,
    seen: HashSet<String>,
}
#[derive(Clone, Debug)]
struct StoredPlan {
    plan: DeletePlan,
    prefix: String,
    view: ProtectedIdentity,
    sizes: Vec<u64>,
}

pub struct ObjectService {
    store: Arc<dyn ObjectStore>,
    guard: ObjectGuard,
    identity: ProtectedIdentity,
    physical_root: Option<PhysicalRoot>,
    config_revision: ConfigRevision,
    browse_traversals: tokio::sync::Mutex<HashMap<String, BrowseTraversal>>,
    plans: Mutex<HashMap<String, StoredPlan>>,
}
fn cleanup_staging(
    dir: &cap_std::fs::Dir,
    temporary: &str,
    error: ObjectServiceError,
) -> Result<(), ObjectServiceError> {
    match dir.remove_file(temporary) {
        Ok(()) => Err(error),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(error),
        Err(source) => Err(ObjectServiceError::Cleanup {
            original: Box::new(error),
            source,
        }),
    }
}

impl ObjectService {
    pub fn try_new(
        store: Arc<dyn ObjectStore>,
        guard: ObjectGuard,
        identity: ProtectedIdentity,
        config_revision: impl Into<String>,
    ) -> Result<Self, ObjectServiceError> {
        let scope = store.scope().ok_or(ObjectServiceError::UnknownStoreScope)?;
        let identity = super::object_guard::canonical(identity)?;
        let physical = super::object_guard::canonical(ProtectedIdentity {
            endpoint: scope.endpoint,
            bucket: scope.bucket,
            prefix: scope.prefix,
            alias: String::new(),
        })?;
        if identity.endpoint != physical.endpoint
            || identity.bucket != physical.bucket
            || identity.prefix != physical.prefix
        {
            return Err(ObjectServiceError::StoreScopeMismatch);
        }
        Ok(Self {
            store,
            guard,
            identity,
            physical_root: None,
            config_revision: ConfigRevision(config_revision.into()),
            browse_traversals: tokio::sync::Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
        })
    }
    pub fn with_physical_root(mut self, root: PhysicalRoot) -> Self {
        self.physical_root = Some(root);
        self
    }
    pub async fn download_to(
        &self,
        key: &str,
        file_name: &str,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        overwrite: bool,
        progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), ObjectServiceError> {
        valid_key(key)?;
        if file_name.is_empty()
            || file_name.contains(['/', '\\', '\0', ':'])
            || file_name == "."
            || file_name == ".."
        {
            return Err(ObjectServiceError::InvalidKey);
        }
        let root = self
            .physical_root
            .as_ref()
            .ok_or(ObjectServiceError::InvalidLocalCapability)?;
        let dir = root
            .dir
            .try_clone()
            .map_err(|source| ObjectServiceError::Io { source })?;
        self.download_into(key, dir, file_name, cancel, overwrite, progress)
            .await
    }
    /// Downloads one object below a caller-provided relative path, creating the
    /// nested directories the folder download needs inside the granted root.
    pub async fn download_to_relative(
        &self,
        key: &str,
        relative_path: &str,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        overwrite: bool,
        progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), ObjectServiceError> {
        valid_key(key)?;
        valid_relative_path(relative_path)?;
        let root = self
            .physical_root
            .as_ref()
            .ok_or(ObjectServiceError::InvalidLocalCapability)?;
        let parts: Vec<&str> = relative_path.split('/').collect();
        let (file_name, parents) = parts.split_last().ok_or(ObjectServiceError::InvalidKey)?;
        let dir = open_download_parent(root, parents)?;
        self.download_into(key, dir, file_name, cancel, overwrite, progress)
            .await
    }
    async fn download_into(
        &self,
        key: &str,
        dir: cap_std::fs::Dir,
        file_name: &str,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        overwrite: bool,
        progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), ObjectServiceError> {
        let metadata = self.store.stat(key).await?;
        let stream = self.store.get(key, None).await?;
        let target = Path::new(file_name);
        match dir.symlink_metadata(target) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(ObjectServiceError::DestinationSymlink);
                }
                if !overwrite {
                    return Err(ObjectServiceError::DestinationExists);
                }
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(ObjectServiceError::Io { source }),
        }
        let mut temp_id = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut temp_id);
        let temporary = format!(".peerdav-{}", hex::encode(temp_id));
        let mut options = CapOpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = dir
            .open_with(&temporary, &options)
            .map_err(|source| ObjectServiceError::Io { source })?
            .into_std();
        let transfer = async {
            let mut received = 0u64;
            futures_util::pin_mut!(stream);
            while let Some(chunk) = stream.next().await {
                if cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
                {
                    return Err(ObjectServiceError::Cancelled);
                }
                let chunk = chunk?;
                received = received
                    .checked_add(chunk.len() as u64)
                    .ok_or(ObjectServiceError::SizeMismatch)?;
                if received > metadata.size {
                    return Err(ObjectServiceError::SizeMismatch);
                }
                file.write_all(&chunk)
                    .map_err(|source| ObjectServiceError::Io { source })?;
                if let Some(progress) = progress.as_ref() {
                    progress(received, metadata.size);
                }
            }
            if cancel
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
            {
                return Err(ObjectServiceError::Cancelled);
            }
            if received != metadata.size {
                return Err(ObjectServiceError::SizeMismatch);
            }
            let after = self.store.stat(key).await?;
            if after.size != metadata.size || after.etag != metadata.etag {
                return Err(ObjectServiceError::Cloud(CloudError::Precondition));
            }
            file.sync_all()
                .map_err(|source| ObjectServiceError::Io { source })?;
            if let Some(progress) = progress.as_ref() {
                progress(metadata.size, metadata.size);
            }
            if cancel
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
            {
                return Err(ObjectServiceError::Cancelled);
            }
            Ok(())
        }
        .await;
        drop(file);
        if let Err(error) = transfer {
            return cleanup_staging(&dir, &temporary, error);
        }
        let commit = (|| {
            if overwrite {
                dir.rename(&temporary, &dir, target)
                    .map_err(|source| ObjectServiceError::Io { source })?;
            } else {
                dir.hard_link(&temporary, &dir, target).map_err(|source| {
                    if source.kind() == std::io::ErrorKind::AlreadyExists {
                        ObjectServiceError::DestinationExists
                    } else {
                        ObjectServiceError::Io { source }
                    }
                })?;
                dir.remove_file(&temporary)
                    .map_err(|source| ObjectServiceError::Io { source })?;
            }
            #[cfg(unix)]
            {
                let sync_fd = rustix::fs::openat(
                    &dir,
                    ".",
                    rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|source| ObjectServiceError::Io {
                    source: source.into(),
                })?;
                rustix::fs::fsync(&sync_fd).map_err(|source| ObjectServiceError::Io {
                    source: source.into(),
                })?;
            }
            #[cfg(not(unix))]
            dir.open_dir(".")
                .map_err(|source| ObjectServiceError::Io { source })?
                .into_std_file()
                .sync_all()
                .map_err(|source| ObjectServiceError::Io { source })?;
            Ok(())
        })();
        match commit {
            Ok(()) => Ok(()),
            Err(error) => cleanup_staging(&dir, &temporary, error),
        }
    }
    /// Query the physical scoped key without discovery or granting mutation trust.
    pub fn is_protected(&self, key: &str) -> Result<bool, ObjectServiceError> {
        // A protection target is a precise key or prefix, not a folder path:
        // unlike browse prefixes it must not carry a trailing separator.
        if !key.is_empty() {
            valid_key(key)?;
        }
        Ok(self.guard.is_protected(&self.target_identity(key))?)
    }
    /// One active continuation chain per prefix. A terminal chain can be
    /// restarted with no token; failed requests retain their operation budget.
    /// S3 tokens additionally carry the provider chain's page/entry counters.
    pub async fn browse(
        &self,
        prefix: &str,
        token: Option<&str>,
    ) -> Result<BrowsePage, ObjectServiceError> {
        valid_prefix(prefix)?;
        let mut traversals = self.browse_traversals.lock().await;
        let traversal = traversals.entry(prefix.to_owned()).or_default();
        if token.is_none() && traversal.complete {
            *traversal = BrowseTraversal::default();
        }
        if traversal.complete || token != traversal.next.as_deref() {
            return Err(ObjectServiceError::DiscoveryIncomplete);
        }
        let page = self
            .browse_with_budget(prefix, token, &mut traversal.ledger)
            .await?;
        if let Some(next) = &page.next
            && !traversal.seen.insert(next.clone())
        {
            return Err(ObjectServiceError::DiscoveryIncomplete);
        }
        traversal.next = page.next.clone();
        traversal.complete = page.complete;
        Ok(page)
    }
    /// The runtime must retain the same ledger across retries of one operation.
    /// This variant permits independent concurrent chains without a new wire DTO.
    pub async fn browse_with_budget(
        &self,
        prefix: &str,
        token: Option<&str>,
        ledger: &mut BudgetLedger,
    ) -> Result<BrowsePage, ObjectServiceError> {
        valid_prefix(prefix)?;
        ledger.reserve_page()?;
        let page = self.store.list_page(prefix, token).await?;
        ledger.record_entries(page.objects.len())?;
        Ok(BrowsePage {
            prefix: prefix.to_owned(),
            objects: page.objects,
            next: page.next_token.clone(),
            complete: page.next_token.is_none(),
        })
    }
    pub async fn discover(&self, prefix: &str) -> Result<(), ObjectServiceError> {
        valid_prefix(prefix)?;
        let mut ledger = BudgetLedger::default();
        let mut token = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            ledger.reserve_page()?;
            let page = self.store.list_page(prefix, token.as_deref()).await?;
            ledger.record_entries(page.objects.len())?;
            for object in &page.objects {
                let key = object.key.as_str();
                let marker = if key == "control/HEAD" {
                    Some(("", false))
                } else if let Some(root) = key.strip_suffix("/control/HEAD") {
                    Some((root, false))
                } else if let Some((root, anchor)) = key.rsplit_once(APPEND_ANCHORS) {
                    let root = root.strip_suffix('/').unwrap_or(root);
                    let Some((repo, digest)) = anchor.split_once('-') else {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    };
                    if repo.len() != 32
                        || digest.len() != 64
                        || !repo
                            .bytes()
                            .chain(digest.bytes())
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                    Some((root, true))
                } else {
                    None
                };
                let Some((root, append)) = marker else {
                    continue;
                };
                // Persist the protection before interpreting untrusted marker bytes.
                let identity = self.identity_with_prefix(root);
                let already_protected = self.guard.is_protected(&identity)?;
                if !already_protected {
                    register_discovered_protection(&self.guard, identity, key)?;
                }
                if object.size > 64 * 1024 {
                    return Err(ObjectServiceError::DiscoveryIncomplete);
                }
                let mut stream = self.store.get(key, None).await?;
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    if bytes
                        .len()
                        .checked_add(chunk.len())
                        .is_none_or(|length| length > 64 * 1024)
                    {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                if append {
                    let anchor: crate::sync::append_types::Anchor = serde_json::from_slice(&bytes)
                        .map_err(|_| ObjectServiceError::DiscoveryIncomplete)?;
                    if anchor.descriptor.version != 2
                        || serde_json::to_vec(&anchor)
                            .map_err(|_| ObjectServiceError::DiscoveryIncomplete)?
                            != bytes
                    {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                    use sha2::{Digest, Sha256};
                    let expected = format!(
                        "{APPEND_ANCHORS}{}-{}",
                        anchor.descriptor.repo_id,
                        hex::encode(Sha256::digest(&bytes))
                    );
                    if key != join_prefix(root, &expected) {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                } else {
                    let head =
                        decode_head(&bytes).map_err(|_| ObjectServiceError::DiscoveryIncomplete)?;
                    if head.descriptor.version != 1 {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                }
            }
            match page.next_token {
                Some(next) => {
                    if !seen.insert(next.clone()) {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                    token = Some(next);
                }
                None => {
                    self.guard
                        .mark_discovery_complete(&self.identity_with_prefix(prefix))?;
                    return Ok(());
                }
            }
        }
        Err(ObjectServiceError::DiscoveryIncomplete)
    }
    fn identity_with_prefix(&self, suffix: &str) -> ProtectedIdentity {
        let mut i = self.identity.clone();
        i.prefix = join_prefix(&self.identity.prefix, suffix);
        i
    }
    fn target_identity(&self, key: &str) -> ProtectedIdentity {
        self.identity_with_prefix(key)
    }
    pub(crate) fn preflight_upload(&self, key: &str) -> Result<(), ObjectServiceError> {
        valid_key(key)?;
        let _lease = self.guard.begin_mutation(&self.target_identity(key))?;
        Ok(())
    }
    pub async fn upload_stream_with_control(
        &self,
        key: &str,
        body: ObjectStream,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        overwrite: Option<String>,
    ) -> Result<ObjectMetadata, ObjectServiceError> {
        valid_key(key)?;
        let _lease = self.guard.begin_mutation(&self.target_identity(key))?;
        let current = self.store.stat(key).await;
        match overwrite {
            None => {
                if !matches!(current, Err(CloudError::NotFound)) {
                    return Err(current
                        .map(|_| ())
                        .err()
                        .unwrap_or(CloudError::Precondition)
                        .into());
                }
            }
            Some(expected) => {
                let actual = current?;
                if actual.etag.as_deref() != Some(expected.as_str()) {
                    return Err(ObjectServiceError::Cloud(CloudError::Precondition));
                }
            }
        }
        Ok(self
            .store
            .upload_stream_with_control(key, body, cancel)
            .await?)
    }
    pub async fn delete_plan(
        &self,
        prefix: &str,
        expires_at_unix: u64,
    ) -> Result<DeletePlan, ObjectServiceError> {
        valid_prefix(prefix)?;
        // Like forbidding `rm -rf /`: a root plan would wipe the whole bucket,
        // so entries under the root must be deleted one by one.
        if prefix.trim_end_matches('/').is_empty() {
            return Err(ObjectServiceError::RootDeleteForbidden);
        }
        let _lease = self.guard.begin_mutation(&self.target_identity(prefix))?;
        let now = now();
        let expiry = expires_at_unix.min(now.saturating_add(MAX_PLAN_TTL));
        if expiry <= now {
            return Err(ObjectServiceError::StaleDeletePlan);
        }
        let mut ledger = BudgetLedger::default();
        let mut token = None;
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        let mut sizes = Vec::new();
        let mut complete = false;
        for _ in 0..MAX_PAGES {
            ledger.reserve_page()?;
            let page = self.store.list_page(prefix, token.as_deref()).await?;
            ledger.record_entries(page.objects.len())?;
            for m in page.objects {
                valid_key(&m.key)?;
                if !within_prefix(prefix, &m.key) {
                    return Err(ObjectServiceError::StaleDeletePlan);
                }
                sizes.push(m.size);
                entries.push(ObjectRef {
                    key: m.key,
                    etag: m.etag,
                });
            }
            if entries.len() > MAX_ENTRIES {
                return Err(ObjectServiceError::DiscoveryIncomplete);
            }
            match page.next_token {
                Some(t) => {
                    if !seen.insert(t.clone()) {
                        return Err(ObjectServiceError::DiscoveryIncomplete);
                    }
                    token = Some(t);
                }
                None => {
                    complete = true;
                    break;
                }
            }
        }
        if !complete {
            return Err(ObjectServiceError::DiscoveryIncomplete);
        }
        let mut id = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut id);
        let plan = DeletePlan {
            id: hex::encode(id),
            expires_at_unix: expiry,
            revision: self.config_revision.0.clone(),
            registry_epoch: self.guard.registry_epoch()?,
            entries,
        };
        let view = self.target_identity(prefix);
        self.plans
            .lock()
            .map_err(|_| ObjectServiceError::StaleDeletePlan)?
            .insert(
                plan.id.clone(),
                StoredPlan {
                    plan: plan.clone(),
                    prefix: prefix.to_owned(),
                    view,
                    sizes,
                },
            );
        Ok(plan)
    }
    /// Exact immutable listing snapshot for an issued plan only. No remote reads.
    /// The conservative JSON bound includes escaping, DTO fields, and envelope.
    pub fn delete_plan_snapshot(
        &self,
        plan_id: &str,
    ) -> Result<Vec<(String, u64, Option<String>)>, ObjectServiceError> {
        let plans = self
            .plans
            .lock()
            .map_err(|_| ObjectServiceError::StaleDeletePlan)?;
        let stored = plans
            .get(plan_id)
            .ok_or(ObjectServiceError::StaleDeletePlan)?;
        if stored.plan.expires_at_unix <= now() || stored.plan.entries.len() != stored.sizes.len() {
            return Err(ObjectServiceError::StaleDeletePlan);
        }
        let mut bound = 1024usize;
        for entry in &stored.plan.entries {
            // Count escaped UTF-8 bytes directly, without allocating JSON or clones.
            let strings = std::iter::once(entry.key.as_bytes())
                .chain(entry.etag.as_ref().map(|etag| etag.as_bytes()));
            for bytes in strings {
                for byte in bytes {
                    let escaped = match byte {
                        b'"' | b'\\' => 2,
                        0..=31 => 6,
                        _ => 1,
                    };
                    bound = bound
                        .checked_add(escaped)
                        .ok_or(ObjectServiceError::DiscoveryIncomplete)?;
                }
            }
            bound = bound
                .checked_add(128)
                .ok_or(ObjectServiceError::DiscoveryIncomplete)?;
            if bound >= 32 * 1024 * 1024 {
                return Err(ObjectServiceError::DiscoveryIncomplete);
            }
        }
        Ok(stored
            .plan
            .entries
            .iter()
            .zip(&stored.sizes)
            .map(|(entry, size)| (entry.key.clone(), *size, entry.etag.clone()))
            .collect())
    }
    pub async fn execute_delete(
        &self,
        plan: &DeletePlan,
        confirmed: bool,
    ) -> Result<Vec<Result<String, CloudError>>, ObjectServiceError> {
        if !confirmed {
            return Err(ObjectServiceError::ConfirmationRequired);
        }
        let stored = self
            .plans
            .lock()
            .map_err(|_| ObjectServiceError::StaleDeletePlan)?
            .get(&plan.id)
            .cloned()
            .ok_or(ObjectServiceError::StaleDeletePlan)?;
        if *plan != stored.plan
            || plan.revision != self.config_revision.0
            || plan.expires_at_unix < now()
        {
            return Err(ObjectServiceError::StaleDeletePlan);
        }
        let _lease = self.guard.begin_mutation(&stored.view)?;
        if plan.registry_epoch != self.guard.registry_epoch()? {
            return Err(ObjectServiceError::StaleDeletePlan);
        }
        if plan.entries.len() != stored.sizes.len() {
            return Err(ObjectServiceError::StaleDeletePlan);
        }
        for (e, size) in plan.entries.iter().zip(&stored.sizes) {
            if !within_prefix(&stored.prefix, &e.key) {
                return Err(ObjectServiceError::StaleDeletePlan);
            }
            valid_key(&e.key)?;
            // Every entry needs a usable version before any deletion is attempted.
            // A preflight HEAD is not atomic protection against concurrent replacement.
            if e.etag.as_deref().is_none_or(str::is_empty) {
                return Err(CloudError::Unsupported.into());
            }
            let m = self.store.stat(&e.key).await?;
            if *size != m.size || m.etag != e.etag {
                return Err(ObjectServiceError::StaleDeletePlan);
            }
        }
        self.plans
            .lock()
            .map_err(|_| ObjectServiceError::StaleDeletePlan)?
            .remove(&plan.id)
            .ok_or(ObjectServiceError::StaleDeletePlan)?;
        let mut result = Vec::with_capacity(plan.entries.len());
        for e in &plan.entries {
            let outcome = match e.etag.as_deref().filter(|etag| !etag.is_empty()) {
                Some(etag) => self.store.delete_if_match(&e.key, etag).await,
                None => Err(CloudError::Unsupported),
            };
            result.push(outcome.map(|_| e.key.clone()));
        }
        Ok(result)
    }
}
fn register_discovered_protection(
    guard: &ObjectGuard,
    identity: ProtectedIdentity,
    key: &str,
) -> Result<(), ObjectServiceError> {
    match guard.register(super::ProtectedMapping {
        identity: identity.clone(),
        repository_id: key.to_owned(),
    }) {
        Ok(()) => Ok(()),
        Err(ObjectGuardError::Conflict) if guard.is_protected(&identity)? => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod discovery_registration_tests {
    use super::*;
    use crate::cloud::{ProtectedMapping, StorageIdentityAssociation};

    fn identity(prefix: &str) -> ProtectedIdentity {
        ProtectedIdentity {
            endpoint: "https://example.test".into(),
            bucket: "bucket".into(),
            prefix: prefix.into(),
            alias: "test".into(),
        }
    }

    #[test]
    fn registration_race_accepts_only_a_real_overlapping_mapping() {
        let directory = tempfile::tempdir().unwrap();
        let guard = ObjectGuard::open(directory.path().join("guard.db"), &[71; 32]).unwrap();
        let target = identity("root");
        assert!(!guard.is_protected(&target).unwrap());
        // Deterministically interleave another registration after discovery's first check.
        guard
            .register(ProtectedMapping {
                identity: identity("root/child"),
                repository_id: "concurrent-repository".into(),
            })
            .unwrap();
        register_discovered_protection(&guard, target.clone(), "root/control/HEAD").unwrap();
        assert!(guard.is_protected(&target).unwrap());
        assert_eq!(guard.mappings().unwrap().len(), 1);
    }

    #[test]
    fn registration_conflict_from_active_lease_is_not_swallowed() {
        let directory = tempfile::tempdir().unwrap();
        let guard = ObjectGuard::open(directory.path().join("guard.db"), &[72; 32]).unwrap();
        let target = identity("root");
        guard
            .register_storage_identity(&target, StorageIdentityAssociation::New)
            .unwrap();
        guard.mark_discovery_complete(&target).unwrap();
        let lease = guard.begin_mutation(&identity("root/file")).unwrap();
        assert!(!guard.is_protected(&target).unwrap());
        assert!(matches!(
            register_discovered_protection(&guard, target.clone(), "root/control/HEAD"),
            Err(ObjectServiceError::Guard(ObjectGuardError::Conflict))
        ));
        assert!(!guard.is_protected(&target).unwrap());
        assert!(guard.mappings().unwrap().is_empty());
        drop(lease);
        register_discovered_protection(&guard, target.clone(), "root/control/HEAD").unwrap();
        assert!(guard.is_protected(&target).unwrap());
    }
}

fn join_prefix(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_owned()
    } else if b.is_empty() {
        a.to_owned()
    } else {
        format!("{a}/{b}")
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX)
}
pub(crate) fn valid_prefix(prefix: &str) -> Result<(), ObjectServiceError> {
    // Folder prefixes carry a trailing slash (e.g. "probes/"); the store itself
    // validates the trimmed segment, so this check must trim the same way.
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        Ok(())
    } else {
        valid_key(trimmed)
    }
}
#[cfg(test)]
mod delete_prefix_tests {
    use super::within_prefix;

    #[test]
    fn delete_prefix_boundaries() {
        for (prefix, key, expected) in [
            ("docs/", "docs/a", true),
            ("docs/", "docs/nested/a", true),
            ("docs/", "docs", false),
            ("docs/", "docs-sibling/a", false),
            ("docs/", "docstring/a", false),
            ("", "docs/a", true),
            ("", "a", true),
            ("docs", "docs", true),
            ("docs", "docs/a", true),
            ("docs", "docs-sibling/a", false),
            ("docs", "docstring/a", false),
            ("docs/a", "docs/ab", false),
        ] {
            assert_eq!(within_prefix(prefix, key), expected, "{prefix:?}: {key:?}");
        }
    }
}

fn within_prefix(prefix: &str, key: &str) -> bool {
    prefix.is_empty()
        || key == prefix
        || key
            .strip_prefix(prefix)
            .is_some_and(|rest| prefix.ends_with('/') || rest.starts_with('/'))
}
fn valid_key(key: &str) -> Result<(), ObjectServiceError> {
    if key.is_empty()
        || key.contains(['\\', '\0', ':'])
        || key
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        Err(ObjectServiceError::InvalidKey)
    } else {
        Ok(())
    }
}
/// Relative download targets must stay below the granted root and mirror the
/// object-key rules, so a crafted key can never escape the selected folder.
pub fn valid_relative_path(path: &str) -> Result<(), ObjectServiceError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', '\0', ':'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        Err(ObjectServiceError::InvalidKey)
    } else {
        Ok(())
    }
}
fn open_download_parent(
    root: &PhysicalRoot,
    parents: &[&str],
) -> Result<cap_std::fs::Dir, ObjectServiceError> {
    let mut dir = root
        .dir
        .try_clone()
        .map_err(|source| ObjectServiceError::Io { source })?;
    for part in parents {
        match dir.create_dir(part) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = dir
                    .symlink_metadata(part)
                    .map_err(|source| ObjectServiceError::Io { source })?;
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(ObjectServiceError::InvalidLocalCapability);
                }
            }
            Err(source) => return Err(ObjectServiceError::Io { source }),
        }
        dir = {
            use cap_fs_ext::DirExt;
            dir.open_dir_nofollow(part)
        }
        .map_err(|source| ObjectServiceError::Io { source })?;
    }
    Ok(dir)
}
pub async fn put_bounded(
    store: &dyn ObjectStore,
    key: &str,
    body: ObjectStream,
    condition: PutCondition,
) -> Result<ObjectMetadata, CloudError> {
    store.put(key, body, condition).await
}
pub async fn download(
    store: &dyn ObjectStore,
    key: &str,
    range: Option<std::ops::Range<u64>>,
) -> Result<ObjectStream, CloudError> {
    store.get(key, range).await
}
pub fn bytes_stream(bytes: Bytes) -> ObjectStream {
    stream::once(async move { Ok(bytes) }).boxed()
}
