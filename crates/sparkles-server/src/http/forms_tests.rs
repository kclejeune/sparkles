//! Editing forms over HTTP (spec U02 §3, §7): the form model of A1 and A7, the server
//! check of A3 against `/{ds}/shacl` on the same state, saves to a session branch while
//! `main` is protected (A4), conflicts (A5) under concurrent writers, the `forms`
//! settings kind and the request checks. Grants are tested in
//! `router_tests::auth::forms`.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

pub(crate) const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://example.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:name "Name" ; sh:minCount 1 ; sh:datatype xsd:string ; sh:order 1 ] ;
  sh:property [ sh:path ex:email ; sh:name "Email" ; sh:maxCount 1 ;
                sh:pattern "^[^@]+@[^@]+\\.[a-z]+$" ; sh:order 2 ] ;
  sh:property [ sh:path ex:worksFor ; sh:name "Works for" ; sh:class ex:Org ; sh:minCount 1 ; sh:order 3 ] ;
  sh:property [ sh:path ex:role ; sh:name "Role" ; sh:in ( "Engineer" "Manager" "Other" ) ; sh:order 4 ] .
"#;

pub(crate) const DATA: &str = r#"
PREFIX ex: <http://example.org/>
INSERT DATA {
  ex:alice a ex:Person ; ex:name "Alice Lima" ; ex:email "alice@example.org" ;
    ex:worksFor ex:acme ; ex:role "Engineer" ; ex:nickname "Al" .
  ex:acme a ex:Org ; ex:name "Acme Corp" .
  ex:bob a ex:Person ; ex:name "Bob" ; ex:worksFor ex:acme .
}"#;

const EX: &str = "http://example.org/";

struct Resp {
    status: StatusCode,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
}

async fn send(app: &Router, method: &str, uri: &str, ct: &str, body: &str) -> Resp {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp { status, body }
}

/// A server with the dataset `people`, its data and a `reject` guard of the shapes.
async fn server() -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let st = Arc::new(st);
    st.create("people", DbType::Persistent).unwrap();
    let app = router(st.clone());
    let r = send(
        &app,
        "POST",
        "/people/update",
        "application/sparql-update",
        DATA,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let cfg = json!({ "mode": "reject", "shapes": { "inline": SHAPES } });
    let r = send(
        &app,
        "PUT",
        "/$/validation/people",
        "application/json",
        &cfg.to_string(),
    )
    .await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    (dir, st, app)
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn model(app: &Router, query: &str) -> J {
    let r = send(
        app,
        "GET",
        &format!(
            "/$/forms/people?focus={}{query}",
            enc(&format!("{EX}alice"))
        ),
        "",
        "",
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json()
}

fn field<'a>(m: &'a J, label: &str) -> &'a J {
    m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["label"] == label)
        .unwrap_or_else(|| panic!("no field {label}: {m}"))
}

fn lit(s: &str) -> J {
    json!({ "type": "literal", "value": s })
}

fn iri(s: &str) -> J {
    json!({ "type": "uri", "value": format!("{EX}{s}") })
}

fn change(field: &str, remove: J, add: J, expect: J) -> J {
    let id = format!("<{EX}{field}>");
    json!({
        "focus": format!("{EX}alice"),
        "changes": [{ "field": id, "remove": remove, "add": add }],
        "expect": { id: expect },
        "message": "fix email",
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a1_a7_the_form_model_over_http() {
    let (_d, _st, app) = server().await;
    let m = model(&app, "").await;
    let labels: Vec<&str> = m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Name", "Email", "Works for", "Role"]);
    assert_eq!(field(&m, "Name")["constraints"]["minCount"], 1);
    assert_eq!(field(&m, "Works for")["constraints"]["minCount"], 1);
    assert_eq!(field(&m, "Role")["widget"], "radio");
    assert_eq!(
        field(&m, "Role")["constraints"]["in"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(m["shapes"]["source"], "guard");
    assert_eq!(m["guard"], json!({ "language": "shacl", "mode": "reject" }));
    assert_eq!(m["other"][0]["predicate"], format!("{EX}nickname"));
    assert_eq!(m["save"]["to"], "main");
    assert_eq!(m["editable"], true);
    // a focus that is missing, and one no shape targets
    let r = send(&app, "GET", "/$/forms/people", "", "").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &app,
        "GET",
        &format!("/$/forms/people?focus={}", enc(&format!("{EX}acme"))),
        "",
        "",
    )
    .await;
    assert_eq!(r.json()["fields"], json!([]));
    // a past state is read-only
    let m = model(&app, "&at=commit:1").await;
    assert_eq!(m["editable"], false);
    assert_eq!(field(&m, "Name")["reason"], "state");
}

#[tokio::test(flavor = "multi_thread")]
async fn a3_the_check_maps_results_and_agrees_with_shacl() {
    let (_d, _st, app) = server().await;
    let c = change(
        "worksFor",
        json!([iri("acme")]),
        json!([iri("bob")]),
        json!([iri("acme")]),
    );
    let r = send(
        &app,
        "POST",
        "/$/forms/people/check",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let j = r.json();
    assert_eq!(j["preview"]["outcome"], "rejected");
    assert_eq!(j["preview"]["dryRun"], true);
    assert_eq!(j["validation"]["checked"], true);
    assert_eq!(j["validation"]["conforms"], false);
    let results = j["validation"]["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|r| r["field"] == format!("<{EX}worksFor>")
                && r["sourceConstraintComponent"]["value"]
                    == "http://www.w3.org/ns/shacl#ClassConstraintComponent"),
        "{j}"
    );
    // the same state, validated by /{ds}/shacl with the guard's shapes, finds as many
    // results on the focus node: write it without the guard
    let u = format!(
        "DELETE DATA {{ <{EX}alice> <{EX}worksFor> <{EX}acme> }} ; INSERT DATA {{ <{EX}alice> <{EX}worksFor> <{EX}bob> }}"
    );
    let off = json!({ "mode": "off", "shapes": { "inline": SHAPES } });
    send(
        &app,
        "PUT",
        "/$/validation/people",
        "application/json",
        &off.to_string(),
    )
    .await;
    send(&app, "DELETE", "/$/validation/people", "", "").await;
    let r = send(
        &app,
        "POST",
        "/people/update",
        "application/sparql-update",
        &u,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = send(
        &app,
        "POST",
        &format!("/people/shacl?target={}", enc(&format!("<{EX}alice>"))),
        "text/turtle",
        SHAPES,
    )
    .await;
    let shacl = send(
        &app,
        "POST",
        &format!(
            "/people/shacl?format=json&target={}",
            enc(&format!("<{EX}alice>"))
        ),
        "text/turtle",
        SHAPES,
    )
    .await
    .json();
    let _ = r;
    let on_alice = results
        .iter()
        .filter(|r| r["focusNode"]["value"] == format!("{EX}alice"))
        .count();
    assert_eq!(
        shacl["results"].as_array().unwrap().len(),
        on_alice,
        "{shacl} {j}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a4_saves_go_to_a_session_branch_while_main_is_protected() {
    let (_d, st, app) = server().await;
    let r = send(
        &app,
        "PATCH",
        "/$/branches/people/main",
        "application/json",
        r#"{"protected": true}"#,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let m = model(&app, "").await;
    assert_eq!(m["save"]["to"], "branch");
    assert_eq!(m["save"]["exists"], false);
    let session = m["save"]["branch"].as_str().unwrap().to_string();
    assert!(session.starts_with("edits."), "{session}");
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    assert!(session.ends_with(&today), "{session}");
    assert_eq!(m["editable"], true);
    let main_head = st.get("people").unwrap().store.snapshot().commit;
    let old = lit("alice@example.org");
    let c = change(
        "email",
        json!([old]),
        json!([lit("al@example.org")]),
        json!([old]),
    );
    // the check before the first save previews the branch's first commit on main's head,
    // which main's protection does not refuse, and creates no branch
    let r = send(
        &app,
        "POST",
        "/$/forms/people/check",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let j: J = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(j["preview"]["outcome"], "commit", "{j}");
    assert_eq!(j["branch"], session.as_str());
    assert!(
        st.get("people")
            .unwrap()
            .store
            .branch_id_of(&session)
            .is_err()
    );
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let j = r.json();
    assert_eq!(j["branch"], session);
    assert_eq!(j["branchCreated"], true);
    assert_eq!(j["committed"], true);
    assert_eq!(j["commit"]["message"], "fix email");
    // main is unchanged, and the branch has the new email
    assert_eq!(st.get("people").unwrap().store.snapshot().commit, main_head);
    let m = model(&app, "").await;
    assert_eq!(
        field(&m, "Email")["values"][0]["term"]["value"],
        "alice@example.org"
    );
    let mb = model(&app, &format!("&branch={session}")).await;
    assert_eq!(
        field(&mb, "Email")["values"][0]["term"]["value"],
        "al@example.org"
    );
    assert_eq!(mb["save"]["to"], "branch");
    assert_eq!(mb["branch"], session);
    // the second save reuses the branch
    let c = change(
        "role",
        json!([lit("Engineer")]),
        json!([lit("Manager")]),
        json!([lit("Engineer")]),
    );
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["branchCreated"], false);
    let b = send(
        &app,
        "GET",
        &format!("/$/branches/people/{session}"),
        "",
        "",
    )
    .await
    .json();
    assert_eq!(b["ahead"], 2);
    // another branch name that does not exist is not created
    let mut c = c;
    c["branch"] = json!("someone.else");
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn a5_a_stale_form_gets_the_conflict_and_concurrent_saves_one_wins() {
    let (_d, _st, app) = server().await;
    let old = lit("alice@example.org");
    let first = change(
        "email",
        json!([old]),
        json!([lit("one@example.org")]),
        json!([old]),
    );
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &first.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let second = change(
        "email",
        json!([old]),
        json!([lit("two@example.org")]),
        json!([old]),
    );
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &second.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let j = r.json();
    assert_eq!(j["code"], "form-conflict");
    assert_eq!(
        j["fields"],
        json!([{ "id": format!("<{EX}email>"), "now": [lit("one@example.org")] }])
    );
    // the check of the stale change reports the same conflict before any save
    let r = send(
        &app,
        "POST",
        "/$/forms/people/check",
        "application/json",
        &second.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["conflicts"], j["fields"]);
    // concurrent saves of the same loaded value: exactly one commits
    let now = lit("one@example.org");
    let mut tasks = Vec::new();
    for i in 0..6 {
        let app = app.clone();
        let c = change(
            "email",
            json!([now]),
            json!([lit(&format!("c{i}@example.org"))]),
            json!([now]),
        );
        tasks.push(tokio::spawn(async move {
            send(
                &app,
                "POST",
                "/$/forms/people/save",
                "application/json",
                &c.to_string(),
            )
            .await
            .status
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            StatusCode::OK => ok += 1,
            StatusCode::CONFLICT => {}
            s => panic!("unexpected {s}"),
        }
    }
    assert_eq!(ok, 1);
    let m = model(&app, "").await;
    assert_eq!(field(&m, "Email")["values"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_saves_and_request_checks() {
    let (_d, _st, app) = server().await;
    // removing the only name: the guard rejects with 422
    let c = change(
        "name",
        json!([lit("Alice Lima")]),
        json!([]),
        json!([lit("Alice Lima")]),
    );
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // missing expect, an unknown field, an unknown member, a bad term
    let mut c = change("email", json!([]), json!([lit("x@example.org")]), json!([]));
    c["expect"] = json!({});
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let c = change("nope", json!([]), json!([lit("x")]), json!([]));
    let r = send(
        &app,
        "POST",
        "/$/forms/people/check",
        "application/json",
        &c.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &app,
        "POST",
        "/$/forms/people/check",
        "application/json",
        r#"{"focus":"http://example.org/alice","changes":[],"x":1}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(&app, "POST", "/$/forms/people/check", "application/json", r#"{"focus":"http://example.org/alice","changes":[{"field":"<http://example.org/email>","add":[{"type":"bnode","value":"b1"}]}]}"#).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forms_settings_kind_chooses_shapes_and_saves() {
    let (_d, st, app) = server().await;
    let r = send(&app, "GET", "/$/settings/people/forms", "", "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["effective"]["saveTo"], "auto");
    // shapes from a graph of the dataset
    let shapes = SHAPES.replace("sh:name \"Name\"", "sh:name \"Full name\"");
    let r = send(
        &app,
        "PUT",
        &format!("/people/data?graph={}", enc("urn:shapes")),
        "text/turtle",
        &shapes,
    )
    .await;
    assert!(r.status.is_success() || r.status == StatusCode::UNPROCESSABLE_ENTITY);
    let off = json!({ "mode": "off", "shapes": { "inline": SHAPES } });
    send(
        &app,
        "PUT",
        "/$/validation/people",
        "application/json",
        &off.to_string(),
    )
    .await;
    let r = send(
        &app,
        "PUT",
        &format!("/people/data?graph={}", enc("urn:shapes")),
        "text/turtle",
        &shapes,
    )
    .await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let s = json!({ "shapes": "graphs", "graphs": ["urn:shapes"], "saveTo": "branch", "branchTemplate": "forms.{user}" });
    let r = send(
        &app,
        "PATCH",
        "/$/settings/people/forms",
        "application/json",
        &s.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let m = model(&app, "").await;
    assert_eq!(m["shapes"]["source"], "graphs");
    assert!(
        m["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["label"] == "Full name"),
        "{m}"
    );
    assert_eq!(m["save"]["to"], "branch");
    assert!(m["save"]["branch"].as_str().unwrap().starts_with("forms."));
    // shapes= overrides the setting
    let m = model(&app, "&shapes=drafted").await;
    assert_eq!(m["shapes"]["source"], "drafted");
    // a bad setting is refused
    let r = send(
        &app,
        "PATCH",
        "/$/settings/people/forms",
        "application/json",
        r#"{"targetGraph":"nope"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let file: J = serde_json::from_slice(
        &std::fs::read(
            st.get("people")
                .unwrap()
                .store
                .root()
                .unwrap()
                .join("forms.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(file["format"], 1);
    assert_eq!(file["shapes"], "graphs");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_instance_gets_a_minted_iri() {
    let (_d, st, app) = server().await;
    let get = |q: String| {
        let app = app.clone();
        async move { send(&app, "GET", &format!("/$/forms/people?{q}"), "", "").await }
    };
    let r = get(format!("class={}", enc(&format!("{EX}Person")))).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let m = r.json();
    assert_eq!(m["focus"], J::Null);
    assert_eq!(m["types"], json!([format!("{EX}Person")]));
    assert_eq!(field(&m, "Name")["values"], json!([]));
    // focus and class together, or neither, are refused
    let r = get(format!("class=x&focus={}", enc(&format!("{EX}alice")))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(String::new()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let id = |p: &str| format!("<{EX}{p}>");
    let body = json!({
        "class": format!("{EX}Person"),
        "changes": [
            { "field": id("name"), "add": [lit("Carol")] },
            { "field": id("worksFor"), "add": [{ "type": "uri", "value": format!("{EX}acme") }] },
        ],
        "expect": { id("name"): [], id("worksFor"): [] },
        "message": "add Carol",
    });
    let r = send(
        &app,
        "POST",
        "/$/forms/people/check",
        "application/json",
        &body.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["validation"]["conforms"], true);
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &body.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let focus = r.json()["focus"].as_str().unwrap().to_string();
    assert!(focus.starts_with("urn:uuid:"), "{focus}");
    let ds = st.get("people").unwrap();
    assert!(
        ds.dataset
            .ask(&format!(
                "ASK {{ <{focus}> a <{EX}Person> ; <{EX}name> \"Carol\" }}"
            ))
            .unwrap()
    );
    // the guard rejects a new instance that misses a required field
    let bad = json!({
        "class": format!("{EX}Person"),
        "changes": [{ "field": id("name"), "add": [lit("Dan")] }],
        "expect": { id("name"): [] },
    });
    let r = send(
        &app,
        "POST",
        "/$/forms/people/save",
        "application/json",
        &bad.to_string(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}
