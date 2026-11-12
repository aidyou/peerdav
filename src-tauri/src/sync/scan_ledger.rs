use super::{scan_ledger_types::{LEDGERS, ScanLedger}, scan_types::ScanError};
use std::{collections::BTreeMap, path::Path, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}};

pub(crate) fn ledger(root: &Path) -> Result<Arc<Mutex<ScanLedger>>, ScanError> {
    let mut ledgers = LEDGERS.get_or_init(|| Mutex::new(BTreeMap::new())).lock().map_err(|_| ScanError::RootChanged)?;
    if let Some(ledger) = ledgers.get(root) { return Ok(ledger.clone()); }
    // Bound process-resident ledgers/watch descriptors. Eviction only loses an
    // optimization; the next access must rebuild from validated filesystem data.
    if ledgers.len() >= 8 { if let Some(key) = ledgers.keys().next().cloned() { ledgers.remove(&key); } }
    let generation = Arc::new(AtomicU64::new(0));
    let changes = generation.clone();
    let dirty = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let pending = dirty.clone();
    let watched_root = root.to_path_buf();
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher_failed = failed.clone();
    let watcher = crate::file_watch::watch(root, true, move |event| {
        match event {
            Ok(crate::file_watch_types::FileChange::Paths(paths)) => {
                if let Ok(mut pending) = pending.lock() {
                    for path in paths { if let Ok(path) = path.strip_prefix(&watched_root) { pending.insert(path.to_path_buf()); } }
                    if pending.len() > 4096 { pending.clear(); changes.fetch_add(1, Ordering::AcqRel); }
                } else { changes.fetch_add(1, Ordering::AcqRel); }
            }
            Ok(crate::file_watch_types::FileChange::Rescan) => { changes.fetch_add(1, Ordering::AcqRel); }
            Err(error) => { log::error!("sync filesystem watch failed: {error}"); watcher_failed.store(true, Ordering::Release); changes.fetch_add(1, Ordering::AcqRel); }
        }
    }).map_err(|error| ScanError::Io { path: root.to_path_buf(), source: std::io::Error::other(error) })?;
    let ledger = Arc::new(Mutex::new(ScanLedger { generation, dirty, failed, files: BTreeMap::new(), _watcher: watcher, observed: 0, persistence: None, loaded: false }));
    ledgers.insert(root.to_path_buf(), ledger.clone());
    Ok(ledger)
}

pub(crate) fn configure(root: &Path, storage: super::scan_ledger_types::LedgerStorage) -> Result<(), ScanError> {
    let ledger = ledger(root)?;
    let mut ledger = ledger.lock().map_err(|_| ScanError::RootChanged)?;
    let changed = ledger.persistence.as_ref().is_none_or(|old| old.path != storage.path || old.scope != storage.scope || old.repository != storage.repository);
    if changed { ledger.files.clear(); ledger.loaded = false; }
    ledger.persistence = Some(storage);
    Ok(())
}

pub(crate) fn restore(ledger: &mut ScanLedger) -> Result<(), ScanError> {
    if ledger.loaded { return Ok(()); }
    let Some(storage) = &ledger.persistence else { return Ok(()); };
    let index = super::index::SyncIndex::open(&storage.path, storage.key.clone(), storage.repository.clone(), storage.scope.clone())
        .map_err(|error| ScanError::Io { path: storage.path.clone(), source: std::io::Error::other(error) })?;
    let paths: Vec<String> = index.load("scan-ledger-v1", "paths").map_err(|error| ScanError::Io { path: storage.path.clone(), source: std::io::Error::other(error) })?.unwrap_or_default();
    if paths.len() > 100_000 { return Err(ScanError::LimitExceeded); }
    let mut remaining = 8 * 1024 * 1024_usize;
    for path in paths {
        super::paths::validate_relative(Path::new(&path))?;
        if let Some(file) = index.load::<super::scan_types::FileScan>("scan-file-v1", &path).map_err(|error| ScanError::Io { path: storage.path.clone(), source: std::io::Error::other(error) })? {
            if file.path != path { return Err(ScanError::RootChanged); }
            let bytes = path.len().saturating_add(256).saturating_add(file.blocks.len().saturating_mul(160));
            if bytes > remaining { continue; }
            remaining -= bytes;
            ledger.files.insert(path, file);
        }
    }
    ledger.loaded = true;
    Ok(())
}

pub(crate) fn persist(ledger: &ScanLedger, previous: &BTreeMap<String, super::scan_types::FileScan>) -> Result<(), ScanError> {
    let Some(storage) = &ledger.persistence else { return Ok(()); };
    if previous == &ledger.files { return Ok(()); }
    let index = super::index::SyncIndex::open(&storage.path, storage.key.clone(), storage.repository.clone(), storage.scope.clone())
        .map_err(|error| ScanError::Io { path: storage.path.clone(), source: std::io::Error::other(error) })?;
    index.save_scan_ledger(&ledger.files, previous).map_err(|error| ScanError::Io { path: storage.path.clone(), source: std::io::Error::other(error) })?;
    Ok(())
}

pub(crate) fn clear() -> Result<(), ScanError> {
    LEDGERS.get_or_init(|| Mutex::new(BTreeMap::new())).lock().map_err(|_| ScanError::RootChanged)?.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{scan::scan, scan_types::ScanOptions, scan_ledger_types::LedgerStorage};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn overflow_and_watch_errors_force_revalidation() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("a"), b"data")?;
        scan(root.path(), &ScanOptions::default())?;
        let state = ledger(root.path())?;
        state.lock().map_err(|_| "lock")?.generation.fetch_add(1, Ordering::AcqRel);
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let options = ScanOptions { on_chunk: Some(Arc::new(move |_| { count.fetch_add(1, Ordering::SeqCst); })), ..Default::default() };
        scan(root.path(), &options)?;
        assert!(reads.load(Ordering::SeqCst) > 0);
        state.lock().map_err(|_| "lock")?.failed.store(true, Ordering::Release);
        reads.store(0, Ordering::SeqCst);
        scan(root.path(), &options)?;
        assert!(reads.load(Ordering::SeqCst) > 0);
        Ok(())
    }

    #[test]
    fn persisted_ledger_survives_reopen_and_detects_offline_equal_size_edit() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let data = tempfile::tempdir()?;
        let file = root.path().join("a");
        std::fs::write(&file, b"before")?;
        let storage = || LedgerStorage { path: data.path().join("ledger.sqlite"), key: zeroize::Zeroizing::new([7; 32]), repository: "repo".into(), scope: "mapping-root".into() };
        configure(root.path(), storage())?;
        let first = scan(root.path(), &ScanOptions::default())?;
        LEDGERS.get().ok_or("ledgers")?.lock().map_err(|_| "lock")?.remove(root.path());
        configure(root.path(), storage())?;
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let options = ScanOptions { on_chunk: Some(Arc::new(move |_| { count.fetch_add(1, Ordering::SeqCst); })), ..Default::default() };
        assert_eq!(scan(root.path(), &options)?, first);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        LEDGERS.get().ok_or("ledgers")?.lock().map_err(|_| "lock")?.remove(root.path());
        std::fs::write(&file, b"edited")?;
        configure(root.path(), storage())?;
        let changed = scan(root.path(), &options)?;
        assert_ne!(changed.files[0].whole_file_hash, first.files[0].whole_file_hash);
        assert!(reads.load(Ordering::SeqCst) > 0);
        std::fs::remove_file(file)?;
        assert!(scan(root.path(), &options)?.files.is_empty());
        LEDGERS.get().ok_or("ledgers")?.lock().map_err(|_| "lock")?.remove(root.path());
        configure(root.path(), storage())?;
        assert!(scan(root.path(), &options)?.files.is_empty());
        Ok(())
    }
}

