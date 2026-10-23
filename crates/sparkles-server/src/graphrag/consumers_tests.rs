//! The consumers of Phase 2 against stub providers (spec C22 §2.4, §2.5, §7, A6, A7
//! and A9): `community_reports`, global answering in Ask, and the communities of
//! `recall` and the brief. No request leaves the loopback interface.

use super::build::Mode;
use super::build::tests::{Fixture, answer, data, settings};
use crate::ask::{self, AskMode, AskOptions};
use crate::auth::Principal;
use crate::mcp::{Call, McpServer, Outcome};
use crate::models::mock::{self, MockModel, Received};
use crate::models::{Models, ModelsConfig};
use crate::state::{AppState, DbType};
use serde_json::{Value, json};
use sparkles::io::{RdfFormat, Source};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The system prompt of a request.
fn system_of(r: &Received) -> String {
    r.body["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// What the stub answers: a report for the build, a scoped or plain draft, points for a
/// map, an answer for a reduce, and a row summary.
fn handler(scope: &'static str) -> impl Fn(&Received, usize) -> (u16, Value) + Send + Sync {
    move |r, _| {
        let sys = system_of(r);
        let body = r.body.to_string();
        let text = if sys.contains("community reports about one dataset") {
            json!({ "points": [
                { "text": "People cluster in two groups that know each other.", "score": 80,
                  "reports": ["R1"], "findings": ["F1.1"] },
                { "text": "Unrelated.", "score": 0, "reports": ["R2"], "findings": [] }
            ] })
            .to_string()
        } else if sys.contains("numbered points") {
            json!({ "text": "The dataset holds two groups of people who know each other [1].", "citations": [1] })
                .to_string()
        } else if sys.contains("translate a person's question") {
            let mut d = json!({
                "query": "SELECT ?s WHERE { ?s ?p ?o } LIMIT 3",
                "explanation": "Lists subjects.",
                "assumptions": [],
                "clarify": { "question": "", "choices": [] },
                "graph": { "subject": "", "predicate": "", "object": "" }
            });
            if body.contains("ScopedDraft") {
                d["scope"] = scope.into();
            }
            d.to_string()
        } else if sys.contains("from the rows of a query result") {
            json!({ "text": "Three subjects [1].", "citations": [1] }).to_string()
        } else {
            answer(r)
        };
        (200, mock::openai(&text))
    }
}

/// A dataset of two clusters whose reports a build wrote, with the draft, summarize
/// and community roles on one stub provider.
fn built(scope: &'static str) -> Fixture {
    let mock = MockModel::start(handler(scope));
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    let pair = json!({ "provider": "p", "model": "m" });
    let cfg = json!({
        "providers": { "p": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 5,
                              "allowedModels": ["m"], "structuredOutput": "json-schema" } },
        "roles": { "community": [pair], "draft": [pair], "summarize": [pair] }
    });
    st.models = arc_swap::ArcSwapOption::from_pointee(Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    ));
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data(3).into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = crate::http::router(st.clone());
    Fixture { st, app, mock }
}

async fn build(f: Fixture) -> Fixture {
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        f.run(settings(json!({})), Mode::Build).unwrap();
        f
    })
    .await
    .unwrap()
}

fn server(f: &Fixture) -> McpServer {
    McpServer::new(f.st.clone(), crate::mcp::McpConfig::default())
}

fn tool(f: &Fixture, name: &str, args: Value) -> Value {
    let call = Call {
        arrived: Instant::now(),
        cancel: Default::default(),
        request_id: "t".into(),
        principal: Principal::local(),
        headers: None,
        held: None,
        grant: None,
    };
    let Value::Object(mut m) = args else {
        panic!("arguments are an object")
    };
    m.insert("dataset".into(), "org".into());
    match server(f).run_now(name, m, &call).unwrap() {
        Outcome::Structured(v) => v,
        Outcome::Text(t) => Value::String(t),
    }
}

fn ask_with(f: &Fixture, mode: AskMode, reviewed_only: bool) -> (Value, Vec<String>) {
    let models = f.st.models().unwrap();
    let o = AskOptions {
        dataset: "org".into(),
        question: "What are the main groups in this dataset?".into(),
        mode,
        reviewed_only,
        ..Default::default()
    };
    let mut events = Vec::new();
    let mut on = |e: &str, _: &Value| events.push(e.to_string());
    let out = ask::ask(&server(f), &models, &Principal::local(), &o, &mut on);
    (out, events)
}

/// `community_reports` lists the reports of the chosen level with their findings and
/// citations, best rated first without a search index.
#[tokio::test(flavor = "multi_thread")]
async fn community_reports_lists_reports_with_findings() {
    let f = build(built("local")).await;
    tokio::task::spawn_blocking(move || {
        let v = tool(&f, "community_reports", json!({ "query": "people" }));
        assert_eq!(v["available"], true, "{v}");
        assert_eq!(v["ranking"], "rating");
        let reports = v["reports"].as_array().unwrap();
        assert_eq!(reports.len(), 2, "{v}");
        let f0 = &reports[0]["findings"][0];
        assert!(f0["text"].as_str().is_some_and(|t| !t.is_empty()), "{v}");
        assert!(
            f0["cites"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c.as_str().unwrap().starts_with("<<("))),
            "{v}"
        );
        // A7: no report was reviewed
        let r = tool(&f, "community_reports", json!({ "reviewedOnly": true }));
        assert_eq!(r["available"], false, "{r}");
        assert_eq!(r["reason"], "no-reports");
    })
    .await
    .unwrap();
}

/// A6: a question the draft classifies as global is answered from the reports, and
/// the answer cites a point whose report and finding cite facts.
#[tokio::test(flavor = "multi_thread")]
async fn a_global_question_is_answered_from_reports() {
    let f = build(built("global")).await;
    tokio::task::spawn_blocking(move || {
        let before = f.mock.requests().len();
        let (out, events) = ask_with(&f, AskMode::Auto, false);
        assert_eq!(out["outcome"], "answered", "{out}");
        assert_eq!(out["mode"], "global", "{out}");
        assert_eq!(out["summary"]["citations"], json!([1]), "{out}");
        let rep = &out["reports"][0];
        assert!(rep["iri"].as_str().is_some(), "{out}");
        assert!(
            rep["findings"][0]["cites"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "{out}"
        );
        assert_eq!(out["points"].as_array().unwrap().len(), 1, "{out}");
        assert!(events.contains(&"reports".to_string()), "{events:?}");
        // draft, one map and one reduce, and no query ran
        let sent = &f.mock.requests()[before..];
        assert_eq!(sent.len(), 3, "{}", sent.len());
        assert!(out["result"].is_null(), "{out}");
    })
    .await
    .unwrap();
}

/// The global mode skips the draft, and the local mode never offers the scope.
#[tokio::test(flavor = "multi_thread")]
async fn the_mode_switch_chooses_the_path() {
    let f = build(built("global")).await;
    tokio::task::spawn_blocking(move || {
        let before = f.mock.requests().len();
        let (out, _) = ask_with(&f, AskMode::Global, false);
        assert_eq!(out["mode"], "global", "{out}");
        let sent = &f.mock.requests()[before..];
        assert!(
            sent.iter()
                .all(|r| !system_of(r).contains("translate a person's question")),
            "the global mode drafted"
        );
        let before = f.mock.requests().len();
        let (out, _) = ask_with(&f, AskMode::Local, false);
        assert_eq!(out["mode"], "local", "{out}");
        let sent = &f.mock.requests()[before..];
        assert!(
            sent.iter()
                .all(|r| !r.body.to_string().contains("ScopedDraft"))
        );
    })
    .await
    .unwrap();
}

/// A7: with reviewedOnly and no reviewed report, a global question is answered from the
/// data, and the answer says why.
#[tokio::test(flavor = "multi_thread")]
async fn reviewed_only_without_reviewed_reports_answers_locally() {
    let f = build(built("global")).await;
    tokio::task::spawn_blocking(move || {
        let (out, _) = ask_with(&f, AskMode::Global, true);
        assert_eq!(out["mode"], "local", "{out}");
        let notes = out["notes"].to_string();
        assert!(notes.contains("no reviewed community report"), "{notes}");
    })
    .await
    .unwrap();
}

/// Without a build there is nothing to answer from, so the automatic mode drafts as
/// before, without the scope.
#[tokio::test(flavor = "multi_thread")]
async fn without_reports_the_draft_has_no_scope() {
    let f = built("global");
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let (out, _) = ask_with(&f, AskMode::Auto, false);
        assert_eq!(out["mode"], "local", "{out}");
        assert!(
            f.mock
                .requests()
                .iter()
                .all(|r| !r.body.to_string().contains("ScopedDraft"))
        );
    })
    .await
    .unwrap();
}

/// A9: `recall` with `communities` adds the report of each seed's community after the
/// facts, in both formats.
#[tokio::test(flavor = "multi_thread")]
async fn recall_adds_the_seeds_communities() {
    let f = build(built("local")).await;
    tokio::task::spawn_blocking(move || {
        let v = tool(
            &f,
            "recall",
            json!({ "seeds": ["http://ex/x0"], "hops": 0, "communities": true, "format": "json" }),
        );
        let v: Value = match v {
            Value::String(s) => serde_json::from_str(&s).unwrap(),
            v => v,
        };
        let c = &v["communities"];
        assert!(c["level"].is_u64(), "{v}");
        assert_eq!(c["reports"][0]["seeds"], json!([1]), "{v}");
        assert_eq!(c["reports"][0]["title"], "A community", "{v}");
        let t = tool(
            &f,
            "recall",
            json!({ "seeds": ["http://ex/x0"], "hops": 0, "communities": true }),
        );
        let t = t.as_str().unwrap();
        assert!(t.contains("# communities level="), "{t}");
        assert!(t.contains("summary \"People who know each other.\""), "{t}");
        // without the argument nothing changes
        let plain = tool(
            &f,
            "recall",
            json!({ "seeds": ["http://ex/x0"], "hops": 0 }),
        );
        assert!(!plain.as_str().unwrap().contains("# communities"));
    })
    .await
    .unwrap();
}

/// The project brief adds the reports of the coarsest level, reviewed ones only unless
/// the brief includes unreviewed facts.
#[tokio::test(flavor = "multi_thread")]
async fn the_project_brief_adds_the_coarsest_reports() {
    let f = build(built("local")).await;
    assert_eq!(
        super::build::tests::patch(
            &f.app,
            "/$/settings/org/memory",
            json!({ "agentGraphs": ["urn:x-sparkles:import/*"], "imports": { "base": "urn:x-sparkles:import/" } })
        )
        .await,
        axum::http::StatusCode::OK
    );
    tokio::task::spawn_blocking(move || {
        let v = tool(
            &f,
            "memory_brief",
            json!({ "scope": "project", "projectKey": "p1", "includeUnreviewed": true }),
        );
        let text = v["text"].as_str().unwrap();
        assert!(text.contains("# communities level=0"), "{text}");
        assert_eq!(v["communities"].as_array().map(Vec::len), Some(2), "{v}");
        let reviewed = tool(
            &f,
            "memory_brief",
            json!({ "scope": "project", "projectKey": "p1" }),
        );
        assert!(reviewed.get("communities").is_none(), "{reviewed}");
    })
    .await
    .unwrap();
}

/// §7.1 and §7.3: a question that names an entity linked exactly stays local, and its
/// draft sees the title and summary of that entity's community.
#[tokio::test(flavor = "multi_thread")]
async fn a_named_entity_is_local_with_its_community() {
    let f = build(built("global")).await;
    tokio::task::spawn_blocking(move || {
        let models = f.st.models().unwrap();
        let o = AskOptions {
            dataset: "org".into(),
            question: "Whom does \"x person 1\" know?".into(),
            ..Default::default()
        };
        let before = f.mock.requests().len();
        let mut on = |_: &str, _: &Value| {};
        let out = ask::ask(&server(&f), &models, &Principal::local(), &o, &mut on);
        assert_eq!(out["mode"], "local", "{out}");
        let draft = f.mock.requests()[before..]
            .iter()
            .find(|r| system_of(r).contains("translate a person's question"))
            .cloned()
            .expect("a draft");
        let body = draft.body.to_string();
        assert!(
            !body.contains("ScopedDraft"),
            "an exact entity offered the scope"
        );
        assert!(
            mock::prompt_of(&draft).contains("Communities of the entities the question names"),
            "{}",
            mock::prompt_of(&draft)
        );
        assert!(mock::prompt_of(&draft).contains("People who know each other."));
    })
    .await
    .unwrap();
}
