use super::*;

#[tokio::test]
async fn latency_fixture_records_overlapping_requests() {
    let remote = Remote::new(1);
    remote.put_object("a", b"a");
    remote.measure_parallelism();
    let store = remote.store();
    let (first, second) =
        futures_util::future::join(store.get("a", None), store.get("a", None)).await;
    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(remote.peak_parallelism(), 2);
}
