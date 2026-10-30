use peerdav_lib::sync::index::{BaselineRecord, SyncIndex};
use rusqlite::{Connection, params};
use zeroize::Zeroizing;

#[test]
fn missing_index_is_unknown_and_paths_are_not_persisted_in_plaintext() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sync.db");
    let mut index = SyncIndex::open(
        &path,
        Zeroizing::new([7; 32]),
        "repo-marker".into(),
        "mapping-marker".into(),
    )
    .expect("index");
    let secret_path = "sensitive-directory-marker/private-name-marker";
    assert_eq!(
        index.baseline(secret_path).expect("baseline"),
        BaselineRecord::Unknown
    );
    let baseline = BaselineRecord::Materialized {
        generation: 1,
        commit: "commit-marker".into(),
        snapshot: "sensitive-body-marker".into(),
    };
    index
        .confirm_materialized(secret_path, &baseline)
        .expect("save");
    assert_eq!(index.baseline(secret_path).expect("load"), baseline);
    for extension in ["sync.db", "sync.db-wal", "sync.db-shm"] {
        let file = directory.path().join(extension);
        if let Ok(bytes) = std::fs::read(file) {
            for marker in [
                "sensitive-directory-marker",
                "private-name-marker",
                "sensitive-body-marker",
                "repo-marker",
                "mapping-marker",
            ] {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|part| part == marker.as_bytes()),
                    "marker {marker}"
                );
            }
        }
    }
    drop(index);
    let reopened = SyncIndex::open(
        &path,
        Zeroizing::new([7; 32]),
        "repo-marker".into(),
        "mapping-marker".into(),
    )
    .expect("reopen");
    assert_eq!(reopened.baseline(secret_path).expect("load"), baseline);
    let new_root = SyncIndex::open(
        &path,
        Zeroizing::new([7; 32]),
        "repo-marker".into(),
        "different-mapping".into(),
    )
    .expect("new root");
    assert_eq!(
        new_root.baseline(secret_path).expect("baseline"),
        BaselineRecord::Unknown
    );
}

#[test]
fn swapped_rows_and_corrupt_ciphertext_fail_authentication() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sync.db");
    let mut index = SyncIndex::open(
        &path,
        Zeroizing::new([8; 32]),
        "repo".into(),
        "mapping".into(),
    )
    .expect("index");
    index.save("test", "a", &"first").expect("save");
    index.save("test", "b", &"second").expect("save");
    let connection = Connection::open(&path).expect("database");
    let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = connection
        .query_row("SELECT nonce,ciphertext FROM records LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("row");
    connection
        .execute(
            "UPDATE records SET nonce=?1,ciphertext=?2",
            params![nonce, ciphertext],
        )
        .expect("swap");
    let a = index.load::<String>("test", "a");
    let b = index.load::<String>("test", "b");
    assert!(a.is_err() || b.is_err());
    connection
        .execute("UPDATE records SET ciphertext=zeroblob(16)", [])
        .expect("tamper");
    assert!(index.load::<String>("test", "a").is_err());
    assert!(index.load::<String>("test", "b").is_err());
}

#[test]
fn future_schema_is_rejected_without_mutation() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sync.db");
    let connection = Connection::open(&path).expect("database");
    connection
        .pragma_update(None, "user_version", 99)
        .expect("version");
    assert!(
        SyncIndex::open(
            &path,
            Zeroizing::new([8; 32]),
            "repo".into(),
            "mapping".into()
        )
        .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .expect("version"),
        99
    );
}
