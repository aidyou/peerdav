mod append_common;

use append_common::Remote;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use peerdav_lib::cloud::traits::ObjectStore;
use peerdav_lib::sync::{
    merge,
    merge_types::{ChangeId, EntityId, LocalChange, RevisionValue},
};
use sha2::Digest;

#[tokio::test]
async fn pages_are_lazy_and_failure_is_not_a_complete_listing() {
    let remote = Remote::new(1);
    remote.put_object("a", b"a");
    remote.put_object("b", b"b");
    remote.fail_list_page(2);
    let store = remote.store();
    let mut listing = store.list("");
    assert!(remote.requests().is_empty());
    assert_eq!(listing.next().await.unwrap().unwrap().key, "a");
    assert_eq!(remote.requests().len(), 1);
    assert!(matches!(
        listing.next().await.unwrap(),
        Err(peerdav_lib::cloud::types::CloudError::Storage { .. })
    ));
    assert!(listing.next().await.is_none());
    assert_eq!(remote.requests().len(), 2);
}

#[tokio::test]
async fn visibility_is_independent_and_missing_requests_are_recorded() {
    let remote = Remote::new(1);
    remote.put_object("a", b"a");
    remote.put_object("b", b"b");
    remote.set_visible_keys(Some(vec!["a".into()]));
    remote.set_get_visible_keys(Some(vec!["b".into()]));
    remote.duplicate_list_page(1);
    let store = remote.store();
    let listed = store.list("").collect::<Vec<_>>().await;
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|item| item.as_ref().unwrap().key == "a"));
    assert!(matches!(
        store.get("a", None).await,
        Err(peerdav_lib::cloud::types::CloudError::NotFound)
    ));
    assert!(store.get("b", None).await.is_ok());
    remote.set_get_visible_keys(None);
    assert!(store.get("a", None).await.is_ok());
    assert!(store.get("missing", None).await.is_err());
    assert!(store.stat("missing").await.is_err());
    store.delete("b").await.unwrap();
    assert!(store.stat("b").await.is_err());
    assert!(
        remote
            .requests()
            .iter()
            .any(|r| r.operation == "delete" && r.key == "b")
    );
    assert_eq!(
        remote
            .requests()
            .iter()
            .filter(|r| r.key == "missing")
            .count(),
        2
    );
    let empty = store.list("empty/").collect::<Vec<_>>().await;
    assert!(empty.is_empty());
    assert!(remote.requests().iter().any(|r| r.key == "empty/#page=1"));
}

fn body(bytes: &'static [u8]) -> peerdav_lib::cloud::types::ObjectStream {
    Box::pin(stream::once(async move { Ok(Bytes::from_static(bytes)) }))
}

#[tokio::test]
async fn conditional_requests_and_same_byte_retry_are_exact() {
    use peerdav_lib::cloud::types::{CloudError, PutCondition};
    let remote = Remote::new(1);
    let store = remote.store();
    let first = store
        .put("x", body(b"one"), PutCondition::CreateOnly)
        .await
        .unwrap();
    assert!(matches!(
        store.put("x", body(b"bad"), PutCondition::CreateOnly).await,
        Err(CloudError::Precondition)
    ));
    store
        .put(
            "x",
            body(b"two"),
            PutCondition::MatchEtag(first.etag.clone().unwrap()),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .put(
                "x",
                body(b"bad"),
                PutCondition::MatchEtag(first.etag.unwrap())
            )
            .await,
        Err(CloudError::Precondition)
    ));
    remote.drop_next_put_response();
    assert!(matches!(
        store.put("new", body(b"frozen"), PutCondition::Any).await,
        Err(CloudError::Storage { .. })
    ));
    let reopened = remote.reopen().store();
    let verified = reopened
        .get("new", None)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert_eq!(
        verified
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .concat(),
        b"frozen"
    );
    reopened
        .put("new", body(b"frozen"), PutCondition::Any)
        .await
        .unwrap();
    let requests = remote.requests();
    let puts = requests
        .iter()
        .filter(|r| r.operation == "put")
        .collect::<Vec<_>>();
    assert_eq!(puts.len(), 6);
    assert!(matches!(puts[0].condition, Some(PutCondition::CreateOnly)));
    assert!(matches!(
        puts[2].condition,
        Some(PutCondition::MatchEtag(_))
    ));
    assert!(matches!(puts[5].condition, Some(PutCondition::Any)));
    assert_eq!(puts[4].bytes, puts[5].bytes);
}

#[test]
fn cas_head_wire_golden_is_unchanged() {
    use peerdav_lib::sync::repository_types::{Head, decode_head, encode_head};
    let bytes = br#"{"descriptor":{"version":1,"repo_id":"00000000000000000000000000000000","mode":"plain","chunker":{"name":"fastcdc","version":"5","normalization":"level1","level":1,"min":1,"average":2,"max":4,"seed":0}},"generation":7,"commit":null,"wrapped_master":null,"auth":"fixed-auth"}"#;
    let head: Head = decode_head(bytes).unwrap();
    assert_eq!(head.generation, 7);
    assert_eq!(encode_head(&head).unwrap(), bytes);
}

#[test]
fn revision_identity_and_v1_frame_golden_are_stable() {
    let change = LocalChange {
        entity: EntityId("entity".into()),
        change_id: ChangeId("change".into()),
        parents: vec![],
        value: RevisionValue::Tombstone,
    };
    let revision = merge::revision(change).unwrap();
    assert_eq!(
        revision.revision_id.0,
        "ef48f5d598c2aeeeff0c5dfcb9a2c78e26e1abd6a65cbb9e46fd0899f5b0bfb5"
    );
    assert_eq!(
        hex::encode(sha2::Sha256::digest(b"peerdav-s3/revision/v1\0")),
        "57c55bb545a69ed15b8feebb39a44f1e992c5a8545d38868d936e9a5d608f3b2"
    );
}

#[tokio::test]
async fn fake_reopens_shared_remote_and_records_exact_bytes() {
    let remote = Remote::new(2);
    remote.put_object("pre", b"seed");
    remote.set_visible_keys(Some(vec!["pre".into()]));
    remote.set_get_visible_keys(Some(vec!["pre".into(), "x".into()]));
    remote.fail_list_page(99);
    remote.duplicate_list_page(99);
    remote.drop_next_put_response();
    let store = remote.store();
    let body = Bytes::from_static(b"golden");
    let put_result = store
        .put(
            "x",
            Box::pin(stream::once(async move { Ok(body.clone()) })),
            peerdav_lib::cloud::types::PutCondition::CreateOnly,
        )
        .await;
    assert!(matches!(
        put_result,
        Err(peerdav_lib::cloud::types::CloudError::Storage { .. })
    ));
    let reopened = remote.reopen().store();
    let got = reopened.get("x", None).await.unwrap();
    let chunks = got.collect::<Vec<_>>().await;
    let bytes = chunks.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(bytes.concat(), b"golden");
    assert_eq!(remote.bytes_for("put"), vec![b"golden".to_vec()]);
    assert!(remote.requests().iter().any(|request| matches!(
        request.condition,
        Some(peerdav_lib::cloud::types::PutCondition::CreateOnly)
    )));
    assert!(remote.requests().iter().any(|request| request.key == "x"));
}
