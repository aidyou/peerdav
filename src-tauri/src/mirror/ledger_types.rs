use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::types::CloudFile;

pub(crate) const MAX_LEDGER_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_WRITERS: usize = 1024;
pub(crate) const FULL_CHECK_SECONDS: u64 = 600;

/// Each installation owns one marker. Never overwrite another installation's state.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterState {
    pub version: u32,
    pub token: String,
    pub pending: bool,
}

/// A remote marker is only a change hint, never authority for deleting local data.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteCache {
    pub scope: String,
    pub markers: BTreeMap<String, String>,
    pub files: BTreeMap<String, CloudFile>,
    pub checked_at: u64,
}
