//! Characterization goldens for the HTTP bodies of every result format in every execution
//! mode (item 828). Each response's status, content type and body is compared with a file
//! under `tests/goldens/writers-http/`, generated on the code before the writers were
//! unified. Native JSON keeps its rows byte for byte and has the per-run figures and the plan
//! of its `meta` normalized. Bodies cut by the result-size budget are recorded by status and
//! error only, since the byte at which a cut body stops is not part of the contract.
//!
//! `UPDATE_GOLDENS=1` writes the files instead of comparing them.

use super::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const DATA: &str = r#"
@prefix ex: <urn:ex:> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:s1 ex:p "plain", "quote\" back\\ nl\n tab\t", "ctl\u0001", "]]> <&>", "😀 é", "",
    "en"@en, "rtl"@ar--rtl, "42"^^xsd:integer, "abc"^^xsd:integer, ex:o, _:b1,
    <<( ex:a ex:b "t" )>> .
_:b1 ex:q ex:s1 .
ex:g { ex:s1 ex:p "in g" . }
"#;

struct Server {
    _dir: tempfile::TempDir,
    app: Router,
}

fn server(limit: Option<u64>) -> Server {
    server_with(limit, "")
}

/// A server over [`DATA`] with `extra` statements added.
fn server_with(limit: Option<u64>, extra: &str) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.limits.max_result_bytes = limit;
    let state = Arc::new(state);
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    let mut data = DATA.to_string();
    // a literal larger than a streamed chunk
    data.push_str(&format!(
        "ex:s2 ex:p \"{}\" .\n",
        "abc\\\"<&>é".repeat(10_000)
    ));
    data.push_str(extra);
    ds.store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    state.set_phase(Phase::Ready);
    let app = router(state.clone());
    Server { _dir: dir, app }
}

async fn post(app: &Router, uri: &str, query: &str, accept: &str) -> (u16, String, Vec<u8>) {
    let req = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, accept)
        .body(Body::from(query.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_else(|e| format!("!BODY ERROR: {e}").into_bytes());
    (status, ct, body)
}

/// Native JSON with the per-run figures and the plan of its `meta` replaced.
fn normalize_native(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(text) = String::from_utf8(bytes.clone()) else {
        return bytes;
    };
    let Some(at) = text.rfind(",\"meta\":") else {
        return bytes;
    };
    let (rows, rest) = text.split_at(at);
    let Some(end) = rest.rfind('}') else {
        return bytes;
    };
    let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&rest[8..end]) else {
        return bytes;
    };
    fn scrub(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    let key = k.to_ascii_lowercase();
                    if key.ends_with("ms")
                        || key.ends_with("bytes")
                        || key.contains("timing")
                        || key.contains("memory")
                        || key == "rowsproduced"
                        || key == "plan"
                        || key == "datasetid"
                    {
                        *v = serde_json::Value::String("<n>".into());
                    } else {
                        scrub(v);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(scrub),
            _ => {}
        }
    }
    scrub(&mut meta);
    format!("{rows},\"meta\":{}{}", meta, &rest[end..]).into_bytes()
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/writers-http")
}

fn check(failures: &mut Vec<String>, name: &str, record: &[u8]) {
    let dir = golden_dir();
    let (file, content) = if record.len() > 64 << 10 {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(record);
        let hex = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        (
            format!("{name}.sha256"),
            format!("{} {hex}\n", record.len()).into_bytes(),
        )
    } else {
        (name.to_string(), record.to_vec())
    };
    let path = dir.join(&file);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, &content).unwrap();
        return;
    }
    match std::fs::read(&path) {
        Ok(expected) if expected == content => {}
        Ok(_) => failures.push(format!("{file} differs")),
        Err(e) => failures.push(format!("{file}: {e}")),
    }
}

const SELECT: &str = "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o";
const ASK: &str = "ASK { ?s ?p ?o }";
const CONSTRUCT: &str =
    "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o FILTER(!isLiteral(?o) || STRLEN(STR(?o)) < 100) }";

const SOLUTIONS: &[(&str, &str)] = &[
    ("json", "application/sparql-results+json"),
    ("xml", "application/sparql-results+xml"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("thrift", "application/sparql-results+thrift"),
    ("native", "application/x-sparkles+json"),
];

const GRAPHS: &[(&str, &str)] = &[
    ("ttl", "text/turtle"),
    ("nt", "application/n-triples"),
    ("nq", "application/n-quads"),
    ("trig", "application/trig"),
    ("rdfxml", "application/rdf+xml"),
    ("rt", "application/rdf+thrift"),
    ("pb", "application/rdf+protobuf"),
    ("rj", "application/rdf+json"),
    ("trix", "application/trix+xml"),
    ("native", "application/x-sparkles+json"),
];

const MODES: &[&str] = &["eager", "streaming", "auto"];

/// The status, content type and body of a response. An error body drops its request id, and
/// the size a refused write would have reached, which depends on where the cut fell.
fn record(status: u16, ct: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("{status} {ct}\n").into_bytes();
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(mut map)) if status >= 400 => {
            map.remove("requestId");
            map.remove("requested");
            // the plan of a failed native stream holds its timings
            map.remove("plan");
            out.extend_from_slice(serde_json::Value::Object(map).to_string().as_bytes());
        }
        _ => out.extend_from_slice(body),
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_http_body_matches_its_golden() {
    let s = server(None);
    let mut failures = Vec::new();
    for (qname, q, formats) in [
        ("select", SELECT, SOLUTIONS),
        ("ask", ASK, SOLUTIONS),
        ("construct", CONSTRUCT, GRAPHS),
    ] {
        for &(fname, accept) in formats {
            for mode in MODES {
                for send in ["", "&send=2"] {
                    if qname == "ask" && !send.is_empty() {
                        continue;
                    }
                    let uri = format!("/ds/sparql?execution={mode}{send}");
                    let (status, ct, mut body) = post(&s.app, &uri, q, accept).await;
                    if fname == "native" {
                        body = normalize_native(body);
                    }
                    let send = if send.is_empty() { "" } else { ".send2" };
                    check(
                        &mut failures,
                        &format!("{qname}{send}.{fname}.{mode}"),
                        &record(status, &ct, &body),
                    );
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} golden mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_cut_bodies_keep_their_status_and_error() {
    let mut failures = Vec::new();
    for limit in [200u64, 5_000, 70_000] {
        let s = server(Some(limit));
        for &(fname, accept) in SOLUTIONS {
            for mode in MODES {
                let (status, ct, body) = post(
                    &s.app,
                    &format!("/ds/sparql?execution={mode}"),
                    SELECT,
                    accept,
                )
                .await;
                // a refused response carries its error, and a body that started only whether it
                // kept within the limit
                let summary = if status == 200 {
                    format!("within the limit: {}\n", body.len() as u64 <= limit)
                } else {
                    String::from_utf8_lossy(&body).into_owned()
                };
                check(
                    &mut failures,
                    &format!("budget{limit}.{fname}.{mode}"),
                    &record(status, &ct, summary.as_bytes()),
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} golden mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A row larger than the result-size budget that the response meets before it commits is
/// refused with 507 in every execution, whatever pieces its encoder writes it in. Its
/// 135,003 bytes of TSV, 90,002 of CSV and more of JSON and XML would pass the 64 KiB at which a
/// streamed response commits if they went on in pieces. The formats listed hand each row
/// on whole. On sparesults' JSON writer, SPARQL JSON in streaming answered 200 with a cut
/// body here, since it reached the response in pieces of 8 KiB, and native JSON still
/// does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_row_past_the_budget_before_the_commit_is_refused() {
    let big = format!("<urn:ex:big> <urn:ex:p> \"{}\" .\n", "x\\t".repeat(45_000));
    let s = server_with(Some(80 << 10), &big);
    let mut failures = Vec::new();
    for &(fname, accept) in SOLUTIONS {
        if !["tsv", "csv", "json", "xml"].contains(&fname) {
            continue;
        }
        for mode in MODES {
            let (status, _, body) = post(
                &s.app,
                &format!("/ds/sparql?execution={mode}"),
                "SELECT ?v WHERE { <urn:ex:big> <urn:ex:p> ?v }",
                accept,
            )
            .await;
            if status != 507 {
                failures.push(format!("{fname} {mode}: {status}, {} bytes", body.len()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
