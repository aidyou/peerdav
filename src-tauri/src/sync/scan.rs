use super::chunker::chunk_reader;
use super::paths::validate_relative;
use super::scan_types::*;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
#[cfg(unix)]
use cap_std::fs::MetadataExt;
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;

fn io_err(path: &Path, source: io::Error) -> ScanError {
    ScanError::Io {
        path: path.to_path_buf(),
        source,
    }
}
fn canceled(options: &ScanOptions) -> Result<(), ScanError> {
    if options.cancel.load(Ordering::Relaxed) {
        Err(ScanError::Canceled)
    } else {
        Ok(())
    }
}
fn stamp(t: cap_std::time::SystemTime) -> i128 {
    t.into_std()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i128
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

fn identity(metadata: &cap_std::fs::Metadata) -> Result<Identity, ScanError> {
    #[cfg(unix)]
    {
        Ok(Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(ScanError::Unsupported)
    }
}

#[cfg(unix)]
fn ctime_stamp(metadata: &cap_std::fs::Metadata) -> i128 {
    let c = metadata.ctime();
    c as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128
}
#[cfg(not(unix))]
fn ctime_stamp(_metadata: &cap_std::fs::Metadata) -> i128 {
    0
}

fn open_root_nofollow(root: &Path) -> Result<Dir, ScanError> {
    if !root.is_absolute() {
        return Err(ScanError::InvalidPath("root must be absolute".into()));
    }
    let mut current =
        Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(|e| io_err(root, e))?;
    for component in root.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                current = current
                    .open_dir_nofollow(name)
                    .map_err(|e| io_err(root, e))?
            }
            _ => return Err(ScanError::InvalidPath("invalid root component".into())),
        }
    }
    Ok(current)
}

/// Validate a cached fingerprint using metadata only, without hashing contents.
/// This is a display hint, never an authorization for sync or deletion.
pub(crate) fn cached_file_matches(root: &Path, previous: &FileScan) -> Result<bool, ScanError> {
    validate_relative(Path::new(&previous.path))?;
    let mut dir = open_root_nofollow(root)?;
    let mut parts = Path::new(&previous.path).components();
    let name = parts
        .next_back()
        .ok_or_else(|| ScanError::InvalidPath(previous.path.clone()))?;
    for part in parts {
        match dir.open_dir_nofollow(part.as_os_str()) {
            Ok(child) => dir = child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_err(root, error)),
        }
    }
    let metadata = match dir.symlink_metadata(name.as_os_str()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_err(root, error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let id = identity(&metadata)?;
    Ok(previous.inode == id.inode
        && previous.device == id.device
        && previous.size == metadata.len()
        && metadata.modified().ok().map(stamp) == Some(previous.modified)
        && ctime_stamp(&metadata) == previous.changed)
}

pub fn scan(root: &Path, options: &ScanOptions) -> Result<ScanReport, ScanError> {
    canceled(options)?;
    let root_link = std::fs::symlink_metadata(root).map_err(|e| io_err(root, e))?;
    if root_link.file_type().is_symlink() || !root_link.is_dir() {
        return Err(ScanError::UnsafeEntry(root.display().to_string()));
    }
    let root_dir = open_root_nofollow(root)?;
    let root_meta = root_dir.dir_metadata().map_err(|e| io_err(root, e))?;
    let root_id = identity(&root_meta)?;
    if !options.ignored.is_empty() {
        return Err(ScanError::UnsupportedIgnore);
    }
    let ledger = super::scan_ledger::ledger(root)?;
    let mut ledger = ledger.lock().map_err(|_| ScanError::RootChanged)?;
    super::scan_ledger::restore(&mut ledger)?;
    let persisted = ledger.files.clone();
    let generation = ledger.generation.load(Ordering::Acquire);
    if generation != ledger.observed || ledger.failed.load(Ordering::Acquire) {
        ledger.files.clear();
    }
    let dirty = std::mem::take(&mut *ledger.dirty.lock().map_err(|_| ScanError::RootChanged)?);
    ledger.files.retain(|path, _| {
        !dirty
            .iter()
            .any(|changed| Path::new(path).starts_with(changed))
    });
    let mut report = ScanReport {
        files: Vec::new(),
        directories: vec![String::new()],
        ignored_paths: Vec::new(),
        complete: false,
    };
    let mut remaining = MAX_SCAN_WORKING_BYTES;
    walk(
        &root_dir,
        Path::new(""),
        root,
        options,
        &mut report,
        &ledger.files,
        &mut remaining,
    )?;
    canceled(options)?;
    let after = open_root_nofollow(root)?
        .dir_metadata()
        .map_err(|e| io_err(root, e))?;
    if identity(&after)? != root_id {
        return Err(ScanError::RootChanged);
    }
    report.complete = true;
    ledger.observed = generation;
    // The ledger is an optimization, not an additional unbounded copy of the
    // full scan. Oversized entries stay uncached and are verified normally.
    let mut remaining = 8 * 1024 * 1024_usize;
    ledger.files = report
        .files
        .iter()
        .filter_map(|file| {
            let bytes = file
                .path
                .len()
                .saturating_add(256)
                .saturating_add(file.blocks.len().saturating_mul(160));
            if bytes > remaining {
                return None;
            }
            remaining -= bytes;
            Some((file.path.clone(), file.clone()))
        })
        .collect();
    if let Err(error) = super::scan_ledger::persist(&ledger, &persisted) {
        ledger.files.clear();
        ledger.loaded = false;
        return Err(error);
    }
    Ok(report)
}

fn reserve_scan(remaining: &mut usize, bytes: usize) -> Result<(), ScanError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(ScanError::LimitExceeded)?;
    Ok(())
}

fn file_scan_bytes(file: &FileScan) -> usize {
    std::mem::size_of::<FileScan>()
        .saturating_add(file.path.capacity())
        .saturating_add(file.whole_file_hash.capacity())
        .saturating_add(
            file.blocks
                .capacity()
                .saturating_mul(std::mem::size_of::<ChunkScan>()),
        )
        .saturating_add(
            file.blocks
                .iter()
                .map(|block| block.signature.capacity())
                .sum::<usize>(),
        )
}

fn walk(
    dir: &Dir,
    rel: &Path,
    display_root: &Path,
    options: &ScanOptions,
    report: &mut ScanReport,
    cached: &std::collections::BTreeMap<String, FileScan>,
    remaining: &mut usize,
) -> Result<(), ScanError> {
    canceled(options)?;
    if rel.components().count() > 128 {
        return Err(ScanError::Unsupported);
    }
    let dir_before = dir.dir_metadata().map_err(|e| io_err(display_root, e))?;
    let dir_id = identity(&dir_before)?;
    let mut entries = Vec::new();
    let mut names_bytes = 0usize;
    for item in dir.read_dir(".").map_err(|e| io_err(display_root, e))? {
        let item = item.map_err(|e| io_err(display_root, e))?;
        let name = item.file_name();
        // Both original and final name lists coexist during identity validation.
        let bytes = name
            .len()
            .saturating_add(std::mem::size_of::<std::ffi::OsString>())
            .saturating_mul(4);
        reserve_scan(remaining, bytes)?;
        names_bytes = names_bytes.saturating_add(bytes);
        entries.push(name);
    }
    entries.sort();
    let original_names = entries.clone();
    for name in entries {
        canceled(options)?;
        let name_text = name
            .to_str()
            .ok_or_else(|| ScanError::UnsafeEntry(name.to_string_lossy().into()))?;
        if options.ignored.iter().any(|v| v == name_text) {
            continue;
        }
        if crate::ignore_rules::is_sync_temporary_path(name_text) {
            continue;
        }
        let child = rel.join(&name);
        validate_relative(&child)?;
        let path = display_root.join(&child);
        let metadata = dir.symlink_metadata(&name).map_err(|e| io_err(&path, e))?;
        if metadata.file_type().is_symlink() {
            return Err(ScanError::UnsafeEntry(child.display().to_string()));
        }
        if metadata.is_dir() {
            let sub = dir.open_dir_nofollow(&name).map_err(|e| io_err(&path, e))?;
            let sub_id = identity(&sub.dir_metadata().map_err(|e| io_err(&path, e))?)?;
            reserve_scan(
                remaining,
                child
                    .as_os_str()
                    .len()
                    .saturating_add(std::mem::size_of::<String>()),
            )?;
            report
                .directories
                .push(child.to_string_lossy().into_owned());
            walk(
                &sub,
                &child,
                display_root,
                options,
                report,
                cached,
                remaining,
            )?;
            let sub_after = sub.dir_metadata().map_err(|e| io_err(&path, e))?;
            if identity(&sub_after)? != sub_id {
                return Err(ScanError::UnsafeEntry(child.display().to_string()));
            }
        } else if metadata.is_file() {
            let mut open = OpenOptions::new();
            open.read(true).follow(FollowSymlinks::No);
            let mut file = dir.open_with(&name, &open).map_err(|e| io_err(&path, e))?;
            let before = file.metadata().map_err(|e| io_err(&path, e))?;
            if !before.is_file() {
                return Err(ScanError::UnsafeEntry(child.display().to_string()));
            }
            let before_id = identity(&before)?;
            if before_id != identity(&metadata)? {
                return Err(ScanError::UnsafeEntry(child.display().to_string()));
            }
            if let Some(previous) =
                cached
                    .get(child.to_string_lossy().as_ref())
                    .filter(|previous| {
                        previous.inode == before_id.inode
                            && previous.device == before_id.device
                            && previous.size == before.len()
                            && before.modified().ok().map(stamp) == Some(previous.modified)
                            && ctime_stamp(&before) == previous.changed
                    })
            {
                let after = dir.symlink_metadata(&name).map_err(|e| io_err(&path, e))?;
                if after.file_type().is_symlink()
                    || identity(&after)? != before_id
                    || after.len() != before.len()
                    || after.modified().ok() != before.modified().ok()
                    || ctime_stamp(&after) != ctime_stamp(&before)
                {
                    return Err(ScanError::UnsafeEntry(child.display().to_string()));
                }
                reserve_scan(remaining, file_scan_bytes(previous))?;
                report.files.push(previous.clone());
                continue;
            }
            reserve_scan(
                remaining,
                std::mem::size_of::<FileScan>()
                    .saturating_add(child.as_os_str().len())
                    .saturating_add(64),
            )?;
            let mut whole = Sha256::new();
            let mut blocks = Vec::new();
            for chunk in chunk_reader(&mut file) {
                canceled(options)?;
                let chunk = chunk.map_err(|e| io_err(&path, e))?;
                let digest = Sha256::digest(&chunk.data);
                let signature = hex::encode(digest);
                reserve_scan(
                    remaining,
                    std::mem::size_of::<ChunkScan>()
                        .saturating_mul(2)
                        .saturating_add(signature.len()),
                )?;
                let entry = ChunkScan {
                    offset: chunk.offset,
                    length: chunk.data.len() as u64,
                    signature: signature.clone(),
                };
                if let Some(callback) = &options.on_chunk {
                    callback(&ScanChunk {
                        path: child.to_string_lossy().into_owned(),
                        offset: entry.offset,
                        length: entry.length,
                        signature: signature.clone(),
                    });
                }
                canceled(options)?;
                whole.update(&chunk.data);
                blocks.push(entry);
                if blocks.len() > 1_000_000 {
                    return Err(ScanError::Unsupported);
                }
            }
            let after = file.metadata().map_err(|e| io_err(&path, e))?;
            let path_after = dir.symlink_metadata(&name).map_err(|e| io_err(&path, e))?;
            if path_after.file_type().is_symlink()
                || !path_after.is_file()
                || before_id != identity(&path_after)?
                || before_id != identity(&after)?
                || before.len() != after.len()
                || before.modified().ok().map(stamp) != after.modified().ok().map(stamp)
                || ctime_stamp(&before) != ctime_stamp(&after)
            {
                return Err(ScanError::UnsafeEntry(child.display().to_string()));
            }
            report.files.push(FileScan {
                path: child.to_string_lossy().into_owned(),
                inode: before_id.inode,
                device: before_id.device,
                size: before.len(),
                modified: stamp(before.modified().map_err(|e| io_err(&path, e))?),
                changed: ctime_stamp(&before),
                blocks,
                whole_file_hash: hex::encode(whole.finalize()),
            });
        } else {
            return Err(ScanError::UnsafeEntry(child.display().to_string()));
        }
    }
    canceled(options)?;
    let dir_after = dir.dir_metadata().map_err(|e| io_err(display_root, e))?;
    let mut current = open_root_nofollow(display_root)?;
    for component in rel.components() {
        current = current
            .open_dir_nofollow(component.as_os_str())
            .map_err(|e| io_err(display_root, e))?;
    }
    let current_meta = current
        .dir_metadata()
        .map_err(|e| io_err(display_root, e))?;
    let mut final_names = Vec::new();
    let mut final_bytes = 0usize;
    for item in current.read_dir(".").map_err(|e| io_err(display_root, e))? {
        let name = item.map_err(|e| io_err(display_root, e))?.file_name();
        final_bytes = final_bytes.saturating_add(
            name.len()
                .saturating_add(std::mem::size_of::<std::ffi::OsString>())
                .saturating_mul(4),
        );
        if final_bytes > names_bytes {
            return Err(ScanError::RootChanged);
        }
        final_names.push(name);
    }
    final_names.sort();
    if identity(&dir_after)? != dir_id
        || identity(&current_meta)? != dir_id
        || ctime_stamp(&dir_before) != ctime_stamp(&dir_after)
        || dir_before.modified().ok().map(stamp) != dir_after.modified().ok().map(stamp)
        || original_names != final_names
    {
        return Err(ScanError::RootChanged);
    }
    *remaining = remaining.saturating_add(names_bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_scan_records_are_not_rejected_by_old_total_count() {
        let root = tempfile::tempdir().unwrap();
        let dir = open_root_nofollow(root.path()).unwrap();
        let mut report = ScanReport {
            files: vec![],
            directories: vec!["d".into(); 100_001],
            ignored_paths: vec![],
            complete: false,
        };
        let mut remaining = MAX_SCAN_WORKING_BYTES;
        walk(
            &dir,
            Path::new(""),
            root.path(),
            &ScanOptions::default(),
            &mut report,
            &std::collections::BTreeMap::new(),
            &mut remaining,
        )
        .expect("bounded records beyond 100k");
    }

    #[test]
    fn scan_workspace_exact_limit_and_one_over() {
        let mut remaining = 32;
        reserve_scan(&mut remaining, 32).expect("exact limit");
        assert_eq!(remaining, 0);
        assert!(matches!(
            reserve_scan(&mut remaining, 1),
            Err(ScanError::LimitExceeded)
        ));
        assert_eq!(remaining, 0);
    }

    #[test]
    fn temporary_files_are_excluded_without_hiding_normal_neighbors() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            ".notes.swp",
            "video.mp4.part",
            "report.tmp",
            "#notes#",
            ".#notes",
        ] {
            std::fs::write(root.path().join(name), b"in progress").unwrap();
        }
        std::fs::create_dir(root.path().join("cache.tmp")).unwrap();
        std::fs::write(root.path().join("cache.tmp/secret.txt"), b"temporary").unwrap();
        for name in [
            "notes.txt",
            "notes.swp",
            "report.tmp.txt",
            ".env",
            "part",
            "template",
        ] {
            std::fs::write(root.path().join(name), b"complete").unwrap();
        }
        let report = scan(root.path(), &ScanOptions::default()).unwrap();
        assert!(report.complete);
        assert!(report.ignored_paths.is_empty());
        assert_eq!(report.files.len(), 6);
        assert_eq!(report.directories, [""]);
        assert!(report.files.iter().all(|file| file.size == 8));
    }
}
