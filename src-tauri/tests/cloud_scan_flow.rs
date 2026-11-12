use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[test]
fn cloud_scan_reads_files_and_empty_directories() {
    let root = tempdir().expect("tempdir");
    fs::create_dir(root.path().join("empty")).expect("empty");
    fs::write(root.path().join("a.txt"), b"hello").expect("file");
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        cancel: Arc::new(AtomicBool::new(false)),
        ..Default::default()
    };
    let report = peerdav_lib::sync::scan::scan(root.path(), &options).expect("scan");
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, "a.txt");
    assert!(report.directories.iter().any(|p| p == "empty"));
}

#[test]
fn cloud_scan_cancels_before_reading() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("a"), b"x").expect("file");
    let cancel = Arc::new(AtomicBool::new(true));
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        cancel,
        ..Default::default()
    };
    assert!(matches!(
        peerdav_lib::sync::scan::scan(root.path(), &options),
        Err(peerdav_lib::sync::scan_types::ScanError::Canceled)
    ));
}

#[test]
fn cloud_scan_detects_same_size_mutation() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("a"), b"original").expect("file");
    let path = root.path().join("a");
    let callback = Arc::new(move |chunk: &peerdav_lib::sync::scan_types::ScanChunk| {
        if chunk.offset == 0 {
            let _ = fs::write(&path, b"changed!");
        }
    });
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        on_chunk: Some(callback),
        ..Default::default()
    };
    assert!(peerdav_lib::sync::scan::scan(root.path(), &options).is_err());
}

#[test]
fn cloud_scan_detects_file_replacement() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("a"), b"original").expect("file");
    let path = root.path().join("a");
    let callback = Arc::new(move |chunk: &peerdav_lib::sync::scan_types::ScanChunk| {
        if chunk.offset == 0 {
            let replacement = path.with_extension("new");
            let _ = fs::write(&replacement, b"original");
            let _ = fs::rename(replacement, &path);
        }
    });
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        on_chunk: Some(callback),
        ..Default::default()
    };
    assert!(peerdav_lib::sync::scan::scan(root.path(), &options).is_err());
}

#[test]
fn cloud_scan_rejects_invalid_relative_paths() {
    use std::path::Path;
    assert!(peerdav_lib::sync::paths::validate_relative(Path::new("../escape")).is_err());
    assert!(peerdav_lib::sync::paths::validate_relative(Path::new("bad:name")).is_err());
}

#[test]
fn cloud_scan_detects_root_removed() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("a"), b"x").expect("file");
    let root_path = root.path().to_path_buf();
    let callback = Arc::new(move |chunk: &peerdav_lib::sync::scan_types::ScanChunk| {
        if chunk.offset == 0 {
            let _ = fs::remove_file(root_path.join("a"));
            let _ = fs::remove_dir(&root_path);
        }
    });
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        on_chunk: Some(callback),
        ..Default::default()
    };
    assert!(peerdav_lib::sync::scan::scan(root.path(), &options).is_err());
}

#[cfg(unix)]
#[test]
fn cloud_scan_rejects_symlinks() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("target"), b"x").expect("file");
    std::os::unix::fs::symlink("target", root.path().join("link")).expect("symlink");
    assert!(peerdav_lib::sync::scan::scan(root.path(), &Default::default()).is_err());
}

#[test]
fn cloud_scan_reports_file_identity() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("a"), b"x").expect("file");
    let report = peerdav_lib::sync::scan::scan(root.path(), &Default::default()).expect("scan");
    assert_ne!(report.files[0].inode, 0);
}

#[cfg(unix)]
#[test]
fn root_ancestor_symlink_is_rejected_before_read() {
    let parent = tempdir().expect("temp");
    fs::create_dir_all(parent.path().join("actual/root")).expect("dir");
    fs::write(parent.path().join("actual/root/private"), b"outside-marker").expect("file");
    std::os::unix::fs::symlink("actual", parent.path().join("alias")).expect("link");
    assert!(
        peerdav_lib::sync::scan::scan(&parent.path().join("alias/root"), &Default::default())
            .is_err()
    );
}

#[test]
fn cancellation_on_final_chunk_is_not_reported_complete() {
    let root = tempdir().expect("temp");
    fs::write(root.path().join("a"), b"small").expect("file");
    let cancel = Arc::new(AtomicBool::new(false));
    let callback_cancel = cancel.clone();
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        cancel,
        on_chunk: Some(Arc::new(move |_| {
            callback_cancel.store(true, std::sync::atomic::Ordering::SeqCst)
        })),
        ..Default::default()
    };
    assert!(matches!(
        peerdav_lib::sync::scan::scan(root.path(), &options),
        Err(peerdav_lib::sync::scan_types::ScanError::Canceled)
    ));
}

#[test]
fn concurrent_directory_addition_invalidates_full_scan() {
    let root = tempdir().expect("temp");
    fs::write(root.path().join("a"), b"small").expect("file");
    let other = root.path().join("unobserved");
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        on_chunk: Some(Arc::new(move |_| {
            fs::write(&other, b"concurrent").expect("concurrent file")
        })),
        ..Default::default()
    };
    assert!(peerdav_lib::sync::scan::scan(root.path(), &options).is_err());
}

#[test]
fn unchanged_scan_reuses_content_fingerprints() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempdir().expect("root");
    fs::write(root.path().join("a"), b"unchanged").expect("file");
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let options = peerdav_lib::sync::scan_types::ScanOptions {
        on_chunk: Some(Arc::new(move |_| { counter.fetch_add(1, Ordering::SeqCst); })),
        ..Default::default()
    };
    let first = peerdav_lib::sync::scan::scan(root.path(), &options).expect("first");
    reads.store(0, Ordering::SeqCst);
    let second = peerdav_lib::sync::scan::scan(root.path(), &options).expect("second");
    assert_eq!(first, second);
    assert_eq!(reads.load(Ordering::SeqCst), 0, "unchanged files must not be read again");
}

#[test]
fn incremental_scan_detects_equal_size_changes_moves_and_timestamp_only_updates() {
    use peerdav_lib::sync::{scan::scan, scan_types::ScanOptions};
    use std::time::{Duration, SystemTime};
    let root = tempdir().expect("root");
    let outside = tempdir().expect("outside");
    fs::write(root.path().join("a"), b"before").expect("file");
    let initial = scan(root.path(), &ScanOptions::default()).expect("initial");
    fs::write(root.path().join("a"), b"edited").expect("equal length change");
    let changed = scan(root.path(), &ScanOptions::default()).expect("changed");
    assert_ne!(initial.files[0].whole_file_hash, changed.files[0].whole_file_hash);
    let file = fs::File::options().write(true).open(root.path().join("a")).expect("open");
    file.set_times(fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(10))).expect("touch");
    let touched = scan(root.path(), &ScanOptions::default()).expect("touch scan");
    assert_eq!(changed.files[0].whole_file_hash, touched.files[0].whole_file_hash);
    fs::rename(root.path().join("a"), root.path().join("renamed")).expect("rename");
    assert_eq!(scan(root.path(), &ScanOptions::default()).expect("rename scan").files[0].path, "renamed");
    fs::rename(root.path().join("renamed"), outside.path().join("a")).expect("move out");
    assert!(scan(root.path(), &ScanOptions::default()).expect("removed").files.is_empty());
    fs::rename(outside.path().join("a"), root.path().join("arrived")).expect("move in");
    fs::write(root.path().join(".arrived.swp"), b"ignore").expect("swap");
    let arrived = scan(root.path(), &ScanOptions::default()).expect("arrived");
    assert_eq!(arrived.files.len(), 1);
    assert_eq!(arrived.files[0].path, "arrived");
}

#[test]
fn portable_reserved_paths_and_overlapping_roots_are_rejected() {
    use peerdav_lib::sync::paths::{validate_relative, validate_root_protection};
    use std::path::Path;
    for path in [
        "NUL",
        "con.txt",
        "COM1",
        "lpt9.log",
        "a\\b",
        "a\0b",
        "a//b",
        "C:/drive",
        "trailing.",
        "space ",
        "../parent",
    ] {
        assert!(validate_relative(Path::new(path)).is_err(), "path {path:?}");
    }
    assert!(validate_relative(Path::new("COM0")).is_ok());
    let root = tempdir().expect("temp");
    fs::create_dir(root.path().join("private")).expect("private");
    assert!(validate_root_protection(root.path(), &[&root.path().join("private")]).is_err());
}
