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

/// Spec X09 A4: a synchronization that fails during a fence of several commits fails every
/// one of their requests with 503, publishes none of them, and leaves the writer refusing
/// later writes. The failure is counted. The commit before the fence is acknowledged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_fence_fails_every_request_of_its_group_with_503() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dataset");
    let opts = StoreOptions {
        experimental_group_commit: true,
        group_commit_idle_direct: false,
        ..Default::default()
    };
    let state = Arc::new(
        AppState::new(&temp.path().join("server"), opts, Duration::from_secs(30)).unwrap(),
    );
    let dataset = state.attach("ds", DbType::Persistent, Some(&root)).unwrap();
    let app = router(state.clone());
    // the first commit's fence waits, so that the others queue behind it
    let (hit_tx, hit) = std::sync::mpsc::channel();
    let (go_tx, go) = std::sync::mpsc::channel::<()>();
    let go = std::sync::Mutex::new(go);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let armed = dataset.clone();
    dataset.store.set_failpoint(
        "group-sealed",
        Some(Arc::new(move |_| {
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => {
                    hit_tx.send(()).unwrap();
                    let _ = go.lock().unwrap().recv_timeout(Duration::from_secs(10));
                }
                // the fence that holds the other nine commits fails its synchronization
                1 => armed.store.fail_next_group_sync(),
                _ => {}
            }
        })),
    );
    let mut writers = tokio::task::JoinSet::new();
    let spawn = |writers: &mut tokio::task::JoinSet<StatusCode>, n: usize| {
        let app = app.clone();
        writers.spawn(async move {
            let (r, _) = sparql_update(
                &app,
                &format!("INSERT DATA {{ <urn:s{n}> <urn:p> <urn:o> }}"),
                None,
            )
            .await;
            r.status
        });
    };
    spawn(&mut writers, 0);
    tokio::task::spawn_blocking(move || hit.recv_timeout(Duration::from_secs(10)))
        .await
        .unwrap()
        .unwrap();
    for n in 1..10 {
        spawn(&mut writers, n);
    }
    // let the nine others reach the queue behind the paused fence
    tokio::time::sleep(Duration::from_millis(300)).await;
    go_tx.send(()).unwrap();
    let mut statuses = Vec::new();
    while let Some(status) = writers.join_next().await {
        statuses.push(status.unwrap());
    }
    let ok = statuses.iter().filter(|s| **s == StatusCode::OK).count();
    let unavailable = statuses
        .iter()
        .filter(|s| **s == StatusCode::SERVICE_UNAVAILABLE)
        .count();
    assert_eq!((ok, unavailable), (1, 9), "{statuses:?}");
    assert_eq!(
        dataset.store.snapshot().commit,
        1,
        "only the first commit was published"
    );
    assert_eq!(dataset.store.commit_metrics().failures, 1);
    let (r, _) = sparql_update(&app, "INSERT DATA { <urn:later> <urn:p> <urn:o> }", None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    // the hook holds the dataset
    dataset.store.set_failpoint("group-sealed", None);
    drop(dataset);
    drop(app);
    drop(state);
    // complete unacknowledged records may recover, but never as a gap
    let reopened = sparkles::store::Store::open(&root, Default::default()).unwrap();
    assert_eq!(reopened.snapshot().len(), reopened.snapshot().commit);
}

/// Spec X09 A6 and section 6: 32 concurrent writes of each HTTP kind join the group, each
/// keeps its own receipt, and every commit message is recorded. The first fence waits, so
/// that the others queue behind it and share the next one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_http_write_kind_groups_and_keeps_its_receipt_and_message() {
    for kind in [
        "update",
        "graph-post",
        "graph-put",
        "graph-delete",
        "upload",
        "patch",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("dataset");
        let opts = StoreOptions {
            experimental_group_commit: true,
            group_commit_idle_direct: false,
            ..Default::default()
        };
        let state = Arc::new(
            AppState::new(&temp.path().join("server"), opts, Duration::from_secs(30)).unwrap(),
        );
        let dataset = state.attach("ds", DbType::Persistent, Some(&root)).unwrap();
        let app = router(state.clone());
        // an empty dataset takes a Graph Store write through the bulk loader, so it starts
        // with data, and a delete needs a graph per request
        let seeds = if kind == "graph-delete" { 32 } else { 1 };
        for n in 0..seeds {
            let (r, _) = sparql_update(
                &app,
                &format!("INSERT DATA {{ GRAPH <urn:g{n}> {{ <urn:seed> <urn:p> {n} }} }}"),
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        }
        let before = dataset.store.snapshot().commit;
        let fences_before =
            dataset.store.commit_metrics().sync[sparkles::store::SyncPath::Group as usize].count();
        let (hit_tx, hit) = std::sync::mpsc::channel();
        let (go_tx, go) = std::sync::mpsc::channel::<()>();
        let go = std::sync::Mutex::new(go);
        let first = std::sync::atomic::AtomicBool::new(true);
        dataset.store.set_failpoint(
            "group-sealed",
            Some(Arc::new(move |_| {
                if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    hit_tx.send(()).unwrap();
                    let _ = go.lock().unwrap().recv_timeout(Duration::from_secs(10));
                }
            })),
        );
        let request = |n: usize| -> Request<Body> {
            let message = format!("write {n}");
            let b = Request::builder().header("sparkles-commit-message", message);
            match kind {
                "update" => b
                    .method("POST")
                    .uri("/ds/update")
                    .header(header::CONTENT_TYPE, "application/sparql-update")
                    .body(Body::from(format!(
                        "INSERT DATA {{ <urn:s{n}> <urn:p> <urn:o> }}"
                    ))),
                "graph-post" => b
                    .method("POST")
                    .uri(format!("/ds/data?graph=urn:g{n}"))
                    .header(header::CONTENT_TYPE, "text/turtle")
                    .body(Body::from("<urn:s> <urn:p> 1 .")),
                "graph-put" => b
                    .method("PUT")
                    .uri(format!("/ds/data?graph=urn:g{n}"))
                    .header(header::CONTENT_TYPE, "text/turtle")
                    .body(Body::from("<urn:s> <urn:p> 1 .")),
                "graph-delete" => b
                    .method("DELETE")
                    .uri(format!("/ds/data?graph=urn:g{n}"))
                    .body(Body::empty()),
                "upload" => b
                    .method("POST")
                    .uri("/ds/upload")
                    .header(header::CONTENT_TYPE, "text/turtle")
                    .body(Body::from(format!("<urn:s{n}> <urn:p> 1 ."))),
                _ => b
                    .method("POST")
                    .uri("/ds/patch")
                    .header(header::CONTENT_TYPE, "application/rdf-patch")
                    .body(Body::from(format!("A <urn:s{n}> <urn:p> 1 ."))),
            }
            .unwrap()
        };
        let mut writers = tokio::task::JoinSet::new();
        let spawn = |writers: &mut tokio::task::JoinSet<(usize, u64)>, n: usize| {
            let app = app.clone();
            let req = request(n);
            writers.spawn(async move {
                let (r, h) = send_h(&app, req).await;
                assert!(r.status.is_success(), "{n}: {} {}", r.status, r.text());
                (n, commit_header(&h))
            });
        };
        spawn(&mut writers, 0);
        tokio::task::spawn_blocking(move || hit.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        for n in 1..32 {
            spawn(&mut writers, n);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        go_tx.send(()).unwrap();
        let mut receipts = Vec::new();
        while let Some(r) = writers.join_next().await {
            receipts.push(r.unwrap());
        }
        let mut seqs: Vec<u64> = receipts.iter().map(|r| r.1).collect();
        seqs.sort_unstable();
        assert_eq!(
            seqs,
            (before + 1..=before + 32).collect::<Vec<_>>(),
            "{kind}"
        );
        let m = dataset.store.commit_metrics();
        assert_eq!(
            m.fallbacks.iter().sum::<u64>(),
            0,
            "{kind}: {:?}",
            m.fallbacks
        );
        let fences = m.sync[sparkles::store::SyncPath::Group as usize].count() - fences_before;
        assert!(fences <= 3, "{kind}: {fences} fences for 32 writes");
        for (n, seq) in receipts {
            let a = dataset.store.annotation(seq).unwrap();
            assert_eq!(a.message.as_deref(), Some(format!("write {n}").as_str()));
        }
        dataset.store.set_failpoint("group-sealed", None);
        drop(dataset);
        drop(app);
        drop(state);
        let reopened = sparkles::store::Store::open(&root, Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, before + 32, "{kind}");
        for seq in before + 1..=before + 32 {
            assert!(reopened.annotation(seq).is_some(), "{kind}: commit {seq}");
        }
    }
}
