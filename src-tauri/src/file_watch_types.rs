use std::path::PathBuf;

/// A content-relevant filesystem event. Paths are hints, never authority to
/// upload or delete; callers must re-open and validate the current filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileChange {
    Paths(Vec<PathBuf>),
    Rescan,
}
