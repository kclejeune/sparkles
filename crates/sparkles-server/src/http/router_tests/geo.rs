//! The spatial index over HTTP: `/$/geo/{ds}` (status, enable, disable, rebuild), its
//! summary in dataset info and statistics, `geo` in `POST /$/datasets`, and its metrics.

use super::*;
use crate::state::task_state;
use std::sync::atomic::{AtomicBool, Ordering};

/// The acceptance fixture: 7 indexable geometries (one of them in `ex:G1`), one
/// malformed, one empty, one in an unknown CRS.
const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:B geo:hasDefaultGeometry ex:gB . ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
ex:C geo:hasDefaultGeometry ex:gC . ex:gC geo:asWKT "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"^^geo:wktLiteral .
ex:p1 geo:hasGeometry ex:g1 .  ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2 .  ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:p3 geo:hasGeometry ex:g3 .  ex:g3 geo:asGeoJSON "{\"type\":\"Point\",\"coordinates\":[30,30]}"^^geo:geoJSONLiteral .
ex:bad geo:hasGeometry ex:gX . ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral .
ex:nil geo:hasGeometry ex:gE . ex:gE geo:asWKT ""^^geo:wktLiteral .
ex:mars geo:hasGeometry ex:gM . ex:gM geo:asWKT "<http://example.org/crs/mars> POINT(1 1)"^^geo:wktLiteral .
ex:G1 { ex:p4 geo:hasGeometry ex:g4 . ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral . }
"#;

/// The router tests' server with the fixture in the base (compacted after the load).
fn geo_server() -> Server {
    let s = server();
    let store = &s.state.get("ds").unwrap().store;
    store
        .load(&[Source::from_bytes(
            FIXTURE.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    s
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

fn put(path: &str, body: &str) -> Request<Body> {
    Request::put(path)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post(path: &str) -> Request<Body> {
    Request::post(path).body(Body::empty()).unwrap()
}

fn delete(path: &str) -> Request<Body> {
    Request::delete(path).body(Body::empty()).unwrap()
}

/// Start a task and wait for it to end; returns the task's JSON.
async fn run_task(s: &Server, req: Request<Body>) -> J {
    let r = send(&s.app, req).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let t = r.json();
    assert_eq!(t["kind"], "geo-index", "{t}");
    let t = super::tasks::wait_done(&s.state, t["id"].as_str().unwrap()).await;
    assert_eq!(t.state, task_state::DONE, "{:?}", t.message);
    send(&s.app, get(&format!("/$/tasks/{}", t.id)))
        .await
        .json()
}

#[tokio::test]
async fn enable_status_rebuild_disable() {
    let s = geo_server();
    assert_eq!(
        send(&s.app, get("/$/geo/ds")).await.json(),
        serde_json::json!({ "enabled": false })
    );
    assert_eq!(
        send(&s.app, get("/$/geo/nope")).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(send(&s.app, get("/$/datasets/ds")).await.json()["geo"].is_null());
    assert!(send(&s.app, get("/$/stats/ds")).await.json()["geo"].is_null());
    // a rebuild needs an enabled index
    let r = send(&s.app, post("/$/geo/ds/rebuild")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"], "spatial index is not enabled");
    // invalid configurations name the problem
    for (body, needle) in [
        ("{", "invalid geo configuration"),
        (r#"{"distance":"flat"}"#, "invalid geo configuration"),
        (r#"{"maxVertices":0}"#, "maxVertices: must be positive"),
        (r#"{"predicates":["not an iri"]}"#, "predicates:"),
    ] {
        let r = send(&s.app, put("/$/geo/ds", body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
        let e = r.json()["error"].as_str().unwrap().to_string();
        assert!(e.contains(needle), "{body}: {e}");
    }
    // enable with the defaults (an empty body)
    let t = run_task(&s, put("/$/geo/ds", "")).await;
    assert!(t["message"].as_str().unwrap().contains("7 rows"), "{t}");
    // the status after the load
    let st = send(&s.app, get("/$/geo/ds")).await.json();
    assert_eq!(st["enabled"], true);
    assert_eq!(st["state"], "ready");
    assert_eq!(
        st["rows"],
        serde_json::json!({ "base": 7, "overlay": 0, "tail": 0 })
    );
    assert_eq!(st["literals"], 7);
    assert_eq!(
        st["skipped"],
        serde_json::json!({ "malformed": 1, "unknownCrs": 1, "tooLarge": 0, "empty": 1 })
    );
    assert_eq!(
        st["crs"],
        serde_json::json!({
            "http://www.opengis.net/def/crs/OGC/1.3/CRS84": 6,
            "http://www.opengis.net/def/crs/EPSG/0/4326": 1,
            "http://example.org/crs/mars": 1,
        })
    );
    assert_eq!(st["formatVersion"], 1);
    assert_eq!(st["config"]["distance"], "geodesic");
    assert!(st["lastBuild"]["rows"].as_u64().is_some(), "{st}");
    // the summary and the statistics
    let info = send(&s.app, get("/$/datasets/ds")).await.json();
    assert_eq!(
        info["geo"],
        serde_json::json!({ "state": "ready", "rows": 7 })
    );
    let stats = send(&s.app, get("/$/stats/ds")).await.json();
    assert_eq!(stats["geo"]["rows"]["base"], 7);
    // an insert lands in the tail
    let (r, _) = super::sparql_update(
        &s.app,
        "PREFIX geo: <http://www.opengis.net/ont/geosparql#> INSERT DATA { \
         <http://example.org/g5> geo:asWKT \"POINT(1 1)\"^^geo:wktLiteral }",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let st = send(&s.app, get("/$/geo/ds")).await.json();
    assert_eq!(
        (st["rows"]["base"].as_u64(), st["rows"]["tail"].as_u64()),
        (Some(7), Some(1))
    );
    // reconfigure, then rebuild
    let _ = run_task(&s, put("/$/geo/ds", r#"{"distance":"haversine"}"#)).await;
    let st = send(&s.app, get("/$/geo/ds")).await.json();
    assert_eq!(st["config"]["distance"], "haversine");
    assert_eq!(st["state"], "ready");
    let _ = run_task(&s, post("/$/geo/ds/rebuild")).await;
    let st = send(&s.app, get("/$/geo/ds")).await.json();
    assert_eq!(
        st["rows"]["base"].as_u64().unwrap()
            + st["rows"]["overlay"].as_u64().unwrap()
            + st["rows"]["tail"].as_u64().unwrap(),
        8,
        "{st}"
    );
    // disable
    assert_eq!(
        send(&s.app, delete("/$/geo/ds")).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&s.app, get("/$/geo/ds")).await.json()["enabled"],
        false
    );
    assert!(send(&s.app, get("/$/datasets/ds")).await.json()["geo"].is_null());
}

#[tokio::test]
async fn one_build_at_a_time() {
    let s = geo_server();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let id = s.state.start_task("geo-index", "ds", move |_| {
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok("stopped".into())
    });
    let r = send(&s.app, put("/$/geo/ds", "{}")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"], "spatial index build already running");
    stop.store(true, Ordering::Relaxed);
    super::tasks::wait_done(&s.state, &id.id).await;
    let _ = run_task(&s, put("/$/geo/ds", "{}")).await;
}

#[tokio::test]
async fn read_only_servers_refuse_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.read_only = true;
    let state = Arc::new(state);
    state.attach("ds", DbType::Mem, None).unwrap();
    let app = router(state);
    for req in [
        put("/$/geo/ds", "{}"),
        delete("/$/geo/ds"),
        post("/$/geo/ds/rebuild"),
    ] {
        assert_eq!(send(&app, req).await.status, StatusCode::FORBIDDEN);
    }
    assert_eq!(send(&app, get("/$/geo/ds")).await.status, StatusCode::OK);
}

#[tokio::test]
async fn datasets_created_with_an_index() {
    let s = server();
    let create = |body: &str| {
        Request::post("/$/datasets")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let r = send(
        &s.app,
        create(r#"{"dbName":"g1","dbType":"mem","geo":true}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["geo"],
        serde_json::json!({ "state": "ready", "rows": 0 })
    );
    let r = send(
        &s.app,
        create(r#"{"dbName":"g2","dbType":"mem","geo":{"distance":"haversine"}}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let st = send(&s.app, get("/$/geo/g2")).await.json();
    assert_eq!(st["config"]["distance"], "haversine");
    // without the option, no index
    let r = send(&s.app, create(r#"{"dbName":"g3","dbType":"mem"}"#)).await;
    assert!(r.json()["geo"].is_null());
    // a bad option creates nothing
    for (name, geo) in [("g4", r#"{"distance":"flat"}"#), ("g5", "3")] {
        let body = format!(r#"{{"dbName":"{name}","dbType":"mem","geo":{geo}}}"#);
        assert_eq!(
            send(&s.app, create(&body)).await.status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(&s.app, get(&format!("/$/datasets/{name}")))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn metrics_report_the_index() {
    let s = geo_server();
    let m = send(&s.app, get("/$/metrics")).await.text();
    assert!(!m.contains("sparkles_geo_rows"), "{m}");
    let _ = run_task(&s, put("/$/geo/ds", "")).await;
    let m = send(&s.app, get("/$/metrics")).await.text();
    for line in [
        "# TYPE sparkles_geo_rows gauge",
        "sparkles_geo_rows{dataset=\"ds\",part=\"base\"} 7",
        "sparkles_geo_rows{dataset=\"ds\",part=\"overlay\"} 0",
        "sparkles_geo_rows{dataset=\"ds\",part=\"tail\"} 0",
        "# TYPE sparkles_geo_build_seconds gauge",
        "sparkles_geo_candidates_total{dataset=\"ds\"} 0",
        "sparkles_geo_refined_total{dataset=\"ds\"} 0",
        "sparkles_geo_matches_total{dataset=\"ds\"} 0",
        "sparkles_geo_rechecked_total{dataset=\"ds\"} 0",
    ] {
        assert!(m.contains(line), "{line}\n{m}");
    }
    assert!(
        m.contains("sparkles_geo_build_seconds{dataset=\"ds\"} "),
        "{m}"
    );
    // a FILTER the index answers counts its work (with enough rows elsewhere for the
    // planner to prefer the index to a scan)
    let store = &s.state.get("ds").unwrap().store;
    let far: String = (0..5000)
        .map(|i| {
            format!(
                "<http://example.org/far{i}> <http://www.opengis.net/ont/geosparql#asWKT> \"POINT({} {})\"^^<http://www.opengis.net/ont/geosparql#wktLiteral> .\n",
                100 + i % 50,
                -40 + i / 50
            )
        })
        .collect();
    store
        .load(&[Source::from_bytes(
            far.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    store.wait_geo();
    let q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
             PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
             SELECT ?g { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \
             \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral)) }";
    let body: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("query", q)
        .finish();
    let r = send(
        &s.app,
        Request::post("/ds/sparql")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["results"]["bindings"].as_array().unwrap().len(), 2);
    let value = |m: &str, name: &str| -> u64 {
        let prefix = format!("{name}{{dataset=\"ds\"}} ");
        m.lines()
            .find_map(|l| l.strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("{name}\n{m}"))
            .parse()
            .unwrap()
    };
    let m = send(&s.app, get("/$/metrics")).await.text();
    let candidates = value(&m, "sparkles_geo_candidates_total");
    assert!(candidates >= 2, "{m}");
    assert!(value(&m, "sparkles_geo_refined_total") >= 2);
    assert_eq!(value(&m, "sparkles_geo_matches_total"), 2);
    // the same series in the JSON snapshot
    let j = send(&s.app, get("/$/metrics?format=json")).await.json();
    let ds = j["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "ds")
        .unwrap();
    assert_eq!(ds["geo"]["enabled"], true);
    assert_eq!(ds["geo"]["rows"]["base"], 5007);
    assert_eq!(ds["geo"]["candidates"], candidates);
    assert_eq!(ds["geo"]["matches"], 2);
    assert!(ds["geo"]["buildSeconds"].as_f64().is_some());
}

#[test]
fn spatial_work_is_summed_over_the_plan() {
    use sparkles::sparql::PlanInfo;
    let node = |counters: Option<J>, children: Vec<PlanInfo>| PlanInfo {
        operator: "x".into(),
        description: String::new(),
        columns: Vec::new(),
        sorted_on: Vec::new(),
        estimated_rows: 0.0,
        estimated_cost: 0.0,
        actual_rows: 0,
        time_ms: 0.0,
        cached: false,
        children,
        counters: counters.map(|c| c.as_object().unwrap().clone()),
        warnings: Vec::new(),
    };
    assert_eq!(crate::geo::plan_work(&node(None, vec![])), None);
    let scan = |c: u64| {
        node(
            Some(serde_json::json!({
                "candidates": c, "refined": c / 2, "matched": 1, "rechecked": 1, "index": "ready"
            })),
            vec![],
        )
    };
    let plan = node(None, vec![scan(10), node(None, vec![scan(4)])]);
    assert_eq!(crate::geo::plan_work(&plan), Some([14, 7, 2, 2]));
}
