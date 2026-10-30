use peerdav_lib::sync::merge::{join, prepare_local_change, project, revision, validate};
use peerdav_lib::sync::merge_types::*;
use std::collections::BTreeMap;

fn change(entity: &str, id: &str, parents: Vec<RevisionId>, value: RevisionValue) -> Revision {
    revision(LocalChange {
        entity: EntityId(entity.into()),
        change_id: ChangeId(id.into()),
        parents,
        value,
    })
    .expect("valid revision")
}
fn live(name: &str) -> RevisionValue {
    RevisionValue::Live {
        parent_dir: EntityId("root".into()),
        name: name.into(),
        node: Node::Dir,
    }
}

#[test]
fn projection_retains_all_frontier_revisions() {
    let a = change("e", "a", vec![], live("a"));
    let b = change("e", "b", vec![a.revision_id.clone()], live("b"));
    let c = change("e", "c", vec![a.revision_id.clone()], live("c"));
    let mut m = BTreeMap::new();
    m.insert(a.revision_id.clone(), a);
    m.insert(b.revision_id.clone(), b);
    m.insert(c.revision_id.clone(), c);
    let p = project(&m).expect("projection");
    assert_eq!(p.entries.len(), 2);
    assert!(p.entries.iter().all(|entry| entry.conflict));
}

#[test]
fn projection_is_json_serializable_and_has_paths() {
    let r = change("entity", "create", vec![], live("name"));
    let mut m = BTreeMap::new();
    m.insert(r.revision_id.clone(), r);
    let p = project(&m).expect("projection");
    let json = serde_json::to_string(&p).expect("json projection");
    assert!(json.contains("projection_path"));
    assert_eq!(p.entries[0].logical_path, vec!["name"]);
}

#[test]
fn unknown_delete_is_rejected() {
    let err = prepare_local_change(
        EntityId("e".into()),
        ChangeId("x".into()),
        Baseline::Unknown,
        RevisionValue::Tombstone,
        &BTreeMap::new(),
        true,
    )
    .expect_err("unknown delete");
    assert!(matches!(err, MergeError::UnknownDelete));
}

#[test]
fn incomplete_delete_is_rejected() {
    let err = prepare_local_change(
        EntityId("e".into()),
        ChangeId("x".into()),
        Baseline::Known { frontier: vec![] },
        RevisionValue::Tombstone,
        &BTreeMap::new(),
        false,
    )
    .expect_err("incomplete delete");
    assert!(matches!(err, MergeError::IncompleteDelete));
}

#[test]
fn missing_parent_is_rejected() {
    let r = change("e", "x", vec![RevisionId("missing".into())], live("x"));
    let mut m = BTreeMap::new();
    m.insert(r.revision_id.clone(), r);
    assert!(matches!(validate(&m), Err(MergeError::MissingParent(_, _))));
}

#[test]
fn different_parent_entity_is_rejected() {
    let parent = change("other", "p", vec![], live("p"));
    let child = change("e", "c", vec![parent.revision_id.clone()], live("c"));
    let mut m = BTreeMap::new();
    m.insert(parent.revision_id.clone(), parent);
    m.insert(child.revision_id.clone(), child);
    assert!(matches!(validate(&m), Err(MergeError::ParentEntity(_, _))));
}

#[test]
fn tombstone_gets_recovery_path() {
    let r = change("deleted", "delete", vec![], RevisionValue::Tombstone);
    let mut m = BTreeMap::new();
    m.insert(r.revision_id.clone(), r);
    let p = project(&m).expect("projection");
    assert_eq!(p.entries[0].projection_path, vec![".recovered-deleted"]);
}

#[test]
fn long_unicode_name_is_utf8_safe_and_bounded() {
    let r = change("e", "x", vec![], live(&"界".repeat(200)));
    let mut m = BTreeMap::new();
    m.insert(r.revision_id.clone(), r);
    let p = project(&m).expect("projection");
    assert!(p.entries[0].projection_path[0].len() <= 255);
    assert!(std::str::from_utf8(p.entries[0].projection_path[0].as_bytes()).is_ok());
}

#[test]
fn duplicate_join_is_deduplicated() {
    let a = change("e", "a", vec![], live("a"));
    let mut m = BTreeMap::new();
    m.insert(a.revision_id.clone(), a);
    let set = MergeSet { revisions: m };
    assert_eq!(join(&set, &set).expect("join").revisions.len(), 1);
}

#[test]
fn delete_edit_frontier_retains_tombstone_and_materializes_edit_as_conflict() {
    let base = change("e", "base", vec![], live("name"));
    let deleted = change(
        "e",
        "delete",
        vec![base.revision_id.clone()],
        RevisionValue::Tombstone,
    );
    let edited = change("e", "edit", vec![base.revision_id.clone()], live("name"));
    let mut m = BTreeMap::new();
    for r in [base, deleted, edited] {
        m.insert(r.revision_id.clone(), r);
    }
    let p = project(&m).expect("projection");
    let live_entry = p
        .entries
        .iter()
        .find(|e| matches!(e.value, RevisionValue::Live { .. }))
        .unwrap();
    let tombstone = p
        .entries
        .iter()
        .find(|e| matches!(e.value, RevisionValue::Tombstone))
        .unwrap();
    assert_eq!(tombstone.projection_path, vec!["name"]);
    assert_ne!(live_entry.projection_path, vec!["name"]);
    assert!(live_entry.conflict);
}

#[test]
fn projection_is_stable_for_all_join_orders() {
    let a = change("e", "a", vec![], live("same"));
    let b = change("e", "b", vec![], live("same"));
    let mut ma = BTreeMap::new();
    ma.insert(a.revision_id.clone(), a.clone());
    let mut mb = BTreeMap::new();
    mb.insert(b.revision_id.clone(), b.clone());
    let ab = join(
        &MergeSet {
            revisions: ma.clone(),
        },
        &MergeSet {
            revisions: mb.clone(),
        },
    )
    .unwrap();
    let ba = join(&MergeSet { revisions: mb }, &MergeSet { revisions: ma }).unwrap();
    assert_eq!(
        project(&ab.revisions).unwrap(),
        project(&ba.revisions).unwrap()
    );
}

#[test]
fn concurrent_same_content_creates_are_retained() {
    let a = change("a", "create", vec![], live("same"));
    let b = change("b", "create", vec![], live("same"));
    let mut m = BTreeMap::new();
    m.insert(a.revision_id.clone(), a);
    m.insert(b.revision_id.clone(), b);
    let p = project(&m).unwrap();
    assert_eq!(p.entries.len(), 2);
    assert_ne!(p.entries[0].projection_path, p.entries[1].projection_path);
}

#[test]
fn suffix_lookalike_names_do_not_collide() {
    let a = change("a", "a", vec![], live("name"));
    let b = change("b", "b", vec![], live("name~other"));
    let mut m = BTreeMap::new();
    m.insert(a.revision_id.clone(), a);
    m.insert(b.revision_id.clone(), b);
    let p = project(&m).unwrap();
    assert_eq!(
        p.entries
            .iter()
            .map(|e| e.projection_path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
}

#[test]
fn file_and_directory_name_collision_is_aliased() {
    let file = change(
        "file",
        "f",
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId("root".into()),
            name: "node".into(),
            node: Node::File(FileNode {
                blocks: vec![],
                length: 0,
                digest: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            }),
        },
    );
    let dir = change("dir", "d", vec![], live("node"));
    let mut m = BTreeMap::new();
    m.insert(file.revision_id.clone(), file);
    m.insert(dir.revision_id.clone(), dir);
    let p = project(&m).unwrap();
    assert_eq!(p.entries.len(), 2);
    assert_ne!(p.entries[0].projection_path, p.entries[1].projection_path);
}

#[test]
fn cyclic_parent_paths_return_explicit_error() {
    let a = change(
        "a",
        "a",
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId("b".into()),
            name: "a".into(),
            node: Node::Dir,
        },
    );
    let b = change(
        "b",
        "b",
        vec![],
        RevisionValue::Live {
            parent_dir: EntityId("a".into()),
            name: "b".into(),
            node: Node::Dir,
        },
    );
    let mut m = BTreeMap::new();
    m.insert(a.revision_id.clone(), a);
    m.insert(b.revision_id.clone(), b);
    assert!(matches!(project(&m), Err(MergeError::PathCycle(_))));
}

#[test]
fn iterative_validation_handles_long_dag() {
    let mut m = BTreeMap::new();
    let mut parent = None;
    for i in 0..10_000 {
        let r = change(
            "e",
            &format!("{i}"),
            parent.clone().into_iter().collect(),
            live("x"),
        );
        parent = Some(r.revision_id.clone());
        m.insert(r.revision_id.clone(), r);
    }
    validate(&m).expect("long dag validates");
}
