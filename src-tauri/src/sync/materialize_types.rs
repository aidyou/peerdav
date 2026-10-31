use super::{engine_types::EngineError, merge_types::Projection, scan_types::ScanReport};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Verified plaintext locations from the source scan, keyed by protocol block id and length.
pub type LocalChunks = BTreeMap<(String, u64), (PathBuf, u64)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalVerifyMode {
    /// Preserve CAS behavior: certify target files, allowing additional local paths.
    TargetFiles,
    /// Require a complete scan and an exact set of target files and directories.
    Strict,
}

pub struct MaterializeRequest<'a> {
    pub root: &'a Path,
    pub private: &'a Path,
    pub mapping: &'a str,
    pub run_id: &'a str,
    pub source_scan: &'a ScanReport,
    pub current_scan: &'a ScanReport,
    pub base_projection: &'a Projection,
    pub target: &'a Projection,
    pub local_chunks: &'a LocalChunks,
    pub final_verify: FinalVerifyMode,
}

/// No baseline/index acknowledgement is performed by shared materialization.
pub struct Materialized {
    pub applied_files: u64,
    pub verified_scan: ScanReport,
}

#[derive(Debug, thiserror::Error)]
pub enum MaterializeError<E: std::error::Error + 'static> {
    #[error("local materialization failed")]
    Local(#[source] EngineError),
    #[error("materialization block read failed")]
    Block(#[source] E),
}
impl<E: std::error::Error + 'static> From<EngineError> for MaterializeError<E> {
    fn from(error: EngineError) -> Self {
        Self::Local(error)
    }
}
