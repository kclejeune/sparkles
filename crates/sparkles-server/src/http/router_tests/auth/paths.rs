//! Path search under a restricted reader (U09 §5, F07 §4.6): a path never crosses a graph
//! the reader may not read or a triple a protection hides, and `path:maxDegree` counts
//! a node's edges in the reader's view, so hidden triples do not make it a hub.

use super::*;

/// The dataset `paths`. In `http://ex/open` a chain `a → b → c → d` of `ex:p`, a hidden
/// shortcut `a ex:hidden d`, and `a → h → d` of `ex:r`. In `http://ex/secret` a
/// shortcut `a ex:p d` and five more `ex:r` edges of `h`. In the default graph
/// `c → k → e` of `ex:r`, and five `ex:hidden` edges of `k`.
const DATA: &str = r#"@prefix ex: <http://ex/> .
<http://ex/open> {
  ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:d .
  ex:a ex:hidden ex:d .
  ex:a ex:r ex:h . ex:h ex:r ex:d .
}
<http://ex/secret> {
  ex:a ex:p ex:d .
  ex:h ex:r ex:s1 , ex:s2 , ex:s3 , ex:s4 , ex:s5 .
}
ex:c ex:r ex:k . ex:k ex:r ex:e .
ex:k ex:hidden ex:z1 , ex:z2 , ex:z3 , ex:z4 , ex:z5 .
"#;

/// `padmin` sees everything. `preader` reads the default graph and `http://ex/open`,
/// under the protection of `ex:hidden`.
fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[protections]]
name = "hidden"
dataset = "paths"
predicates = ["http://ex/hidden"]

[[users]]
name = "padmin"
password = "{padmin}"
datasets = {{ paths = "admin" }}

[[users]]
name = "preader"
password = "{preader}"
[[users.grants]]
dataset = "paths"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://ex/open"]
"#,
        padmin = h("padmin-pw"),
        preader = h("preader-pw"),
    )
}

fn server() -> AuthServer {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    let ds = s.state.attach("paths", DbType::Persistent, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    s
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The path lengths a search returns, sorted, or the status and body of a failure.
async fn lengths(
    app: &Router,
    user: &str,
    graph: &str,
    params: &str,
) -> Result<Vec<String>, (StatusCode, String)> {
    let service = format!(
        "SERVICE path:search {{ [] path:source ex:a ; path:target ex:{params} ; path:length ?len }}"
    );
    let pattern = match graph {
        "" => service,
        g => format!("GRAPH <{g}> {{ {service} }}"),
    };
    let q = format!(
        "PREFIX ex: <http://ex/> PREFIX path: <urn:x-sparkles:path#> SELECT ?len WHERE {{ {pattern} }}"
    );
    let r = call(
        app,
        "GET",
        &format!("/paths/sparql?query={}", enc(&q)),
        &[("authorization", &b(user)), ("accept", "text/csv")],
        "",
    )
    .await;
    if r.status != StatusCode::OK {
        return Err((r.status, r.text()));
    }
    let mut v: Vec<String> = r.text().lines().skip(1).map(str::to_string).collect();
    v.sort();
    Ok(v)
}

const UNION: &str = "urn:x-arq:UnionGraph";

#[tokio::test]
async fn paths_stay_inside_the_readers_view() {
    let s = server();
    let ok = |v: &[&str]| Ok(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // A6: the shortcut in the secret graph is the admin's shortest path only
    let q = "d ; path:predicate ex:p";
    assert_eq!(lengths(&s.app, "padmin", UNION, q).await, ok(&["1"]));
    assert_eq!(lengths(&s.app, "preader", UNION, q).await, ok(&["3"]));
    // the secret graph itself is not the reader's to search
    assert_eq!(
        lengths(&s.app, "preader", "http://ex/secret", q).await,
        ok(&[])
    );
    // a hidden triple is no edge
    let q = "d ; path:predicate ex:hidden";
    assert_eq!(lengths(&s.app, "padmin", UNION, q).await, ok(&["1"]));
    assert_eq!(lengths(&s.app, "preader", UNION, q).await, ok(&[]));
    // excluded predicates apply in the view
    let q = "d ; path:excludePredicate ex:p , ex:hidden";
    assert_eq!(lengths(&s.app, "padmin", UNION, q).await, ok(&["2"]));
    assert_eq!(lengths(&s.app, "preader", UNION, q).await, ok(&["2"]));
}

#[tokio::test]
async fn degrees_are_counted_in_the_readers_view() {
    let s = server();
    let ok = |v: &[&str]| Ok(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // h has six ex:r edges for the admin, of which five are in the secret graph
    let q = "d ; path:predicate ex:r ; path:maxDegree 3";
    assert_eq!(lengths(&s.app, "padmin", UNION, q).await, ok(&[]));
    assert_eq!(lengths(&s.app, "preader", UNION, q).await, ok(&["2"]));
    // k has six edges for the admin, of which five are hidden triples
    let qk = |d: u32| {
        format!(
            "PREFIX ex: <http://ex/> PREFIX path: <urn:x-sparkles:path#> SELECT ?len WHERE {{ \
             SERVICE path:search {{ [] path:source ex:c ; path:target ex:e ; path:maxDegree {d} ; \
             path:excludePredicate ex:p ; path:length ?len }} }}"
        )
    };
    for (user, d, want) in [("padmin", 5, ""), ("padmin", 6, "2"), ("preader", 1, "2")] {
        let r = call(
            &s.app,
            "GET",
            &format!("/paths/sparql?query={}", enc(&qk(d))),
            &[("authorization", &b(user)), ("accept", "text/csv")],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let got: Vec<String> = r.text().lines().skip(1).map(str::to_string).collect();
        assert_eq!(got.join(","), want, "{user} maxDegree {d}");
    }
}

#[tokio::test]
async fn malformed_filters_are_a_400() {
    let s = server();
    // A3
    let r = lengths(
        &s.app,
        "preader",
        "",
        "d ; path:predicate ex:p ; path:excludePredicate ex:hidden",
    )
    .await
    .unwrap_err();
    assert_eq!(r.0, StatusCode::BAD_REQUEST);
    assert!(
        r.1.contains("path:search: path:excludePredicate cannot be combined"),
        "{}",
        r.1
    );
    let r = lengths(&s.app, "preader", "", "d ; path:maxDegree 0")
        .await
        .unwrap_err();
    assert_eq!(r.0, StatusCode::BAD_REQUEST);
    assert!(
        r.1.contains("path:search: path:maxDegree must be positive"),
        "{}",
        r.1
    );
}
