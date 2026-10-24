//! Editing forms under grants (spec U02 §1 goal 5, A6): a field whose predicate a
//! protection hides from reads is absent, one it protects from writes is read-only, a
//! reader gets a form it cannot save, and a caller limited to some graphs gets checks
//! without the validation results.

use super::*;
use crate::http::forms_tests::{DATA, SHAPES};

const EX: &str = "http://example.org/";

/// `fadmin` sees everything. `fwriter` writes under the protection of `ex:email`,
/// `fhalf` also reads emails, `freader` only reads, and `fgraph` reads and writes the
/// default graph alone.
fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[protections]]
name = "emails"
dataset = "ppl"
predicates = ["http://example.org/email"]

[[users]]
name = "fadmin"
password = "{fadmin}"
datasets = {{ ppl = "admin" }}

[[users]]
name = "fwriter"
password = "{fwriter}"
datasets = {{ ppl = "write" }}

[[users]]
name = "fhalf"
password = "{fhalf}"
datasets = {{ ppl = "write" }}
[[users.grants]]
dataset = "ppl"
level = "read"
lifts = ["emails"]

[[users]]
name = "freader"
password = "{freader}"
datasets = {{ ppl = "read" }}

[[users]]
name = "fgraph"
password = "{fgraph}"
[[users.grants]]
dataset = "ppl"
level = "write"
graphs = ["urn:x-arq:DefaultGraph"]
lifts = ["emails"]
"#,
        fadmin = h("fadmin-pw"),
        fwriter = h("fwriter-pw"),
        fhalf = h("fhalf-pw"),
        freader = h("freader-pw"),
        fgraph = h("fgraph-pw"),
    )
}

async fn server() -> AuthServer {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    s.state.attach("ppl", DbType::Persistent, None).unwrap();
    let admin = b("fadmin");
    let r = call(
        &s.app,
        "POST",
        "/ppl/update",
        &[
            ("authorization", &admin),
            ("content-type", "application/sparql-update"),
        ],
        DATA,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cfg = json!({ "mode": "warn", "shapes": { "inline": SHAPES } }).to_string();
    let r = call(
        &s.app,
        "PUT",
        "/$/validation/ppl",
        &[
            ("authorization", &admin),
            ("content-type", "application/json"),
        ],
        &cfg,
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    s
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn model(app: &Router, user: &str) -> J {
    let r = call(
        app,
        "GET",
        &format!("/$/forms/ppl?focus={}", enc(&format!("{EX}alice"))),
        &[("authorization", &b(user))],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{user}: {}", r.text());
    r.json()
}

fn labels(m: &J) -> Vec<String> {
    m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["label"].as_str().unwrap().to_string())
        .collect()
}

fn field<'a>(m: &'a J, label: &str) -> &'a J {
    m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["label"] == label)
        .unwrap_or_else(|| panic!("no field {label}: {m}"))
}

async fn post(app: &Router, user: &str, route: &str, body: &J) -> R {
    call(
        app,
        "POST",
        &format!("/$/forms/ppl/{route}"),
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

fn name_change() -> J {
    let id = format!("<{EX}name>");
    let old = json!({ "type": "literal", "value": "Alice Lima" });
    json!({
        "focus": format!("{EX}alice"),
        "changes": [{ "field": id, "remove": [old], "add": [{ "type": "literal", "value": "Alice L." }] }],
        "expect": { id: [old] },
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a6_protected_fields_are_absent_or_read_only() {
    let s = server().await;
    assert_eq!(
        labels(&model(&s.app, "fadmin").await),
        ["Name", "Email", "Works for", "Role"]
    );
    // the protection hides emails from fwriter: no field, and no other property either
    let m = model(&s.app, "fwriter").await;
    assert_eq!(labels(&m), ["Name", "Works for", "Role"]);
    assert!(!m.to_string().contains("alice@example.org"), "{m}");
    assert_eq!(m["editable"], true);
    // fwriter can still save the name
    let r = post(&s.app, "fwriter", "save", &name_change()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // and cannot name the hidden field
    let id = format!("<{EX}email>");
    let c = json!({
        "focus": format!("{EX}alice"),
        "changes": [{ "field": &id, "add": [{ "type": "literal", "value": "x@example.org" }] }],
        "expect": { &id: [] },
    });
    let r = post(&s.app, "fwriter", "save", &c).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // fhalf reads emails but may not write them
    let m = model(&s.app, "fhalf").await;
    let email = field(&m, "Email");
    assert_eq!(email["editable"], false);
    assert_eq!(email["reason"], "protected");
    assert_eq!(email["values"][0]["term"]["value"], "alice@example.org");
    let c = json!({
        "focus": format!("{EX}alice"),
        "changes": [{ "field": id, "remove": [{ "type": "literal", "value": "alice@example.org" }] }],
        "expect": { id: [{ "type": "literal", "value": "alice@example.org" }] },
    });
    let r = post(&s.app, "fhalf", "save", &c).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn readers_cannot_save_and_limited_callers_get_no_validation() {
    let s = server().await;
    let m = model(&s.app, "freader").await;
    assert_eq!(m["editable"], false);
    assert!(
        m["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["editable"] == false)
    );
    let r = post(&s.app, "freader", "save", &name_change()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = post(&s.app, "freader", "check", &name_change()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // a caller limited to the default graph: the check previews, without results
    let r = post(&s.app, "fgraph", "check", &name_change()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["validation"]["withheld"], true);
    assert_eq!(j["validation"]["results"], J::Null);
    assert!(j["preview"].get("validation").is_none(), "{j}");
    assert_eq!(j["preview"]["outcome"], "commit");
    let r = post(&s.app, "fgraph", "save", &name_change()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["commit"].get("quads").is_none());
}
