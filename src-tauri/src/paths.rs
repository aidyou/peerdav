//! Filesystem locations used by PeerDav.
//!
//! Debug builds keep every piece of mutable state under the repository-level
//! `dev_data` directory so development runs never touch the user profile and
//! can be reset by deleting a single folder.

/// Directory that holds development-only state.
pub const DEV_DATA_DIR_NAME: &str = "dev_data";

/// Returns the repository-level `dev_data` directory in debug builds.
///
/// Release builds return `None` so callers fall back to the platform data
/// directory. The project path is only compiled into debug binaries.
#[cfg(debug_assertions)]
pub fn dev_data_root() -> Option<std::path::PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest_dir.parent().unwrap_or(manifest_dir);
    Some(project_root.join(DEV_DATA_DIR_NAME))
}

/// Returns the repository-level `dev_data` directory in debug builds.
#[cfg(not(debug_assertions))]
pub fn dev_data_root() -> Option<std::path::PathBuf> {
    None
}

/// Returns a named subdirectory of `dev_data` for debug builds.
pub fn dev_data_subdir(name: &str) -> Option<std::path::PathBuf> {
    dev_data_root().map(|root| root.join(name))
}
