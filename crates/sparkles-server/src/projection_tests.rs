//! The projection routes (spec U11 §3.3): the PCA places three separated clusters apart
//! (A6), entities with several vectors sit at their mean, the density grid counts every
//! point, regions and limits sample by priority, the binary format matches the JSON,
//! `colour=` names values, a commit makes a projection stale, the neighbour layout runs
//! as a task and repeats with its seed, the server keeps at most four projections, and
//! the request checks. The `403` of a restricted caller is tested in
//! `router_tests::auth::overview`.

use crate::http::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use serde_json::{Value as J, json};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const EX: &str = "http://ex/";
const DIM: usize = 16;

/// A pseudo-random number in [-0.5, 0.5).
fn noise(i: u64) -> f32 {
    let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32 - 0.5
}

/// `per` entities in each of three clusters of class A, B and C, far apart in 16
/// dimensions, each with a group label. Entity a0 has a second vector.
fn data(per: usize) -> String {
    let mut t = String::new();
    for (c, class) in ["A", "B", "C"].iter().enumerate() {
        for i in 0..per {
            let v: Vec<String> = (0..DIM)
                .map(|d| {
                    let centre = if d % 3 == c { 10.0 } else { 0.0 };
                    format!("{}", centre + noise((c * 100_000 + i * DIM + d) as u64))
                })
                .collect();
            writeln!(
                t,
                "<{EX}{}{i}> a <{EX}{class}> ; <{EX}group> \"g{}\" ; <{EX}emb> \"[{}]\"^^<urn:x-sparkles:vector> .",
                class.to_lowercase(),
                i % 2,
                v.join(", ")
            )
            .unwrap();
        }
    }
    let v: Vec<String> = (0..DIM)
        .map(|d| if d % 3 == 0 { "12".into() } else { "0".into() })
        .collect();
    writeln!(
        t,
        "<{EX}a0> <{EX}emb> \"[{}]\"^^<urn:x-sparkles:vector> .",
        v.join(", ")
    )
    .unwrap();
    t
}

struct Server {
    _dir: tempfile::TempDir,
    app: Router,
    state: Arc<AppState>,
}

async fn server(per: usize) -> Server {
    server_with(per, DbType::Mem, None).await
}

async fn server_with(per: usize, kind: DbType, pool: Option<u64>) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        pool,
    )
    .unwrap();
    let state = Arc::new(state);
    let ds = state.attach("d", kind, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data(per).into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    ds.store.compact().unwrap();
    state.set_phase(Phase::Ready);
    let app = router(state.clone());
    let s = Server {
        _dir: dir,
        app,
        state,
    };
    index(&s, "e", "emb").await;
    s
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let ct = r
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let b = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, b.to_vec(), ct)
}

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, J) {
    let (s, b, _) = send(app, req).await;
    (s, serde_json::from_slice(&b).unwrap_or(J::Null))
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn wait_task(app: &Router, id: &str) -> J {
    for _ in 0..1000 {
        let (_, t) = json_of(app, get(&format!("/$/tasks/{id}"))).await;
        if t["state"] != "running" && t["state"] != "queued" {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("task {id} did not end");
}

/// Create index `name` on predicate `p` and wait for its build.
async fn index(s: &Server, name: &str, p: &str) {
    let (st, j) = json_of(
        &s.app,
        Request::put(format!("/$/vector/d/{name}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                r#"{{"predicate":"{EX}{p}","dimension":{DIM},"metric":"euclidean","hnsw":{{"m":8}},"exactThreshold":0}}"#
            )))
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    let t = wait_task(&s.app, j["task"]["id"].as_str().unwrap()).await;
    assert_eq!(t["state"], "done", "{t}");
}

fn points(j: &J) -> Vec<(String, f64, f64, Option<u64>)> {
    j["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["entity"].as_str().unwrap().to_string(),
                p["x"].as_f64().unwrap(),
                p["y"].as_f64().unwrap(),
                p["class"].as_u64(),
            )
        })
        .collect()
}

#[tokio::test]
async fn pca_places_the_three_classes_apart() {
    let s = server(300).await;
    let (st, sum) = json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    assert_eq!(st, StatusCode::OK, "{sum}");
    assert_eq!(sum["method"], "pca");
    assert_eq!(sum["entities"], 900);
    // a0 has two vectors and one point
    assert_eq!(sum["vectors"], 901);
    assert_eq!(sum["stale"], false);
    let (st, j) = json_of(&s.app, get("/$/vector/d/e/projection")).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    let classes: Vec<String> = j["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            c["iri"]
                .as_str()
                .unwrap()
                .trim_start_matches(EX)
                .to_string()
        })
        .collect();
    let pts = points(&j);
    assert_eq!(pts.len(), 900);
    // every point is nearer its own class's centroid than any other (A6)
    let mut sums: BTreeMap<u64, (f64, f64, f64)> = BTreeMap::new();
    for (_, x, y, c) in &pts {
        let e = sums
            .entry(c.expect("every entity has a class"))
            .or_default();
        e.0 += x;
        e.1 += y;
        e.2 += 1.0;
    }
    assert_eq!(sums.len(), 3);
    let centre: BTreeMap<u64, (f64, f64)> = sums
        .iter()
        .map(|(k, v)| (*k, (v.0 / v.2, v.1 / v.2)))
        .collect();
    for (e, x, y, c) in &pts {
        let near = centre
            .iter()
            .min_by(|a, b| {
                let da = (a.1.0 - x).powi(2) + (a.1.1 - y).powi(2);
                let db = (b.1.0 - x).powi(2) + (b.1.1 - y).powi(2);
                da.total_cmp(&db)
            })
            .unwrap()
            .0;
        assert_eq!(Some(*near), *c, "{e} sits with another class");
    }
    // the class names follow the data: entity a… is of class A
    let a = pts.iter().find(|p| p.0 == format!("{EX}a1")).unwrap();
    assert_eq!(classes[a.3.unwrap() as usize], "A");
    // two components carry most of the variance of three clusters
    let explained: f64 = j["explained"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .sum();
    assert!(explained > 0.8, "{explained}");
}

#[tokio::test]
async fn density_regions_limits_and_colours() {
    let s = server(300).await;
    let (st, _) = json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    assert_eq!(st, StatusCode::OK);
    let (_, all) = json_of(&s.app, get("/$/vector/d/e/projection")).await;
    let d = &all["density"];
    assert_eq!(d["size"], 256);
    let raw = base64::engine::general_purpose::STANDARD
        .decode(d["counts"].as_str().unwrap())
        .unwrap();
    assert_eq!(raw.len(), 256 * 256 * 4);
    let total: u64 = raw
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()) as u64)
        .sum();
    assert_eq!(total, 900);
    // a region: only points inside, and the same points first at any limit
    let b = all["bounds"].as_array().unwrap();
    let (x0, y0, x1, y1) = (
        b[0].as_f64().unwrap(),
        b[1].as_f64().unwrap(),
        b[2].as_f64().unwrap(),
        b[3].as_f64().unwrap(),
    );
    let mid = (x0 + x1) / 2.0;
    let bbox = format!("{x0},{y0},{mid},{y1}");
    let (_, half) = json_of(
        &s.app,
        get(&format!("/$/vector/d/e/projection?bbox={bbox}")),
    )
    .await;
    let inside = points(&half);
    assert!(
        inside.iter().all(|p| p.1 <= mid + 1e-4),
        "a point outside the region"
    );
    let expected = points(&all).iter().filter(|p| p.1 <= mid).count();
    assert_eq!(half["region"]["total"].as_u64().unwrap() as usize, expected);
    let (_, few) = json_of(&s.app, get("/$/vector/d/e/projection?limit=50")).await;
    assert_eq!(few["region"]["shown"], 50);
    assert_eq!(few["region"]["sampled"], true);
    let (_, few_half) = json_of(
        &s.app,
        get(&format!("/$/vector/d/e/projection?limit=50&bbox={bbox}")),
    )
    .await;
    let shown: std::collections::BTreeSet<String> =
        points(&few_half).into_iter().map(|p| p.0).collect();
    for p in points(&few).iter().filter(|p| p.1 <= mid) {
        assert!(shown.contains(&p.0), "zooming in lost {}", p.0);
    }
    // colour by a predicate
    let (_, col) = json_of(
        &s.app,
        get(&format!("/$/vector/d/e/projection?colour={EX}group")),
    )
    .await;
    let values = col["colour"]["values"].as_array().unwrap();
    assert_eq!(values.len(), 2);
    let counted: u64 = values.iter().map(|v| v["count"].as_u64().unwrap()).sum();
    assert_eq!(counted, 900);
    assert!(
        col["points"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["colour"].is_u64())
    );
}

#[tokio::test]
async fn the_binary_format_matches_the_json() {
    let s = server(50).await;
    json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    let (_, j) = json_of(&s.app, get("/$/vector/d/e/projection?limit=40")).await;
    let (st, b, ct) = send(
        &s.app,
        Request::get("/$/vector/d/e/projection?limit=40")
            .header(header::ACCEPT, "application/x-sparkles-points")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/x-sparkles-points"));
    assert_eq!(&b[..8], b"SPKPTS\0\x01");
    let hl = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    let head: J = serde_json::from_slice(&b[12..12 + hl]).unwrap();
    let n = head["count"].as_u64().unwrap() as usize;
    assert_eq!(n, 40);
    let tb = head["termsBytes"].as_u64().unwrap() as usize;
    let terms: Vec<&str> = std::str::from_utf8(&b[12 + hl..12 + hl + tb])
        .unwrap()
        .split('\n')
        .collect();
    let mut at = 12 + hl + tb;
    at = at.div_ceil(4) * 4;
    let f = |i: usize| f32::from_le_bytes(b[at + i * 4..at + i * 4 + 4].try_into().unwrap());
    let u = |i: usize| {
        let o = at + n * 8 + i * 4;
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    };
    for (i, p) in points(&j).iter().enumerate() {
        assert_eq!(terms[i], p.0);
        assert!((f(2 * i) as f64 - p.1).abs() < 1e-5);
        assert!((f(2 * i + 1) as f64 - p.2).abs() < 1e-5);
        assert_eq!(Some(u(i) as u64), p.3);
    }
    assert_eq!(b.len(), at + n * 8 + n * 4);
}

#[tokio::test]
async fn a_commit_makes_the_projection_stale() {
    let s = server(30).await;
    json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    let ds = s.state.get("d").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            format!("<{EX}late> a <{EX}A> .\n").into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let (_, j) = json_of(&s.app, get("/$/vector/d/e/projection")).await;
    assert_eq!(j["stale"], true);
    let (_, sum) = json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    assert_eq!(sum["stale"], false);
}

#[tokio::test]
async fn the_neighbour_layout_is_a_task_and_repeats_with_its_seed() {
    let s = server(60).await;
    let mut runs = Vec::new();
    for _ in 0..2 {
        let (st, b, _) = send(
            &s.app,
            post(
                "/$/vector/d/e/projection?method=neighbours",
                r#"{"seed": 7, "iterations": 30}"#,
            ),
        )
        .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{}", String::from_utf8_lossy(&b));
        let task: J = serde_json::from_slice(&b).unwrap();
        assert_eq!(task["kind"], "vector-projection");
        let t = wait_task(&s.app, task["id"].as_str().unwrap()).await;
        assert_eq!(t["state"], "done", "{t}");
        let (st, j) = json_of(&s.app, get("/$/vector/d/e/projection?method=neighbours")).await;
        assert_eq!(st, StatusCode::OK, "{j}");
        assert_eq!(j["method"], "neighbours");
        runs.push(points(&j));
    }
    assert_eq!(runs[0], runs[1]);
    // the PCA is kept apart from the layout
    let (st, _) = json_of(&s.app, get("/$/vector/d/e/projection?method=pca")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_server_keeps_four_projections() {
    let s = server(20).await;
    for name in ["f", "g", "h", "i"] {
        // each index gets its own predicate, a copy of emb
        let ds = s.state.get("d").unwrap();
        let mut t = String::new();
        for i in 0..20 {
            let v: Vec<String> = (0..DIM)
                .map(|d| format!("{}", noise((i * DIM + d) as u64 + 7)))
                .collect();
            writeln!(
                t,
                "<{EX}{name}{i}> <{EX}{name}emb> \"[{}]\"^^<urn:x-sparkles:vector> .",
                v.join(", ")
            )
            .unwrap();
        }
        ds.store
            .load(&[Source::from_bytes(
                t.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        ds.store.compact().unwrap();
        index(&s, name, &format!("{name}emb")).await;
    }
    for name in ["e", "f", "g", "h", "i"] {
        let (st, j) = json_of(&s.app, post(&format!("/$/vector/d/{name}/projection"), "")).await;
        assert_eq!(st, StatusCode::OK, "{name}: {j}");
    }
    assert_eq!(crate::projection::kept(&s.state).0, 4);
    // the least recently used, e, went first
    let (st, _) = json_of(&s.app, get("/$/vector/d/e/projection")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = json_of(&s.app, get("/$/vector/d/i/projection")).await;
    assert_eq!(st, StatusCode::OK);
    let metrics = send(&s.app, get("/$/metrics")).await;
    let text = String::from_utf8_lossy(&metrics.1).to_string();
    assert!(text.contains("sparkles_vector_projections 4"), "{text}");
}

#[tokio::test]
async fn requests_are_checked() {
    let s = server(20).await;
    let (st, j) = json_of(&s.app, get("/$/vector/d/e/projection")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(j.to_string().contains("POST"), "{j}");
    for (req, why) in [
        (post("/$/vector/d/nope/projection", ""), "no vector index"),
        (post("/$/vector/d/e/projection?method=umap", ""), "method"),
        (
            post("/$/vector/d/e/projection", r#"{"sample": 0}"#),
            "sample",
        ),
        (
            post("/$/vector/d/e/projection", r#"{"iterations": 5000}"#),
            "iterations",
        ),
        (
            post("/$/vector/d/e/projection", r#"{"colour": 1}"#),
            "unknown member",
        ),
        (post("/$/vector/d/e/projection", "[1]"), "object"),
    ] {
        let (st, j) = json_of(&s.app, req).await;
        assert!(
            st == StatusCode::BAD_REQUEST || st == StatusCode::NOT_FOUND,
            "{why}: {st} {j}"
        );
        assert!(j.to_string().contains(why), "{why}: {j}");
    }
    json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    for (uri, why) in [
        ("/$/vector/d/e/projection?bbox=1,2,3", "bbox"),
        ("/$/vector/d/e/projection?bbox=3,0,1,1", "bbox"),
        ("/$/vector/d/e/projection?limit=60000", "limit"),
        ("/$/vector/d/e/projection?colour=not%20an%20iri", "colour"),
    ] {
        let (st, j) = json_of(&s.app, get(uri)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri}: {j}");
        assert!(j.to_string().contains(why), "{why}: {j}");
    }
    let _ = json!({});
}

/// A projection of a branch is kept for that branch, not swept as a stale dataset.
#[tokio::test]
async fn a_branch_keeps_its_projection() {
    let s = server_with(20, DbType::Persistent, None).await;
    let (st, j) = json_of(&s.app, post("/$/branches/d", r#"{"name":"dev"}"#)).await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    let (st, j) = json_of(&s.app, post("/$/vector/d/e/projection?branch=dev", "")).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    let (st, j) = json_of(&s.app, get("/$/vector/d/e/projection?branch=dev")).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["entities"], 60);
}

/// An index replaced under the same name, with another configuration, forgets the old
/// index's projection.
#[tokio::test]
async fn a_replaced_index_forgets_its_projection() {
    let s = server(20).await;
    let (st, _) = json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    assert_eq!(st, StatusCode::OK);
    let (st, j) = json_of(
        &s.app,
        Request::put("/$/vector/d/e")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                r#"{{"predicate":"{EX}emb","dimension":{DIM},"metric":"cosine","hnsw":{{"m":8}},"exactThreshold":0}}"#
            )))
            .unwrap(),
    )
    .await;
    assert!(st.is_success(), "{j}");
    let t = wait_task(&s.app, j["task"]["id"].as_str().unwrap()).await;
    assert_eq!(t["state"], "done", "{t}");
    let (st, j) = json_of(&s.app, get("/$/vector/d/e/projection")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{j}");
}

/// An index created before its predicate had a vector projects the vectors inserted
/// since.
#[tokio::test]
async fn an_index_built_before_its_vectors_projects_them() {
    let s = server(5).await;
    index(&s, "late", "late").await;
    let v: Vec<String> = (0..DIM).map(|d| format!("{d}")).collect();
    s.state
        .get("d")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            format!(
                "<{EX}x> <{EX}late> \"[{}]\"^^<urn:x-sparkles:vector> .\n",
                v.join(", ")
            )
            .into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let (st, j) = json_of(&s.app, post("/$/vector/d/late/projection", "")).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["entities"], 1);
}

/// The query memory pool pays for a projection: one the pool cannot hold is refused with
/// `507` before it is computed, and a kept one holds about its own bytes.
#[tokio::test]
async fn the_memory_pool_pays_for_projections() {
    // 6,000 vectors need about 1.4 MB to compute, more than the whole pool
    let s = server_with(2000, DbType::Mem, Some(1 << 20)).await;
    let pool = s.state.catalog.pool().unwrap().clone();
    let (st, j) = json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    assert_eq!(pool.granted(), 0);
    // a neighbour layout's task is refused before it starts
    let (st, j) = json_of(
        &s.app,
        post("/$/vector/d/e/projection?method=neighbours", ""),
    )
    .await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    assert_eq!(pool.granted(), 0);
    // a kept projection holds a grant until it is dropped
    let s = server_with(200, DbType::Mem, Some(64 << 20)).await;
    let pool = s.state.catalog.pool().unwrap().clone();
    let (st, j) = json_of(&s.app, post("/$/vector/d/e/projection", "")).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert!(pool.granted() >= j["bytes"].as_u64().unwrap());
    assert!(
        pool.granted() <= sparkles::sparql::pool::GRANULE,
        "{}",
        pool.granted()
    );
}
