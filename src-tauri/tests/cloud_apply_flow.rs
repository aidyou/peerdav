#![cfg(target_os = "linux")]

use peerdav_lib::sync::apply::{apply_bytes, recover, restore};
use peerdav_lib::sync::apply_types::{
    ApplyAction, ApplyError, ApplyIntent, ApplyPhase, FileIdentity,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Write};
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn identity(path: &Path) -> FileIdentity {
    let metadata = fs::metadata(path).expect("metadata");
    FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

fn workspace() -> (TempDir, PathBuf, PathBuf) {
    let parent = tempdir().expect("tempdir");
    let root = parent.path().join("root");
    let private = parent.path().join("private");
    fs::create_dir(&root).expect("root");
    fs::create_dir(&private).expect("private");
    (parent, root, private)
}

fn apply(
    root: &Path,
    private: &Path,
    op: &str,
    relative: &Path,
    action: ApplyAction,
    data: &[u8],
    old: Option<FileIdentity>,
    old_digest: Option<&str>,
    old_size: Option<u64>,
    journal: &mut dyn FnMut(&ApplyIntent) -> Result<(), ApplyError>,
) -> Result<ApplyIntent, ApplyError> {
    apply_bytes(
        root,
        private,
        op,
        relative,
        action,
        Cursor::new(data),
        old,
        old_digest,
        old_size,
        &digest(data),
        data.len() as u64,
        journal,
    )
}

#[test]
fn create_existing_unchanged() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let result = apply(
        &root,
        &private,
        "create-existing",
        Path::new("a.txt"),
        ApplyAction::Create,
        b"new",
        None,
        None,
        None,
        &mut |_| Ok(()),
    );
    assert!(matches!(result, Err(ApplyError::AlreadyExists)));
    assert_eq!(fs::read(target).expect("read"), b"old");
}

#[test]
fn replace_original_in_recovery_and_new_target_installed() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let old = identity(&target);
    let mut phases = Vec::new();
    let intent = apply(
        &root,
        &private,
        "replace",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(old.clone()),
        Some(&digest(b"old")),
        Some(3),
        &mut |i| {
            phases.push(i.phase.clone());
            Ok(())
        },
    )
    .expect("replace");
    assert_eq!(fs::read(&target).expect("read target"), b"new");
    assert_eq!(
        fs::read(&intent.recovery_path).expect("read recovery"),
        b"old"
    );
    assert_eq!(
        phases,
        vec![
            ApplyPhase::Prepared,
            ApplyPhase::OriginalPreserved,
            ApplyPhase::Installed
        ]
    );
    assert_eq!(identity(&intent.recovery_path), old);
}

#[test]
fn delete_original_in_recovery_target_absent() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let intent = apply(
        &root,
        &private,
        "delete",
        Path::new("a.txt"),
        ApplyAction::Delete,
        b"",
        Some(identity(&target)),
        Some(&digest(b"old")),
        Some(3),
        &mut |_| Ok(()),
    )
    .expect("delete");
    assert!(!target.exists());
    assert_eq!(fs::read(&intent.recovery_path).expect("recovery"), b"old");
    assert_eq!(intent.phase, ApplyPhase::Deleted);
}

#[test]
fn wrong_digest_target_unchanged() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let old = identity(&target);
    let result = apply(
        &root,
        &private,
        "wrong-digest",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(old),
        Some(&digest(b"wrong")),
        Some(5),
        &mut |_| Ok(()),
    );
    assert!(matches!(result, Err(ApplyError::IdentityChanged)));
    assert_eq!(fs::read(target).expect("read"), b"old");
}

#[test]
fn old_writable_handle_after_install_changes_recovery_not_new_target() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let old = identity(&target);
    let mut old_handle: Option<File> = None;
    let intent = apply(
        &root,
        &private,
        "handle",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(old),
        Some(&digest(b"old")),
        Some(3),
        &mut |i| {
            if i.phase == ApplyPhase::OriginalPreserved {
                old_handle = Some(
                    OpenOptions::new()
                        .write(true)
                        .open(&i.recovery_path)
                        .expect("open recovery"),
                );
            }
            if i.phase == ApplyPhase::Installed {
                old_handle
                    .as_mut()
                    .expect("handle")
                    .write_all(b"mutated")
                    .expect("write old");
            }
            Ok(())
        },
    )
    .expect("replace");
    assert_eq!(fs::read(&target).expect("new target"), b"new");
    assert_eq!(
        fs::read(&intent.recovery_path).expect("recovery"),
        b"mutated"
    );
}

#[test]
fn prepared_callback_swap_to_other_file_identity_error_no_replace() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    let other = root.join("other.txt");
    fs::write(&target, b"old").expect("write");
    let old = identity(&target);
    let result = apply(
        &root,
        &private,
        "swap",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(old),
        Some(&digest(b"old")),
        Some(3),
        &mut |_| {
            fs::rename(&target, &other).expect("swap away");
            fs::write(&target, b"replacement").expect("replacement");
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::IdentityChanged)));
    assert_eq!(fs::read(&target).expect("target"), b"replacement");
    assert_eq!(fs::read(&other).expect("other"), b"old");
}

#[test]
fn original_preserved_callback_create_target_no_overwrite_keeps_old() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let old = identity(&target);
    let result = apply(
        &root,
        &private,
        "recreate",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(old),
        Some(&digest(b"old")),
        Some(3),
        &mut |i| {
            if i.phase == ApplyPhase::OriginalPreserved {
                fs::write(&target, b"callback").expect("callback target");
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::Io(_))));
    assert_eq!(fs::read(&target).expect("target"), b"callback");
}

#[test]
fn final_symlink_root_outside_zero_writes() {
    let parent = tempdir().expect("tempdir");
    let outside = parent.path().join("outside");
    let root_link = parent.path().join("root-link");
    fs::create_dir(&outside).expect("outside");
    symlink(&outside, &root_link).expect("root symlink");
    let private = parent.path().join("private");
    fs::create_dir(&private).expect("private");
    let result = apply(
        &root_link,
        &private,
        "symlink-root",
        Path::new("a.txt"),
        ApplyAction::Create,
        b"new",
        None,
        None,
        None,
        &mut |_| Ok(()),
    );
    assert!(result.is_err());
    assert!(!outside.join("a.txt").exists());
}

#[test]
fn post_install_callback_error_recovery_snapshot_then_idempotent() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    fs::write(&target, b"old").expect("write");
    let mut snapshot = None;
    let result = apply(
        &root,
        &private,
        "post-error",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(identity(&target)),
        Some(&digest(b"old")),
        Some(3),
        &mut |i| {
            if i.phase == ApplyPhase::Installed {
                snapshot = Some(i.clone());
                return Err(ApplyError::Journal);
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::Journal)));
    let snapshot = snapshot.expect("installed snapshot");
    assert_eq!(fs::read(&target).expect("target"), b"new");
    assert_eq!(
        recover(&snapshot, &root).expect("recover"),
        ApplyPhase::Installed
    );
    assert_eq!(
        recover(&snapshot, &root).expect("recover idempotent"),
        ApplyPhase::Installed
    );
}

#[test]
fn restore_chosen_empty_target_no_replace_and_keeps_recovery() {
    let (_tmp, root, private) = workspace();
    let target = root.join("a.txt");
    let chosen = root.join("chosen.txt");
    fs::write(&target, b"old").expect("write");
    let intent = apply(
        &root,
        &private,
        "restore",
        Path::new("a.txt"),
        ApplyAction::Replace,
        b"new",
        Some(identity(&target)),
        Some(&digest(b"old")),
        Some(3),
        &mut |_| Ok(()),
    )
    .expect("replace");
    fs::remove_file(&target).expect("remove target");
    restore(&intent, &root, Path::new("chosen.txt")).expect("restore");
    assert_eq!(fs::read(&chosen).expect("chosen"), b"old");
    assert_eq!(fs::read(&intent.recovery_path).expect("recovery"), b"old");
    assert!(matches!(
        restore(&intent, &root, Path::new("chosen.txt")),
        Err(ApplyError::AlreadyExists)
    ));
    assert_eq!(
        fs::read(&intent.recovery_path).expect("recovery retained"),
        b"old"
    );
}

#[test]
fn create_preserves_staged_contract_and_calls_prepared() {
    let (_tmp, root, private) = workspace();
    let data = b"new content";
    let mut phases = Vec::new();
    let intent = apply(
        &root,
        &private,
        "create-1",
        Path::new("a.txt"),
        ApplyAction::Create,
        data,
        None,
        None,
        None,
        &mut |i| {
            phases.push(i.phase.clone());
            Ok(())
        },
    )
    .expect("apply");
    assert_eq!(fs::read(root.join("a.txt")).expect("read"), data);
    assert_eq!(phases.first(), Some(&ApplyPhase::Prepared));
    assert!(!intent.recovery_path.exists());
}

#[test]
fn callback_failure_leaves_create_target_unchanged() {
    let (_tmp, root, private) = workspace();
    let result = apply(
        &root,
        &private,
        "fail-1",
        Path::new("a.txt"),
        ApplyAction::Create,
        b"x",
        None,
        None,
        None,
        &mut |_| Err(ApplyError::Journal),
    );
    assert!(matches!(result, Err(ApplyError::Journal)));
    assert!(!root.join("a.txt").exists());
}
