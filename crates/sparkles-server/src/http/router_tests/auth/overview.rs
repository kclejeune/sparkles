//! The graph overview's groups route (U06) under restricted views: a caller limited to
//! one named graph, callers under protections of a class and a predicate, all or some of
//! them lifted, and a caller
//! whose grant covers only the info endpoint. An answer never holds a class, a count or a
//! member the caller's SPARQL queries could not read, and the cache never serves one
//! caller's answer to another.

use super::*;
use serde_json::json;

/// The dataset `ov`, read as the union of its named graphs: half of the people in each.
const DATA: &str = r#"@prefix ex: <http://ex/> .
<http://ex/ga> {
  ex:p1 a ex:Person ; ex:city "Oslo" ; ex:knows ex:p3 .
  ex:p2 a ex:Person ; ex:city "Oslo" ; ex:knows ex:s1 .
}
<http://ex/gb> {
  ex:p3 a ex:Person ; ex:city "Kyoto" ; ex:salary 10 .
  ex:p4 a ex:Person ; ex:city "Kyoto" ; ex:salary 20 .
  ex:s1 a ex:Secret ; ex:city "Oslo" .
}
"#;

fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[protections]]
name = "secrets"
dataset = "ov"
classes = ["http://ex/Secret"]

[[protections]]
name = "salaries"
dataset = "ov"
predicates = ["http://ex/salary"]

[roles.reader]
[[roles.reader.grants]]
dataset = "ov"
level = "read"
lifts = ["secrets", "salaries"]

[[users]]
name = "ofull"
password = "{ofull}"
roles = ["reader"]

[[users]]
name = "oga"
password = "{oga}"
roles = []
[[users.grants]]
dataset = "ov"
level = "read"
graphs = ["http://ex/ga"]
lifts = ["secrets", "salaries"]

[[users]]
name = "oprot"
password = "{oprot}"
datasets = {{ ov = "read" }}

[[users]]
name = "olift"
password = "{olift}"
roles = []
[[users.grants]]
dataset = "ov"
level = "read"
lifts = ["secrets"]

[[users]]
name = "oinfo"
password = "{oinfo}"
[[users.grants]]
dataset = "ov"
level = "read"
endpoints = ["info"]
"#,
        ofull = h("ofull-pw"),
        oga = h("oga-pw"),
        oprot = h("oprot-pw"),
        olift = h("olift-pw"),
        oinfo = h("oinfo-pw"),
    )
}

fn server() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture {
        extra: users(),
        ..Default::default()
    };
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let opts = StoreOptions {
        union_default_graph: true,
        ..Default::default()
    };
    let mut st = AppState::new(dir.path(), opts, Duration::from_secs(30)).unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let st = Arc::new(st);
    let ds = st.attach("ov", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    st.set_phase(crate::obs::Phase::Ready);
    let app = router(st);
    (dir, app)
}

async fn groups(app: &Router, user: &str, body: J) -> R {
    call(
        app,
        "POST",
        "/$/graph/ov/groups",
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

fn sizes(a: &J) -> Vec<(String, u64)> {
    let mut v: Vec<_> = a["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["label"].as_str().unwrap().to_string(),
                g["size"].as_u64().unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn the_overview_shows_only_what_the_caller_reads() {
    let (_dir, app) = server();
    let by_city = json!({ "within": { "class": "http://ex/Person" }, "by": { "property": "http://ex/city" } });
    let members = json!({ "within": { "class": "http://ex/Person" }, "by": "members" });

    // a reader of everything: four people in two cities
    let r = groups(&app, "ofull", by_city.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        sizes(&r.json()),
        vec![("Kyoto".into(), 2), ("Oslo".into(), 2)]
    );
    let r = groups(&app, "ofull", members.clone()).await;
    assert_eq!(r.json()["members"].as_array().unwrap().len(), 4);
    assert!(r.text().contains("c:http://ex/Secret"), "{}", r.text());

    // limited to the graph of half of the people (A7): the same requests, right after
    // the wider caller's, count the visible half only
    let r = groups(&app, "oga", by_city.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let a = r.json();
    assert_eq!(a["within"]["size"], 2, "{a}");
    assert_eq!(sizes(&a), vec![("Oslo".into(), 2)]);
    assert!(!r.text().contains("Kyoto"), "{}", r.text());
    let r = groups(&app, "oga", members.clone()).await;
    let a = r.json();
    let iris: Vec<&str> = a["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["iri"].as_str().unwrap())
        .collect();
    assert_eq!(iris, ["http://ex/p1", "http://ex/p2"]);
    // p1 knows p3, whose type is in the other graph: no edge reveals it
    assert!(
        !r.text().contains("p3") && !r.text().contains("p4"),
        "{}",
        r.text()
    );

    // under the protections: no Secret class, member, count or link, and no salary
    let r = groups(&app, "oprot", members.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["members"].as_array().unwrap().len(), 4);
    assert!(
        !r.text().contains("Secret") && !r.text().contains("s1"),
        "{}",
        r.text()
    );
    let r = groups(
        &app,
        "oprot",
        json!({ "within": { "class": "http://ex/Secret" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["within"]["size"], 0, "{}", r.text());
    assert!(r.json()["members"].as_array().unwrap().is_empty());
    let r = groups(
        &app,
        "oprot",
        json!({ "within": { "class": "http://ex/Person" }, "by": { "property": "http://ex/salary" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // a reader with the protections lifted sees the salaries
    let r = groups(
        &app,
        "ofull",
        json!({ "within": { "class": "http://ex/Person" }, "by": { "property": "http://ex/salary" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // a grant of the info endpoint alone reads the class profiles, not the groups
    let r = groups(&app, "oinfo", by_city).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = call(
        &app,
        "GET",
        "/$/schema/ov/profiles",
        &[("authorization", &b("oinfo"))],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Callers with the same graphs but different lifted protections read different data, and
/// the cache keeps their answers apart, for the groups and for the class profiles.
#[tokio::test]
async fn partly_lifted_protections_do_not_share_cached_answers() {
    let (_dir, app) = server();
    let members = json!({ "within": { "class": "http://ex/Person" }, "by": "members" });
    // olift lifts the Secret protection only, and warms the cache first
    let r = groups(&app, "olift", members.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("c:http://ex/Secret"), "{}", r.text());
    let r = groups(&app, "oprot", members).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("Secret"), "{}", r.text());

    let profiles = |user: &'static str| {
        let app = app.clone();
        async move {
            call(
                &app,
                "GET",
                "/$/schema/ov/profiles",
                &[("authorization", &b(user))],
                "",
            )
            .await
        }
    };
    let r = profiles("olift").await;
    assert!(r.text().contains("http://ex/Secret"), "{}", r.text());
    let r = profiles("oprot").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("http://ex/Secret"), "{}", r.text());
}

/// The dataset a request names with `default-graph-uri` is part of the cached answer's key.
#[tokio::test]
async fn the_selected_default_graph_is_part_of_the_cache_key() {
    let (_dir, app) = server();
    let body = json!({ "within": { "class": "http://ex/Person" }, "by": "members" }).to_string();
    let mut seen = Vec::new();
    for g in ["http://ex/ga", "http://ex/gb"] {
        let r = call(
            &app,
            "POST",
            &format!(
                "/$/graph/ov/groups?default-graph-uri={}",
                percent_encoding::utf8_percent_encode(g, percent_encoding::NON_ALPHANUMERIC)
            ),
            &[
                ("authorization", &b("ofull")),
                ("content-type", "application/json"),
            ],
            &body,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let iris: Vec<String> = r.json()["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["iri"].as_str().unwrap().to_string())
            .collect();
        seen.push(iris);
    }
    assert_eq!(seen[0], ["http://ex/p1", "http://ex/p2"]);
    assert_eq!(seen[1], ["http://ex/p3", "http://ex/p4"]);
}

async fn facets(app: &Router, user: &str, body: J) -> R {
    call(
        app,
        "POST",
        "/$/graph/ov/facets",
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

/// The facets route (U11 A8) counts only what the caller reads: a caller limited to one
/// graph counts its half of the people, protected salaries count as missing, and an
/// answer cached for a wider caller is never served to a narrower one.
#[tokio::test]
async fn facets_count_only_what_the_caller_reads() {
    let (_dir, app) = server();
    let body = json!({
        "within": { "class": "http://ex/Person" },
        "facets": [{ "property": "http://ex/city" }, { "property": "http://ex/salary" }],
        "candidates": true,
    });
    let city = |a: &J| -> Vec<(String, u64)> {
        let mut v: Vec<_> = a["facets"][0]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["value"]["value"].as_str().unwrap().to_string(),
                    v["count"].as_u64().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    let r = facets(&app, "ofull", body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let a = r.json();
    assert_eq!(a["size"], 4, "{a}");
    assert_eq!(city(&a), vec![("Kyoto".into(), 2), ("Oslo".into(), 2)]);
    assert_eq!(a["facets"][1]["missing"], 2, "{a}");

    let r = facets(&app, "oga", body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let a = r.json();
    assert_eq!(a["size"], 2, "{a}");
    assert_eq!(city(&a), vec![("Oslo".into(), 2)]);
    assert!(!r.text().contains("Kyoto"), "{}", r.text());
    assert_eq!(a["facets"][1]["missing"], 2, "{a}");

    // a protected predicate's values count as missing, and it is no candidate
    let r = facets(&app, "olift", body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let a = r.json();
    assert_eq!(a["size"], 4, "{a}");
    assert_eq!(a["facets"][1]["missing"], 4, "{a}");
    assert!(
        !a["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["property"] == "http://ex/salary"),
        "{a}"
    );
    // a brush on the hidden predicate keeps nobody
    let r = facets(
        &app,
        "olift",
        json!({
            "within": { "class": "http://ex/Person" },
            "facets": [{ "property": "http://ex/city" }],
            "filters": [{ "property": "http://ex/salary", "range": [0, 100] }],
        }),
    )
    .await;
    assert_eq!(r.json()["matching"], 0, "{}", r.text());

    // a grant of the info endpoint only cannot count
    let r = facets(&app, "oinfo", body).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}
