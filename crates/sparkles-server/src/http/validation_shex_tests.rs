//! Write-time ShEx validation over HTTP: `/$/validation/{ds}` with `"language":
//! "shex"`, 422 rejections with ShEx results, the `Sparkles-Validation` header, metrics,
//! imports, switching languages and reopening.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::guard::config::SHEX_SCHEMA_SHEXC_FILE;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

const SCHEMA: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ;
  foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city [\"Paris\" \"Kyoto\"] }";

const DATA: &str = "@prefix ex: <http://ex.org/> . @prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name \"Alice\" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob a ex:Person ; foaf:name \"Bob\" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme a ex:Org ; foaf:name \"ACME\" ; ex:city \"Paris\" ; ex:mayor ex:bob .";

const MAP: &str = "{FOCUS a ex:Person}@ex:Person";

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, n: &str) -> String {
        self.headers
            .get(n)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

fn req(method: &str, uri: &str, ct: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn update(app: &Router, u: &str) -> Resp {
    send(
        app,
        req(
            "POST",
            "/v/update",
            "application/sparql-update",
            &format!("PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> {u}"),
        ),
    )
    .await
}

async fn get(app: &Router, uri: &str) -> Resp {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

fn state(dir: &std::path::Path, configure: impl FnOnce(&mut AppState)) -> Arc<AppState> {
    let mut st = AppState::new(dir, StoreOptions::default(), Duration::from_secs(30)).unwrap();
    configure(&mut st);
    Arc::new(st)
}

/// A server with dataset `v` holding the acceptance example's data.
async fn server(
    configure: impl FnOnce(&mut AppState),
) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path(), configure);
    st.create("v", DbType::Persistent).unwrap();
    let app = router(st.clone());
    let r = send(&app, req("PUT", "/v/data?default", "text/turtle", DATA)).await;
    assert!(r.status.is_success(), "{}", r.text());
    (dir, st, app)
}

async fn put_config(app: &Router, cfg: &J) -> Resp {
    send(
        app,
        req(
            "PUT",
            "/$/validation/v",
            "application/json",
            &cfg.to_string(),
        ),
    )
    .await
}

fn shex(schema: &str, map: &str) -> J {
    json!({"language": "shex", "mode": "reject",
        "schema": {"inline": schema, "format": "shexc"}, "shapeMap": map})
}

async fn fix_carol(app: &Router) {
    let r = update(
        app,
        "DELETE DATA { ex:carol foaf:age 200 } ; INSERT DATA { ex:carol foaf:name \"Carol\" }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// The acceptance example over HTTP.
#[tokio::test]
async fn reject_mode_over_http() {
    let (dir, st, app) = server(|_| {}).await;
    // carol does not conform
    let r = put_config(&app, &shex(SCHEMA, MAP)).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let j = r.json();
    assert_eq!(j["validation"]["language"], "shex");
    assert_eq!(
        j["validation"]["results"][0]["node"]["value"],
        "http://ex.org/carol"
    );
    assert_eq!(get(&app, "/$/validation/v").await.json()["config"], J::Null);

    fix_carol(&app).await;
    let r = put_config(&app, &shex(SCHEMA, MAP)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["language"], "shex");
    assert_eq!(j["config"]["format"], 2);
    assert_eq!(j["config"]["schema"]["file"], SHEX_SCHEMA_SHEXC_FILE);
    assert!(j["config"]["schema"].get("inline").is_none());
    assert_eq!(j["status"]["shapeCount"], 2);
    assert_eq!(j["status"]["associations"], 3);
    assert_eq!(j["status"]["baseline"]["conforms"], true);
    let root = st.get("v").unwrap().store.root().unwrap().to_path_buf();
    assert!(root.join(SHEX_SCHEMA_SHEXC_FILE).exists());

    // a person without a name
    let r = update(&app, "INSERT DATA { ex:dave a ex:Person }").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let v = &r.json()["validation"];
    assert_eq!(v["language"], "shex");
    assert_eq!(v["blocking"], 1);
    assert_eq!(v["total"], 4);
    assert_eq!(v["threshold"], "violation");
    assert_eq!(v["results"][0]["status"], "nonconformant");
    assert_eq!(v["results"][0]["node"]["value"], "http://ex.org/dave");
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .starts_with("ShEx validation failed: 1 nonconformant association")
    );
    let h = r.header("sparkles-validation");
    assert!(
        h.starts_with("status=rejected, mode=reject, strategy=incremental, lang=shex, blocking=1,"),
        "{h}"
    );
    // there is no Turtle report: still JSON
    let mut rq = req(
        "POST",
        "/v/update",
        "application/sparql-update",
        "PREFIX ex: <http://ex.org/> INSERT DATA { ex:dave a ex:Person }",
    );
    rq.headers_mut()
        .insert(header::ACCEPT, "text/turtle".parse().unwrap());
    let r = send(&app, rq).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.header("content-type").starts_with("application/json"));
    assert_eq!(
        r.json()["validation"]["results"][0]["status"],
        "nonconformant"
    );

    // a write to a graph outside the data graph is skipped
    let r = update(&app, "INSERT DATA { GRAPH ex:g { ex:erin a ex:Person } }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.header("sparkles-validation")
            .starts_with("status=skipped, mode=reject, strategy=none, lang=shex,"),
        "{}",
        r.header("sparkles-validation")
    );
    let r = update(
        &app,
        "INSERT DATA { ex:dave a ex:Person ; foaf:name \"Dave\" }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.header("sparkles-validation").contains("status=passed"));

    let s = get(&app, "/$/validation/v").await.json();
    assert_eq!(s["language"], "shex");
    assert_eq!(s["status"]["counters"]["rejected"], 2);
    assert_eq!(s["status"]["counters"]["skipped"], 1);

    let m = get(&app, "/$/metrics").await.text();
    let line = r#"sparkles_validation_total{dataset="v",language="shex",status="rejected"} 2"#;
    assert!(m.lines().any(|l| l == line), "{m}");

    // reopened: the guard is installed from the copy
    drop(app);
    drop(st);
    let st = state(dir.path(), |_| {});
    let app = router(st.clone());
    let r = update(&app, "INSERT DATA { ex:fay a ex:Person }").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(
        get(&app, "/$/validation/v").await.json()["language"],
        "shex"
    );
}

#[tokio::test]
async fn unusable_configurations_are_400() {
    let (_d, _st, app) = server(|_| {}).await;
    let bad = |schema: &str, map: &str| shex(schema, map);
    let ok = "PREFIX ex: <http://ex.org/> ex:S { ex:p . }";
    for (cfg, msg) in [
        (bad("ex:S {", "<urn:a>@START"), "schema: line 1"),
        (bad(ok, "<urn:a>@ex:T"), "does not define"),
        (bad(ok, "SPARQL '''SELECT ?focus {}'''@ex:S"), "SPARQL"),
        (
            bad("PREFIX ex: <http://ex.org/> ex:S EXTERNAL", "<urn:a>@ex:S"),
            "EXTERNAL",
        ),
        (
            json!({"language": "shex", "mode": "reject", "threshold": "warning",
                "schema": {"inline": ok}, "shapeMap": "<urn:a>@<http://ex.org/S>"}),
            "threshold",
        ),
        (
            json!({"language": "shex", "mode": "reject", "schema": {}, "shapeMap": "<urn:a>@<http://ex.org/S>"}),
            "schema",
        ),
    ] {
        let r = put_config(&app, &cfg).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{cfg}: {}", r.text());
        assert!(r.text().contains(msg), "{cfg}: {}", r.text());
    }
    assert_eq!(get(&app, "/$/validation/v").await.json()["config"], J::Null);
}

/// Imports resolve from `--load-dir` (relative IRIs against it); a file outside it, or
/// an address the outbound policy refuses, is a 400.
#[tokio::test]
async fn imports_from_the_load_directory() {
    let load = tempfile::tempdir().unwrap();
    std::fs::write(
        load.path().join("common.shex"),
        "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
         <http://ex.org/Named> { foaf:name xsd:string }",
    )
    .unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.shex"), "<http://ex.org/X> {}").unwrap();
    let files = sparkles::sparql::FileLoads::under(load.path()).unwrap();
    let (dir, st, app) = server(move |st| st.file_loads = files).await;
    fix_carol(&app).await;
    let schema = |import: &str| {
        format!(
            "PREFIX ex: <http://ex.org/> IMPORT <{import}> ex:Person @ex:Named AND EXTRA a {{ a [ex:Person] }}"
        )
    };
    let outside_url = sparkles_shex::resolve::file_url(&outside.path().join("x.shex"));
    for import in [outside_url.as_str(), "http://10.0.0.1/x"] {
        let r = put_config(&app, &shex(&schema(import), MAP)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{import}: {}", r.text());
        assert!(r.text().contains("import"), "{}", r.text());
    }
    let r = put_config(&app, &shex(&schema("common"), MAP)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["config"]["schema"]["file"], "validation-schema.json");
    assert_eq!(j["config"]["schema"]["prefixes"]["ex"], "http://ex.org/");
    assert_eq!(j["status"]["shapeCount"], 2);
    let r = update(&app, "INSERT DATA { ex:dave a ex:Person }").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());

    // the import is not read again when the dataset is reopened
    std::fs::remove_file(load.path().join("common.shex")).unwrap();
    drop(app);
    drop(st);
    let st = state(dir.path(), |_| {});
    let app = router(st.clone());
    let r = update(&app, "INSERT DATA { ex:dave a ex:Person }").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // a predicate no triple constraint mentions (no shape is CLOSED) is not validated
    let r = update(&app, "INSERT DATA { ex:alice ex:nickname \"Al\" }").await;
    assert!(
        r.header("sparkles-validation")
            .starts_with("status=skipped"),
        "{}",
        r.header("sparkles-validation")
    );
}

/// SHACL → ShEx leaves only the ShEx files; DELETE removes every file; a reopened
/// dataset then has no guard.
#[cfg(feature = "shacl")]
#[tokio::test]
async fn switching_languages() {
    let (dir, st, app) = server(|_| {}).await;
    fix_carol(&app).await;
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
        ex:S a sh:NodeShape ; sh:targetClass ex:Person ;
          sh:property [ sh:path <http://xmlns.com/foaf/0.1/name> ; sh:minCount 1 ] .";
    let r = put_config(
        &app,
        &json!({"mode": "reject", "shapes": {"inline": shapes}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // SHACL configurations are written as format 2 too
    assert_eq!(r.json()["language"], "shacl");
    assert_eq!(r.json()["config"]["format"], 2);
    assert_eq!(r.json()["config"]["language"], "shacl");
    let root = st.get("v").unwrap().store.root().unwrap().to_path_buf();
    assert!(
        root.join(sparkles::guard::config::SHACL_SHAPES_FILE)
            .exists()
    );

    let r = put_config(&app, &shex(SCHEMA, MAP)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        !root
            .join(sparkles::guard::config::SHACL_SHAPES_FILE)
            .exists()
    );
    assert!(root.join(SHEX_SCHEMA_SHEXC_FILE).exists());
    assert_eq!(
        sparkles::guard::config::config_language(&root).unwrap(),
        Some(sparkles::guard::GuardLanguage::Shex)
    );

    let r = send(
        &app,
        Request::delete("/$/validation/v")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    for f in sparkles::guard::config::FILES {
        assert!(!root.join(f).exists(), "{f}");
    }
    drop(app);
    drop(st);
    let st = state(dir.path(), |_| {});
    let app = router(st.clone());
    assert_eq!(get(&app, "/$/validation/v").await.json()["config"], J::Null);
    let r = update(&app, "INSERT DATA { ex:dave a ex:Person }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// A schema kept in a ShExR graph of the dataset: configured by naming the graph,
/// validated again when a write changes it, refused when a write breaks it, and read
/// from the graph after a restart.
#[tokio::test]
async fn schema_in_a_graph() {
    let (dir, st, app) = server(|_| {}).await;
    let shexr = sparkles_shex::parse_schema(SCHEMA, None, None)
        .unwrap()
        .to_shexr_turtle();
    let r = send(
        &app,
        req("PUT", "/v/data?graph=urn:x:schema", "text/turtle", &shexr),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let cfg = json!({"language": "shex", "mode": "warn",
        "schema": {"graphs": ["urn:x:schema"], "prefixes": {"ex": "http://ex.org/"}},
        "shapeMap": MAP});
    let r = put_config(&app, &cfg).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = get(&app, "/$/validation/v").await.json();
    assert_eq!(j["config"]["schema"]["graphs"], json!(["urn:x:schema"]));
    assert!(!dir.path().join("v").join(SHEX_SCHEMA_SHEXC_FILE).exists());
    // people may now be 250
    let r = update(
        &app,
        "PREFIX sx: <http://www.w3.org/ns/shex#>
         DELETE { GRAPH <urn:x:schema> { ?x sx:maxinclusive ?m } }
         INSERT { GRAPH <urn:x:schema> { ?x sx:maxinclusive 250 } }
         WHERE { GRAPH <urn:x:schema> { ?x sx:maxinclusive ?m } }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.header("sparkles-validation").contains("lang=shex"),
        "{:?}",
        r.headers
    );
    // a write that leaves no schema is refused
    let r = update(&app, "DROP GRAPH <urn:x:schema>").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(r.text().contains("hold no triples"), "{}", r.text());
    // a restart reads the schema from the graph
    drop(app);
    drop(st);
    let st = state(dir.path(), |_| {});
    let app = router(st.clone());
    let r = update(
        &app,
        "INSERT DATA { ex:dan a ex:Person ; foaf:name \"Dan\" ; foaf:age 240 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = get(&app, "/$/validation/v").await.json();
    assert_eq!(
        j["config"]["schema"]["graphs"],
        json!(["urn:x:schema"]),
        "{j}"
    );
    // carol still has no name; dan conforms under the raised maximum
    assert_eq!(j["status"]["baseline"]["blocking"], 1, "{j}");
}
