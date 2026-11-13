use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

pub const MAX_SCAN_WORKING_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct ScanOptions {
    pub ignored: Vec<String>,
    pub cancel: Arc<AtomicBool>,
    pub on_chunk: Option<Arc<dyn Fn(&ScanChunk) + Send + Sync>>,
}

impl std::fmt::Debug for ScanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanOptions")
            .field("ignored", &self.ignored)
            .field("cancel", &self.cancel)
            .field("on_chunk", &self.on_chunk.as_ref().map(|_| "callback"))
            .finish()
    }
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            ignored: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            on_chunk: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkScan {
    pub offset: u64,
    pub length: u64,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanChunk {
    pub path: String,
    pub offset: u64,
    pub length: u64,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileScan {
    pub path: String,
    pub inode: u64,
    pub device: u64,
    pub size: u64,
    pub modified: i128,
    pub changed: i128,
    pub blocks: Vec<ChunkScan>,
    pub whole_file_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanReport {
    pub files: Vec<FileScan>,
    pub directories: Vec<String>,
    pub ignored_paths: Vec<String>,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("scan I/O failed at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid scan path: {0}")]
    InvalidPath(String),
    #[error("unsafe or changing filesystem entry: {0}")]
    UnsafeEntry(String),
    #[error("scan canceled")]
    Canceled,
    #[error("scan root changed during scan")]
    RootChanged,
    #[error("unsupported filesystem identity")]
    Unsupported,
    #[error("ignored paths require explicit exclusion handling")]
    UnsupportedIgnore,
    #[error("scan limit exceeded")]
    LimitExceeded,
}
