use std::fs;
use tempfile::tempdir;

use peerdav_lib::sync::root_state::{
    authorize_manual_after_preview, bind_initial, inspect_binding, reselect, timer_allowed,
};
use peerdav_lib::sync::root_types::{RootBindingStatus, RootPauseReason, RootStatus};

#[cfg(unix)]
#[test]
fn rename_missing_pauses_and_restore_never_resumes() {
    let area = tempdir().unwrap();
    let root = area.path().join("root");
    fs::create_dir(&root).unwrap();
    let mut binding = bind_initial(&root).unwrap();
    fs::rename(&root, area.path().join("moved")).unwrap();
    assert!(matches!(
        inspect_binding(&mut binding).unwrap(),
        RootStatus::Paused(RootPauseReason::Missing)
    ));
    fs::rename(area.path().join("moved"), &root).unwrap();
    assert!(matches!(
        inspect_binding(&mut binding).unwrap(),
        RootStatus::Paused(RootPauseReason::Missing)
    ));
    assert!(!timer_allowed(&binding));
}

#[cfg(unix)]
#[test]
fn same_path_replacement_pauses() {
    let area = tempdir().unwrap();
    let root = area.path().join("root");
    fs::create_dir(&root).unwrap();
    let mut binding = bind_initial(&root).unwrap();
    fs::rename(&root, area.path().join("old")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(matches!(
        inspect_binding(&mut binding).unwrap(),
        RootStatus::Paused(RootPauseReason::Replaced)
    ));
}

#[cfg(unix)]
#[test]
fn restart_loads_binding_and_stays_paused() {
    let area = tempdir().unwrap();
    let root = area.path().join("root");
    fs::create_dir(&root).unwrap();
    let binding = bind_initial(&root).unwrap();
    let mut loaded = binding.clone();
    fs::remove_dir(&root).unwrap();
    assert!(matches!(
        inspect_binding(&mut loaded).unwrap(),
        RootStatus::Paused(RootPauseReason::Missing)
    ));
}

#[cfg(unix)]
#[test]
fn reselect_advances_epoch_and_requires_manual_preview() {
    let area = tempdir().unwrap();
    let first = area.path().join("first");
    let second = area.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let old = bind_initial(&first).unwrap();
    let next = reselect(&old, &second).unwrap();
    assert_eq!(next.epoch, old.epoch + 1);
    assert!(matches!(
        next.status,
        RootBindingStatus::Paused {
            reason: RootPauseReason::ReselectedAwaitingManual
        }
    ));
    assert!(!timer_allowed(&next));
}

#[cfg(unix)]
#[test]
fn stale_preview_is_rejected() {
    let area = tempdir().unwrap();
    let first = area.path().join("first");
    let second = area.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let old = bind_initial(&first).unwrap();
    let mut next = reselect(&old, &second).unwrap();
    assert!(authorize_manual_after_preview(&mut next, old.epoch, &second, true).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_ancestor_is_rejected() {
    let area = tempdir().unwrap();
    let actual = area.path().join("actual");
    let alias = area.path().join("alias");
    fs::create_dir(&actual).unwrap();
    fs::create_dir(&actual.join("root")).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    assert!(bind_initial(alias.join("root")).is_err());
}

#[cfg(unix)]
#[test]
fn unknown_root_baseline_stays_unknown_and_survives_encrypted_reload() {
    use peerdav_lib::sync::index::{BaselineRecord, SyncIndex};
    let area = tempdir().unwrap();
    let db = area.path().join("index.sqlite");
    let mut index = SyncIndex::open(
        &db,
        zeroize::Zeroizing::new([7; 32]),
        "repo".into(),
        "mapping".into(),
    )
    .unwrap();
    index
        .unknown_root_baseline("sensitive-file-name", 4)
        .unwrap();
    assert!(matches!(
        index.root_baseline("sensitive-file-name", 4).unwrap(),
        BaselineRecord::Unknown
    ));
    let binding = bind_initial(area.path()).unwrap();
    index.record_binding(&binding).unwrap();
    drop(index);
    let index = SyncIndex::open(
        &db,
        zeroize::Zeroizing::new([7; 32]),
        "repo".into(),
        "mapping".into(),
    )
    .unwrap();
    let loaded = index.load_binding().unwrap().unwrap();
    assert!(loaded == binding);
    assert!(matches!(
        index.root_baseline("sensitive-file-name", 4).unwrap(),
        BaselineRecord::Unknown
    ));
    assert!(matches!(
        index.root_baseline("sensitive-file-name", 5).unwrap(),
        BaselineRecord::Unknown
    ));
}

#[cfg(unix)]
#[test]
fn empty_initial_root_is_ready_not_missing() {
    let area = tempdir().unwrap();
    let binding = bind_initial(area.path()).unwrap();
    assert!(matches!(binding.status, RootBindingStatus::Ready));
    assert!(!binding.requires_preview);
    assert!(timer_allowed(&binding));
}
