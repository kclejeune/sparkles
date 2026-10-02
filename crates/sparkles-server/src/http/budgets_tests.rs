//! Budgets a request asks for (`memory-mb`, `max-rows`, `max-rows-produced`,
//! `max-result-mb`), the rows-produced budget, and per-dataset storage quotas.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn json(&self) -> J {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.text()))
    }
    /// A `507` naming `budget`, with its `limit`.
    fn budget(&self, budget: &str) -> u64 {
        assert_eq!(
            self.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "{}",
            self.text()
        );
        let j = self.json();
        assert_eq!(j["budget"], budget, "{j}");
        assert!(j["requested"].as_u64().unwrap() > j["limit"].as_u64().unwrap());
        j["limit"].as_u64().unwrap()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp { status, body }
}

fn req(method: &str, uri: &str, ct: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(body.into())
        .unwrap()
}

async fn get(app: &Router, uri: &str) -> Resp {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn query(app: &Router, ds: &str, q: &str, params: &str) -> Resp {
    let uri = format!("/{ds}/sparql?{params}");
    send(
        app,
        req("POST", &uri, "application/sparql-query", q.to_string()),
    )
    .await
}

async fn update(app: &Router, ds: &str, u: &str, params: &str) -> Resp {
    let uri = format!("/{ds}/update?{params}");
    send(
        app,
        req("POST", &uri, "application/sparql-update", u.to_string()),
    )
    .await
}

fn ntriples(n: usize, tag: &str) -> String {
    (0..n)
        .map(|i| format!("<urn:{tag}{i}> <urn:p> \"value {tag} {i}\" .\n"))
        .collect()
}

fn multipart(name: &str, data: &str) -> (String, Vec<u8>) {
    let b = "XbOuNdArY";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/n-triples\r\n\r\n{data}\r\n--{b}--\r\n"
    );
    (
        format!("multipart/form-data; boundary={b}"),
        body.into_bytes(),
    )
}

fn server(
    opts: StoreOptions,
    set: impl FnOnce(&mut AppState),
) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(dir.path(), opts, Duration::from_secs(30)).unwrap();
    set(&mut st);
    let st = Arc::new(st);
    let c = st.attach("c", DbType::Mem, None).unwrap();
    c.store
        .load(&[Source::from_bytes(
            ntriples(9, "c").into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let app = router(st.clone());
    (dir, st, app)
}

fn len(st: &AppState, ds: &str) -> u64 {
    st.get(ds).unwrap().store.snapshot().len()
}

const CROSS4: &str = "SELECT * { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i . ?j ?k ?l }";
const CROSS5: &str = "SELECT * { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i . ?j ?k ?l . ?m ?n ?o }";

// ------------------------------------------------------------ request budgets ------

#[tokio::test]
async fn request_budgets_lower_the_servers() {
    let (_d, st, app) = server(StoreOptions::default(), |_| {});
    // 9^5 rows of 15 columns: about 7 MiB of intermediate results
    assert_eq!(query(&app, "c", CROSS5, "").await.status, StatusCode::OK);
    let r = query(&app, "c", CROSS5, "memory-mb=1").await;
    assert_eq!(r.budget("memory"), 1 << 20);
    // rows of an intermediate result
    let all = "SELECT * { ?s ?p ?o }";
    assert_eq!(query(&app, "c", all, "max-rows=5").await.budget("rows"), 5);
    assert_eq!(
        query(&app, "c", all, "max-rows=9").await.status,
        StatusCode::OK
    );
    // rows produced by all operators: the scan's 9 rows pass 8, and the 4-way cross
    // product's operators produce more than its 6561 result rows
    let r = query(&app, "c", all, "max-rows-produced=8").await;
    assert_eq!(r.budget("rows-produced"), 8);
    let r = query(&app, "c", CROSS4, "max-rows-produced=6561&nocache").await;
    assert_eq!(r.budget("rows-produced"), 6561);
    // the response: even the smallest encoding of 59049 rows of 15 terms passes 1 MiB,
    // so it is refused before anything is sent
    let r = query(&app, "c", CROSS5, "format=tsv&max-result-mb=1").await;
    assert_eq!(r.budget("result-bytes"), 1 << 20);
    // also in a form body, and on the dataset's own endpoint
    let form = format!(
        "query={}&max-rows=5",
        form_urlencoded::byte_serialize(all.as_bytes()).collect::<String>()
    );
    let r = send(
        &app,
        req("POST", "/c", "application/x-www-form-urlencoded", form),
    )
    .await;
    assert_eq!(r.budget("rows"), 5);
    // the WHERE clause of an update; nothing is written
    let ins = "INSERT { ?s <urn:q> ?o } WHERE { ?s ?p ?o }";
    let before = len(&st, "c");
    let r = update(&app, "c", ins, "max-rows-produced=3").await;
    assert_eq!(r.budget("rows-produced"), 3);
    let r = update(&app, "c", ins, "memory-mb=1&max-rows=4").await;
    assert_eq!(r.budget("rows"), 4);
    assert_eq!(len(&st, "c"), before);
    // malformed values are refused, whatever the endpoint
    for p in [
        "memory-mb=0",
        "max-rows=-1",
        "max-rows-produced=lots",
        "max-result-mb=1.5",
    ] {
        let r = query(&app, "c", all, p).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{p}: {}", r.text());
        assert!(
            r.json()["error"]
                .as_str()
                .unwrap()
                .contains("positive whole number")
        );
        assert_eq!(
            update(&app, "c", ins, p).await.status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn request_budgets_never_raise_the_servers() {
    let all = "SELECT * { ?s ?p ?o }";
    // larger than every server budget below: each request is held to the server's
    let big = "memory-mb=100000&max-rows=1000000&max-rows-produced=1000000&max-result-mb=1000\
               &format=tsv";
    type Set = fn(&mut AppState);
    let cases: [(Set, &str, &str, u64); 4] = [
        (
            |st| st.limits.query_memory_bytes = Some(4096),
            CROSS4,
            "memory",
            4096,
        ),
        (|st| st.limits.max_rows = 5, all, "rows", 5),
        (
            |st| st.limits.max_rows_produced = Some(5),
            all,
            "rows-produced",
            5,
        ),
        // nine rows of three terms are over 200 bytes of TSV
        (
            |st| st.limits.max_result_bytes = Some(200),
            all,
            "result-bytes",
            200,
        ),
    ];
    for (set, q, budget, limit) in cases {
        let (_d, _st, app) = server(StoreOptions::default(), set);
        assert_eq!(
            query(&app, "c", q, big).await.budget(budget),
            limit,
            "{budget}"
        );
    }
}

#[tokio::test]
async fn the_server_has_a_rows_produced_budget() {
    let (_d, _st, app) = server(StoreOptions::default(), |st| {
        st.limits.max_rows_produced = Some(5);
    });
    let r = query(&app, "c", "SELECT * { ?s ?p ?o }", "").await;
    assert_eq!(r.budget("rows-produced"), 5);
    let r = query(&app, "c", "ASK { <urn:c1> ?p ?o }", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let limits = get(&app, "/$/server").await.json()["limits"].clone();
    assert_eq!(limits["maxRowsProduced"], 5);
    assert_eq!(limits["maxDatasetBytes"], 0);
    // counted by budget kind
    let m = get(&app, "/$/metrics").await.text();
    assert!(
        m.contains(r#"sparkles_budget_exceeded_total{dataset="c",budget="rows-produced"} 1"#),
        "{m}"
    );
}

// --------------------------------------------------------------- storage quota ------

/// A quota of one byte refuses every write that adds quads.
#[tokio::test]
async fn a_quota_refuses_writes_that_add_data_and_never_reads() {
    let (_d, st, app) = server(StoreOptions::default(), |_| {});
    let p = st.create("p", DbType::Persistent).unwrap();
    p.store
        .load(&[Source::from_bytes(
            ntriples(20, "a").into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let q = get(&app, "/$/quota/p").await.json();
    assert_eq!(q["dataset"], "p");
    assert_eq!(q["source"], "default");
    assert!(
        q["maxBytes"].is_null() && q["defaultMaxBytes"].is_null(),
        "{q}"
    );
    assert!(q["usedBytes"].as_u64().unwrap() > 0);
    let put = |body: &str| req("PUT", "/$/quota/p", "application/json", body.to_string());
    let r = send(&app, put(r#"{"maxBytes": 1}"#)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["maxBytes"], 1);
    assert_eq!(r.json()["source"], "dataset");
    let before = len(&st, "p");
    let head = p.store.head_commit().seq;
    // every way of adding quads
    let small = ntriples(3, "b");
    let r = update(&app, "p", "INSERT DATA { <urn:x> <urn:p> 1 }", "").await;
    assert_eq!(r.budget("dataset-bytes"), 1);
    assert!(
        r.json()["error"].as_str().unwrap().contains("quota"),
        "{}",
        r.text()
    );
    for (method, uri) in [
        ("POST", "/p/data"),
        ("PUT", "/p/data?default"),
        ("POST", "/p"),
        ("PUT", "/p/data?graph=urn:g"),
    ] {
        let r = send(
            &app,
            req(method, uri, "application/n-triples", small.clone()),
        )
        .await;
        assert_eq!(r.budget("dataset-bytes"), 1, "{method} {uri}");
    }
    let (ct, mp) = multipart("b.nt", &small);
    assert_eq!(
        send(&app, req("POST", "/p/upload", &ct, mp))
            .await
            .budget("dataset-bytes"),
        1
    );
    assert_eq!(len(&st, "p"), before);
    assert_eq!(p.store.head_commit().seq, head);
    // reads go on, and so do writes that only delete
    let r = query(&app, "p", "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }", "").await;
    assert_eq!(r.status, StatusCode::OK);
    let r = update(&app, "p", "DELETE WHERE { <urn:a0> ?p ?o }", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(len(&st, "p"), before - 1);
    // usage against the quota in the stats and the metrics
    let s = get(&app, "/$/stats/p").await.json();
    assert_eq!(s["quota"]["maxBytes"], 1, "{s}");
    assert!(s["quota"]["usedBytes"].as_u64().unwrap() > 1);
    assert!(get(&app, "/$/stats/c").await.json()["quota"].is_null());
    let m = get(&app, "/$/metrics").await.text();
    assert!(
        m.contains(r#"sparkles_dataset_quota_bytes{dataset="p"} 1"#),
        "{m}"
    );
    assert!(
        m.contains(r#"sparkles_budget_exceeded_total{dataset="p",budget="dataset-bytes"} 6"#),
        "{m}"
    );
    // unlimited for this dataset, then back to the server's default
    let r = send(&app, put(r#"{"maxMb": 0}"#)).await;
    assert!(r.json()["maxBytes"].is_null() && r.json()["source"] == "dataset");
    assert_eq!(
        update(&app, "p", "INSERT DATA { <urn:x> <urn:p> 1 }", "")
            .await
            .status,
        StatusCode::OK
    );
    let r = send(
        &app,
        Request::delete("/$/quota/p").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["source"], "default");
    // malformed requests
    for body in [
        "{}",
        r#"{"maxBytes": -1}"#,
        r#"{"maxBytes": 1, "maxMb": 1}"#,
        "nope",
    ] {
        assert_eq!(
            send(&app, put(body)).await.status,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    // an in-memory dataset has --max-mem-dataset-mb instead
    let r = send(
        &app,
        req(
            "PUT",
            "/$/quota/c",
            "application/json",
            r#"{"maxBytes": 1}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(
        get(&app, "/$/quota/nope").await.status,
        StatusCode::NOT_FOUND
    );
}

/// `--max-dataset-mb`: the default quota of every persistent dataset, bulk loads
/// included, and clones larger than it.
#[tokio::test]
async fn the_default_quota_covers_bulk_loads_and_clones() {
    let opts = StoreOptions {
        max_disk_bytes: Some(1),
        bulk_threshold: 10,
        ..Default::default()
    };
    let (_d, st, app) = server(opts, |st| st.limits.max_dataset_bytes = Some(1));
    let p = st.create("p", DbType::Persistent).unwrap();
    let limits = get(&app, "/$/server").await.json()["limits"].clone();
    assert_eq!(limits["maxDatasetBytes"], 1);
    let q = get(&app, "/$/quota/p").await.json();
    assert_eq!(
        (q["maxBytes"].as_u64(), q["defaultMaxBytes"].as_u64()),
        (Some(1), Some(1))
    );
    // a bulk load (into an empty dataset, and above the threshold) is built, then refused
    let data = ntriples(50, "a");
    let r = send(
        &app,
        req("POST", "/p/data", "application/n-triples", data.clone()),
    )
    .await;
    assert_eq!(r.budget("dataset-bytes"), 1);
    assert_eq!(len(&st, "p"), 0);
    // a quota of its own lifts the default
    p.store.set_quota(Some(0)).unwrap();
    let r = send(&app, req("POST", "/p/data", "application/n-triples", data)).await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(len(&st, "p"), 50);
    // a clone gets the default quota, so this copy is refused and nothing is created
    let r = send(
        &app,
        Request::post("/$/datasets/p/clone?name=copy")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let t0 = std::time::Instant::now();
    let task = loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if !t.active() {
            break t;
        }
        assert!(t0.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(task.state, "failed");
    assert!(
        task.message.as_deref().unwrap_or("").contains("quota"),
        "{task:?}"
    );
    assert!(st.get("copy").is_none());
}
