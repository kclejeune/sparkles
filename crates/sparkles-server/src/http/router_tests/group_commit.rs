//! The experimental option reaches real persistent HTTP updates; every request
//! retains its receipt and the final durable dataset survives reopening.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opt_in_persistent_updates_keep_independent_durable_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dataset");
    let opts = StoreOptions {
        experimental_group_commit: true,
        ..Default::default()
    };
    let state = Arc::new(
        AppState::new(&temp.path().join("server"), opts, Duration::from_secs(30)).unwrap(),
    );
    let dataset = state.attach("ds", DbType::Persistent, Some(&root)).unwrap();
    assert!(dataset.store.options().experimental_group_commit);
    let app = router(state.clone());
    let mut writers = tokio::task::JoinSet::new();
    for n in 0..32 {
        let app = app.clone();
        writers.spawn(async move {
            let (r, headers) = sparql_update(
                &app,
                &format!("INSERT DATA {{ <urn:s{n}> <urn:p> <urn:o> }}"),
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert_eq!(r.json()["inserted"], 1);
            assert_eq!(r.json()["deleted"], 0);
            commit_header(&headers)
        });
    }
    let mut receipts = Vec::new();
    while let Some(receipt) = writers.join_next().await {
        receipts.push(receipt.unwrap());
    }
    receipts.sort_unstable();
    assert_eq!(receipts, (1..=32).collect::<Vec<_>>());
    assert_eq!(dataset.store.snapshot().len(), 32);
    assert_eq!(dataset.store.snapshot().commit, 32);
    let snapshot = dataset.store.snapshot();
    let predicate = snapshot.lookup_iri("urn:p").unwrap();
    let object = snapshot.lookup_iri("urn:o").unwrap();
    for n in 0..32 {
        let subject = snapshot.lookup_iri(&format!("urn:s{n}")).unwrap();
        assert!(
            snapshot
                .contains(&[subject, predicate, object, sparkles::id::Id::DEFAULT_GRAPH])
                .unwrap()
        );
    }
    drop(snapshot);
    let (r, h) = sparql_update(&app, "INSERT DATA { <urn:s0> <urn:p> <urn:o> }", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], 0);
    assert_eq!(commit_header(&h), 32, "a no-op is not credited as a commit");
    drop(dataset);
    drop(app);
    drop(state);
    let reopened = sparkles::store::Store::open(&root, Default::default()).unwrap();
    assert_eq!(reopened.snapshot().len(), 32);
    assert_eq!(reopened.snapshot().commit, 32);
}
