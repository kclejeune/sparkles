//! Short requests run on the thread that received them (multi-threaded runtime only).

use super::*;
use crate::http::inline::RAN;
use std::sync::atomic::Ordering;

fn ran() -> u64 {
    RAN.load(Ordering::SeqCst)
}

async fn select(app: &Router, q: &str, accept: &str) -> Resp {
    let req = Request::post("/ds/sparql")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, accept)
        .body(Body::from(q.to_string()))
        .unwrap();
    send(app, req).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quick_query_runs_in_place_the_second_time_with_the_same_answer() {
    let s = server();
    let q = "SELECT ?s ?n WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?n } ORDER BY ?n";
    for accept in [
        "application/sparql-results+json",
        "text/csv",
        "application/x-sparkles+json",
    ] {
        let first = select(&s.app, q, accept).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.text());
        let before = ran();
        let second = select(&s.app, q, accept).await;
        assert_eq!(second.status, StatusCode::OK, "{}", second.text());
        assert!(ran() > before, "the second run is in place ({accept})");
        if accept == "application/x-sparkles+json" {
            // the document carries timings, which differ between runs
            let (a, b) = (first.json(), second.json());
            assert_eq!(a["results"], b["results"]);
            assert_eq!(a["head"], b["head"]);
        } else {
            assert_eq!(first.text(), second.text());
        }
    }
    // a graph result too
    let c = "CONSTRUCT { ?s <urn:named> ?n } WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?n }";
    let first = select(&s.app, c, "application/n-triples").await;
    let second = select(&s.app, c, "application/n-triples").await;
    assert_eq!(second.status, StatusCode::OK);
    let lines = |r: &Resp| {
        let mut l: Vec<String> = r.text().lines().map(str::to_string).collect();
        l.sort();
        l
    };
    assert_eq!(lines(&first), lines(&second));
    assert_eq!(lines(&second).len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_quick_query_reports_its_error_and_runs_on_the_pool_next() {
    let s = server();
    let form = |budget: &str| {
        Request::post(format!("/ds/sparql{budget}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(
                "query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D",
            ))
            .unwrap()
    };
    let first = send(&s.app, form("")).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    // the same text, quick last time, under a row budget it exceeds
    let before = ran();
    let r = send(&s.app, form("?max-rows=1")).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert!(ran() > before, "the failing run was in place");
    assert_eq!(r.json()["budget"], "rows");
    // a failed run is not known to be quick: the next one uses the blocking pool
    let ok = send(&s.app, form("")).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.text(), first.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_data_updates_run_in_place() {
    let s = server();
    let before = ran();
    let (r, h) = sparql_update(&s.app, "INSERT DATA { <urn:x> <urn:p> 1 }", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(commit_header(&h), 2);
    assert!(ran() > before, "a data update runs in place");
    let (r, h) = sparql_update(
        &s.app,
        "DELETE DATA { <urn:x> <urn:p> 1 } ; INSERT DATA { <urn:y> <urn:p> 2 }",
        Some("application/x-sparkles+json"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(commit_header(&h), 3);
    assert_eq!(r.json()["commit"]["seq"], 3);
    // a pattern update runs on the blocking pool, with the same result
    let (r, h) = sparql_update(&s.app, "DELETE WHERE { <urn:y> ?p ?o }", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(commit_header(&h), 4);
    assert_eq!(
        select_rows(&s.app, "SELECT * WHERE { ?s <urn:p> ?o }").await,
        0
    );
    // errors are reported as before
    let (r, _) = sparql_update(&s.app, "INSERT DATA { <urn:x> <urn:p> ?v }", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_data_update_waits_for_a_busy_writer_on_the_blocking_pool() {
    let s = server();
    let ds = s.state.get("ds").unwrap();
    let (held, release) = (
        std::sync::Arc::new(std::sync::Barrier::new(2)),
        std::sync::Arc::new(std::sync::Barrier::new(2)),
    );
    let holder = std::thread::spawn({
        let (ds, held, release) = (ds.clone(), held.clone(), release.clone());
        move || {
            let txn = ds.store.write();
            held.wait();
            release.wait();
            drop(txn);
        }
    });
    held.wait();
    let app = s.app.clone();
    let update = tokio::spawn(async move {
        sparql_update(&app, "INSERT DATA { <urn:w> <urn:p> 1 }", None).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !update.is_finished(),
        "the update waits for the writer lock"
    );
    release.wait();
    let (r, h) = update.await.unwrap();
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(commit_header(&h), 2);
    holder.join().unwrap();
}
