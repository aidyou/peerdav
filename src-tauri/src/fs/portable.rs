use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use dav_server::fs::{DavFile, FsError, FsResult};

use super::{DavFileHandle, DirEntry, Meta, RequestCredentials, ShareConfig, ShareRoot};
use crate::error::DomainError;
use crate::types::{UploadOutcome, UploadSession};

fn components(relative: &Path) -> FsResult<PathBuf> {
    let mut path = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                #[cfg(windows)]
                {
                    let value = name.to_str().ok_or(FsError::Forbidden)?;
                    let stem = value.split('.').next().unwrap_or("").to_ascii_uppercase();
                    if value.contains([':', '\\', '*', '?', '"', '<', '>', '|'])
                        || value.ends_with(['.', ' '])
                        || matches!(
                            stem.as_str(),
                            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                        )
                        || (stem.len() == 4
                            && (stem.starts_with("COM") || stem.starts_with("LPT"))
                            && stem.as_bytes()[3].is_ascii_digit())
                    {
                        return Err(FsError::Forbidden);
                    }
                }
                path.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FsError::Forbidden);
            }
        }
    }
    Ok(path)
}

fn io_error(error: std::io::Error) -> FsError {
    log::debug!("peerdav.fs IO error: {error:?}");
    match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound,
        std::io::ErrorKind::AlreadyExists => FsError::Exists,
        std::io::ErrorKind::PermissionDenied
        | std::io::ErrorKind::InvalidInput
        | std::io::ErrorKind::InvalidData => FsError::Forbidden,
        std::io::ErrorKind::WriteZero => FsError::InsufficientStorage,
        _ => FsError::GeneralFailure,
    }
}

fn cap_meta(metadata: &cap_std::fs::Metadata) -> FsResult<Meta> {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(FsError::Forbidden);
        }
    }
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(FsError::Forbidden);
    }
    let modified = metadata.modified().map_err(io_error)?.into_std();
    Ok(Meta {
        len: metadata.len(),
        modified,
        is_dir: metadata.is_dir(),
    })
}
fn open_dir(root: &ShareRoot, relative: &Path) -> FsResult<Dir> {
    let path = components(relative)?;
    let mut directory = root.directory.try_clone().map_err(io_error)?;
    for component in path.components() {
        directory = directory
            .open_dir_nofollow(component.as_os_str())
            .map_err(io_error)?;
        cap_meta(&directory.dir_metadata().map_err(io_error)?)?;
    }
    Ok(directory)
}

fn parent(root: &ShareRoot, relative: &Path) -> FsResult<(Dir, std::ffi::OsString)> {
    let path = components(relative)?;
    let name = path.file_name().ok_or(FsError::Forbidden)?.to_owned();
    let directory = open_dir(root, path.parent().unwrap_or_else(|| Path::new(".")))?;
    Ok((directory, name))
}

#[cfg(not(unix))]
impl ShareRoot {
    pub fn open(path: &Path) -> Result<Self, DomainError> {
        let directory =
            Dir::open_ambient_dir(path, cap_std::ambient_authority()).map_err(|source| {
                DomainError::Io {
                    operation: "open share root",
                    path: Some(path.to_path_buf()),
                    source,
                }
            })?;
        Ok(Self { directory })
    }
}

pub(super) fn metadata_for(share: &ShareConfig, relative: &Path) -> FsResult<Meta> {
    let path = components(relative)?;
    let metadata = if path.as_os_str().is_empty() {
        share.root.directory.dir_metadata().map_err(io_error)?
    } else {
        let (directory, name) = parent(&share.root, &path)?;
        directory.symlink_metadata(name).map_err(io_error)?
    };
    cap_meta(&metadata)
}

pub(super) fn open_file(
    share: &ShareConfig,
    relative: &Path,
    options: dav_server::fs::OpenOptions,
    credentials: &RequestCredentials,
) -> FsResult<Box<dyn DavFile>> {
    if options.read || !options.write {
        credentials.require_read()?;
    }
    if options.write || options.create || options.create_new || options.truncate || options.append {
        credentials.require_write()?;
    }
    let path = components(relative)?;
    if path.as_os_str().is_empty() {
        return Err(FsError::Forbidden);
    }
    let mut open = OpenOptions::new();
    open.read(options.read || !options.write)
        .write(options.write)
        .create(options.create)
        .create_new(options.create_new)
        .truncate(options.truncate)
        .append(options.append);
    open.follow(FollowSymlinks::No);
    let (directory, name) = parent(&share.root, &path)?;
    match directory.symlink_metadata(&name) {
        Ok(metadata) => {
            cap_meta(&metadata)?;
            if !metadata.is_file() {
                return Err(FsError::Forbidden);
            }
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && (options.create || options.create_new) => {}
        Err(error) => return Err(io_error(error)),
    }
    let file = directory.open_with(name, &open).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    cap_meta(&metadata)?;
    if !metadata.is_file() {
        return Err(FsError::Forbidden);
    }
    Ok(Box::new(DavFileHandle {
        file: file.into_std(),
    }))
}

pub(super) fn read_entries(share: &ShareConfig, relative: &Path) -> FsResult<Vec<DirEntry>> {
    let directory = open_dir(&share.root, relative)?;
    let mut entries = Vec::new();
    for entry in directory.read_dir(".").map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        let metadata = match directory.symlink_metadata(&name) {
            Ok(metadata) if !metadata.file_type().is_symlink() => metadata,
            Ok(_) => continue,
            Err(error) => {
                log::debug!("peerdav.fs skip unreadable entry: {error}");
                continue;
            }
        };
        let meta = match cap_meta(&metadata) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        entries.push(DirEntry {
            name: name.to_string_lossy().into_owned().into_bytes(),
            meta,
        });
    }
    Ok(entries)
}

pub(super) fn create_dir_at(share: &ShareConfig, relative: &Path) -> FsResult<()> {
    let path = components(relative)?;
    if path.as_os_str().is_empty() {
        return Err(FsError::Exists);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or(FsError::Forbidden)?;
    let directory = open_dir(&share.root, parent)?;
    directory.create_dir(name).map_err(io_error)
}

pub(super) fn remove_at(share: &ShareConfig, relative: &Path, directory: bool) -> FsResult<()> {
    let path = components(relative)?;
    if path.as_os_str().is_empty() {
        return Err(FsError::Forbidden);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or(FsError::Forbidden)?;
    let directory_handle = open_dir(&share.root, parent)?;
    if directory {
        directory_handle.remove_dir(name).map_err(io_error)
    } else {
        directory_handle.remove_file(name).map_err(io_error)
    }
}

pub(super) fn rename_at(share: &ShareConfig, from: &Path, to: &Path) -> FsResult<()> {
    let from = components(from)?;
    let to = components(to)?;
    if from.as_os_str().is_empty() || to.as_os_str().is_empty() {
        return Err(FsError::Forbidden);
    }
    let source_parent = open_dir(&share.root, from.parent().unwrap_or_else(|| Path::new(".")))?;
    let target_parent = open_dir(&share.root, to.parent().unwrap_or_else(|| Path::new(".")))?;
    source_parent
        .rename(
            from.file_name().ok_or(FsError::Forbidden)?,
            &target_parent,
            to.file_name().ok_or(FsError::Forbidden)?,
        )
        .map_err(io_error)
}

pub(super) fn copy_at(share: &ShareConfig, from: &Path, to: &Path) -> FsResult<()> {
    let from = components(from)?;
    let to = components(to)?;
    if from.as_os_str().is_empty() || to.as_os_str().is_empty() {
        return Err(FsError::Forbidden);
    }
    let source_parent = open_dir(&share.root, from.parent().unwrap_or_else(|| Path::new(".")))?;
    let mut read_options = OpenOptions::new();
    read_options.read(true).follow(FollowSymlinks::No);
    let source = source_parent
        .open_with(from.file_name().ok_or(FsError::Forbidden)?, &read_options)
        .map_err(io_error)?;
    if !source.metadata().map_err(io_error)?.is_file() {
        return Err(FsError::Forbidden);
    }
    let target_parent = open_dir(&share.root, to.parent().unwrap_or_else(|| Path::new(".")))?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .follow(FollowSymlinks::No);
    let mut target = target_parent
        .open_with(to.file_name().ok_or(FsError::Forbidden)?, &options)
        .map_err(io_error)?;
    let mut source = source;
    std::io::copy(&mut source, &mut target)
        .map(|_| ())
        .map_err(io_error)
}

pub(super) fn commit_upload_parts(
    share: &ShareConfig,
    relative: &Path,
    directory: &Path,
    size: u64,
    digest: &str,
    session: &UploadSession,
) -> Result<(), DomainError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let destination_error = |error| DomainError::Io {
        operation: "commit upload",
        path: None,
        source: error,
    };
    let (destination, name) = parent(&share.root, relative).map_err(|error| {
        destination_error(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            error,
        ))
    })?;
    match destination.symlink_metadata(&name) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(destination_error(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "upload destination is not a regular file",
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(destination_error(error)),
    }
    let mut random = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let temporary = format!(".peerdav-{}.part", hex::encode(random));
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut target = destination
        .open_with(&temporary, &options)
        .map_err(destination_error)?;
    let result = (|| {
        let source = Dir::open_ambient_dir(directory, cap_std::ambient_authority())?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        for index in 0..size.div_ceil(crate::types::TRANSFER_PART_BYTES).max(1) {
            let expected = size
                .saturating_sub(index * crate::types::TRANSFER_PART_BYTES)
                .min(crate::types::TRANSFER_PART_BYTES);
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let mut input = source.open_with(index.to_string(), &options)?;
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
        target.sync_all()?;
        // Close before replacement on Windows; the parent capability remains open.
        drop(target);
        let mut outcome = session
            .outcome
            .lock()
            .map_err(|_| std::io::Error::other("upload state lock poisoned"))?;
        if *outcome == UploadOutcome::Canceled || *session.cancel.borrow() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "upload canceled",
            ));
        }
        destination.rename(&temporary, &destination, &name)?;
        *outcome = UploadOutcome::Completed;
        Ok(())
    })();
    if let Err(source) = result {
        if let Err(error) = destination.remove_file(&temporary) {
            log::warn!("failed to remove upload staging entry: {error}");
        }
        if source.kind() == std::io::ErrorKind::Interrupted && *session.cancel.borrow() {
            return Err(DomainError::TransferCanceled);
        }
        return Err(destination_error(source));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SharePermissions;
    use std::sync::Arc;

    fn share(path: &Path) -> ShareConfig {
        ShareConfig {
            id: "test".into(),
            name: "test".into(),
            root_path: path.to_path_buf(),
            permissions: SharePermissions {
                can_read: true,
                can_write: true,
                can_delete: true,
            },
            rules: Vec::new(),
            display_rules: Vec::new(),
            root: Arc::new(ShareRoot::open(path).expect("root")),
        }
    }

    #[tokio::test]
    async fn portable_file_and_directory_operations() {
        let directory = tempfile::tempdir().expect("directory");
        let share = share(directory.path());
        let credentials = RequestCredentials {
            share_id: share.id.clone(),
            permissions: share.permissions,
        };
        assert!(
            metadata_for(&share, Path::new(""))
                .expect("root metadata")
                .is_dir
        );
        create_dir_at(&share, Path::new("folder")).expect("mkdir");
        let mut options = dav_server::fs::OpenOptions::default();
        options.write = true;
        options.create_new = true;
        let mut file =
            open_file(&share, Path::new("folder/file"), options, &credentials).expect("open");
        file.write_bytes(bytes::Bytes::from_static(b"data"))
            .await
            .expect("write");
        file.flush().await.expect("flush");
        drop(file);
        assert_eq!(
            metadata_for(&share, Path::new("folder/file"))
                .expect("metadata")
                .len,
            4
        );
        assert_eq!(
            read_entries(&share, Path::new("folder"))
                .expect("entries")
                .len(),
            1
        );
        copy_at(&share, Path::new("folder/file"), Path::new("copy")).expect("copy");
        rename_at(&share, Path::new("copy"), Path::new("moved")).expect("rename");
        remove_at(&share, Path::new("moved"), false).expect("remove");
        remove_at(&share, Path::new("folder/file"), false).expect("remove");
        remove_at(&share, Path::new("folder"), true).expect("rmdir");
        assert!(metadata_for(&share, Path::new("../outside")).is_err());
        assert!(remove_at(&share, Path::new(""), true).is_err());
        assert!(create_dir_at(&share, Path::new("")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_device_names_streams_and_junctions() {
        for path in [
            "file:stream",
            "CON",
            "nul.txt",
            "COM1",
            "folder/name.",
            "folder/name ",
            "C:\\outside",
            "../outside",
        ] {
            assert!(components(Path::new(path)).is_err(), "accepted {path}");
        }
        let directory = tempfile::tempdir().expect("directory");
        let outside = tempfile::tempdir().expect("outside");
        let link = directory.path().join("junction");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .status()
            .expect("create junction");
        assert!(status.success());
        let share = share(directory.path());
        assert!(read_entries(&share, Path::new("junction")).is_err());
        assert!(metadata_for(&share, Path::new("junction")).is_err());
        assert!(create_dir_at(&share, Path::new("junction/new")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn portable_rejects_links_in_every_component() {
        let directory = tempfile::tempdir().expect("directory");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret"), b"secret").expect("secret");
        std::os::unix::fs::symlink(outside.path(), directory.path().join("link")).expect("link");
        let share = share(directory.path());
        assert!(metadata_for(&share, Path::new("link/secret")).is_err());
        assert!(read_entries(&share, Path::new("link")).is_err());
        assert!(create_dir_at(&share, Path::new("link/new")).is_err());
        assert!(copy_at(&share, Path::new("link/secret"), Path::new("copy")).is_err());
        assert!(
            read_entries(&share, Path::new(""))
                .expect("entries")
                .is_empty()
        );
    }

    #[test]
    fn portable_upload_verifies_and_preserves_existing_destination_on_failure() {
        use sha2::{Digest, Sha256};
        let directory = tempfile::tempdir().expect("directory");
        let parts = tempfile::tempdir().expect("parts");
        let share = share(directory.path());
        std::fs::write(parts.path().join("0"), b"data").expect("part");
        std::fs::write(directory.path().join("target"), b"original").expect("target");
        let (cancel, _) = tokio::sync::watch::channel(false);
        let session = UploadSession {
            cancel,
            active: Arc::new(tokio::sync::RwLock::new(())),
            outcome: std::sync::Mutex::new(UploadOutcome::Active),
        };
        assert!(
            commit_upload_parts(
                &share,
                Path::new("target"),
                parts.path(),
                4,
                "invalid",
                &session
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(directory.path().join("target")).expect("read"),
            b"original"
        );
        let digest = hex::encode(Sha256::digest(b"data"));
        commit_upload_parts(
            &share,
            Path::new("target"),
            parts.path(),
            4,
            &digest,
            &session,
        )
        .expect("commit");
        assert_eq!(
            std::fs::read(directory.path().join("target")).expect("read"),
            b"data"
        );
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("entries")
                .count(),
            1
        );
    }
}
