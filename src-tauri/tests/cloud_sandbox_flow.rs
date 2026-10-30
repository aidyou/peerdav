#![cfg(target_os = "linux")]

use peerdav_lib::sync::{sandbox::run_confined, sandbox_types::SandboxError};
use rustix::fs::{Mode, OFlags, RenameFlags, openat, renameat_with};
use std::fs::{self, File};
use std::os::fd::AsFd;
use std::sync::mpsc;

#[test]
fn held_parent_moved_outside_cannot_receive_staged_file() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("root");
    let private = temp.path().join("private");
    let outside = temp.path().join("outside");
    fs::create_dir_all(root.join("a")).expect("root");
    fs::create_dir(&private).expect("private");
    fs::create_dir(&outside).expect("outside");
    fs::write(private.join("stage"), b"new content").expect("stage");
    let root_fd = File::open(&root).expect("root fd");
    let private_fd = File::open(&private).expect("private fd");
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        run_confined(root_fd, private_fd, move |root, private| {
            let parent = openat(
                &root,
                "a",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .expect("held parent");
            ready_tx.send(()).expect("ready");
            go_rx.recv().expect("continue");
            let error = renameat_with(&private, "stage", &parent, "file", RenameFlags::NOREPLACE)
                .expect_err("kernel must reject moved parent");
            assert!(matches!(
                error,
                rustix::io::Errno::ACCESS | rustix::io::Errno::XDEV
            ));
            Ok::<_, SandboxError>(())
        })
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker ready");
    fs::rename(root.join("a"), outside.join("moved")).expect("external directory move");
    go_tx.send(()).expect("continue");
    worker
        .join()
        .expect("worker join")
        .expect("sandbox available and enforced");
    assert!(!outside.join("moved/file").exists());
    assert_eq!(
        fs::read(private.join("stage")).expect("preserved staging"),
        b"new content"
    );
}

#[test]
fn held_parent_moved_outside_cannot_remove_original_into_recovery() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("root");
    let private = temp.path().join("private");
    let outside = temp.path().join("outside");
    fs::create_dir_all(root.join("a")).expect("root");
    fs::create_dir(&private).expect("private");
    fs::create_dir(&outside).expect("outside");
    fs::write(root.join("a/original"), b"old content").expect("original");
    let root_fd = File::open(&root).expect("root fd");
    let private_fd = File::open(&private).expect("private fd");
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        run_confined(root_fd, private_fd, move |root, private| {
            let parent = openat(
                &root,
                "a",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .expect("held parent");
            ready_tx.send(()).expect("ready");
            go_rx.recv().expect("continue");
            let error = renameat_with(
                &parent,
                "original",
                &private,
                "recovery",
                RenameFlags::NOREPLACE,
            )
            .expect_err("kernel must reject moved source");
            assert!(matches!(
                error,
                rustix::io::Errno::ACCESS | rustix::io::Errno::XDEV
            ));
            Ok::<_, SandboxError>(())
        })
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker ready");
    fs::rename(root.join("a"), outside.join("moved")).expect("external directory move");
    go_tx.send(()).expect("continue");
    worker
        .join()
        .expect("worker join")
        .expect("sandbox available and enforced");
    assert_eq!(
        fs::read(outside.join("moved/original")).expect("preserved original"),
        b"old content"
    );
    assert!(!private.join("recovery").exists());
}

#[test]
fn normal_cross_directory_rename_and_parent_thread_remain_usable() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("root");
    let private = temp.path().join("private");
    let outside = temp.path().join("outside");
    fs::create_dir(&root).expect("root");
    fs::create_dir(&private).expect("private");
    fs::create_dir(&outside).expect("outside");
    fs::write(root.join("original"), b"original").expect("original");
    run_confined(
        File::open(&root).expect("root fd"),
        File::open(&private).expect("private fd"),
        |root, private| {
            renameat_with(
                root.as_fd(),
                "original",
                private.as_fd(),
                "recovery",
                RenameFlags::NOREPLACE,
            )
            .expect("move into recovery");
            renameat_with(
                private.as_fd(),
                "recovery",
                root.as_fd(),
                "restored",
                RenameFlags::NOREPLACE,
            )
            .expect("restore no-replace");
            Ok::<_, SandboxError>(())
        },
    )
    .expect("fully enforced sandbox");
    assert_eq!(
        fs::read(root.join("restored")).expect("restored"),
        b"original"
    );
    fs::write(
        outside.join("parent-write"),
        b"LAN and parent threads not restricted",
    )
    .expect("parent remains unconfined");
}
