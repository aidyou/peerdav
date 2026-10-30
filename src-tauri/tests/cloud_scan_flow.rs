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
