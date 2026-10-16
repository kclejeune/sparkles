//! Short requests run on the thread that received them (multi-threaded runtime only).

use super::*;
use crate::http::inline::RAN;
use std::sync::atomic::Ordering;

fn ran() -> u64 {
    RAN.load(Ordering::SeqCst)
}

/// Runs of `query` on the test dataset count as quick however long they take, so that
/// what follows a quick run does not depend on the machine's load.
fn ignore_time(query: &str) {
    crate::http::QUICK.ignore_time(crate::http::inline::QuickQueries::key("ds", query));
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
    ignore_time(q);
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

/// A graph-algorithm call can take milliseconds on a cached projection and much longer
/// after a commit, and work in place cannot stop when its client goes away. A query
/// that calls an algorithm is never remembered as quick, so it never runs in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_algorithm_query_is_never_run_in_place() {
    let s = server();
    let q = "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:node ?n ; algo:component ?c } }";
    ignore_time(q);
    for _ in 0..2 {
        let r = select(&s.app, q, "application/sparql-results+json").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let key = crate::http::inline::QuickQueries::key("ds", q);
    assert!(!crate::http::QUICK.is_quick(key));
}

/// The algorithm namespace is found in the parsed query, not in its text. A prefixed
/// name can spell the IRI of the service without the namespace appearing in the text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_algorithm_named_by_a_prefix_is_never_run_in_place() {
    let s = server();
    for q in [
        // `\#` in a local name is the `#` of the namespace
        "PREFIX a: <urn:x-sparkles:> SELECT ?n ?c WHERE { SERVICE a:algo\\#wcc { [] a:algo\\#node ?n ; a:algo\\#component ?c } }",
        // the namespace is split between the prefix and the local name
        "PREFIX a: <urn:x-sparkles:algo> SELECT ?n ?c WHERE { SERVICE a:\\#wcc { [] a:\\#node ?n ; a:\\#component ?c } }",
    ] {
        assert!(!q.contains(sparkles::sparql::algosvc::NS), "{q}");
        ignore_time(q);
        for _ in 0..3 {
            let r = select(&s.app, q, "application/sparql-results+json").await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert!(
                !r.json()["results"]["bindings"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let key = crate::http::inline::QuickQueries::key("ds", q);
        assert!(!crate::http::QUICK.is_quick(key), "{q}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_quick_query_reports_its_error_and_runs_on_the_pool_next() {
    let s = server();
    ignore_time("SELECT ?s WHERE { ?s ?p ?o }");
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
