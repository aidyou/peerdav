use peerdav_lib::cloud::object_types::StorageIdentityAssociation::{Existing, New};
use peerdav_lib::cloud::{ObjectGuard, ObjectGuardError, ProtectedIdentity, ProtectedMapping};
use tempfile::tempdir;

fn identity(prefix: &str, alias: &str) -> ProtectedIdentity {
    ProtectedIdentity {
        endpoint: "HTTPS://Example.com:443/".into(),
        bucket: "bucket".into(),
        prefix: prefix.into(),
        alias: alias.into(),
    }
}
fn mapping(prefix: &str, alias: &str) -> ProtectedMapping {
    ProtectedMapping {
        identity: identity(prefix, alias),
        repository_id: "repo".into(),
    }
}

#[test]
fn empty_registry_rejects_wrong_key_after_reopen() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("guard.db");
    drop(ObjectGuard::open(&path, &[1; 32]).unwrap());
    assert!(matches!(
        ObjectGuard::open(&path, &[2; 32]),
        Err(ObjectGuardError::KeyOrCorruption)
    ));
}

#[test]
fn distinct_nondefault_ports_do_not_alias_and_adjacent_prefixes_do_not_overlap() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[1; 32]).unwrap();
    guard.register(mapping("a", "one")).unwrap();
    assert!(!guard.is_protected(&identity("ab", "two")).unwrap());
    let mut other_port = identity("a", "three");
    other_port.endpoint = "https://example.com:9443".into();
    assert!(!guard.is_protected(&other_port).unwrap());
    let mut default_port = identity("a/child", "four");
    default_port.endpoint = "https://example.com".into();
    assert!(guard.is_protected(&default_port).unwrap());
}

#[test]
fn empty_prefix_protects_entire_bucket_and_removal_is_forbidden() {
    let directory = tempdir().unwrap();
    let guard = ObjectGuard::open(directory.path().join("guard.db"), &[1; 32]).unwrap();
    guard.register(mapping("", "one")).unwrap();
    assert!(guard.is_protected(&identity("any/path", "two")).unwrap());
    assert!(matches!(
        guard.remove(&identity("", "one")),
        Err(ObjectGuardError::Protected)
    ));
}

#[cfg(unix)]
#[test]
fn database_and_lock_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let directory = tempdir().unwrap();
    let victim = directory.path().join("victim");
    std::fs::write(&victim, b"untouched").unwrap();
    let database = directory.path().join("guard.db");
    symlink(&victim, &database).unwrap();
    assert!(ObjectGuard::open(&database, &[1; 32]).is_err());
    std::fs::remove_file(&database).unwrap();
    symlink(&victim, directory.path().join("guard.db.lock")).unwrap();
    assert!(ObjectGuard::open(&database, &[1; 32]).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
}

#[test]
fn corrupted_sentinel_is_not_an_empty_registry() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("guard.db");
    drop(ObjectGuard::open(&path, &[1; 32]).unwrap());
    let database = rusqlite::Connection::open(&path).unwrap();
    database
        .execute("UPDATE object_guard_keycheck SET record=zeroblob(32)", [])
        .unwrap();
    drop(database);
    assert!(matches!(
        ObjectGuard::open(&path, &[1; 32]),
        Err(ObjectGuardError::KeyOrCorruption)
    ));
}

#[test]
fn register_and_reopen_and_alias_cannot_escape() {
    let d = tempdir().unwrap();
    let p = d.path().join("guard.db");
    let key = [7u8; 32];
    let g = ObjectGuard::open(&p, &key).unwrap();
    g.register(mapping("root", "one")).unwrap();
    drop(g);
    let g = ObjectGuard::open(&p, &key).unwrap();
    assert_eq!(g.mappings().unwrap().len(), 1);
    assert!(matches!(
        g.register(mapping("root/child", "two")),
        Err(ObjectGuardError::Conflict)
    ));
    assert!(matches!(
        ObjectGuard::open(&p, &[8u8; 32]),
        Err(ObjectGuardError::KeyOrCorruption) | Err(ObjectGuardError::Busy)
    ));
}
#[test]
fn discovery_and_mutation_lifecycle_is_strict() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1u8; 32]).unwrap();
    g.register_storage_identity(&identity("", "x"), New)
        .unwrap();
    assert!(matches!(
        g.admit(&identity("root/a", "x")),
        Err(ObjectGuardError::Protected)
    ));
    g.register(mapping("root", "x")).unwrap();
    assert!(matches!(
        g.admit(&identity("root/a", "x")),
        Err(ObjectGuardError::Protected)
    ));
    g.mark_discovery_complete(&identity("root", "x")).unwrap();
    assert!(matches!(
        g.begin_mutation(&identity("root/a", "different")),
        Err(ObjectGuardError::Protected)
    ));
    g.mark_discovery_complete(&identity("ordinary", "x"))
        .unwrap();
    let lease = g
        .begin_mutation(&identity("ordinary/a", "different"))
        .unwrap();
    assert!(matches!(
        g.begin_create(mapping("ordinary", "other")),
        Err(ObjectGuardError::OperationInProgress)
    ));
    drop(lease);
    g.begin_create(mapping("ordinary", "other")).unwrap();
}
#[test]
fn creating_lease_blocks_mutation_and_persists_failure() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[2u8; 32]).unwrap();
    g.register_storage_identity(&identity("", "a"), New)
        .unwrap();
    let lease = g.begin_create(mapping("new", "a")).unwrap();
    assert!(matches!(
        g.begin_mutation(&identity("new/x", "b")),
        Err(ObjectGuardError::Protected | ObjectGuardError::OperationInProgress)
    ));
    drop(lease);
    assert!(matches!(
        g.begin_mutation(&identity("new/x", "b")),
        Err(ObjectGuardError::Protected)
    ));
    assert_eq!(g.mappings().unwrap().len(), 1);
}

fn second(prefix: &str) -> ProtectedIdentity {
    let mut id = identity(prefix, "same-alias");
    id.endpoint = "https://other.example.com".into();
    id
}

#[test]
fn discovery_does_not_authorize_identity_or_creation() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    g.complete_discovery(&identity("", "x")).unwrap();
    assert!(matches!(
        g.admit(&identity("a", "x")),
        Err(ObjectGuardError::IdentityUnconfirmed)
    ));
    assert!(matches!(
        g.begin_create(mapping("a", "x")),
        Err(ObjectGuardError::IdentityUnconfirmed)
    ));
    g.register(mapping("protect", "x")).unwrap();
    assert!(g.is_protected(&identity("protect/a", "y")).unwrap());
}

#[test]
fn arbitrary_existing_id_is_rejected_without_epoch_change() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    assert!(matches!(
        g.register_storage_identity(&identity("", "x"), Existing("invented".into())),
        Err(ObjectGuardError::InvalidBinding)
    ));
    assert_eq!(g.registry_epoch().unwrap(), 0);
}

#[test]
fn associated_endpoints_share_protection_without_prefix_translation() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    let id = g
        .register_storage_identity(&identity("root", "a"), New)
        .unwrap();
    g.register_storage_identity(&second("other"), Existing(id))
        .unwrap();
    g.register(mapping("root", "a")).unwrap();
    assert!(g.is_protected(&second("root/child")).unwrap());
    assert!(!g.is_protected(&second("other/child")).unwrap());
    assert!(matches!(
        g.begin_create(ProtectedMapping {
            identity: second("root/child"),
            repository_id: "repo2".into()
        }),
        Err(ObjectGuardError::Conflict)
    ));
}

#[test]
fn associated_endpoints_share_discovery_and_active_leases() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    let id = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    g.register_storage_identity(&second(""), Existing(id))
        .unwrap();
    g.complete_discovery(&identity("", "a")).unwrap();
    let lease = g.begin_mutation(&identity("ordinary", "a")).unwrap();
    assert!(matches!(
        g.admit(&second("ordinary/child")),
        Err(ObjectGuardError::OperationInProgress)
    ));
    assert!(matches!(
        g.begin_create(ProtectedMapping {
            identity: second("ordinary"),
            repository_id: "r".into()
        }),
        Err(ObjectGuardError::OperationInProgress)
    ));
    drop(lease);
    g.admit(&second("ordinary/child")).unwrap();
}

#[test]
fn associated_nonoverlapping_prefixes_can_write_concurrently() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    let id = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    g.register_storage_identity(&second(""), Existing(id))
        .unwrap();
    g.complete_discovery(&identity("", "a")).unwrap();
    let one = g.admit(&identity("a", "a")).unwrap();
    let two = g.admit(&second("ab")).unwrap();
    drop((one, two));
}

#[test]
fn identity_and_epoch_survive_reopen() {
    let d = tempdir().unwrap();
    let path = d.path().join("x");
    let g = ObjectGuard::open(&path, &[1; 32]).unwrap();
    let id = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    g.register_storage_identity(&second(""), Existing(id.clone()))
        .unwrap();
    g.complete_discovery(&identity("", "a")).unwrap();
    drop(g);
    let g = ObjectGuard::open(&path, &[1; 32]).unwrap();
    assert_eq!(g.registry_epoch().unwrap(), 2);
    assert_eq!(
        g.register_storage_identity(&second(""), Existing(id))
            .unwrap()
            .len(),
        64
    );
    g.admit(&second("ordinary")).unwrap();
}

#[test]
fn alias_never_confirms_another_endpoint_or_bucket() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    g.register_storage_identity(&identity("", "same-alias"), New)
        .unwrap();
    g.complete_discovery(&second("")).unwrap();
    assert!(matches!(
        g.admit(&second("a")),
        Err(ObjectGuardError::IdentityUnconfirmed)
    ));
    let mut bucket = identity("a", "same-alias");
    bucket.bucket = "different-bucket".into();
    g.complete_discovery(&bucket).unwrap();
    assert!(matches!(
        g.admit(&bucket),
        Err(ObjectGuardError::IdentityUnconfirmed)
    ));
}

#[test]
fn rebinding_is_forbidden_and_default_port_is_canonical() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    let original = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    let other = g.register_storage_identity(&second(""), New).unwrap();
    assert_ne!(original, other);
    let mut canonical = identity("other", "b");
    canonical.endpoint = "https://example.com".into();
    assert!(matches!(
        g.register_storage_identity(&canonical, Existing(other)),
        Err(ObjectGuardError::InvalidBinding)
    ));
    assert!(matches!(
        g.register_storage_identity(&canonical, New),
        Err(ObjectGuardError::InvalidBinding)
    ));
    assert_eq!(
        g.register_storage_identity(&canonical, Existing(original.clone()))
            .unwrap(),
        original
    );
    assert_eq!(g.registry_epoch().unwrap(), 2);
}

#[test]
fn epoch_increments_only_for_committed_new_bindings_and_busy_is_conservative() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    assert_eq!(g.registry_epoch().unwrap(), 0);
    let id = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    assert_eq!(g.registry_epoch().unwrap(), 1);
    g.complete_discovery(&identity("", "a")).unwrap();
    let lease = g.admit(&identity("a", "a")).unwrap();
    assert!(matches!(
        g.register_storage_identity(&second("unrelated"), Existing(id.clone())),
        Err(ObjectGuardError::BindingBusy)
    ));
    assert_eq!(g.registry_epoch().unwrap(), 1);
    drop(lease);
    g.register_storage_identity(&second(""), Existing(id))
        .unwrap();
    assert_eq!(g.registry_epoch().unwrap(), 2);
}

#[test]
fn protection_recorded_before_confirmation_is_projected_after_association() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    g.register(mapping("root", "old")).unwrap();
    let old = g.mappings().unwrap();
    let id = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    g.register_storage_identity(&second(""), Existing(id))
        .unwrap();
    assert_eq!(g.mappings().unwrap(), old);
    assert!(g.is_protected(&second("root/child")).unwrap());
    g.complete_discovery(&second("")).unwrap();
    assert!(matches!(
        g.admit(&second("root/child")),
        Err(ObjectGuardError::Protected)
    ));
}

#[test]
fn deleted_identity_registry_record_is_corruption() {
    let d = tempdir().unwrap();
    let path = d.path().join("x");
    let g = ObjectGuard::open(&path, &[1; 32]).unwrap();
    g.register_storage_identity(&identity("", "a"), New)
        .unwrap();
    drop(g);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("DELETE FROM object_storage_identity_v1", [])
        .unwrap();
    drop(db);
    assert!(matches!(
        ObjectGuard::open(&path, &[1; 32]),
        Err(ObjectGuardError::KeyOrCorruption)
    ));
}

#[test]
fn same_physical_id_does_not_translate_buckets() {
    let d = tempdir().unwrap();
    let g = ObjectGuard::open(d.path().join("x"), &[1; 32]).unwrap();
    let id = g
        .register_storage_identity(&identity("", "a"), New)
        .unwrap();
    g.register(mapping("root", "a")).unwrap();
    let mut other = second("root");
    other.bucket = "other-bucket".into();
    g.register_storage_identity(&other, Existing(id)).unwrap();
    assert!(!g.is_protected(&other).unwrap());
    g.complete_discovery(&identity("", "a")).unwrap();
    assert!(matches!(g.admit(&other), Err(ObjectGuardError::Protected)));
    g.complete_discovery(&other).unwrap();
    g.admit(&other).unwrap();
}

#[test]
fn corrupted_identity_registry_and_cross_type_ciphertext_are_rejected() {
    for replacement in [
        "zeroblob(32)",
        "(SELECT record FROM object_guard_keycheck WHERE id=1)",
    ] {
        let d = tempdir().unwrap();
        let path = d.path().join("x");
        let g = ObjectGuard::open(&path, &[1; 32]).unwrap();
        g.register_storage_identity(&identity("", "a"), New)
            .unwrap();
        drop(g);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            &format!("UPDATE object_storage_identity_v1 SET record={replacement}"),
            [],
        )
        .unwrap();
        drop(db);
        assert!(matches!(
            ObjectGuard::open(&path, &[1; 32]),
            Err(ObjectGuardError::KeyOrCorruption)
        ));
    }
}
