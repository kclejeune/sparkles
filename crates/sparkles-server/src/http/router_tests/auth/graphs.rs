//! Graph-level grants and endpoint permissions (C12): a matrix of principals, graphs
//! and endpoints over a dataset with a default graph and three named graphs.

use super::*;
use crate::auth::config::FileConfig;

/// The dataset `graphs`: the default graph, `http://ex/a/1`, `http://ex/a/2` and
/// `http://ex/b/1`, each with a `fox`-labelled subject.
pub(crate) const DATA: &str = r#"@prefix ex: <http://ex/> .
ex:d ex:p "default fox" .
<http://ex/a/1> { ex:a1 ex:p "alpha fox" . ex:a1 ex:q ex:d . }
<http://ex/a/2> { ex:a2 ex:p "alpha two" . }
<http://ex/b/1> { ex:b1 ex:p "secret fox" . ex:b1 ex:q ex:a1 . ex:a1 ex:q ex:b1 . }
"#;

/// Users of the matrix, each with the password `<name>-pw`:
/// * `gfull` writes the whole dataset;
/// * `gra` reads `http://ex/a/*`;
/// * `grad` reads the default graph and `http://ex/a/*`, and writes `http://ex/a/1`;
/// * `gep` reads every graph, through the Graph Store reads only;
/// * `gmix` reads every graph through `query` and only `http://ex/a/1` through `gsp-r`.
pub(crate) fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "gfull"
password = "{gfull}"
datasets = {{ graphs = "write" }}

[[users]]
name = "gra"
password = "{gra}"
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["http://ex/a/*"]

[[users]]
name = "grad"
password = "{grad}"
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://ex/a/*"]
[[users.grants]]
dataset = "graphs"
level = "write"
graphs = ["http://ex/a/1"]

[[users]]
name = "gep"
password = "{gep}"
[[users.grants]]
dataset = "graphs"
level = "read"
endpoints = ["gsp-r"]

[[users]]
name = "gmix"
password = "{gmix}"
[[users.grants]]
dataset = "graphs"
level = "read"
endpoints = ["query"]
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["http://ex/a/1"]
endpoints = ["gsp-r", "info"]
"#,
        gfull = h("gfull-pw"),
        gra = h("gra-pw"),
        grad = h("grad-pw"),
        gep = h("gep-pw"),
        gmix = h("gmix-pw"),
    )
}

fn server() -> AuthServer {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    // persistent, so that diffs can read past commits
    let ds = s.state.attach("graphs", DbType::Persistent, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    #[cfg(feature = "text")]
    ds.store
        .enable_text(sparkles::text::TextConfig::default())
        .unwrap();
    s
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The values of the first column of a SELECT, sorted (`-` for unbound).
async fn column(app: &Router, user: &str, q: &str) -> Result<Vec<String>, (StatusCode, String)> {
    let r = call(
        app,
        "GET",
        &format!("/graphs/sparql?query={}", enc(q)),
        &[("authorization", &b(user)), ("accept", "text/csv")],
        "",
    )
    .await;
    if r.status != StatusCode::OK {
        return Err((r.status, r.text()));
    }
    let mut v: Vec<String> = r.text().lines().skip(1).map(|l| l.to_string()).collect();
    v.sort();
    Ok(v)
}

const GRAPHS_Q: &str = "SELECT ?g { GRAPH ?g { } }";

#[tokio::test]
async fn queries_see_the_graphs_of_the_grants() {
    let s = server();
    let all = ["http://ex/a/1", "http://ex/a/2", "http://ex/b/1"];
    assert_eq!(column(&s.app, "gfull", GRAPHS_Q).await.unwrap(), all);
    assert_eq!(
        column(&s.app, "gra", GRAPHS_Q).await.unwrap(),
        ["http://ex/a/1", "http://ex/a/2"]
    );
    assert_eq!(
        column(&s.app, "grad", GRAPHS_Q).await.unwrap(),
        ["http://ex/a/1", "http://ex/a/2"]
    );
    // the query endpoint is not among gep's
    let (st, body) = column(&s.app, "gep", GRAPHS_Q).await.unwrap_err();
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(
        body.contains("the query endpoint of /graphs is not allowed"),
        "{body}"
    );
    assert_eq!(column(&s.app, "gmix", GRAPHS_Q).await.unwrap(), all);
    // the default graph, counts over every graph, paths and FROM of a hidden graph
    let cases: [(&str, [&[&str]; 3]); 6] = [
        (
            "SELECT ?s { ?s ?p ?o }",
            [&["http://ex/d"], &[], &["http://ex/d"]],
        ),
        (
            "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
            [&["6"], &["3"], &["3"]],
        ),
        (
            "SELECT (COUNT(*) AS ?n) { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } }",
            [&["6"], &["3"], &["3"]],
        ),
        (
            "SELECT ?o { GRAPH ?g { <http://ex/b1> <http://ex/q>+ ?o } }",
            [&["http://ex/a1", "http://ex/b1"], &[], &[]],
        ),
        (
            "SELECT ?s FROM <http://ex/b/1> { ?s ?p ?o }",
            [&["http://ex/a1", "http://ex/b1", "http://ex/b1"], &[], &[]],
        ),
        (
            "SELECT ?x { GRAPH <http://ex/b/1> { } BIND(1 AS ?x) }",
            [&["1"], &[], &[]],
        ),
    ];
    for (q, want) in cases {
        for (user, want) in ["gfull", "gra", "grad"].into_iter().zip(want) {
            let got = column(&s.app, user, q).await.unwrap();
            let mut want: Vec<String> = want.iter().map(|w| w.to_string()).collect();
            want.sort();
            assert_eq!(got, want, "{user}: {q}");
        }
    }
}

#[tokio::test]
async fn the_result_cache_keeps_views_apart() {
    let s = server();
    let q = "SELECT ?s { GRAPH ?g { ?s <http://ex/p> ?o } }";
    let full = column(&s.app, "gfull", q).await.unwrap();
    assert_eq!(full.len(), 3);
    assert_eq!(
        column(&s.app, "gra", q).await.unwrap(),
        ["http://ex/a1", "http://ex/a2"]
    );
    assert_eq!(column(&s.app, "gfull", q).await.unwrap(), full);
}

#[tokio::test]
async fn graph_store_reads_hide_graphs_like_missing_ones() {
    let s = server();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "GET",
                u,
                &[
                    ("authorization", &b(user)),
                    ("accept", "application/n-quads"),
                ],
                "",
            )
            .await
        }
    };
    // a hidden graph and a missing one: identical 404s
    let hidden = get("/graphs/get?graph=http%3A%2F%2Fex%2Fb%2F1", "gra").await;
    let missing = get("/graphs/get?graph=http%3A%2F%2Fex%2Fb%2F9", "gra").await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(
        hidden.err()["error"]
            .as_str()
            .unwrap()
            .replace("b/1", "b/9"),
        missing.err()["error"]
    );
    let r = get("/graphs/get?default", "gra").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get("/graphs/get?graph=http%3A%2F%2Fex%2Fa%2F1", "gra").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("alpha fox"));
    // the whole dataset: the visible graphs' quads only
    let r = get("/graphs/get", "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let text = r.text();
    assert!(
        text.contains("alpha fox") && text.contains("alpha two"),
        "{text}"
    );
    assert!(
        !text.contains("secret") && !text.contains("default fox"),
        "{text}"
    );
    let full = get("/graphs/get", "gfull").await.text();
    assert!(full.contains("secret") && full.contains("default fox"));
    // gep may read every graph through the Graph Store
    let r = get("/graphs/data", "gep").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("secret"));
    // gmix sees one graph there
    let r = get("/graphs/get", "gmix").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("alpha fox") && !r.text().contains("alpha two"));
}

#[tokio::test]
async fn updates_write_only_the_write_graphs() {
    let s = server();
    let up = |u: &str| format!("PREFIX ex: <http://ex/> {u}");
    let r = update_as(
        &s.app,
        "graphs",
        &b("grad"),
        &up("INSERT DATA { GRAPH <http://ex/a/1> { ex:n ex:p 1 } }"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let start = head(&s.state, "graphs");
    for u in [
        "INSERT DATA { GRAPH <http://ex/a/2> { ex:n ex:p 1 } }",
        "INSERT DATA { ex:n ex:p 1 }",
        "INSERT { GRAPH ?g { ex:n ex:p 1 } } WHERE { BIND(<http://ex/b/1> AS ?g) }",
        "DELETE WHERE { GRAPH ?g { ?s ?p ?o } }",
        "CLEAR ALL",
        "DROP GRAPH <http://ex/b/1>",
        "COPY <http://ex/a/1> TO <http://ex/a/2>",
    ] {
        let r = update_as(&s.app, "graphs", &b("grad"), &up(u)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{u}: {}", r.text());
    }
    // a quad that exists and one that does not: the same refusal
    let a = update_as(
        &s.app,
        "graphs",
        &b("grad"),
        &up("DELETE DATA { GRAPH <http://ex/b/1> { ex:b1 ex:p \"secret fox\" } }"),
    )
    .await;
    let z = update_as(
        &s.app,
        "graphs",
        &b("grad"),
        &up("DELETE DATA { GRAPH <http://ex/b/1> { ex:zz ex:p \"none\" } }"),
    )
    .await;
    assert_eq!(a.status, StatusCode::FORBIDDEN);
    assert_eq!(a.err(), z.err());
    assert_eq!(head(&s.state, "graphs"), start);
    // the receipt leaves out the counts of the whole dataset
    let r = call(
        &s.app,
        "POST",
        "/graphs/update?receipt=true",
        &[
            ("authorization", &b("grad")),
            ("content-type", "application/sparql-update"),
        ],
        &up("INSERT DATA { GRAPH <http://ex/a/1> { ex:n2 ex:p 2 } }"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j["commit"]["seq"].is_u64(), "{j}");
    assert!(j["commit"].get("quads").is_none(), "{j}");
    // read-only principals cannot update at all
    let r = update_as(
        &s.app,
        "graphs",
        &b("gra"),
        &up("INSERT DATA { ex:n ex:p 1 }"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.err()["error"], "write access to /graphs required");
}

#[tokio::test]
async fn graph_store_writes_check_the_target_first() {
    let s = server();
    let send = |method: &'static str, uri: &'static str, ct: &'static str, body: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                method,
                uri,
                &[("authorization", &b("grad")), ("content-type", ct)],
                body,
            )
            .await
        }
    };
    let ttl = "<http://ex/n> <http://ex/p> 1 .";
    let r = send(
        "PUT",
        "/graphs/data?graph=http%3A%2F%2Fex%2Fa%2F1",
        "text/turtle",
        ttl,
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let start = head(&s.state, "graphs");
    // hidden or read-only targets, and the whole dataset
    for (m, u) in [
        ("PUT", "/graphs/data?graph=http%3A%2F%2Fex%2Fb%2F1"),
        ("POST", "/graphs/data?graph=http%3A%2F%2Fex%2Fa%2F2"),
        ("PUT", "/graphs/data?default"),
        ("PUT", "/graphs/data"),
        ("DELETE", "/graphs/data"),
    ] {
        let r = send(m, u, "text/turtle", ttl).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {u}: {}", r.text());
    }
    // deleting a hidden graph and a missing one: identical refusals
    let a = send(
        "DELETE",
        "/graphs/data?graph=http%3A%2F%2Fex%2Fb%2F1",
        "text/turtle",
        "",
    )
    .await;
    let z = send(
        "DELETE",
        "/graphs/data?graph=http%3A%2F%2Fex%2Fb%2F9",
        "text/turtle",
        "",
    )
    .await;
    assert_eq!(a.status, StatusCode::FORBIDDEN);
    assert_eq!(
        a.err()["error"].as_str().unwrap().replace("b/1", "b/9"),
        z.err()["error"]
    );
    // quads posted to the dataset: each one is checked
    let nq = "<http://ex/n> <http://ex/p> \"3\" <http://ex/a/1> .\n<http://ex/n> <http://ex/p> \"4\" <http://ex/b/1> .\n";
    let r = send("POST", "/graphs/data", "application/n-quads", nq).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(head(&s.state, "graphs"), start);
    let ok = "<http://ex/n> <http://ex/p> \"3\" <http://ex/a/1> .\n";
    let r = send("POST", "/graphs/data", "application/n-quads", ok).await;
    assert!(r.status.is_success(), "{}", r.text());
}

#[tokio::test]
async fn routes_that_cover_every_graph_refuse_a_limited_view() {
    let s = server();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", u, &[("authorization", &b(user))], "").await }
    };
    for u in [
        "/$/stats/graphs",
        "/$/text/graphs",
        "/$/reason/graphs",
        "/$/history/graphs",
    ] {
        let r = get(u, "gra").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{u}: {}", r.text());
        assert!(
            r.err()["error"]
                .as_str()
                .unwrap()
                .contains("your access is limited to some graphs")
        );
        assert_ne!(get(u, "gfull").await.status, StatusCode::FORBIDDEN, "{u}");
    }
    let r = call(
        &s.app,
        "POST",
        "/graphs/shacl",
        &[
            ("authorization", &b("gra")),
            ("content-type", "text/turtle"),
        ],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // the dataset's description leaves out its size
    let r = get("/$/datasets/graphs", "gra").await.json();
    assert_eq!(r["quads"], 3, "{r}");
    assert_eq!(r["graphs"], "limited");
    let r = get("/$/datasets/graphs", "gfull").await.json();
    assert!(r["quads"].is_u64(), "{r}");
    // commits without counts
    let r = get("/$/commits/graphs", "gra").await.json();
    assert!(r["commits"][0]["seq"].is_u64(), "{r}");
    assert!(r["commits"][0].get("quads").is_none(), "{r}");
    assert!(r["commits"][0].get("inserted").is_none(), "{r}");
    let r = get("/$/commits/graphs", "gfull").await.json();
    assert!(r["commits"][0]["quads"].is_u64(), "{r}");
}

#[tokio::test]
async fn schema_explain_text_and_diff_cover_the_view() {
    let s = server();
    let get = |u: String, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", &u, &[("authorization", &b(user))], "").await }
    };
    // schema: the triples of the visible graphs
    let r = get("/$/schema/graphs?graph=union".into(), "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["totals"]["triples"], 3);
    let r = get("/$/schema/graphs?graph=union".into(), "gfull").await;
    assert_eq!(r.json()["totals"]["triples"], 7);
    // a hidden graph is missing
    let r = get(
        "/$/schema/graphs?graph=http%3A%2F%2Fex%2Fb%2F1".into(),
        "gra",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // explain: no estimates from statistics of every graph
    let r = get(
        format!(
            "/graphs/explain?query={}",
            enc("SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }")
        ),
        "gra",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["plan"]["estimatedRows"], -1.0, "{j}");
    // full-text search: hits of the visible graphs
    #[cfg(feature = "text")]
    {
        let hits = |r: R| {
            let mut v: Vec<String> = r.json()["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["s"]["value"].as_str().unwrap().to_string())
                .collect();
            v.sort();
            v
        };
        // the default graph, then one named graph
        assert_eq!(
            hits(get("/graphs/text?q=fox".into(), "gfull").await),
            ["http://ex/d"]
        );
        assert!(hits(get("/graphs/text?q=fox".into(), "gra").await).is_empty());
        assert_eq!(
            hits(get("/graphs/text?q=fox".into(), "grad").await),
            ["http://ex/d"]
        );
        let b1 = "/graphs/text?q=fox&graph=http%3A%2F%2Fex%2Fb%2F1";
        assert_eq!(hits(get(b1.into(), "gfull").await), ["http://ex/b1"]);
        assert!(hits(get(b1.into(), "gra").await).is_empty());
        // text:query in every graph
        let q = "SELECT ?s { GRAPH ?g { ?s <http://jena.apache.org/text#query> \"fox\" } }";
        assert_eq!(
            column(&s.app, "gfull", q).await.unwrap(),
            ["http://ex/a1", "http://ex/b1"]
        );
        assert_eq!(column(&s.app, "gra", q).await.unwrap(), ["http://ex/a1"]);
    }
    // diff: the changes of the visible graphs
    let up = "INSERT DATA { GRAPH <http://ex/b/1> { <http://ex/x> <http://ex/p> 1 } \
              GRAPH <http://ex/a/2> { <http://ex/x> <http://ex/p> 2 } }";
    assert_eq!(
        update_as(&s.app, "graphs", &b("gfull"), up).await.status,
        StatusCode::OK
    );
    let d = get("/graphs/diff?quads=true".into(), "gra").await.json();
    assert_eq!(d["added"], 1, "{d}");
    assert!(d.get("logChanges").is_none(), "{d}");
    assert!(d["to"]["commit"].get("quads").is_none(), "{d}");
    let d = get("/graphs/diff?quads=true".into(), "gfull").await.json();
    assert_eq!(d["added"], 2, "{d}");
    // the same diff as RDF Patch
    let patch = get("/graphs/diff?format=patch".into(), "gra").await;
    assert_eq!(patch.status, StatusCode::OK, "{}", patch.text());
    let text = patch.text();
    assert!(
        text.contains("<http://ex/a/2>") && !text.contains("<http://ex/b/1>"),
        "{text}"
    );
    assert!(
        get("/graphs/diff?format=patch".into(), "gfull")
            .await
            .text()
            .contains("<http://ex/b/1>")
    );
    // the change feed after the load: the visible changes, without the commits' counts
    let feed = get("/graphs/changes?after=1".into(), "gra").await;
    assert_eq!(feed.status, StatusCode::OK, "{}", feed.text());
    let f = feed.json();
    let c = &f["commits"][0];
    assert_eq!(c["added"], 1, "{f}");
    assert!(c["commit"].get("quads").is_none(), "{f}");
    let changes = c["changes"].to_string();
    assert!(
        changes.contains("ex/a/2") && !changes.contains("ex/b/1"),
        "{f}"
    );
    let f = get("/graphs/changes?after=1".into(), "gfull").await.json();
    assert_eq!(f["commits"][0]["added"], 2, "{f}");
    assert!(f["commits"][0]["commit"]["quads"].is_u64(), "{f}");
    let patch = get("/graphs/changes?after=1&format=patch".into(), "gra")
        .await
        .text();
    assert!(
        patch.contains("<http://ex/a/2>") && !patch.contains("<http://ex/b/1>"),
        "{patch}"
    );
    // a commit of hidden graphs only is listed, with no changes
    let up = "INSERT DATA { GRAPH <http://ex/b/1> { <http://ex/y> <http://ex/p> 3 } }";
    assert_eq!(
        update_as(&s.app, "graphs", &b("gfull"), up).await.status,
        StatusCode::OK
    );
    let f = get("/graphs/changes?after=2".into(), "gra").await.json();
    assert_eq!(f["commits"][0]["added"], 0, "{f}");
    assert_eq!(f["commits"][0]["changes"], serde_json::json!([]), "{f}");
}

#[tokio::test]
async fn drafted_shapes_cover_the_view() {
    let s = server();
    let ds = s.state.datasets.read().get("graphs").cloned().unwrap();
    ds.store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://ex/> .
<http://ex/a/1> { ex:a1 a ex:T . }
<http://ex/b/1> { ex:b1 a ex:T ; ex:hidden "secret" . }"#
                .to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", u, &[("authorization", &b(user))], "").await }
    };
    let r = get("/$/schema/graphs/shapes?graph=union", "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["shapes"][0]["instances"], 1, "{j}");
    assert!(!j.to_string().contains("hidden"), "{j}");
    let j = get("/$/schema/graphs/shapes?graph=union", "gfull")
        .await
        .json();
    assert_eq!(j["shapes"][0]["instances"], 2, "{j}");
    assert!(j.to_string().contains("hidden"), "{j}");
    let r = get(
        "/$/schema/graphs/shapes?graph=http%3A%2F%2Fex%2Fb%2F1",
        "gra",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // a grant limited to the Graph Store reads does not reach the info endpoint
    let r = get("/$/schema/graphs/shapes", "gep").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn constraints_cover_the_view() {
    let s = server();
    let ds = s.state.datasets.read().get("graphs").cloned().unwrap();
    ds.store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://ex/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
<http://ex/a/shapes> { ex:S a sh:NodeShape ; sh:targetClass ex:T ;
    sh:property [ sh:path ex:p ; sh:maxCount 1 ] . }
<http://ex/b/shapes> { ex:H a sh:NodeShape ; sh:targetClass ex:Hidden ;
    sh:property [ sh:path ex:p ; sh:minCount 1 ] . }"#
                .to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", u, &[("authorization", &b(user))], "").await }
    };
    let a = "/$/schema/graphs/constraints?shapes=http%3A%2F%2Fex%2Fa%2Fshapes";
    let hidden = "/$/schema/graphs/constraints?shapes=http%3A%2F%2Fex%2Fb%2Fshapes";
    let r = get(a, "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["constraints"]["sources"][0]["classes"][0]["class"],
        "http://ex/T"
    );
    assert_eq!(get(hidden, "gra").await.status, StatusCode::NOT_FOUND);
    assert_eq!(get(hidden, "gfull").await.status, StatusCode::OK);

    // the guard's shapes reach only callers that may read its shapes graphs
    let cfg: sparkles_shacl::guard::ValidationConfig = serde_json::from_value(serde_json::json!({
        "mode": "warn", "shapes": { "graphs": ["http://ex/b/shapes"] }
    }))
    .unwrap();
    match sparkles_shacl::guard::set_config(&ds.store, Some(cfg)).unwrap() {
        sparkles_shacl::guard::SetOutcome::Installed(g, _) => {
            *ds.validation.write() = Some(crate::state::Validation::Shacl(g));
        }
        _ => panic!("guard not installed"),
    }
    let j = get("/$/schema/graphs/constraints", "gfull").await.json();
    assert_eq!(j["constraints"]["sources"][0]["kind"], "guard", "{j}");
    let r = get("/$/schema/graphs/constraints", "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    let r = get("/$/schema/graphs/constraints?shapes=guard", "gra").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stored_queries_run_on_the_view() {
    let s = server();
    let ds = s.state.datasets.read().get("graphs").cloned().unwrap();
    let def: sparkles::stored::Definition = serde_json::from_value(serde_json::json!({
        "query": "SELECT ?s WHERE { GRAPH ?g { ?s <http://ex/p> ?o FILTER(CONTAINS(?o, ?word)) } } ORDER BY ?s",
        "parameters": { "word": { "type": "string", "default": "fox" } }
    }))
    .unwrap();
    ds.queries
        .put("foxes", def.clone(), sparkles::stored::Change::default())
        .unwrap();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "GET",
                u,
                &[("authorization", &b(user)), ("accept", "text/csv")],
                "",
            )
            .await
        }
    };
    let rows = |r: &R| -> Vec<String> { r.text().lines().skip(1).map(str::to_string).collect() };
    let r = get("/graphs/queries/foxes", "gfull").await;
    assert_eq!(rows(&r), ["http://ex/a1", "http://ex/b1"], "{}", r.text());
    // a reader of http://ex/a/* sees the foxes of those graphs only
    let r = get("/graphs/queries/foxes", "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(rows(&r), ["http://ex/a1"]);
    // the definitions are dataset settings that a limited reader may list
    let r = get("/$/queries/graphs", "gra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a grant limited to the Graph Store reads reaches no query
    let r = get("/graphs/queries/foxes", "gep").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // changing a definition needs admin
    let r = call(
        &s.app,
        "PUT",
        "/$/queries/graphs/foxes",
        &[
            ("authorization", &b("gfull")),
            ("content-type", "application/json"),
        ],
        &serde_json::to_string(&def).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[tokio::test]
async fn endpoint_permissions() {
    let s = server();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", u, &[("authorization", &b(user))], "").await }
    };
    assert_eq!(
        get("/graphs/get?default", "gep").await.status,
        StatusCode::OK
    );
    for u in [
        "/graphs/sparql?query=ASK%7B%7D",
        "/$/schema/graphs",
        "/graphs/diff",
        "/graphs/changes",
    ] {
        let r = get(u, "gep").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{u}");
        assert!(
            r.err()["error"]
                .as_str()
                .unwrap()
                .contains("endpoint of /graphs is not allowed")
        );
    }
    // other datasets stay hidden
    assert_eq!(
        get("/secret/sparql?query=ASK%7B%7D", "gep").await.status,
        StatusCode::NOT_FOUND
    );
    // the dataset is listed
    let r = get("/$/datasets", "gep").await.json();
    assert!(names(&r).contains(&"graphs".to_string()));
    // whoami names the limits, never the patterns
    let who = get("/$/whoami", "gep").await.json();
    assert_eq!(
        who["restricted"]["graphs"]["endpoints"],
        serde_json::json!(["gsp-r"])
    );
    assert_eq!(who["restricted"]["graphs"]["graphs"], false);
    let who = get("/$/whoami", "gra").await.json();
    assert_eq!(who["restricted"]["graphs"]["graphs"], true);
    assert!(!who.to_string().contains("ex/a"), "{who}");
    let who = get("/$/whoami", "gfull").await.json();
    assert!(who["restricted"].get("graphs").is_none(), "{who}");
}

#[test]
fn configurations_are_validated() {
    let base = r#"version = 1
[[users]]
name = "u"
password = "$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo"
[[users.grants]]
dataset = "ds"
"#;
    let parse = |grant: &str| FileConfig::parse(&format!("{base}{grant}"));
    assert!(parse(r#"level = "read""#).is_ok());
    assert!(parse("level = \"write\"\ngraphs = [\"http://ex/*\", \"default\"]").is_ok());
    for (bad, msg) in [
        (
            "level = \"admin\"\ngraphs = [\"http://ex/g\"]",
            "cannot be admin",
        ),
        ("level = \"read\"\ngraphs = []", "empty graphs list"),
        (
            "level = \"read\"\ngraphs = [\"urn:x-arq:UnionGraph\"]",
            "not a graph",
        ),
        ("level = \"read\"\ngraphs = [\"no iri\"]", "invalid graph"),
        (
            "level = \"read\"\nendpoints = [\"sparql\"]",
            "unknown endpoint",
        ),
        ("level = \"read\"\nendpoints = []", "empty endpoints list"),
        ("level = \"read\"\ncolour = \"red\"", "unknown field"),
    ] {
        let e = format!("{:#}", parse(bad).unwrap_err());
        assert!(e.contains(msg), "{bad}: {e}");
    }
    let cfg = parse("level = \"read\"\ngraphs = [\"*\"]").unwrap();
    assert!(
        cfg.warnings()
            .iter()
            .any(|w| w.contains("urn:x-sparkles:inferred")),
        "{:?}",
        cfg.warnings()
    );
}

/// A dry run needs the permission of the write it previews (C15 §4.6), and a limited
/// caller's preview leaves out what covers every graph.
#[tokio::test]
async fn dry_runs_need_the_write_permission() {
    let s = server();
    let up = |u: &str| format!("PREFIX ex: <http://ex/> {u}");
    let start = head(&s.state, "graphs");
    let dry = |user: &'static str, body: String| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "POST",
                "/graphs/update?dryRun=true&changes=5",
                &[
                    ("authorization", &b(user)),
                    ("content-type", "application/sparql-update"),
                ],
                &body,
            )
            .await
        }
    };
    // a reader may not preview a write
    let r = dry(
        "gra",
        up("INSERT DATA { GRAPH <http://ex/a/1> { ex:n ex:p 1 } }"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // nor may a writer preview a write to a graph it cannot write
    let r = dry(
        "grad",
        up("INSERT DATA { GRAPH <http://ex/b/1> { ex:n ex:p 1 } }"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        r.err()["error"],
        "write access to graph <http://ex/b/1> required"
    );
    // a preview of its own graph has the counts of that graph, not the dataset's
    let r = dry(
        "grad",
        up("INSERT DATA { GRAPH <http://ex/a/1> { ex:n ex:p 1 } }"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["dryRun"], true);
    assert!(j["commit"].get("quads").is_none(), "{j}");
    assert!(j["commit"].get("inserted").is_none(), "{j}");
    assert_eq!(
        j["graphs"],
        serde_json::json!([{ "graph": "http://ex/a/1", "inserted": 1, "deleted": 0 }])
    );
    assert_eq!(j["changes"]["total"], 1);
    assert!(j["storage"].get("used").is_none(), "{j}");
    // a whole-dataset clear by a limited caller touches the visible graphs only
    let r = dry("grad", up("CLEAR GRAPH <http://ex/a/1>")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["graphs"][0]["deleted"], 2);
    let r = call(
        &s.app,
        "PUT",
        "/graphs/data?graph=http%3A%2F%2Fex%2Fb%2F1&dryRun",
        &[
            ("authorization", &b("grad")),
            ("content-type", "text/turtle"),
        ],
        "<http://ex/n> <http://ex/p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(head(&s.state, "graphs"), start);
}

/// The service description of `/graphs/sparql` for `user`, as N-Triples.
async fn service_description(app: &Router, user: &str) -> (StatusCode, String) {
    let r = call(
        app,
        "GET",
        "/graphs/sparql",
        &[
            ("authorization", &b(user)),
            ("accept", "application/n-triples"),
        ],
        "",
    )
    .await;
    (r.status, r.text())
}

#[tokio::test]
async fn the_service_description_follows_the_grants() {
    let s = server();
    let name = |g: &str| format!("<http://www.w3.org/ns/sparql-service-description#name> <{g}>");
    let triples = "<http://rdfs.org/ns/void#triples>";
    // every graph, with counts
    let (st, full) = service_description(&s.app, "gfull").await;
    assert_eq!(st, StatusCode::OK, "{full}");
    for g in ["http://ex/a/1", "http://ex/a/2", "http://ex/b/1"] {
        assert!(full.contains(&name(g)), "{full}");
    }
    assert!(full.contains(&format!("{triples} \"3\"")), "{full}");
    // only the readable graphs, and no counts
    let (st, part) = service_description(&s.app, "gra").await;
    assert_eq!(st, StatusCode::OK, "{part}");
    assert!(part.contains(&name("http://ex/a/1")) && part.contains(&name("http://ex/a/2")));
    assert!(
        !part.contains("http://ex/b/1") && !part.contains(triples),
        "{part}"
    );
    // a caller that may not query the dataset gets no description
    let (st, _) = service_description(&s.app, "gep").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let r = call(
        &s.app,
        "GET",
        "/graphs/sparql",
        &[("accept", "text/turtle")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}
