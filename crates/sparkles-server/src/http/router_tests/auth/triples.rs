//! Protections of triples (C12 Phase 2): a matrix of principals over a dataset whose
//! salaries, patients and documents are protected, by predicate, by class and by a
//! pattern on the document's owner.

use super::*;

/// The dataset `hr`, all in the default graph.
pub(crate) const DATA: &str = r#"@prefix ex: <http://ex/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:alice ex:salary 100 ; ex:name "Alice" ; rdfs:label "alice fox" .
ex:bob a ex:InPatient ; ex:name "Bob" ; rdfs:label "bob fox" .
ex:InPatient rdfs:subClassOf ex:Patient .
ex:d1 a ex:Doc ; ex:owner "tcarol" ; ex:title "carol's" ; rdfs:label "carol fox" .
ex:d2 a ex:Doc ; ex:owner "dave" ; ex:title "dave's" ; rdfs:label "dave fox" .
"#;

/// The protections and users, each with the password `<name>-pw`:
/// * `tadmin` has `admin`, which lifts every protection;
/// * `tstaff` writes the dataset, under every protection;
/// * `thr` also reads and writes salaries (role `hr`);
/// * `tdoc` also reads patients (role `doctor`);
/// * `tcarol` reads the dataset and her own documents.
pub(crate) fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[protections]]
name = "salaries"
dataset = "hr"
predicates = ["http://ex/salary"]

[[protections]]
name = "patients"
dataset = "hr"
classes = ["http://ex/Patient"]

[[protections]]
name = "docs"
dataset = "hr"
classes = ["http://ex/Doc"]
pattern = "?s ex:owner ?user"
prefixes = {{ ex = "http://ex/" }}

[roles.hr]
datasets = {{ hr = "write" }}
[[roles.hr.grants]]
dataset = "hr"
level = "write"
lifts = ["salaries"]

[roles.doctor]
[[roles.doctor.grants]]
dataset = "hr"
level = "read"
lifts = ["patients"]

[[users]]
name = "tadmin"
password = "{tadmin}"
datasets = {{ hr = "admin" }}

[[users]]
name = "tstaff"
password = "{tstaff}"
datasets = {{ hr = "write" }}

[[users]]
name = "thr"
password = "{thr}"
roles = ["hr"]

[[users]]
name = "tdoc"
password = "{tdoc}"
roles = ["doctor"]
datasets = {{ hr = "read" }}

[[users]]
name = "tcarol"
password = "{tcarol}"
datasets = {{ hr = "read" }}

[[tokens]]
name = "tcarol"
hash = "{token}"
datasets = {{ hr = "read" }}
"#,
        token = token_hash(&tok('W')),
        tadmin = h("tadmin-pw"),
        tstaff = h("tstaff-pw"),
        thr = h("thr-pw"),
        tdoc = h("tdoc-pw"),
        tcarol = h("tcarol-pw"),
    )
}

fn server() -> AuthServer {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    let ds = s.state.attach("hr", DbType::Persistent, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
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

/// The rows of a SELECT as CSV lines, sorted.
async fn rows(app: &Router, user: &str, q: &str) -> Vec<String> {
    let r = call(
        app,
        "GET",
        &format!("/hr/sparql?query={}", enc(q)),
        &[("authorization", &b(user)), ("accept", "text/csv")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{user}: {}", r.text());
    let mut v: Vec<String> = r.text().lines().skip(1).map(str::to_string).collect();
    v.sort();
    v
}

const SUBJECTS: &str = "SELECT DISTINCT ?s { ?s ?p ?o }";

#[tokio::test]
async fn queries_see_what_the_protections_leave() {
    let s = server();
    let all = [
        "http://ex/InPatient",
        "http://ex/alice",
        "http://ex/bob",
        "http://ex/d1",
        "http://ex/d2",
    ];
    assert_eq!(rows(&s.app, "tadmin", SUBJECTS).await, all);
    // bob is a patient (through the subclass), and the documents are their owners'
    assert_eq!(
        rows(&s.app, "tstaff", SUBJECTS).await,
        ["http://ex/InPatient", "http://ex/alice"]
    );
    assert_eq!(
        rows(&s.app, "tdoc", SUBJECTS).await,
        ["http://ex/InPatient", "http://ex/alice", "http://ex/bob"]
    );
    assert_eq!(
        rows(&s.app, "tcarol", SUBJECTS).await,
        ["http://ex/InPatient", "http://ex/alice", "http://ex/d1"]
    );
    // a static token's name is its ?user
    let r = call(
        &s.app,
        "GET",
        &format!("/hr/sparql?query={}", enc(SUBJECTS)),
        &[
            ("authorization", &bearer(&tok('W'))),
            ("accept", "text/csv"),
        ],
        "",
    )
    .await;
    assert!(
        r.text().contains("http://ex/d1") && !r.text().contains("http://ex/d2"),
        "{}",
        r.text()
    );
    let pay = "SELECT ?s ?v { ?s <http://ex/salary> ?v }";
    assert!(rows(&s.app, "tstaff", pay).await.is_empty());
    assert_eq!(rows(&s.app, "thr", pay).await, ["http://ex/alice,100"]);
    // counts from the index statistics count the visible triples only
    let n = "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }";
    assert_eq!(rows(&s.app, "tadmin", n).await, ["15"]);
    assert_eq!(rows(&s.app, "tstaff", n).await, ["3"]);
    assert_eq!(rows(&s.app, "thr", n).await, ["4"]);
    // the result cache keeps the callers apart
    assert_eq!(rows(&s.app, "tadmin", n).await, ["15"]);
    assert_eq!(rows(&s.app, "tstaff", n).await, ["3"]);
    // ASK of a hidden triple answers like a missing one
    let ask = "ASK { <http://ex/alice> <http://ex/salary> 100 }";
    let missing = "ASK { <http://ex/alice> <http://ex/salary> 101 }";
    assert_eq!(
        rows(&s.app, "tstaff", ask).await,
        rows(&s.app, "tstaff", missing).await
    );
}

#[tokio::test]
async fn graph_store_reads_and_explain_follow_the_protections() {
    let s = server();
    let get = |u: String, user: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "GET",
                &u,
                &[
                    ("authorization", &b(user)),
                    ("accept", "application/n-triples"),
                ],
                "",
            )
            .await
        }
    };
    let r = get("/hr/data?default".into(), "tstaff").await;
    assert_eq!(r.status, StatusCode::OK);
    let t = r.text();
    assert!(
        !t.contains("salary") && !t.contains("bob") && t.contains("Alice"),
        "{t}"
    );
    let t = get("/hr/data?default".into(), "tadmin").await.text();
    assert!(t.contains("salary") && t.contains("bob"), "{t}");
    // explain: no estimates from the statistics of every triple
    let r = get(
        format!(
            "/hr/explain?query={}",
            enc("SELECT * { ?s <http://ex/salary> ?o }")
        ),
        "tstaff",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["plan"]["estimatedRows"], -1.0);
    // DESCRIBE leaves the hidden triples out
    let r = get(
        format!("/hr/sparql?query={}", enc("DESCRIBE <http://ex/alice>")),
        "tstaff",
    )
    .await;
    assert!(!r.text().contains("salary"), "{}", r.text());
    // full-text search: hits of visible triples only
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
        assert_eq!(
            hits(get("/hr/text?q=fox".into(), "tadmin").await),
            [
                "http://ex/alice",
                "http://ex/bob",
                "http://ex/d1",
                "http://ex/d2"
            ]
        );
        assert_eq!(
            hits(get("/hr/text?q=fox".into(), "tcarol").await),
            ["http://ex/alice", "http://ex/d1"]
        );
    }
}

#[tokio::test]
async fn writes_of_protected_triples_are_refused_alike() {
    let s = server();
    let up = |user: &'static str, u: &'static str| {
        let app = s.app.clone();
        async move { update_as(&app, "hr", &b(user), u).await }
    };
    // the same refusal whether or not the triple exists
    let a = up(
        "tstaff",
        "DELETE DATA { <http://ex/alice> <http://ex/salary> 100 }",
    )
    .await;
    let b2 = up(
        "tstaff",
        "DELETE DATA { <http://ex/alice> <http://ex/salary> 7 }",
    )
    .await;
    assert_eq!(a.status, StatusCode::FORBIDDEN, "{}", a.text());
    assert_eq!(b2.status, StatusCode::FORBIDDEN);
    assert!(!a.text().contains("salaries"), "{}", a.text());
    let r = up(
        "tstaff",
        "INSERT DATA { <http://ex/carl> <http://ex/salary> 7 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // the role that lifts salaries writes them
    let r = up(
        "thr",
        "INSERT DATA { <http://ex/carl> <http://ex/salary> 7 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a WHERE clause reads through the view: nothing hidden is deleted
    let r = up("tstaff", "DELETE WHERE { ?s <http://ex/salary> ?o }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        rows(&s.app, "tadmin", "SELECT ?s { ?s <http://ex/salary> ?o }").await,
        ["http://ex/alice", "http://ex/carl"]
    );
    // carol may write her own documents only
    let r = up(
        "tcarol",
        "INSERT DATA { <http://ex/d1> <http://ex/title> \"x\" }",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "a reader: {}", r.text());
    // Graph Store and upload writes are checked the same way
    let r = call(
        &s.app,
        "POST",
        "/hr/data?default",
        &[
            ("authorization", &b("tstaff")),
            ("content-type", "application/n-triples"),
        ],
        "<http://ex/x> <http://ex/salary> \"1\" .",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // a dry run is refused before it previews
    let r = call(
        &s.app,
        "POST",
        "/hr/update?dryRun=true",
        &[
            ("authorization", &b("tstaff")),
            ("content-type", "application/sparql-update"),
        ],
        "INSERT DATA { <http://ex/x> <http://ex/salary> 1 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[tokio::test]
async fn routes_listings_and_whoami() {
    let s = server();
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", u, &[("authorization", &b(user))], "").await }
    };
    // routes that report on every triple refuse a protected caller
    for u in ["/$/stats/hr", "/$/text/hr", "/$/history/hr"] {
        let r = get(u, "tstaff").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{u}: {}", r.text());
        assert_ne!(get(u, "tadmin").await.status, StatusCode::FORBIDDEN, "{u}");
    }
    let r = call(
        &s.app,
        "POST",
        "/hr/shacl",
        &[
            ("authorization", &b("tstaff")),
            ("content-type", "text/turtle"),
        ],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // the size of the dataset is the visible triples'
    let r = get("/$/datasets/hr", "tstaff").await.json();
    assert_eq!(r["quads"], 3, "{r}");
    let r = get("/$/datasets/hr", "tadmin").await.json();
    assert_eq!(r["quads"], 15, "{r}");
    // whoami says that triples are protected, never which
    let w = get("/$/whoami", "tstaff").await.json();
    assert_eq!(w["restricted"]["hr"]["triples"], true, "{w}");
    assert_eq!(w["restricted"]["hr"]["graphs"], false, "{w}");
    assert!(!w.to_string().contains("salar"), "{w}");
    let w = get("/$/whoami", "tadmin").await.json();
    assert!(w["restricted"].get("hr").is_none(), "{w}");
    // schema reports and drafted shapes count the visible triples
    let r = get("/$/schema/hr", "tstaff").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["totals"]["triples"], 3, "{}", r.text());
    assert!(!r.text().contains("salary"));
    let r = get("/$/schema/hr/shapes", "tstaff").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        !r.text().contains("salary") && !r.text().contains("Patient"),
        "{}",
        r.text()
    );
}

#[tokio::test]
async fn diffs_and_the_change_feed() {
    let s = server();
    let r = update_as(
        &s.app,
        "hr",
        &b("tadmin"),
        "INSERT DATA { <http://ex/alice> a <http://ex/Patient> . <http://ex/eve> <http://ex/name> \"Eve\" }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let get = |u: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "GET", u, &[("authorization", &b(user))], "").await }
    };
    // alice became a patient: for staff her name disappears, eve's appears
    let d = get("/hr/diff?quads=true", "tstaff").await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.text());
    let d = d.json();
    assert_eq!(d["added"], 1, "{d}");
    assert_eq!(d["removed"], 2, "{d}");
    let t = d.to_string();
    assert!(t.contains("Eve") && !t.contains("Patient"), "{d}");
    // the feed would need the view at every commit
    let r = get("/hr/changes?after=1", "tstaff").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(
        get("/hr/changes?after=1", "tadmin").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn stored_queries_run_on_the_protected_view() {
    let s = server();
    let ds = s.state.datasets.read().get("hr").cloned().unwrap();
    let def: sparkles::stored::Definition = serde_json::from_value(serde_json::json!({
        "query": "SELECT ?s ?v WHERE { ?s <http://ex/salary> ?v }"
    }))
    .unwrap();
    ds.queries
        .put("pay", def, sparkles::stored::Change::default())
        .unwrap();
    let run = |user: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "GET",
                "/hr/queries/pay",
                &[("authorization", &b(user)), ("accept", "text/csv")],
                "",
            )
            .await
            .text()
        }
    };
    assert!(run("thr").await.contains("alice"));
    assert!(!run("tstaff").await.contains("alice"));
}

#[test]
fn protections_are_validated() {
    let base = "version = 1\n";
    let bad = [
        (
            "[[protections]]\nname = \"p\"\ndataset = \"hr\"\npredicates = []\n",
            "empty predicates",
        ),
        (
            "[[protections]]\nname = \"p\"\ndataset = \"hr\"\nclasses = [\"http://ex/*\"]\n",
            "invalid class",
        ),
        (
            "[[protections]]\nname = \"p\"\ndataset = \"hr\"\npattern = \"?s ex:owner\"\n",
            "does not parse",
        ),
        (
            "[[protections]]\nname = \"p\"\ndataset = \"hr\"\n[[protections]]\nname = \"p\"\ndataset = \"x\"\n",
            "duplicate protection",
        ),
        (
            "[[anonymous.grants]]\ndataset = \"hr\"\nlevel = \"read\"\nlifts = [\"nope\"]\n",
            "unknown protection 'nope'",
        ),
        (
            "[[protections]]\nname = \"p\"\ndataset = \"hr\"\nweird = 1\n",
            "unknown field",
        ),
    ];
    for (toml, want) in bad {
        let e = crate::auth::config::FileConfig::parse(&format!("{base}{toml}"))
            .err()
            .unwrap_or_else(|| panic!("accepted: {toml}"));
        let msg = format!("{e:#}");
        assert!(msg.contains(want), "{toml}: {msg}");
    }
    crate::auth::config::FileConfig::parse(&format!(
        "{base}[[protections]]\nname = \"p\"\ndataset = \"hr\"\npattern = \"?s ex:owner ?user\"\n\
         prefixes = {{ ex = \"http://ex/\" }}\n[[anonymous.grants]]\ndataset = \"hr\"\n\
         level = \"read\"\nlifts = [\"p\"]\n"
    ))
    .unwrap();
}
