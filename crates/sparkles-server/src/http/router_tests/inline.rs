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
    assert_eq!(
        crate::http::inline::ran_by_key(key),
        0,
        "no run was in place"
    );
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
        assert_eq!(
            crate::http::inline::ran_by_key(key),
            0,
            "no run was in place: {q}"
        );
    }
}

/// `EXISTS` is planned only when rows reach it, so over an empty outer pattern the
/// planner never meets the call. The algebra decides, and the text is never quick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_algorithm_inside_exists_over_an_empty_pattern_is_never_run_in_place() {
    let s = server();
    for q in [
        "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?s WHERE { ?s <urn:no:rows> ?o FILTER EXISTS { SERVICE algo:bfs { [] algo:source ?s ; algo:node ?n } } }",
        "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?s WHERE { ?s <urn:no:rows> ?o FILTER NOT EXISTS { SERVICE algo:bfs { [] algo:source ?s ; algo:node ?n } } }",
        // spelled with a prefix that hides the namespace
        "PREFIX a: <urn:x-sparkles:> SELECT ?s WHERE { ?s <urn:no:rows> ?o FILTER EXISTS { SERVICE a:algo\\#bfs { [] a:algo\\#source ?s ; a:algo\\#node ?n } } }",
    ] {
        ignore_time(q);
        for _ in 0..3 {
            let r = select(&s.app, q, "application/sparql-results+json").await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert!(
                r.json()["results"]["bindings"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let key = crate::http::inline::QuickQueries::key("ds", q);
        assert!(!crate::http::QUICK.is_quick(key), "{q}");
        assert_eq!(
            crate::http::inline::ran_by_key(key),
            0,
            "no run was in place: {q}"
        );
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

/// Automatic execution streams a scan of a few thousand rows on its first run. A streamed
/// answer that completes in one whole body is remembered as quick like an eager one, so
/// the next run of the same text runs in place, and a run that `send` stops early is not
/// remembered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quick_streamed_scan_runs_in_place_the_second_time() {
    let s = server();
    let mut data = String::new();
    for i in 0..3000 {
        data.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
    }
    let ds = s.state.attach("scan", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let scan = |q: &str, extra: &str| {
        Request::post(format!("/scan/sparql{extra}"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/x-sparkles+json")
            .body(Body::from(q.to_string()))
            .unwrap()
    };
    let rows = |r: &Resp| r.json()["rows"].as_array().unwrap().len();
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let key = crate::http::inline::QuickQueries::key("scan", q);
    crate::http::QUICK.ignore_time(key);
    let first = send(&s.app, scan(q, "")).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    assert_eq!(
        first.json()["meta"]["status"],
        "complete",
        "the first run streams"
    );
    assert_eq!(rows(&first), 3000);
    assert!(crate::http::QUICK.is_quick(key));
    let second = send(&s.app, scan(q, "")).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    assert_eq!(
        crate::http::inline::ran_by_key(key),
        2,
        "the first run of the new text and the second run are in place"
    );
    assert_eq!(second.json()["rows"], first.json()["rows"]);
    // a stream that `send` stops has not run the whole query
    let q = "SELECT ?o ?s WHERE { ?s <urn:p> ?o }";
    let key = crate::http::inline::QuickQueries::key("scan", q);
    crate::http::QUICK.ignore_time(key);
    let stopped = send(&s.app, scan(q, "?send=10")).await;
    assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.text());
    assert_eq!(stopped.json()["meta"]["status"], "stopped");
    assert!(!crate::http::QUICK.is_quick(key));
}

const BUDGETED_ASK: &str = "ASK {?left <urn:left> ?x . ?right <urn:right> ?y FILTER(?x = ?y)}";

fn budgeted_ask_server(name: &str) -> Server {
    let s = server();
    let ds = s.state.attach(name, DbType::Mem, None).unwrap();
    let mut data = String::new();
    for i in 0..64 {
        data.push_str(&format!("<urn:left:{i}> <urn:left> \"same\" .\n"));
        data.push_str(&format!("<urn:right:{i}> <urn:right> \"same\" .\n"));
    }
    ds.store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    s
}

async fn budgeted_ask(app: &Router, name: &str, options: &str) -> Resp {
    send(
        app,
        Request::post(format!("/{name}/sparql?nocache=true&{options}"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(BUDGETED_ASK))
            .unwrap(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quick_budgeted_ask_keeps_automatic_admission_on_repeated_http_requests() {
    let name = "quick-native-ask";
    let s = budgeted_ask_server(name);
    let key = crate::http::inline::QuickQueries::key(name, BUDGETED_ASK);
    crate::http::QUICK.ignore_time(key);
    for run in 0..4 {
        let r = budgeted_ask(&s.app, name, "execution=auto&max-rows=1024").await;
        assert_eq!(r.status, StatusCode::OK, "run {run}: {}", r.text());
        assert_eq!(r.json()["boolean"], true);
        assert!(crate::http::QUICK.is_quick(key));
    }
    assert_eq!(crate::http::inline::ran_by_key(key), 4);
    // Keeping Auto is a hint for automatic requests, not a forced execution mode.
    let eager = budgeted_ask(&s.app, name, "execution=eager&max-rows=1024").await;
    assert_eq!(eager.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(eager.json()["budget"], "rows");
    assert!(!crate::http::QUICK.is_quick(key));
    let recovered = budgeted_ask(&s.app, name, "execution=auto&max-rows=1024").await;
    assert_eq!(recovered.status, StatusCode::OK, "{}", recovered.text());
    assert_eq!(recovered.json()["boolean"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quick_eager_ask_keeps_auto_when_a_repeated_http_request_lowers_its_budget() {
    let name = "quick-eager-ask";
    let s = budgeted_ask_server(name);
    let key = crate::http::inline::QuickQueries::key(name, BUDGETED_ASK);
    crate::http::QUICK.ignore_time(key);
    let eager = budgeted_ask(&s.app, name, "execution=eager&max-rows=8192").await;
    assert_eq!(eager.status, StatusCode::OK, "{}", eager.text());
    assert_eq!(eager.json()["boolean"], true);
    assert!(crate::http::QUICK.is_quick(key));
    for run in 0..3 {
        let r = budgeted_ask(&s.app, name, "execution=auto&max-rows=1024").await;
        assert_eq!(r.status, StatusCode::OK, "run {run}: {}", r.text());
        assert_eq!(r.json()["boolean"], true);
    }
    assert_eq!(crate::http::inline::ran_by_key(key), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quick_ordinary_ask_stays_in_place_with_the_same_answer() {
    let s = server();
    let query = "ASK {<http://example.org/alice> <http://xmlns.com/foaf/0.1/name> \"Alice\"}";
    ignore_time(query);
    let key = crate::http::inline::QuickQueries::key("ds", query);
    for _ in 0..3 {
        let r = select(&s.app, query, "application/sparql-results+json").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["boolean"], true);
    }
    assert_eq!(crate::http::inline::ran_by_key(key), 3);
}

/// A current-thread runtime cannot execute in place, so its requests do not need
/// scheduling history even when the previous response was quick.
#[tokio::test]
async fn an_ineligible_request_does_not_read_quick_query_history() {
    let s = server();
    let query = "SELECT ?eligibility_probe WHERE { VALUES ?eligibility_probe { 42 } }";
    let key = crate::http::inline::QuickQueries::key("ds", query);
    crate::http::QUICK.ignore_time(key);
    assert!(!crate::http::inline::available());
    let before = crate::http::QUICK.reads_by_key(key);
    for _ in 0..2 {
        let response = select(&s.app, query, "application/sparql-results+json").await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text());
        assert_eq!(
            response.json()["results"]["bindings"][0]["eligibility_probe"]["value"],
            "42"
        );
    }
    assert_eq!(crate::http::inline::ran_by_key(key), 0);
    assert_eq!(crate::http::QUICK.reads_by_key(key), before);
}

/// The first run of a text the server has not seen runs in place, because the request
/// has a timeout. It cannot be cancelled when its client disconnects, so the timeout
/// must still stop it, and a text that failed or was slow runs on the blocking pool
/// next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_in_place_still_stops_at_its_timeout() {
    let s = server();
    let ds = s.state.attach("slow", DbType::Mem, None).unwrap();
    let mut data = String::new();
    for i in 0..3000 {
        data.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
    }
    ds.store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // nine million pairs, each tested by an expression that no pair passes
    let q = "SELECT ?a ?b WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?x + ?y < 0) }";
    let key = crate::http::inline::QuickQueries::key("slow", q);
    let request = || {
        Request::post("/slow/sparql?timeout=0.2")
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(q))
            .unwrap()
    };
    let started = std::time::Instant::now();
    let r = send(&s.app, request()).await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the timeout stopped the run after {:?}",
        started.elapsed()
    );
    assert_eq!(
        crate::http::inline::ran_by_key(key),
        1,
        "the first run was in place"
    );
    assert_eq!(
        crate::http::QUICK.history(key),
        crate::http::inline::History::Slow
    );
    let r = send(&s.app, request()).await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());
    assert_eq!(
        crate::http::inline::ran_by_key(key),
        1,
        "the second run used the blocking pool"
    );
}

/// A text that may call a SERVICE, which can wait on another server, runs its first run
/// on the blocking pool, where a disconnect still cancels it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_that_may_call_a_service_uses_the_blocking_pool() {
    let s = server();
    for q in [
        "SELECT * WHERE { SERVICE SILENT <http://127.0.0.1:9/none/sparql> { ?s ?p ?o } }",
        "SELECT * WHERE { service silent <http://127.0.0.1:9/none/sparql> { ?s ?p ?o } }",
        "SELECT * WHERE { \\u0053ERVICE SILENT <http://127.0.0.1:9/none/sparql> { ?s ?p ?o } }",
    ] {
        // the answer does not matter here: a loopback target is refused
        select(&s.app, q, "application/sparql-results+json").await;
        let key = crate::http::inline::QuickQueries::key("ds", q);
        assert_eq!(crate::http::inline::ran_by_key(key), 0, "{q}");
    }
}

#[test]
fn only_texts_without_a_service_keyword_run_their_first_run_in_place() {
    use crate::http::inline::first_run_in_place;
    assert!(first_run_in_place("SELECT * { ?s ?p ?o }"));
    assert!(!first_run_in_place(
        "SELECT * { SERVICE <urn:x> { ?s ?p ?o } }"
    ));
    assert!(!first_run_in_place(
        "SELECT * { sErViCe <urn:x> { ?s ?p ?o } }"
    ));
    assert!(!first_run_in_place(
        "SELECT * { \\u0053ERVICE <urn:x> { } }"
    ));
    assert!(!first_run_in_place(
        "SELECT * { \\U00000053ERVICE <urn:x> { } }"
    ));
}

/// A first run that fails after it ran, here because its answer outgrows the result
/// budget, or whose stream a client stops, leaves its text slow, so the next run of the
/// text uses the blocking pool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_that_fails_to_answer_sends_the_next_run_to_the_pool() {
    let s = server();
    let ds = s.state.attach("big", DbType::Mem, None).unwrap();
    let mut data = String::new();
    let long = "x".repeat(600);
    for i in 0..3000 {
        data.push_str(&format!("<urn:s{i}> <urn:p> \"{long}{i}\" .\n"));
    }
    ds.store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let request = |q: &str, extra: &str| {
        Request::post(format!("/big/sparql?{extra}"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(q.to_string()))
            .unwrap()
    };
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let key = crate::http::inline::QuickQueries::key("big", q);
    for _ in 0..2 {
        let r = send(&s.app, request(q, "execution=eager&max-result-mb=1")).await;
        assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    }
    assert_eq!(
        crate::http::inline::ran_by_key(key),
        1,
        "only the first run was in place"
    );
    assert_eq!(
        crate::http::QUICK.history(key),
        crate::http::inline::History::Slow
    );
    // a stream that `send` stops did not answer the whole query
    let q = "SELECT ?o ?s WHERE { ?s <urn:p> ?o }";
    let key = crate::http::inline::QuickQueries::key("big", q);
    for _ in 0..2 {
        let r = send(&s.app, request(q, "send=10")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    assert_eq!(crate::http::inline::ran_by_key(key), 1);
    assert_eq!(
        crate::http::QUICK.history(key),
        crate::http::inline::History::Slow
    );
}
