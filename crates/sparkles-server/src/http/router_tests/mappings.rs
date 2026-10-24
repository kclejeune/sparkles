//! Mappings over HTTP (spec F19 §2.2 and §2.3): stored mappings, their runs in add and
//! replace mode, R2RML, RML and YARRRML mappings in uploads, CSVW standard mode and key
//! checks, template lookups, CSV bodies in the Graph Store protocol, and the
//! `connections` settings kind.

use super::{select_rows, send, server};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};

const PEOPLE: &str = "id,name,city\n1,Ann,b\n2,Bo,p\n";
const CITIES: &str = "id,label\nb,Berlin\np,Paris\n";

const YARRRML: &str = r#"prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    graphs: ex:g
    po:
      - [ex:name, $(name)]
      - p: ex:city
        o:
          mapping: city
          condition:
            function: equal
            parameters:
              - [str1, $(city)]
              - [str2, $(id)]
  city:
    sources:
      - [cities.csv~csv]
    s: ex:city/$(id)
    graphs: ex:g
    po:
      - [ex:label, $(label)]
"#;

/// A mapping of one CSV file into the default graph.
const ONE_TABLE: &str = r#"prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    po:
      - [ex:name, $(name)]
"#;

fn req(method: &str, uri: &str, ct: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(ct) = ct {
        b = b.header(header::CONTENT_TYPE, ct);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// A multipart body: `(field name, file name, content)`.
fn multipart(uri: &str, parts: &[(&str, Option<&str>, &str)]) -> Request<Body> {
    let b = "sparkles-test-boundary";
    let mut body = String::new();
    for (name, file, content) in parts {
        body.push_str(&format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"{name}\""
        ));
        if let Some(f) = file {
            body.push_str(&format!("; filename=\"{f}\""));
        }
        body.push_str(&format!("\r\n\r\n{content}\r\n"));
    }
    body.push_str(&format!("--{b}--\r\n"));
    Request::post(uri)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={b}"),
        )
        .body(Body::from(body))
        .unwrap()
}

async fn count_graph(app: &axum::Router, g: &str) -> usize {
    select_rows(app, &format!("SELECT * {{ GRAPH <{g}> {{ ?s ?p ?o }} }}")).await
}

#[tokio::test]
async fn stored_mappings_are_versioned_and_run() {
    let s = server();
    // the kind comes from the media type
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/people?description=People",
            Some("text/plain"),
            YARRRML,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{}", r.text());
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/people",
            Some("application/yaml"),
            "mappings: [nope",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/people?description=People",
            Some("application/yaml"),
            YARRRML,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert_eq!(j["kind"], "yarrrml");
    assert_eq!(
        j["summary"]["files"],
        serde_json::json!(["people.csv", "cities.csv"])
    );
    assert_eq!(
        j["summary"]["graphs"],
        serde_json::json!(["http://ex.org/g"])
    );
    // a stale If-Match is refused
    let mut put = req(
        "PUT",
        "/$/mappings/ds/people",
        Some("application/yaml"),
        ONE_TABLE,
    );
    put.headers_mut()
        .insert(header::IF_MATCH, "\"v7\"".parse().unwrap());
    assert_eq!(
        send(&s.app, put).await.status,
        StatusCode::PRECONDITION_FAILED
    );

    let r = send(&s.app, req("GET", "/$/mappings/ds", None, "")).await;
    let j = r.json();
    assert_eq!(j["mappings"][0]["name"], "people");
    assert!(j["mappings"][0].get("text").is_none());
    let r = send(&s.app, req("GET", "/$/mappings/ds/people", None, "")).await;
    assert_eq!(r.json()["text"], YARRRML);
    let mut get = req("GET", "/$/mappings/ds/people", None, "");
    get.headers_mut()
        .insert(header::ACCEPT, "application/yaml".parse().unwrap());
    assert_eq!(send(&s.app, get).await.text(), YARRRML);

    // a run without its files names the one it misses
    let r = send(&s.app, req("POST", "/ds/mappings/people/run", None, "")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["code"], "mapping-source-missing");
    // a dry run commits nothing
    let parts = [
        ("file", Some("people.csv"), PEOPLE),
        ("file", Some("cities.csv"), CITIES),
    ];
    let r = send(
        &s.app,
        multipart("/ds/mappings/people/run?dryRun=true", &parts),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(count_graph(&s.app, "http://ex.org/g").await, 0);
    let r = send(&s.app, multipart("/ds/mappings/people/run", &parts)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["quads"], 6);
    assert_eq!(j["version"], 1);
    assert_eq!(count_graph(&s.app, "http://ex.org/g").await, 6);
    let r = send(&s.app, req("GET", "/$/mappings/ds/people", None, "")).await;
    assert_eq!(r.json()["lastRun"]["quads"], 6);
    assert_eq!(r.json()["lastRun"]["mode"], "add");

    // replace mode removes what the files no longer hold, and leaves other graphs
    let before = select_rows(&s.app, "SELECT * { ?s ?p ?o }").await;
    let fewer = [
        ("file", Some("people.csv"), "id,name,city\n1,Ann,b\n"),
        ("file", Some("cities.csv"), CITIES),
    ];
    let r = send(
        &s.app,
        multipart("/ds/mappings/people/run?mode=replace", &fewer),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(count_graph(&s.app, "http://ex.org/g").await, 4);
    assert_eq!(select_rows(&s.app, "SELECT * { ?s ?p ?o }").await, before);
    // a file the mapping does not read is refused
    let extra = [
        ("file", Some("people.csv"), PEOPLE),
        ("file", Some("cities.csv"), CITIES),
        ("file", Some("other.ttl"), "<http://e/s> <http://e/p> 1 ."),
    ];
    let r = send(&s.app, multipart("/ds/mappings/people/run", &extra)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("other.ttl"), "{}", r.text());

    // a template graph map cannot be replaced
    let templated = ONE_TABLE.replace(
        "    s: ex:person/$(id)\n",
        "    s: ex:person/$(id)\n    graphs: ex:g/$(id)\n",
    );
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/tg",
            Some("application/yaml"),
            &templated,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = send(
        &s.app,
        multipart(
            "/ds/mappings/tg/run?mode=replace",
            &[("file", Some("people.csv"), PEOPLE)],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    let r = send(
        &s.app,
        req("GET", "/$/mappings/ds/people/versions", None, ""),
    )
    .await;
    assert_eq!(r.json()["versions"].as_array().unwrap().len(), 1);
    let r = send(&s.app, req("DELETE", "/$/mappings/ds/people", None, "")).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(&s.app, req("GET", "/$/mappings/ds/people", None, "")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uploads_take_rml_and_yarrrml_mappings() {
    let s = server();
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                ("mapping", None, YARRRML),
                ("file", Some("people.csv"), PEOPLE),
                ("file", Some("cities.csv"), CITIES),
                ("file", Some("more.ttl"), "<http://e/s> <http://e/p> 1 ."),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["tables"][0]["quads"], 6);
    assert_eq!(count_graph(&s.app, "http://ex.org/g").await, 6);
    assert_eq!(
        select_rows(&s.app, "SELECT * { <http://e/s> <http://e/p> 1 }").await,
        1
    );
    // a source without its part
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                ("mapping", None, YARRRML),
                ("file", Some("people.csv"), PEOPLE),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["code"], "mapping-source-missing");
    // a stored mapping by name
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/one",
            Some("application/yaml"),
            ONE_TABLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = send(
        &s.app,
        multipart(
            "/ds/upload?mapping=one",
            &[("file", Some("people.csv"), PEOPLE)],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["tables"][0]["mapping"]["name"], "one");
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { <http://ex.org/person/2> <http://ex.org/name> \"Bo\" }"
        )
        .await,
        1
    );
    let r = send(
        &s.app,
        multipart(
            "/ds/upload?mapping=nope",
            &[("file", Some("people.csv"), PEOPLE)],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

const META: &str = r#"{"@context": "http://www.w3.org/ns/csvw", "tables": [
    {"url": "customer.csv", "tableSchema": {"primaryKey": "id",
      "aboutUrl": "http://ex.org/c/{id}",
      "columns": [{"name": "id"}, {"name": "name"}]}},
    {"url": "orders.csv", "tableSchema": {
      "aboutUrl": "http://ex.org/o/{id}",
      "columns": [{"name": "id"}, {"name": "customer"}],
      "foreignKeys": [{"columnReference": "customer",
        "reference": {"resource": "customer.csv", "columnReference": "id"}}]}}]}"#;

#[tokio::test]
async fn uploads_check_csvw_keys_in_standard_mode() {
    let s = server();
    // the orders come first in the upload, and are checked after the customers
    let bad = [
        ("mapping", None, META),
        ("file", Some("orders.csv"), "id,customer\n10,1\n11,2\n"),
        ("file", Some("customer.csv"), "id,name\n1,Ann\n"),
    ];
    let r = send(
        &s.app,
        multipart("/ds/upload?csvwMode=standard&base=http://ex.org/t/", &bad),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        r.text().contains("is not among the values of column id"),
        "{}",
        r.text()
    );
    let good = [
        ("mapping", None, META),
        ("file", Some("orders.csv"), "id,customer\n10,1\n"),
        ("file", Some("customer.csv"), "id,name\n1,Ann\n"),
    ];
    let r = send(
        &s.app,
        multipart("/ds/upload?csvwMode=standard&base=http://ex.org/t/", &good),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { ?g a <http://www.w3.org/ns/csvw#TableGroup> ; <http://www.w3.org/ns/csvw#table> ?t }"
        )
        .await,
        2
    );
    // keys without standard mode, and a duplicate primary key
    let dup = [
        ("mapping", None, META),
        ("file", Some("customer.csv"), "id,name\n1,Ann\n1,Bo\n"),
    ];
    let r = send(
        &s.app,
        multipart("/ds/upload?csvwCheck=keys&base=http://ex.org/t/", &dup),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("same primary key"), "{}", r.text());
    let r = send(
        &s.app,
        multipart(
            "/ds/upload?csvwMode=standard&base=http://ex.org/t/",
            &[("file", Some("a.csv"), "x\n1\n")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn templates_read_the_dataset_with_lookup() {
    let s = server();
    let r = send(
        &s.app,
        req(
            "POST",
            "/ds/data",
            Some("text/turtle"),
            "<http://ex.org/country/de> <http://ex.org/isoCode> \"DE\" .",
        ),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let template = "PREFIX ex: <http://ex.org/>\nCONSTRUCT { ?p ex:country ?c } WHERE { BIND(IRI(CONCAT(\"http://ex.org/p/\", ?id)) AS ?p) ?c ex:isoCode ?code }";
    let parts = [
        ("template", None, template),
        ("file", Some("people.csv"), "id,code\n1,DE\n"),
    ];
    let r = send(&s.app, multipart("/ds/upload?lookup=true", &parts)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.json()["tables"][0]["lookupCommit"].is_u64(),
        "{}",
        r.text()
    );
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { <http://ex.org/p/1> <http://ex.org/country> <http://ex.org/country/de> }"
        )
        .await,
        1
    );
    // lookup belongs to templates
    let r = send(
        &s.app,
        multipart(
            "/ds/upload?lookup=true&base=http://e/",
            &[("file", Some("a.csv"), "x\n1\n")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn graph_store_writes_take_csv_bodies() {
    let s = server();
    // without a mapping a table is not RDF
    let r = send(
        &s.app,
        req("PUT", "/ds/data?graph=urn:g", Some("text/csv"), PEOPLE),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{}", r.text());
    // the default mapping
    let r = send(
        &s.app,
        req(
            "PUT",
            "/ds/data?graph=urn:g&base=http://e/&key=id",
            Some("text/csv"),
            PEOPLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(count_graph(&s.app, "urn:g").await, 6);
    // a stored mapping whose only source is the body; its triples go into the graph
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/one",
            Some("application/yaml"),
            ONE_TABLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = send(
        &s.app,
        req(
            "PUT",
            "/ds/data?graph=urn:g&mapping=one",
            Some("text/csv"),
            PEOPLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(count_graph(&s.app, "urn:g").await, 2);
    let r = send(
        &s.app,
        req(
            "POST",
            "/ds/data?graph=urn:g&mapping=one",
            Some("text/csv"),
            "id,name,city\n3,Cy,b\n",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(count_graph(&s.app, "urn:g").await, 3);
    // DELETE with a body removes the mapped triples
    let r = send(
        &s.app,
        req(
            "DELETE",
            "/ds/data?graph=urn:g&mapping=one",
            Some("text/csv"),
            "id,name,city\n1,Ann,b\n",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["count"], 1);
    assert_eq!(count_graph(&s.app, "urn:g").await, 2);
    // a mapping with its own named graphs cannot write into another
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/mappings/ds/graphs",
            Some("application/yaml"),
            YARRRML,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = send(
        &s.app,
        req(
            "PUT",
            "/ds/data?graph=urn:g&mapping=graphs&source=people.csv",
            Some("text/csv"),
            PEOPLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // and a mapping that reads two files needs source=NAME
    let r = send(
        &s.app,
        req(
            "PUT",
            "/ds/data?graph=urn:g&mapping=graphs",
            Some("text/csv"),
            PEOPLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("source=NAME"), "{}", r.text());
    // the default graph, with a mapping into the default graph
    let r = send(
        &s.app,
        req(
            "POST",
            "/ds/data?default&mapping=one",
            Some("text/csv"),
            PEOPLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { <http://ex.org/person/1> <http://ex.org/name> \"Ann\" }"
        )
        .await,
        1
    );
}

#[cfg(feature = "mapping-sqlite")]
#[tokio::test]
async fn r2rml_mappings_read_the_servers_connections() {
    let s = server();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("shop.sqlite");
    let c = sparkles::mapping::rusqlite::Connection::open(&db).unwrap();
    c.execute_batch(
        "CREATE TABLE customer (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO customer VALUES (1, 'Ann');",
    )
    .unwrap();
    drop(c);
    let r2rml = r#"@prefix rr: <http://www.w3.org/ns/r2rml#> .
<#c> rr:logicalTable [ rr:tableName "customer" ] ;
  rr:subjectMap [ rr:template "http://ex.org/c/{id}" ] ;
  rr:predicateObjectMap [ rr:predicate <http://ex.org/name> ; rr:objectMap [ rr:column "name" ] ] .
"#;
    // stored before the connection exists: a warning, and a run that fails
    let r = send(
        &s.app,
        req("PUT", "/$/mappings/ds/shop", Some("text/turtle"), r2rml),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert!(
        r.text().contains("connections settings do not declare"),
        "{}",
        r.text()
    );
    let r = send(&s.app, req("POST", "/ds/mappings/shop/run", None, "")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // a relative path and a PostgreSQL connection are refused
    let bad =
        serde_json::json!({"connections": {"shop": {"kind": "sqlite", "path": "shop.sqlite"}}});
    let r = send(
        &s.app,
        req(
            "PATCH",
            "/$/server/settings/connections",
            Some("application/json"),
            &bad.to_string(),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let ok = serde_json::json!({"connections": {"shop": {"kind": "sqlite", "path": db.to_str().unwrap()}}});
    let r = send(
        &s.app,
        req(
            "PATCH",
            "/$/server/settings/connections",
            Some("application/json"),
            &ok.to_string(),
        ),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = send(&s.app, req("POST", "/ds/mappings/shop/run", None, "")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { <http://ex.org/c/1> <http://ex.org/name> \"Ann\" }"
        )
        .await,
        1
    );
}
