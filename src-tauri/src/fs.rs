//! Read-only-by-construction view of one share, enforced by the request gate
//! and again by the filesystem layer.
//!
//! Path safety: every operation walks the share root file descriptor with
//! `openat(..., O_NOFOLLOW)` and never follows a symlink. Intermediate
//! directories are opened with `O_DIRECTORY | O_NOFOLLOW`, and the final
//! component with `O_NOFOLLOW` as well. A concurrent attacker who swaps a
//! component for a symlink therefore makes the operation fail with `ELOOP`
//! or `ENOTDIR`; a single `canonicalize()` followed by a path-based open is
//! explicitly *not* used because it cannot survive that race. The share root
//! itself is canonicalized once when the administrator configures the share,
//! then kept as an open descriptor for the lifetime of the process.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use bytes::{Buf, Bytes};
use dav_server::davpath::DavPath;
use dav_server::fs::{
    DavDirEntry, DavFile, DavMetaData, FsError, FsFuture, FsResult, FsStream, GuardedFileSystem,
    OpenOptions, ReadDirMeta,
};
use futures_util::stream;

use crate::error::DomainError;
use crate::policy::CompiledRule;
use crate::types::{AccessRule, ShareInfo, SharePermissions};

/// Maximum single read returned to `dav-server` (64 KiB).
const READ_CHUNK_LIMIT: usize = 64 * 1024;

/// Per-request credentials handed to the guarded filesystem.
#[derive(Debug, Clone)]
pub struct RequestCredentials {
    pub share_id: String,
    pub permissions: SharePermissions,
}

impl RequestCredentials {
    fn require_read(&self) -> FsResult<()> {
        if self.permissions.can_read {
            Ok(())
        } else {
            Err(FsError::Forbidden)
        }
    }

    fn require_write(&self) -> FsResult<()> {
        if self.permissions.can_write {
            Ok(())
        } else {
            Err(FsError::Forbidden)
        }
    }

    fn require_delete(&self) -> FsResult<()> {
        if self.permissions.can_delete {
            Ok(())
        } else {
            Err(FsError::Forbidden)
        }
    }

    /// Metadata is needed internally by PUT/DELETE/MOVE even when the client
    /// only holds write or delete permission. The HTTP gate still enforces
    /// read permission for GET/PROPFIND/HEAD, so this does not expose listings.
    fn require_any(&self) -> FsResult<()> {
        if self.permissions.can_read || self.permissions.can_write || self.permissions.can_delete {
            Ok(())
        } else {
            Err(FsError::Forbidden)
        }
    }

    /// Directory listing is also needed internally for recursive DELETE, so
    /// delete permission is sufficient here. The HTTP gate still requires read
    /// permission for PROPFIND, so this does not let delete-only clients list.
    fn require_listing(&self) -> FsResult<()> {
        if self.permissions.can_read || self.permissions.can_delete {
            Ok(())
        } else {
            Err(FsError::Forbidden)
        }
    }
}

/// A share as the filesystem sees it. The root descriptor is opened once and
/// reused for every request.
#[derive(Debug, Clone)]
pub struct ShareConfig {
    pub id: String,
    pub name: String,
    pub root_path: PathBuf,
    pub permissions: SharePermissions,
    pub rules: Vec<CompiledRule>,
    pub display_rules: Vec<AccessRule>,
    pub root: Arc<ShareRoot>,
}

impl ShareConfig {
    pub fn info(&self) -> ShareInfo {
        ShareInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            root_path: self.root_path.display().to_string(),
            permissions: self.permissions,
            rules: self.display_rules.clone(),
        }
    }
}

pub type ShareMap = HashMap<String, Arc<ShareConfig>>;

/// Open share directory handle.
#[cfg(unix)]
#[derive(Debug)]
pub struct ShareRoot {
    fd: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl ShareRoot {
    pub fn open(path: &Path) -> Result<Self, DomainError> {
        use rustix::fs::{Mode, OFlags, openat};

        let canonical = std::fs::canonicalize(path).map_err(|source| DomainError::Io {
            operation: "resolve share root",
            path: Some(path.to_path_buf()),
            source,
        })?;
        let metadata = std::fs::metadata(&canonical).map_err(|source| DomainError::Io {
            operation: "inspect share root",
            path: Some(canonical.clone()),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(DomainError::InvalidShareConfig {
                detail: format!("root path `{}` is not a directory", canonical.display()),
            });
        }

        let fd = openat(
            rustix::fs::CWD,
            &canonical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| DomainError::Io {
            operation: "open share root",
            path: Some(canonical.clone()),
            source: source.into(),
        })?;

        Ok(Self { fd })
    }
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct ShareRoot {
    _private: (),
}

#[cfg(not(unix))]
impl ShareRoot {
    pub fn open(path: &Path) -> Result<Self, DomainError> {
        Err(DomainError::InvalidShareConfig {
            detail: format!(
                "secure share filesystem is not implemented on this platform (`{}`)",
                path.display()
            ),
        })
    }
}

/// The guarded filesystem served by the WebDAV handler. It owns no mutable
/// state; the share map is shared with the server controller.
#[derive(Debug, Clone)]
pub struct RestrictedFs {
    shares: Arc<RwLock<ShareMap>>,
}

impl RestrictedFs {
    pub fn new(shares: Arc<RwLock<ShareMap>>) -> Self {
        Self { shares }
    }

    fn share(&self, share_id: &str) -> FsResult<Arc<ShareConfig>> {
        let shares = self.shares.read().map_err(|_| FsError::GeneralFailure)?;
        shares.get(share_id).cloned().ok_or(FsError::Forbidden)
    }
}

/// Splits `/share/<id>/<relative>` into its share id and the share-relative
/// path. `DavPath` has already normalized and percent-decoded the path.
pub fn split_share_path(path: &DavPath) -> Option<(String, PathBuf)> {
    let raw = path.as_bytes();
    let rest = raw.strip_prefix(crate::types::SHARE_URL_PREFIX.as_bytes())?;
    let split = rest
        .iter()
        .position(|byte| *byte == b'/')
        .unwrap_or(rest.len());
    let id_bytes = &rest[..split];
    if id_bytes.is_empty() || id_bytes.len() > 64 {
        return None;
    }
    if !id_bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    {
        return None;
    }
    let id = std::str::from_utf8(id_bytes).ok()?.to_owned();

    let relative_bytes = if split == rest.len() {
        b"/".as_slice()
    } else {
        &rest[split..]
    };
    if relative_bytes
        .iter()
        .any(|byte| *byte == 0 || *byte == b'\\')
    {
        return None;
    }

    let relative = relative_bytes.strip_prefix(b"/").unwrap_or(relative_bytes);
    Some((id, path_from_bytes(relative)))
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// One resolved path below a share root.
#[cfg(unix)]
struct Resolved {
    parent: std::os::fd::OwnedFd,
    name: Option<std::ffi::OsString>,
}

#[cfg(unix)]
fn split_components(relative: &Path) -> FsResult<Vec<std::ffi::OsString>> {
    use std::path::Component;

    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                if name.is_empty() {
                    return Err(FsError::Forbidden);
                }
                components.push(name.to_owned());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FsError::Forbidden);
            }
        }
    }
    Ok(components)
}

#[cfg(unix)]
fn resolve(root: &ShareRoot, relative: &Path) -> FsResult<Resolved> {
    use rustix::fs::{Mode, OFlags, openat};

    let components = split_components(relative)?;
    let mut current = root.fd.try_clone().map_err(|_| FsError::GeneralFailure)?;
    let Some((name, parents)) = components.split_last() else {
        return Ok(Resolved {
            parent: current,
            name: None,
        });
    };

    for component in parents {
        let next = openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno)?;
        current = next;
    }

    Ok(Resolved {
        parent: current,
        name: Some(name.clone()),
    })
}

#[cfg(unix)]
fn errno(error: rustix::io::Errno) -> FsError {
    use rustix::io::Errno;

    let mapped = match error {
        Errno::NOENT => FsError::NotFound,
        Errno::EXIST => FsError::Exists,
        Errno::ACCESS | Errno::PERM | Errno::ISDIR | Errno::NOTDIR | Errno::LOOP | Errno::INVAL => {
            FsError::Forbidden
        }
        Errno::NAMETOOLONG => FsError::PathTooLong,
        Errno::NOSPC => FsError::InsufficientStorage,
        Errno::XDEV => FsError::IsRemote,
        _ => FsError::GeneralFailure,
    };
    log::debug!("peerdav.fs syscall error: {error:?} mapped to {mapped:?}");
    mapped
}

#[derive(Debug, Clone)]
struct Meta {
    len: u64,
    modified: SystemTime,
    is_dir: bool,
}

impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.len
    }

    fn modified(&self) -> FsResult<SystemTime> {
        Ok(self.modified)
    }

    fn is_dir(&self) -> bool {
        self.is_dir
    }
}

#[cfg(unix)]
fn stat_meta(stat: &rustix::fs::Stat) -> FsResult<Meta> {
    let file_type = rustix::fs::FileType::from_raw_mode(stat.st_mode as _);
    if file_type.is_symlink() {
        return Err(FsError::Forbidden);
    }

    let seconds = u64::try_from(stat.st_mtime).unwrap_or(0);
    let nanos = u32::try_from(stat.st_mtime_nsec)
        .unwrap_or(0)
        .min(999_999_999);
    let modified = SystemTime::UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanos))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    Ok(Meta {
        len: u64::try_from(stat.st_size).unwrap_or(0),
        modified,
        is_dir: file_type.is_dir(),
    })
}

#[cfg(unix)]
fn file_meta(metadata: &std::fs::Metadata) -> FsResult<Meta> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(FsError::Forbidden);
    }
    let modified = metadata.modified().map_err(|_| FsError::GeneralFailure)?;
    Ok(Meta {
        len: metadata.len(),
        modified,
        is_dir: metadata.is_dir(),
    })
}

#[cfg(unix)]
fn metadata_for(share: &ShareConfig, relative: &Path) -> FsResult<Meta> {
    let resolved = resolve(&share.root, relative)?;
    match resolved.name {
        None => {
            let file = File::from(resolved.parent);
            let metadata = file.metadata().map_err(|_| FsError::GeneralFailure)?;
            file_meta(&metadata)
        }
        Some(name) => {
            let stat = rustix::fs::statat(
                &resolved.parent,
                &name,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(errno)?;
            stat_meta(&stat)
        }
    }
}

#[cfg(unix)]
fn open_regular_at(
    parent: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    flags: rustix::fs::OFlags,
    mode: rustix::fs::Mode,
    allow_missing: bool,
) -> FsResult<File> {
    use rustix::fs::{AtFlags, FileType, OFlags, openat, statat};

    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            let file_type = FileType::from_raw_mode(stat.st_mode as _);
            if !file_type.is_file() {
                return Err(FsError::Forbidden);
            }
        }
        Err(rustix::io::Errno::NOENT) => {
            if !allow_missing {
                return Err(FsError::NotFound);
            }
        }
        Err(error) => return Err(errno(error)),
    }

    let fd = openat(parent, name, flags | OFlags::NONBLOCK, mode).map_err(errno)?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(|_| FsError::GeneralFailure)?;
    if !metadata.is_file() {
        return Err(FsError::Forbidden);
    }
    Ok(file)
}

#[cfg(unix)]
fn open_file(
    share: &ShareConfig,
    relative: &Path,
    options: OpenOptions,
    credentials: &RequestCredentials,
) -> FsResult<Box<dyn DavFile>> {
    use rustix::fs::{Mode, OFlags};

    if options.read || !options.write {
        credentials.require_read()?;
    }
    if options.write || options.create || options.create_new || options.truncate || options.append {
        credentials.require_write()?;
    }

    let resolved = resolve(&share.root, relative)?;
    let Some(name) = resolved.name else {
        return Err(FsError::Forbidden);
    };

    let mut flags = OFlags::CLOEXEC | OFlags::NOFOLLOW;
    flags |= match (options.read, options.write) {
        (true, false) => OFlags::RDONLY,
        (false, true) => OFlags::WRONLY,
        (true, true) => OFlags::RDWR,
        (false, false) => OFlags::RDONLY,
    };
    if options.append {
        flags |= OFlags::APPEND;
    }
    if options.truncate {
        flags |= OFlags::TRUNC;
    }
    if options.create || options.create_new {
        flags |= OFlags::CREATE;
    }
    if options.create_new {
        flags |= OFlags::EXCL;
    }

    let file = open_regular_at(
        &resolved.parent,
        &name,
        flags,
        Mode::from_bits_truncate(0o644),
        options.create || options.create_new,
    )?;
    Ok(Box::new(DavFileHandle { file }))
}

#[cfg(not(unix))]
fn open_file(
    _share: &ShareConfig,
    _relative: &Path,
    _options: OpenOptions,
    _credentials: &RequestCredentials,
) -> FsResult<Box<dyn DavFile>> {
    Err(FsError::Forbidden)
}

#[derive(Debug)]
struct DavFileHandle {
    file: File,
}

impl DavFile for DavFileHandle {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let metadata = self.file.metadata().map_err(|_| FsError::GeneralFailure)?;
            Ok(Box::new(file_meta(&metadata)?) as Box<dyn DavMetaData>)
        })
    }

    fn write_buf(&mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async move {
            while buf.has_remaining() {
                let chunk = buf.copy_to_bytes(buf.remaining().min(READ_CHUNK_LIMIT));
                self.file
                    .write_all(&chunk)
                    .map_err(|_| FsError::GeneralFailure)?;
            }
            Ok(())
        })
    }

    fn write_bytes(&mut self, buf: Bytes) -> FsFuture<'_, ()> {
        Box::pin(async move {
            self.file
                .write_all(&buf)
                .map_err(|_| FsError::GeneralFailure)
        })
    }

    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        Box::pin(async move {
            let size = count.min(READ_CHUNK_LIMIT);
            let mut buffer = vec![0_u8; size];
            let read = self
                .file
                .read(&mut buffer)
                .map_err(|_| FsError::GeneralFailure)?;
            buffer.truncate(read);
            Ok(Bytes::from(buffer))
        })
    }

    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move { self.file.seek(pos).map_err(|_| FsError::GeneralFailure) })
    }

    fn flush(&mut self) -> FsFuture<'_, ()> {
        Box::pin(async move { self.file.flush().map_err(|_| FsError::GeneralFailure) })
    }
}

#[derive(Debug, Clone)]
struct DirEntry {
    name: Vec<u8>,
    meta: Meta,
}

impl DavDirEntry for DirEntry {
    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move { Ok(Box::new(self.meta.clone()) as Box<dyn DavMetaData>) })
    }

    fn is_dir(&self) -> FsFuture<'_, bool> {
        Box::pin(async move { Ok(self.meta.is_dir) })
    }

    fn is_file(&self) -> FsFuture<'_, bool> {
        Box::pin(async move { Ok(!self.meta.is_dir) })
    }

    fn is_symlink(&self) -> FsFuture<'_, bool> {
        Box::pin(async move { Ok(false) })
    }
}

#[cfg(unix)]
fn read_entries(share: &ShareConfig, relative: &Path) -> FsResult<Vec<DirEntry>> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let resolved = resolve(&share.root, relative)?;
    let dir_fd = if let Some(name) = &resolved.name {
        rustix::fs::openat(
            &resolved.parent,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(errno)?
    } else {
        resolved.parent
    };

    let mut entries = Vec::new();
    let directory = rustix::fs::Dir::read_from(&dir_fd).map_err(errno)?;
    for entry in directory {
        let entry = entry.map_err(errno)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if entry.file_type().is_symlink() {
            continue;
        }
        let stat = match rustix::fs::statat(
            &dir_fd,
            OsString::from_vec(name.to_vec()),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(error) => {
                log::debug!("peerdav.fs skip unreadable entry: {error:?}");
                continue;
            }
        };
        let meta = match stat_meta(&stat) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        entries.push(DirEntry {
            name: name.to_vec(),
            meta,
        });
    }
    Ok(entries)
}

#[cfg(not(unix))]
fn read_entries(_share: &ShareConfig, _relative: &Path) -> FsResult<Vec<DirEntry>> {
    Err(FsError::Forbidden)
}

impl GuardedFileSystem<RequestCredentials> for RestrictedFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let (share_id, relative) = split_share_path(path).ok_or(FsError::Forbidden)?;
            if share_id != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&share_id)?;
            open_file(&share, &relative, options, credentials)
        })
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            credentials.require_listing()?;
            let (share_id, relative) = split_share_path(path).ok_or(FsError::Forbidden)?;
            if share_id != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&share_id)?;
            let entries = read_entries(&share, &relative)?;
            let stream: Vec<FsResult<Box<dyn DavDirEntry>>> = entries
                .into_iter()
                .map(|entry| Ok(Box::new(entry) as Box<dyn DavDirEntry>))
                .collect();
            let stream: FsStream<Box<dyn DavDirEntry>> = Box::pin(stream::iter(stream));
            Ok(stream)
        })
    }

    fn metadata<'a>(
        &'a self,
        path: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            credentials.require_any()?;
            let (share_id, relative) = split_share_path(path).ok_or(FsError::Forbidden)?;
            if share_id != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&share_id)?;
            Ok(Box::new(metadata_for(&share, &relative)?) as Box<dyn DavMetaData>)
        })
    }

    fn symlink_metadata<'a>(
        &'a self,
        path: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        self.metadata(path, credentials)
    }

    fn create_dir<'a>(
        &'a self,
        path: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, ()> {
        Box::pin(async move {
            credentials.require_write()?;
            let (share_id, relative) = split_share_path(path).ok_or(FsError::Forbidden)?;
            if share_id != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&share_id)?;
            create_dir_at(&share, &relative)
        })
    }

    fn remove_dir<'a>(
        &'a self,
        path: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, ()> {
        Box::pin(async move {
            credentials.require_delete()?;
            let (share_id, relative) = split_share_path(path).ok_or(FsError::Forbidden)?;
            if share_id != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&share_id)?;
            remove_at(&share, &relative, true)
        })
    }

    fn remove_file<'a>(
        &'a self,
        path: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, ()> {
        Box::pin(async move {
            credentials.require_delete()?;
            let (share_id, relative) = split_share_path(path).ok_or(FsError::Forbidden)?;
            if share_id != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&share_id)?;
            remove_at(&share, &relative, false)
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a DavPath,
        to: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, ()> {
        Box::pin(async move {
            credentials.require_delete()?;
            credentials.require_write()?;
            let (from_share, from_relative) = split_share_path(from).ok_or(FsError::Forbidden)?;
            let (to_share, to_relative) = split_share_path(to).ok_or(FsError::Forbidden)?;
            if from_share != credentials.share_id || to_share != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&from_share)?;
            rename_at(&share, &from_relative, &to_relative)
        })
    }

    fn copy<'a>(
        &'a self,
        from: &'a DavPath,
        to: &'a DavPath,
        credentials: &'a RequestCredentials,
    ) -> FsFuture<'a, ()> {
        Box::pin(async move {
            credentials.require_read()?;
            credentials.require_write()?;
            let (from_share, from_relative) = split_share_path(from).ok_or(FsError::Forbidden)?;
            let (to_share, to_relative) = split_share_path(to).ok_or(FsError::Forbidden)?;
            if from_share != credentials.share_id || to_share != credentials.share_id {
                return Err(FsError::Forbidden);
            }
            let share = self.share(&from_share)?;
            copy_at(&share, &from_relative, &to_relative)
        })
    }
}

#[cfg(unix)]
fn create_dir_at(share: &ShareConfig, relative: &Path) -> FsResult<()> {
    use rustix::fs::{Mode, mkdirat};

    let resolved = resolve(&share.root, relative)?;
    let Some(name) = resolved.name else {
        return Err(FsError::Exists);
    };
    mkdirat(&resolved.parent, &name, Mode::from_bits_truncate(0o755)).map_err(errno)
}

#[cfg(not(unix))]
fn create_dir_at(_share: &ShareConfig, _relative: &Path) -> FsResult<()> {
    Err(FsError::Forbidden)
}

#[cfg(unix)]
fn remove_at(share: &ShareConfig, relative: &Path, directory: bool) -> FsResult<()> {
    use rustix::fs::{AtFlags, unlinkat};

    let resolved = resolve(&share.root, relative)?;
    let Some(name) = resolved.name else {
        return Err(FsError::Forbidden);
    };
    let flags = if directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    unlinkat(&resolved.parent, &name, flags).map_err(errno)
}

#[cfg(not(unix))]
fn remove_at(_share: &ShareConfig, _relative: &Path, _directory: bool) -> FsResult<()> {
    Err(FsError::Forbidden)
}

#[cfg(unix)]
fn rename_at(share: &ShareConfig, from: &Path, to: &Path) -> FsResult<()> {
    use rustix::fs::renameat;

    let source = resolve(&share.root, from)?;
    let target = resolve(&share.root, to)?;
    let (Some(source_name), Some(target_name)) = (source.name, target.name) else {
        return Err(FsError::Forbidden);
    };
    renameat(&source.parent, &source_name, &target.parent, &target_name).map_err(errno)
}

#[cfg(not(unix))]
fn rename_at(_share: &ShareConfig, _from: &Path, _to: &Path) -> FsResult<()> {
    Err(FsError::Forbidden)
}

/// Commit a verified upload through the same descriptor-relative path checks
/// as WebDAV. The temporary sibling is never visible at the destination name.
#[cfg(all(unix, test))]
pub(crate) fn commit_upload(
    share: &ShareConfig,
    relative: &Path,
    source: &Path,
    session: &crate::types::UploadSession,
) -> Result<(), DomainError> {
    commit_upload_with(share, relative, session, |target| {
        let mut input = File::open(source)?;
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            if *session.cancel.borrow() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "upload canceled",
                ));
            }
            let len = input.read(&mut buffer)?;
            if len == 0 {
                break;
            }
            target.write_all(&buffer[..len])?;
        }
        Ok(())
    })
}

/// Merge and verify directly into the descriptor-relative destination sibling.
#[cfg(unix)]
pub(crate) fn commit_upload_parts(
    share: &ShareConfig,
    relative: &Path,
    directory: &Path,
    size: u64,
    digest: &str,
    session: &crate::types::UploadSession,
) -> Result<(), DomainError> {
    use sha2::{Digest, Sha256};
    commit_upload_with(share, relative, session, |target| {
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        for index in 0..size.div_ceil(crate::types::TRANSFER_PART_BYTES).max(1) {
            let path = directory.join(index.to_string());
            let expected = size
                .saturating_sub(index * crate::types::TRANSFER_PART_BYTES)
                .min(crate::types::TRANSFER_PART_BYTES);
            let fd = rustix::fs::open(
                &path,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?;
            let mut input = File::from(fd);
            let metadata = input.metadata()?;
            if !metadata.is_file() || metadata.len() != expected {
                return Err(std::io::Error::other("upload part is incomplete"));
            }
            let mut remaining = expected;
            while remaining > 0 {
                if *session.cancel.borrow() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "upload canceled",
                    ));
                }
                let length = remaining.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..length])?;
                hash.update(&buffer[..length]);
                target.write_all(&buffer[..length])?;
                remaining -= length as u64;
            }
        }
        if hex::encode(hash.finalize()) != digest {
            return Err(std::io::Error::other(
                "upload content changed; retry with the current file",
            ));
        }
        Ok(())
    })
}

#[cfg(not(unix))]
pub(crate) fn commit_upload_parts(
    _share: &ShareConfig,
    _relative: &Path,
    _directory: &Path,
    _size: u64,
    _digest: &str,
    _session: &crate::types::UploadSession,
) -> Result<(), DomainError> {
    Err(DomainError::InvalidShareConfig {
        detail: "secure uploads are unavailable on this platform".into(),
    })
}

#[cfg(unix)]
fn commit_upload_with(
    share: &ShareConfig,
    relative: &Path,
    session: &crate::types::UploadSession,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<(), DomainError> {
    use rustix::fs::{Mode, OFlags};
    let resolved =
        resolve(&share.root, relative).map_err(|error| DomainError::InvalidShareConfig {
            detail: format!("upload destination: {error:?}"),
        })?;
    let Some(name) = resolved.name else {
        return Err(DomainError::InvalidShareConfig {
            detail: "cannot replace a share root".into(),
        });
    };
    // Reject links and non-files at the destination before atomic replacement.
    match rustix::fs::statat(
        &resolved.parent,
        &name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) if !rustix::fs::FileType::from_raw_mode(stat.st_mode as _).is_file() => {
            return Err(DomainError::InvalidShareConfig {
                detail: "upload destination is not a regular file".into(),
            });
        }
        Ok(_) => {}
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => {
            return Err(DomainError::Io {
                operation: "inspect upload destination",
                path: None,
                source: error.into(),
            });
        }
    }
    let mut random = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let temporary = format!(".peerdav-{}.part", hex::encode(random));
    let fd = rustix::fs::openat(
        &resolved.parent,
        &temporary,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|error| DomainError::Io {
        operation: "stage upload",
        path: None,
        source: error.into(),
    })?;
    let result = (|| {
        let mut target = File::from(fd);
        write(&mut target)?;
        target.sync_all()?;
        // Serialize cancel and atomic replacement. Once renamed, cancellation
        // reports completion and never removes the destination.
        let mut outcome = session
            .outcome
            .lock()
            .map_err(|_| std::io::Error::other("upload state lock poisoned"))?;
        if *outcome == crate::types::UploadOutcome::Canceled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "upload canceled",
            ));
        }
        rustix::fs::renameat(&resolved.parent, &temporary, &resolved.parent, &name)
            .map_err(std::io::Error::from)?;
        *outcome = crate::types::UploadOutcome::Completed;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(source) = result {
        if let Err(error) =
            rustix::fs::unlinkat(&resolved.parent, &temporary, rustix::fs::AtFlags::empty())
        {
            log::warn!("failed to remove upload staging entry: {error}");
        }
        if source.kind() == std::io::ErrorKind::Interrupted && *session.cancel.borrow() {
            return Err(DomainError::TransferCanceled);
        }
        return Err(DomainError::Io {
            operation: "commit upload",
            path: None,
            source,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn copy_at(share: &ShareConfig, from: &Path, to: &Path) -> FsResult<()> {
    use rustix::fs::{Mode, OFlags};

    let source = resolve(&share.root, from)?;
    let target = resolve(&share.root, to)?;
    let (Some(source_name), Some(target_name)) = (source.name, target.name) else {
        return Err(FsError::Forbidden);
    };

    let mut source_file = open_regular_at(
        &source.parent,
        &source_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        false,
    )?;
    let mut target_file = open_regular_at(
        &target.parent,
        &target_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o644),
        true,
    )?;

    std::io::copy(&mut source_file, &mut target_file)
        .map(|_| ())
        .map_err(|_| FsError::GeneralFailure)
}

#[cfg(not(unix))]
fn copy_at(_share: &ShareConfig, _from: &Path, _to: &Path) -> FsResult<()> {
    Err(FsError::Forbidden)
}

#[cfg(all(test, unix))]
mod upload_tests {
    use super::*;
    use crate::types::{UploadOutcome, UploadSession};

    #[test]
    fn cancel_during_staging_copy_preserves_destination_and_removes_sibling() {
        let directory = tempfile::tempdir().expect("test directory");
        let source = directory.path().join("source");
        File::create(&source)
            .expect("source")
            .set_len(128 * 1024 * 1024)
            .expect("sparse source");
        let root = directory.path().join("share");
        std::fs::create_dir(&root).expect("share");
        std::fs::write(root.join("target"), b"original").expect("original");
        let share = ShareConfig {
            id: "test".into(),
            name: "test".into(),
            root_path: root.clone(),
            permissions: SharePermissions {
                can_read: true,
                can_write: true,
                can_delete: true,
            },
            rules: Vec::new(),
            display_rules: Vec::new(),
            root: Arc::new(ShareRoot::open(&root).expect("root")),
        };
        let (cancel, _) = tokio::sync::watch::channel(false);
        let session = Arc::new(UploadSession {
            cancel,
            active: Arc::new(tokio::sync::RwLock::new(())),
            outcome: std::sync::Mutex::new(UploadOutcome::Active),
        });
        let copying_session = Arc::clone(&session);
        let copying = std::thread::spawn(move || {
            commit_upload(&share, Path::new("target"), &source, &copying_session)
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let writing = std::fs::read_dir(&root).expect("entries").any(|entry| {
                let entry = entry.expect("entry");
                entry.file_name().to_string_lossy().starts_with(".peerdav-")
                    && entry.metadata().expect("metadata").len() > 0
            });
            if writing {
                let mut outcome = session.outcome.lock().expect("outcome");
                *outcome = UploadOutcome::Canceled;
                session.cancel.send_replace(true);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "copy never started");
            std::thread::yield_now();
        }
        assert!(matches!(
            copying.join().expect("copy thread"),
            Err(DomainError::TransferCanceled)
        ));
        assert_eq!(
            std::fs::read(root.join("target")).expect("retained"),
            b"original"
        );
        assert_eq!(std::fs::read_dir(&root).expect("clean root").count(), 1);
    }
}
