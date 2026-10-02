//! Fuseki's admin routes and protocol details (`http/fuseki.rs`, `http/jena_formats.rs`):
//! dataset descriptions, offline datasets, `/$/stats`, `/$/backups-list`, the
//! validators, assembler bodies, Jena's binary syntaxes, `?graph=union`, direct naming
//! and `using-graph-uri`. The Jena client tests (`mise run test:jena-clients`) run Jena's
//! own clients against the same routes.

use super::*;
use crate::http::jena_formats::{JenaFormat, RdfWriter};

async fn call(app: &Router, method: &str, uri: &str, ct: Option<&str>, body: Vec<u8>) -> Resp {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(ct) = ct {
        req = req.header(header::CONTENT_TYPE, ct);
    }
    send(app, req.body(Body::from(body)).unwrap()).await
}

async fn get(app: &Router, uri: &str, accept: &str) -> Resp {
    send(
        app,
        Request::get(uri)
            .header(header::ACCEPT, accept)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn datasets_are_described_the_fuseki_way() {
    let s = server();
    let d = get(&s.app, "/$/datasets/ds", "*/*").await.json();
    assert_eq!(d["ds.name"], "/ds");
    assert_eq!(d["ds.state"], true);
    let services = d["ds.services"].as_array().unwrap();
    let query = services.iter().find(|s| s["srv.type"] == "query").unwrap();
    assert_eq!(
        query["srv.endpoints"],
        serde_json::json!(["", "sparql", "query"])
    );
    let server = get(&s.app, "/$/server", "*/*").await.json();
    assert!(server["uptime"].is_u64() && server["startDateTime"].is_string());
    assert_eq!(server["datasets"][0]["ds.name"], "/ds");
}

#[tokio::test]
async fn an_offline_dataset_refuses_its_services() {
    let s = server();
    let ask = "/ds/sparql?query=ASK%7B%7D";
    let r = call(&s.app, "POST", "/$/datasets/ds?state=offline", None, vec![]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        get(&s.app, ask, "*/*").await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        get(&s.app, "/$/datasets/ds", "*/*").await.json()["ds.state"],
        false
    );
    // the admin routes still answer
    assert_eq!(
        get(&s.app, "/$/stats/ds", "*/*").await.status,
        StatusCode::OK
    );
    let r = call(&s.app, "POST", "/$/datasets/ds?state=active", None, vec![]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(get(&s.app, ask, "*/*").await.status, StatusCode::OK);
    let r = call(&s.app, "POST", "/$/datasets/ds?state=asleep", None, vec![]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(&s.app, "POST", "/$/datasets/ds", None, vec![]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stats_count_requests_per_dataset() {
    let s = server();
    get(&s.app, "/ds/sparql?query=ASK%7B%7D", "*/*").await;
    get(&s.app, "/ds/sparql?query=nonsense", "*/*").await;
    let all = get(&s.app, "/$/stats", "*/*").await.json();
    let ds = &all["datasets"]["/ds"];
    assert_eq!(ds["Requests"], 2, "{all}");
    assert_eq!(ds["RequestsGood"], 1);
    assert_eq!(ds["RequestsBad"], 1);
    assert_eq!(ds["endpoints"]["sparql"]["operation"], "query");
    let one = get(&s.app, "/$/stats/ds", "*/*").await.json();
    assert_eq!(one["datasets"]["/ds"]["Requests"], 2);
    assert!(one["quads"].is_u64(), "Sparkles' own members stay");
}

#[tokio::test]
async fn tasks_carry_fuseki_names_and_backups_are_listed() {
    let s = server();
    // Fuseki's alias of /$/backup/{ds}: no body
    let r = call(&s.app, "POST", "/$/backups/ds", None, vec![]).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let t = r.json();
    assert_eq!(t["taskId"], t["id"]);
    assert_eq!(t["task"], "Backup");
    let id = t["id"].as_str().unwrap().to_string();
    let done = super::tasks::wait_done(&s.state, &id).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
    let t = get(&s.app, &format!("/$/tasks/{id}"), "*/*").await.json();
    assert_eq!(t["success"], true);
    assert!(t["finished"].is_string());
    let list = get(&s.app, "/$/backups-list", "*/*").await.json();
    let files = list["backups"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{list}");
    assert!(files[0].as_str().unwrap().starts_with("ds_"));
}

#[tokio::test]
async fn compaction_takes_delete_old() {
    let s = server();
    let r = call(&s.app, "POST", "/$/compact/ds?deleteOld=true", None, vec![]).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    super::tasks::wait_done(&s.state, r.json()["id"].as_str().unwrap()).await;
    let r = call(
        &s.app,
        "POST",
        "/$/compact/ds?deleteOld=false",
        None,
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn validators_answer_json_and_html() {
    let s = server();
    let j = "application/json";
    let ok = get(
        &s.app,
        "/$/validate/query?query=SELECT%20*%20%7B%3Fs%20%3Fp%20%3Fo%7D",
        j,
    )
    .await;
    let ok = ok.json();
    assert!(
        ok["formatted"].is_string() && ok["algebra"].is_string(),
        "{ok}"
    );
    let bad = get(&s.app, "/$/validate/query?query=SELECT%20*%20%7B", j)
        .await
        .json();
    assert_eq!(bad["errors"][0]["parse-error-line"], 1, "{bad}");
    let r = get(&s.app, "/$/validate/query", j).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let u = get(&s.app, "/$/validate/update?update=CLEAR%20ALL", j)
        .await
        .json();
    assert!(u["formatted"].is_string(), "{u}");
    let iri = get(
        &s.app,
        "/$/validate/iri?iri=http%3A%2F%2Fa%2Fb&iri=rel&iri=a%20b%3Ac",
        j,
    )
    .await
    .json();
    assert_eq!(iri["iris"][0]["errors"], serde_json::json!([]));
    assert_eq!(iri["iris"][1]["warning"].as_array().unwrap().len(), 1);
    assert_eq!(iri["iris"][2]["errors"].as_array().unwrap().len(), 1);
    let form = call(
        &s.app,
        "POST",
        "/$/validate/data",
        Some("application/x-www-form-urlencoded"),
        b"languageSyntax=Turtle&data=%3Ca%3E%20%3Cb%3E%20.".to_vec(),
    )
    .await;
    assert!(
        form.content_type.starts_with("text/html"),
        "{}",
        form.content_type
    );
    assert!(form.text().contains("parse-error"), "{}", form.text());
    let tag = get(&s.app, "/$/validate/langtag?langtag=en-us", j)
        .await
        .json();
    assert_eq!(tag["langtags"][0]["formatted"], "en-US");
}

#[tokio::test]
async fn datasets_from_assemblers() {
    let s = server();
    let cfg = r#"
        @prefix fuseki: <http://jena.apache.org/fuseki#> .
        @prefix ja: <http://jena.hpl.hp.com/2005/11/Assembler#> .
        [] a fuseki:Service ; fuseki:name "fromcfg" ;
           fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "sparql" ] ;
           fuseki:endpoint [ fuseki:operation fuseki:update ] ;
           fuseki:dataset [ a ja:MemoryDataset ] .
    "#;
    let r = call(
        &s.app,
        "POST",
        "/$/datasets",
        Some("text/turtle"),
        cfg.into(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["type"], "mem");
    let bad = cfg
        .replace("ja:MemoryDataset", "ja:InfModel")
        .replace("fromcfg", "x");
    let r = call(
        &s.app,
        "POST",
        "/$/datasets",
        Some("text/turtle"),
        bad.into(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json()["error"].as_str().unwrap().contains("ja:InfModel"));
    // parameters come first, as in Fuseki
    let r = call(
        &s.app,
        "POST",
        "/$/datasets?dbName=params&dbType=mem",
        Some("text/turtle"),
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn graph_store_takes_and_gives_jena_binary_syntaxes() {
    let s = server();
    let quad = oxrdf::Quad::new(
        oxrdf::NamedNode::new("http://example.org/x").unwrap(),
        oxrdf::NamedNode::new("http://example.org/p").unwrap(),
        oxrdf::Literal::new_simple_literal("thrift"),
        oxrdf::GraphName::DefaultGraph,
    );
    for (fmt, ct) in [
        (JenaFormat::Thrift, "application/rdf+thrift"),
        (JenaFormat::Protobuf, "application/rdf+protobuf"),
        (JenaFormat::RdfJson, "application/rdf+json"),
    ] {
        let mut w = RdfWriter::new(fmt, Vec::new());
        w.quad(&quad).unwrap();
        let body = w.finish().unwrap();
        let uri = "/ds/data?graph=http%3A%2F%2Fexample.org%2Fbin";
        let r = call(&s.app, "PUT", uri, Some(ct), body).await;
        assert_eq!(r.status, StatusCode::OK, "{ct}: {}", r.text());
        let nt = get(&s.app, uri, "application/n-triples").await.text();
        assert_eq!(
            nt.trim(),
            "<http://example.org/x> <http://example.org/p> \"thrift\" .",
            "{ct}"
        );
        let back = get(&s.app, uri, ct).await;
        assert_eq!(back.content_type, ct);
        let mut out = Vec::new();
        crate::http::jena_formats::transcode(fmt, &back.body[..], false, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), nt, "{ct}");
    }
    let r = call(
        &s.app,
        "PUT",
        "/ds/data?default",
        Some("application/rdf+thrift"),
        vec![1, 2, 3],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn the_union_graph_is_read_only() {
    let s = server();
    let r = get(&s.app, "/ds/data?graph=union", "application/n-triples").await;
    assert_eq!(r.status, StatusCode::OK);
    // ex:dave's two triples in ex:g1; nothing of the default graph
    assert_eq!(r.text().lines().count(), 2, "{}", r.text());
    let r = call(
        &s.app,
        "PUT",
        "/ds/data?graph=union",
        Some("text/turtle"),
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn direct_naming_needs_the_flag() {
    let s = server();
    let r = get(&s.app, "/ds/graphs/one", "*/*").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.gsp_direct_naming = true;
    let st = Arc::new(st);
    st.attach("ds", DbType::Mem, None).unwrap();
    let app = router(st);
    let put = Request::put("/ds/graphs/one")
        .header(header::HOST, "localhost:5230")
        .header(header::CONTENT_TYPE, "text/turtle")
        .body(Body::from("<urn:s> <urn:p> <urn:o> ."))
        .unwrap();
    assert_eq!(send(&app, put).await.status, StatusCode::OK);
    let q = "/ds/sparql?query=ASK%7BGRAPH%20%3Chttp%3A%2F%2Flocalhost%3A5230%2Fds%2Fgraphs%2Fone%3E%7B%3Fs%20%3Fp%20%3Fo%7D%7D";
    let r = get(&app, q, "application/sparql-results+json").await.json();
    assert_eq!(r["boolean"], true, "{r}");
    let r = get(&app, "/ds/graphs/one?graph=x", "*/*").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        get(&app, "/$/no/such/route", "*/*").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn updates_take_using_graph_uri() {
    let s = server();
    let form = "update=INSERT%20%7B%20GRAPH%20%3Curn%3Aout%3E%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20%7D%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D\
                &using-graph-uri=http%3A%2F%2Fexample.org%2Fg1";
    let r = call(
        &s.app,
        "POST",
        "/ds/update",
        Some("application/x-www-form-urlencoded"),
        form.into(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], 2, "only ex:g1's two triples");
}

#[tokio::test]
async fn ask_in_csv_and_tsv_is_jenas_form() {
    let s = server();
    let csv = get(&s.app, "/ds/sparql?query=ASK%7B%7D", "text/csv")
        .await
        .text();
    assert_eq!(csv, "_askResult\r\ntrue\r\n");
    let tsv = get(
        &s.app,
        "/ds/sparql?query=ASK%7B%7D",
        "text/tab-separated-values",
    )
    .await
    .text();
    assert_eq!(tsv, "?_askResult\ntrue\n");
}
