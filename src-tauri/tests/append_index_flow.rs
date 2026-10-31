use hmac::{Hmac, Mac};
use peerdav_lib::sync::{
    append_index::{AppendIndex, KnownRecord},
    append_types::{DiscoveryState, ObjectRef, OperationState},
    index_types::IndexError,
};
use rusqlite::{Connection, params};
use sha2::Sha256;
use std::{
    path::Path,
    sync::{Arc, Barrier},
};
use zeroize::Zeroizing;

fn open(path: &Path) -> AppendIndex {
    AppendIndex::open(path, Zeroizing::new([7; 32]), "repo".into()).unwrap()
}
fn known(key: &str, state: DiscoveryState, fingerprint: Option<&str>) -> KnownRecord {
    KnownRecord {
        key: key.into(),
        state,
        fingerprint: fingerprint.map(str::to_owned),
        error: None,
    }
}
fn ledger(path: &Path, name: &str) -> (i64, i64) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT used,reserved FROM append_ledger WHERE name=?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}
fn prepare(index: &mut AppendIndex) {
    index
        .prepare_operation(
            "op",
            &["parent".into()],
            ObjectRef {
                kind: "tree".into(),
                digest: "root".into(),
            },
            &[],
            "digest",
        )
        .unwrap();
}

#[test]
fn known_iteration_is_incremental_and_reports_size_before_loading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    for key in ["a", "bb", "ccc"] {
        index
            .save(&known(key, DiscoveryState::Observed, None))
            .unwrap();
    }
    assert_eq!(index.next_known_key_len("").unwrap(), Some(1));
    assert_eq!(index.next_known_key("").unwrap().as_deref(), Some("a"));
    assert_eq!(index.next_known_key_len("a").unwrap(), Some(2));
    assert_eq!(index.next_known_key("bb").unwrap().as_deref(), Some("ccc"));
    assert_eq!(index.next_known_key("ccc").unwrap(), None);
    // Legacy validated states are not authentication context.
    index
        .save(&known("a", DiscoveryState::Validated, Some("digest")))
        .unwrap();
    assert_eq!(index.verification("a").unwrap(), None);
}

#[test]
fn known_union_survives_reopen_and_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let a = known("a", DiscoveryState::Observed, None);
    let b = known("b", DiscoveryState::Pending, Some("b-fp"));
    {
        let mut index = open(&path);
        index.save(&a).unwrap();
        index.save(&b).unwrap();
    }
    let c = known("c", DiscoveryState::Validated, Some("c-fp"));
    {
        let mut index = open(&path);
        assert_eq!(index.records().unwrap(), vec![a.clone(), b.clone()]);
        index.save(&c).unwrap();
    }
    assert_eq!(open(&path).records().unwrap(), vec![a, b, c]);
}

#[test]
fn validated_record_is_not_overwritten_by_lower_states() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let validated = known("a", DiscoveryState::Validated, Some("fp"));
    let mut index = open(&path);
    index.save(&validated).unwrap();
    for state in [
        DiscoveryState::Observed,
        DiscoveryState::Pending,
        DiscoveryState::Invalid,
    ] {
        let mut lower = known("a", state, None);
        lower.error = Some("untrusted observation".into());
        index.save(&lower).unwrap();
        assert_eq!(index.load("a").unwrap(), Some(validated.clone()));
    }
    drop(index);
    assert_eq!(open(&path).load("a").unwrap(), Some(validated));
}

#[test]
fn invalid_record_can_recover_to_validated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    let mut invalid = known("a", DiscoveryState::Invalid, None);
    invalid.error = Some("previous failure".into());
    index.save(&invalid).unwrap();
    let validated = known("a", DiscoveryState::Validated, Some("fp"));
    index.save(&validated).unwrap();
    drop(index);
    assert_eq!(open(&path).load("a").unwrap(), Some(validated));
}

#[test]
fn fingerprint_conflict_preserves_original_for_every_candidate_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    let original = known("a", DiscoveryState::Validated, Some("original"));
    index.save(&original).unwrap();
    for state in [
        DiscoveryState::Observed,
        DiscoveryState::Pending,
        DiscoveryState::Invalid,
        DiscoveryState::Validated,
    ] {
        for fp in ["different", ""] {
            assert!(matches!(
                index.save(&known("a", state.clone(), Some(fp))),
                Err(IndexError::InvalidRecord)
            ));
            assert_eq!(index.load("a").unwrap(), Some(original.clone()));
        }
    }
    drop(index);
    assert_eq!(open(&path).load("a").unwrap(), Some(original));
}

#[test]
fn fingerprints_can_be_filled_but_not_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    index
        .save(&known("a", DiscoveryState::Observed, None))
        .unwrap();
    index
        .save(&known("a", DiscoveryState::Pending, Some("fp")))
        .unwrap();
    index
        .save(&known("a", DiscoveryState::Validated, None))
        .unwrap();
    let expected = known("a", DiscoveryState::Validated, Some("fp"));
    assert_eq!(index.load("a").unwrap(), Some(expected.clone()));
    index
        .save(&known("a", DiscoveryState::Validated, None))
        .unwrap();
    drop(index);
    assert_eq!(open(&path).load("a").unwrap(), Some(expected));
}

#[test]
fn empty_known_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut index = open(&dir.path().join("index.db"));
    assert!(matches!(
        index.save(&known("", DiscoveryState::Observed, None)),
        Err(IndexError::InvalidRecord)
    ));
    assert!(index.records().unwrap().is_empty());
}

#[test]
fn wrong_key_or_repository_is_rejected_without_creating_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    drop(open(&path));
    assert!(matches!(
        AppendIndex::open(&path, Zeroizing::new([8; 32]), "repo".into()),
        Err(IndexError::Authentication)
    ));
    assert!(matches!(
        AppendIndex::open(&path, Zeroizing::new([7; 32]), "other".into()),
        Err(IndexError::Authentication)
    ));
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM append_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(open(&path));
}

#[test]
fn metadata_ciphertext_and_nonce_corruption_are_rejected() {
    for column in ["ciphertext", "nonce"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        drop(open(&path));
        Connection::open(&path)
            .unwrap()
            .execute(&format!("UPDATE append_meta SET {column}=X'00'"), [])
            .unwrap();
        assert!(matches!(
            AppendIndex::open(&path, Zeroizing::new([7; 32]), "repo".into()),
            Err(IndexError::Authentication)
        ));
    }
}

#[test]
fn missing_sentinel_on_populated_index_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    index
        .save(&known("a", DiscoveryState::Observed, None))
        .unwrap();
    drop(index);
    Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM append_meta", [])
        .unwrap();
    assert!(matches!(
        AppendIndex::open(&path, Zeroizing::new([7; 32]), "repo".into()),
        Err(IndexError::Authentication)
    ));
}

#[test]
fn legacy_record_sentinel_is_verified_and_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let record = known("a", DiscoveryState::Pending, Some("fp"));
    {
        let mut index = open(&path);
        index.save(&record).unwrap();
    }
    let db = Connection::open(&path).unwrap();
    let scope: Vec<u8> = db
        .query_row("SELECT scope FROM append_meta", [], |r| r.get(0))
        .unwrap();
    let mut frame = b"peerdav-append-index-v1".to_vec();
    for part in [&scope[..], b"meta", b"sentinel"] {
        frame.extend_from_slice(&(part.len() as u64).to_be_bytes());
        frame.extend_from_slice(part);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&[7; 32]).unwrap();
    mac.update(&frame);
    let token = mac.finalize().into_bytes().to_vec();
    db.execute("INSERT INTO append_records(scope,token,kind,name,nonce,ciphertext) SELECT scope,?1,'meta','sentinel',nonce,ciphertext FROM append_meta", params![token]).unwrap();
    db.execute("DELETE FROM append_meta", []).unwrap();
    // The legacy row must authenticate, not merely exist.
    assert_eq!(open(&path).load("a").unwrap(), Some(record));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM append_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.execute("DELETE FROM append_meta", []).unwrap();
    db.execute(
        "UPDATE append_records SET ciphertext=X'00' WHERE kind='meta'",
        [],
    )
    .unwrap();
    assert!(matches!(
        AppendIndex::open(&path, Zeroizing::new([7; 32]), "repo".into()),
        Err(IndexError::Authentication)
    ));
}

#[test]
fn corrupted_known_record_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    let record = known("a", DiscoveryState::Validated, Some("fp"));
    index.save(&record).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE append_records SET ciphertext=X'00' WHERE kind='known'",
            [],
        )
        .unwrap();
    assert!(matches!(index.load("a"), Err(IndexError::Authentication)));
    assert!(matches!(
        index.save(&record),
        Err(IndexError::Authentication)
    ));
}

#[test]
fn applied_is_terminal_and_idempotent_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    prepare(&mut index);
    assert!(matches!(
        index.set_operation_state("op", OperationState::Applied),
        Err(IndexError::InvalidRecord)
    ));
    for state in [
        OperationState::Publishing,
        OperationState::PublishedUnknown,
        OperationState::Published,
        OperationState::ApplyPending,
        OperationState::Applied,
    ] {
        index.set_operation_state("op", state).unwrap();
    }
    drop(index);
    let mut index = open(&path);
    let applied = index.load_operation("op").unwrap().unwrap();
    index
        .set_operation_state("op", OperationState::Applied)
        .unwrap();
    prepare(&mut index);
    for state in [
        OperationState::Prepared,
        OperationState::Publishing,
        OperationState::PublishedUnknown,
        OperationState::Published,
        OperationState::ApplyPending,
    ] {
        assert!(matches!(
            index.set_operation_state("op", state),
            Err(IndexError::InvalidRecord)
        ));
        assert_eq!(index.load_operation("op").unwrap(), Some(applied.clone()));
    }
}

#[test]
fn reserve_exact_limit_plus_one_and_reopen_accumulate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    {
        let mut index = open(&path);
        index.reserve("bytes", 4, 10).unwrap();
    }
    let mut index = open(&path);
    index.reserve("bytes", 6, 10).unwrap();
    assert_eq!(ledger(&path, "bytes"), (10, 10));
    assert!(matches!(
        index.reserve("bytes", 1, 10),
        Err(IndexError::InvalidRecord)
    ));
    assert!(matches!(
        index.reserve("bytes", 0, 9),
        Err(IndexError::InvalidRecord)
    ));
    index.reserve("bytes", 0, 10).unwrap();
    assert_eq!(ledger(&path, "bytes"), (10, 10));
    drop(index);
    assert!(open(&path).reserve("bytes", 1, 10).is_err());
    assert_eq!(ledger(&path, "bytes"), (10, 10));
}

#[test]
fn reserve_rejects_integer_overflow_without_resetting_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let max = i64::MAX as u64;
    let mut index = open(&path);
    index.reserve("bytes", max, u64::MAX).unwrap();
    for amount in [1, max + 1, u64::MAX] {
        assert!(matches!(
            index.reserve("bytes", amount, u64::MAX),
            Err(IndexError::InvalidRecord)
        ));
        assert_eq!(ledger(&path, "bytes"), (i64::MAX, i64::MAX));
    }
    drop(index);
    assert!(open(&path).reserve("bytes", 1, u64::MAX).is_err());
    assert_eq!(ledger(&path, "bytes"), (i64::MAX, i64::MAX));
}

#[test]
fn reserve_checks_reserved_overflow_and_negative_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let mut index = open(&path);
    index.reserve("bytes", 1, 10).unwrap();
    let db = Connection::open(&path).unwrap();
    for (used, reserved) in [(1, i64::MAX), (-1, 0), (0, -1)] {
        db.execute(
            "UPDATE append_ledger SET used=?1,reserved=?2",
            params![used, reserved],
        )
        .unwrap();
        assert!(matches!(
            index.reserve("bytes", 1, u64::MAX),
            Err(IndexError::InvalidRecord)
        ));
        assert_eq!(ledger(&path, "bytes"), (used, reserved));
    }
}

#[test]
fn concurrent_reservations_cannot_overspend() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    drop(open(&path));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut index = open(&path);
                barrier.wait();
                index.reserve("bytes", 6, 10)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(IndexError::InvalidRecord)))
            .count(),
        1
    );
    assert_eq!(ledger(&path, "bytes"), (6, 6));
}
