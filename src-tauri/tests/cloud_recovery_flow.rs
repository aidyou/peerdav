#![cfg(target_os = "linux")]

use peerdav_lib::sync::apply::apply_bytes;
use peerdav_lib::sync::apply_types::{
    ApplyAction, ApplyError, ApplyIntent, ApplyPhase, FileIdentity,
};
use peerdav_lib::sync::index::SyncIndex;
use peerdav_lib::sync::recovery::recover_pending;
use peerdav_lib::sync::recovery_types::RecoveryError;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};
use zeroize::Zeroizing;

fn digest(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
fn id(path: &Path) -> FileIdentity {
    let m = fs::metadata(path).expect("metadata");
    FileIdentity {
        dev: m.dev(),
        ino: m.ino(),
    }
}
fn setup() -> (TempDir, PathBuf, PathBuf, SyncIndex) {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let private = temp.path().join("private");
    fs::create_dir(&root).expect("root");
    fs::create_dir(&private).expect("private");
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).expect("mode");
    let index = SyncIndex::open(
        &temp.path().join("index.db"),
        Zeroizing::new([9; 32]),
        "repo".into(),
        "mapping".into(),
    )
    .expect("index");
    (temp, root, private, index)
}
fn apply(
    root: &Path,
    private: &Path,
    op: &str,
    rel: &Path,
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
        rel,
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
fn append_prepared_and_legacy_paused_resume_all_actions() {
    for action in [
        ApplyAction::Create,
        ApplyAction::Replace,
        ApplyAction::Delete,
        ApplyAction::DeleteDirectory,
    ] {
        for paused in [false, true] {
            let (_tmp, root, private, _index) = setup();
            let target = root.join("a");
            let directory = action == ApplyAction::DeleteDirectory;
            let existing = action != ApplyAction::Create;
            if directory {
                fs::create_dir(&target).unwrap();
            } else if existing {
                fs::write(&target, b"old").unwrap();
            }
            let old = existing.then(|| id(&target));
            let old_digest = (existing && !directory).then(|| digest(b"old"));
            let mut saved = None;
            assert!(
                apply(
                    &root,
                    &private,
                    "append-resume",
                    Path::new("a"),
                    action.clone(),
                    b"new",
                    old,
                    old_digest.as_deref(),
                    (existing && !directory).then_some(3),
                    &mut |intent| {
                        saved = Some(intent.clone());
                        Err(ApplyError::Journal)
                    }
                )
                .is_err()
            );
            let mut intent = saved.unwrap();
            if paused {
                intent.phase = ApplyPhase::Paused;
            }
            let mut durable = intent.clone();
            let summary = peerdav_lib::sync::recovery::recover_append_records(
                &root,
                &private,
                vec![("record-id".into(), intent.clone())],
                &mut |record, next| {
                    assert_eq!(record, "record-id");
                    durable = next.clone();
                    Ok(())
                },
            )
            .unwrap();
            assert!(!summary.needs_user_confirmation);
            if matches!(action, ApplyAction::Delete | ApplyAction::DeleteDirectory) {
                assert!(!target.exists());
                assert_eq!(durable.phase, ApplyPhase::Deleted);
            } else {
                assert_eq!(fs::read(&target).unwrap(), b"new");
                assert_eq!(durable.phase, ApplyPhase::Installed);
            }
            if existing {
                assert!(intent.recovery_path.exists());
            }
            // Replaying a completed pending record is idempotent.
            let summary = peerdav_lib::sync::recovery::recover_append_records(
                &root,
                &private,
                vec![("record-id".into(), durable)],
                &mut |_, _| Ok(()),
            )
            .unwrap();
            assert!(!summary.needs_user_confirmation);
        }
    }
}

#[test]
fn append_resume_refuses_uncertain_prepared_states_without_mutation() {
    for fault in [
        "old-edit",
        "stage-edit",
        "stage-missing",
        "symlink",
        "root",
        "nonempty",
    ] {
        let (_tmp, root, private, _index) = setup();
        let target = root.join("a");
        let directory = fault == "nonempty";
        if directory {
            fs::create_dir(&target).unwrap();
        } else {
            fs::write(&target, b"old").unwrap();
        }
        let mut saved = None;
        assert!(
            apply(
                &root,
                &private,
                "uncertain",
                Path::new("a"),
                if directory {
                    ApplyAction::DeleteDirectory
                } else {
                    ApplyAction::Replace
                },
                b"new",
                Some(id(&target)),
                (!directory).then(|| digest(b"old")).as_deref(),
                (!directory).then_some(3),
                &mut |intent| {
                    saved = Some(intent.clone());
                    Err(ApplyError::Journal)
                }
            )
            .is_err()
        );
        let intent = saved.unwrap();
        match fault {
            "old-edit" => fs::write(&target, b"concurrent").unwrap(),
            "stage-edit" => fs::write(&intent.staging_path, b"tamper").unwrap(),
            "stage-missing" => fs::remove_file(&intent.staging_path).unwrap(),
            "symlink" => {
                fs::remove_file(&target).unwrap();
                std::os::unix::fs::symlink("elsewhere", &target).unwrap();
            }
            "root" => {
                fs::rename(&root, root.with_file_name("old-root")).unwrap();
                fs::create_dir(&root).unwrap();
            }
            "nonempty" => fs::write(target.join("child"), b"precious").unwrap(),
            _ => unreachable!(),
        }
        let summary = peerdav_lib::sync::recovery::recover_append_records(
            &root,
            &private,
            vec![("id".into(), intent.clone())],
            &mut |_, _| panic!("must not journal uncertain evidence"),
        )
        .unwrap();
        assert!(summary.needs_user_confirmation, "{fault}");
        assert!(!intent.recovery_path.exists(), "{fault}");
    }
}

#[test]
fn append_recognizes_rename_before_phase_write() {
    for install in [false, true] {
        let (_tmp, root, private, _index) = setup();
        let target = root.join("a");
        fs::write(&target, b"old").unwrap();
        let mut saved = None;
        assert!(
            apply(
                &root,
                &private,
                "rename-window",
                Path::new("a"),
                ApplyAction::Replace,
                b"new",
                Some(id(&target)),
                Some(&digest(b"old")),
                Some(3),
                &mut |intent| {
                    saved = Some(intent.clone());
                    Err(ApplyError::Journal)
                }
            )
            .is_err()
        );
        let intent = saved.unwrap();
        fs::rename(&target, &intent.recovery_path).unwrap();
        if install {
            fs::rename(&intent.staging_path, &target).unwrap();
        }
        let summary = peerdav_lib::sync::recovery::recover_append_records(
            &root,
            &private,
            vec![("id".into(), intent.clone())],
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert!(!summary.needs_user_confirmation);
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(&intent.recovery_path).unwrap(), b"old");
    }
}

#[test]
fn empty_index_is_unknown_and_has_no_recovery() {
    let (_tmp, root, private, mut index) = setup();
    let summary = recover_pending(&mut index, &root, &private).expect("recover");
    assert!(!summary.requires_remote_reconcile);
    assert!(!summary.needs_user_confirmation);
    assert!(summary.recovered_operations.is_empty());
    assert_eq!(
        index.baseline("a").expect("baseline"),
        peerdav_lib::sync::index::BaselineRecord::Unknown
    );
}

#[test]
fn prepared_callback_failure_keeps_original_and_pauses_on_recovery() {
    let (_tmp, root, private, mut index) = setup();
    let target = root.join("a");
    fs::write(&target, b"old").expect("old");
    let old = id(&target);
    let mut saved = None;
    let result = apply(
        &root,
        &private,
        "pause",
        Path::new("a"),
        ApplyAction::Replace,
        b"new",
        Some(old.clone()),
        Some(&digest(b"old")),
        Some(3),
        &mut |intent| {
            if intent.phase == ApplyPhase::Prepared {
                saved = Some(intent.clone());
                return Err(ApplyError::Journal);
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::Journal)));
    assert_eq!(fs::read(&target).expect("target"), b"old");
    index
        .record_apply("pause", &saved.expect("record"))
        .expect("record");
    let summary = recover_pending(&mut index, &root, &private).expect("recover");
    assert!(summary.needs_user_confirmation);
    assert!(summary.requires_remote_reconcile);
    assert_eq!(summary.operation.as_deref(), Some("pause"));
}

#[test]
fn original_preserved_recovery_installs_stage() {
    let (_tmp, root, private, mut index) = setup();
    let target = root.join("a");
    fs::write(&target, b"old").expect("old");
    let old = id(&target);
    let mut saved = None;
    let result = apply(
        &root,
        &private,
        "replace",
        Path::new("a"),
        ApplyAction::Replace,
        b"new",
        Some(old),
        Some(&digest(b"old")),
        Some(3),
        &mut |intent| {
            if intent.phase == ApplyPhase::OriginalPreserved {
                saved = Some(intent.clone());
                return Err(ApplyError::Journal);
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::Journal)));
    let intent = saved.expect("record");
    index.record_apply("replace", &intent).expect("record");
    let summary = recover_pending(&mut index, &root, &private).expect("recover");
    assert!(summary.requires_remote_reconcile);
    assert_eq!(fs::read(&target).expect("target"), b"new");
}

#[test]
fn corrupted_stage_refuses_recovery_and_root_replacement_cannot_escape() {
    let (_tmp, root, private, mut index) = setup();
    let mut saved = None;
    let result = apply(
        &root,
        &private,
        "corrupt",
        Path::new("a"),
        ApplyAction::Create,
        b"new",
        None,
        None,
        None,
        &mut |intent| {
            if intent.phase == ApplyPhase::Prepared {
                saved = Some(intent.clone());
                return Err(ApplyError::Journal);
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    let intent = saved.expect("record");
    fs::write(&intent.staging_path, b"tampered").expect("tamper");
    index.record_apply("corrupt", &intent).expect("record");
    assert!(matches!(
        recover_pending(&mut index, &root, &private),
        Err(RecoveryError::Concurrent)
    ));
    let outside = tempdir().expect("outside");
    let link = root.with_file_name("root-link");
    std::os::unix::fs::symlink(outside.path(), &link).expect("link");
    assert!(
        apply(
            &link,
            &private,
            "escape",
            Path::new("x"),
            ApplyAction::Create,
            b"x",
            None,
            None,
            None,
            &mut |_| Ok(())
        )
        .is_err()
    );
    assert!(!outside.path().join("x").exists());
}

#[test]
fn original_preserved_stage_gone_target_is_installed_and_old_recovery_is_preserved() {
    let (_tmp, root, private, mut index) = setup();
    let target = root.join("a");
    fs::write(&target, b"old").expect("old");
    let old = id(&target);
    let mut saved = None;
    let result = apply(
        &root,
        &private,
        "stage-gone-original",
        Path::new("a"),
        ApplyAction::Replace,
        b"new",
        Some(old),
        Some(&digest(b"old")),
        Some(3),
        &mut |intent| {
            if intent.phase == ApplyPhase::OriginalPreserved {
                saved = Some(intent.clone());
                return Err(ApplyError::Journal);
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::Journal)));
    let intent = saved.expect("record");
    fs::rename(&intent.staging_path, &target).expect("install staged target");
    index
        .record_apply("stage-gone-original", &intent)
        .expect("record");
    let summary = recover_pending(&mut index, &root, &private).expect("recover");
    assert!(summary.requires_remote_reconcile);
    assert!(!summary.needs_user_confirmation);
    assert_eq!(fs::read(&target).expect("target"), b"new");
    assert_eq!(fs::read(&intent.recovery_path).expect("recovery"), b"old");
}

#[test]
fn prepared_stage_gone_target_is_recognized_as_installed_without_overwrite() {
    let (_tmp, root, private, mut index) = setup();
    let mut saved = None;
    let result = apply(
        &root,
        &private,
        "stage-gone-prepared",
        Path::new("a"),
        ApplyAction::Create,
        b"new",
        None,
        None,
        None,
        &mut |intent| {
            if intent.phase == ApplyPhase::Prepared {
                saved = Some(intent.clone());
                return Err(ApplyError::Journal);
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(ApplyError::Journal)));
    let intent = saved.expect("record");
    fs::rename(&intent.staging_path, root.join("a")).expect("install staged target");
    index
        .record_apply("stage-gone-prepared", &intent)
        .expect("record");
    let summary = recover_pending(&mut index, &root, &private).expect("recover");
    assert!(summary.requires_remote_reconcile);
    assert!(!summary.needs_user_confirmation);
    assert_eq!(fs::read(root.join("a")).expect("target"), b"new");
}
#[test]
fn installed_record_is_validated_without_reinitializing_stage() {
    let (_tmp, root, private, mut index) = setup();
    let target = root.join("a");
    let mut saved = None;
    let intent = apply(
        &root,
        &private,
        "installed",
        Path::new("a"),
        ApplyAction::Create,
        b"new",
        None,
        None,
        None,
        &mut |intent| {
            if intent.phase == ApplyPhase::Installed {
                saved = Some(intent.clone());
            }
            Ok(())
        },
    )
    .expect("apply");
    let mut installed = saved.unwrap_or(intent);
    installed.staging_path = private.join("missing-stage");
    index.record_apply("installed", &installed).expect("record");
    let summary = recover_pending(&mut index, &root, &private).expect("recover");
    assert!(summary.requires_remote_reconcile);
    assert_eq!(fs::read(target).expect("target"), b"new");
}
