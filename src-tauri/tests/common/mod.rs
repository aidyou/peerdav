//! Shared helpers for PeerDav integration tests.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use peerdav_lib::paths;

/// Test-owned directory under `dev_data/tests/<scope>`.
pub struct DevTestDir {
    path: PathBuf,
}

impl DevTestDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Creates a fresh per-test directory under the repository `dev_data` folder.
///
/// Test artifacts stay inspectable between runs instead of being written to the
/// system temporary directory or the user profile.
pub fn dev_test_dir(scope: &str) -> DevTestDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    static CLEANED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

    let tests_root = paths::dev_data_root()
        .unwrap_or_else(std::env::temp_dir)
        .join("tests");
    let scope_root = tests_root.join(scope);

    let cleaned = CLEANED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = cleaned.lock().expect("test scope lock");
    if guard.insert(scope.to_owned()) && scope_root.exists() {
        std::fs::remove_dir_all(&scope_root).expect("clean test scope");
    }
    drop(guard);
    std::fs::create_dir_all(&scope_root).expect("create test scope");

    let thread = std::thread::current()
        .name()
        .map(sanitize_name)
        .unwrap_or_else(|| format!("test-{}", std::process::id()));
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = scope_root.join(format!("{thread}-{index}"));

    if path.exists() {
        std::fs::remove_dir_all(&path).expect("clean test directory");
    }
    std::fs::create_dir_all(&path).expect("create test directory");

    DevTestDir { path }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}
