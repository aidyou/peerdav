use super::scan_types::FileScan;
use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex, OnceLock, atomic::AtomicU64}};

pub(crate) struct ScanLedger {
    pub generation: Arc<AtomicU64>,
    pub dirty: Arc<Mutex<std::collections::BTreeSet<PathBuf>>>,
    pub files: BTreeMap<String, FileScan>,
    pub _watcher: notify::RecommendedWatcher,
    pub observed: u64,
    pub failed: Arc<std::sync::atomic::AtomicBool>,
    pub persistence: Option<LedgerStorage>,
    pub loaded: bool,
}

pub(crate) struct LedgerStorage {
    pub path: PathBuf,
    pub key: zeroize::Zeroizing<[u8; 32]>,
    pub repository: String,
    pub scope: String,
}

pub(crate) static LEDGERS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<ScanLedger>>>>> = OnceLock::new();
