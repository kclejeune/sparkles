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
    let pair = json!({ "provider": "p", "model": "m" });
    built_with(handler(scope), json!([pair]))
}

/// [`built`] with its own stub and summarize list. The provider allows the models `m`
/// and `m2`.
fn built_with(
    handler: impl Fn(&Received, usize) -> (u16, Value) + Send + Sync + 'static,
    summarize: Value,
) -> Fixture {
    let mock = MockModel::start(handler);
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    let pair = json!({ "provider": "p", "model": "m" });
    let cfg = json!({
        "providers": { "p": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 5,
                              "allowedModels": ["m", "m2"], "structuredOutput": "json-schema" } },
        "roles": { "community": [pair], "draft": [pair], "summarize": summarize }
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

/// Round 1, finding 8: an index over unrelated text that sorts first does not hide the
/// index over report summaries, so a search still ranks the reports.
#[tokio::test(flavor = "multi_thread")]
async fn report_search_uses_the_index_over_summaries() {
    use sparkles::vector::embed::Environment;
    use sparkles::vector::embed::mock::MockProvider;
    let f = build(built("local")).await;
    let emb = MockProvider::start(8);
    tokio::task::spawn_blocking(move || {
        let ds = f.st.get("org").unwrap();
        let v = ds.dataset.indexes().vector();
        v.set_embedding_environment(Some(Environment {
            outbound: sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
            ..Default::default()
        }));
        for (name, pred, over) in [
            ("a-chunks", "urn:x-test:chunks", "urn:x-test:body"),
            (
                "reports",
                "urn:x-test:summaries",
                super::guard::gr("summary").as_str(),
            ),
        ] {
            let cfg: sparkles::vector::VectorIndexConfig = serde_json::from_value(json!({
                "predicate": pred,
                "dimension": 8,
                "embedding": { "url": emb.url(), "model": "mock", "predicates": [over] }
            }))
            .unwrap();
            v.put(name, cfg).unwrap();
        }
        v.embed_until_idle(Duration::from_secs(30)).unwrap();
        let r = tool(
            &f,
            "community_reports",
            json!({ "query": "people who know each other" }),
        );
        assert_eq!(r["available"], true, "{r}");
        assert_eq!(r["ranking"], "search", "{r}");
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
        assert!(c["level"].is_null(), "{v}");
        assert!(c["reports"][0]["level"].is_u64(), "{v}");
        assert_eq!(c["reports"][0]["seeds"], json!([1]), "{v}");
        assert_eq!(c["reports"][0]["title"], "A community", "{v}");
        let t = tool(
            &f,
            "recall",
            json!({ "seeds": ["http://ex/x0"], "hops": 0, "communities": true }),
        );
        let t = t.as_str().unwrap();
        assert!(t.contains("# communities level=finest"), "{t}");
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

/// Round 1, finding 6: without a level each seed takes its finest community, so a seed
/// whose finer community a build folded away keeps its coarser report.
#[tokio::test(flavor = "multi_thread")]
async fn recall_takes_each_seeds_finest_community() {
    use super::guard::gr;
    let f = build(built("local")).await;
    tokio::task::spawn_blocking(move || {
        let ds = f.st.get("org").unwrap();
        let graph = super::settings::GraphragSettings::default().reports_graph;
        // a level-1 community that holds x0 alone, with its own report
        let u = format!(
            "INSERT {{ GRAPH <{graph}> {{ <urn:x-test:c1> a <{community}> ; <{level}> 1 ; <{member}> <http://ex/x0> ; <{count}> 1 ; <{report}> <urn:x-test:r1> . <urn:x-test:r1> ?p ?o ; <{label}> \"A finer community\" }} }} WHERE {{ GRAPH <{graph}> {{ ?c <{member}> <http://ex/x0> ; <{level}> 0 ; <{report}> ?r . ?r ?p ?o FILTER(?p != <{label}>) }} }}",
            community = gr("Community"),
            level = gr("level"),
            member = gr("member"),
            count = gr("memberCount"),
            report = gr("report"),
            label = super::store::RDFS_LABEL,
        );
        sparkles::sparql::update::update(&ds.store, &u, &Default::default()).unwrap();
        let recall = |args: Value| -> Value {
            match tool(&f, "recall", args) {
                Value::String(s) => serde_json::from_str(&s).unwrap(),
                v => v,
            }
        };
        let v = recall(json!({
            "seeds": ["http://ex/x0", "http://ex/x1"], "hops": 0, "communities": true, "format": "json"
        }));
        let reports = v["communities"]["reports"].as_array().unwrap();
        let by_title = |t: &str| reports.iter().find(|r| r["title"] == t).cloned();
        let fine = by_title("A finer community").expect("x0's finer report");
        assert_eq!(fine["level"], 1, "{v}");
        assert_eq!(fine["seeds"], json!([1]), "{v}");
        let coarse = by_title("A community").expect("x1 keeps its coarser report");
        assert_eq!(coarse["level"], 0, "{v}");
        assert_eq!(coarse["seeds"], json!([2]), "{v}");
        // an explicit level reads that level only
        let one = recall(json!({
            "seeds": ["http://ex/x0", "http://ex/x1"], "hops": 0, "communities": true,
            "communityLevel": 1, "format": "json"
        }));
        assert_eq!(one["communities"]["level"], 1, "{one}");
        assert_eq!(one["communities"]["reports"].as_array().map(Vec::len), Some(1), "{one}");
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

/// Round 1, finding 7: the brief's reports count toward `maxChars`, so long reports are
/// dropped until the brief fits.
#[tokio::test(flavor = "multi_thread")]
async fn the_brief_reports_fit_max_chars() {
    let inner = handler("local");
    let long = "People who know each other and many other things besides. ".repeat(5);
    let pair = json!({ "provider": "p", "model": "m" });
    let f = build(built_with(
        move |r, i| {
            let (code, body) = inner(r, i);
            if system_of(r).contains("translate a person's question")
                || system_of(r).contains("community reports about one dataset")
            {
                return (code, body);
            }
            let text = body["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("");
            match serde_json::from_str::<Value>(text) {
                Ok(mut v) if v.get("summary").is_some() => {
                    v["summary"] = long.clone().into();
                    (code, mock::openai(&v.to_string()))
                }
                _ => (code, body),
            }
        },
        json!([pair]),
    ))
    .await;
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
        let wide = tool(
            &f,
            "memory_brief",
            json!({ "scope": "project", "projectKey": "p1", "includeUnreviewed": true }),
        );
        assert_eq!(wide["communities"].as_array().map(Vec::len), Some(2), "{wide}");
        let v = tool(
            &f,
            "memory_brief",
            json!({ "scope": "project", "projectKey": "p1", "includeUnreviewed": true, "maxChars": 500 }),
        );
        let text = v["text"].as_str().unwrap();
        assert!(text.chars().count() <= 500, "{} chars: {text}", text.chars().count());
        let kept = v["communities"].as_array().map_or(0, Vec::len);
        assert!(kept < 2, "{v}");
        assert_eq!(text.matches("\n## ").count(), kept, "{text}");
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
            report_context: true,
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

/// Round 1, finding 1: a draft provider that may receive only the schema never sees
/// report prose, even when the summarize provider may receive rows.
#[tokio::test(flavor = "multi_thread")]
async fn a_schema_only_draft_sees_no_report_prose() {
    use crate::assistant::{AssistantSettings, Send};
    use crate::models::{Pair, Role};
    let f = build(built("global")).await;
    tokio::task::spawn_blocking(move || {
        let models = f.st.models().unwrap();
        let pair = |p: &str| Pair::new(p, "m");
        let mut lists = ask::RoleLists::new();
        lists.insert(Role::Draft, vec![pair("schema")]);
        lists.insert(Role::Summarize, vec![pair("p")]);
        let mut s = AssistantSettings {
            send: Send::Rows,
            ..Default::default()
        };
        s.send_by_provider.insert("schema".into(), Send::Schema);
        let draft = [Role::Draft, Role::Repair];
        assert!(!crate::assistant::rows_allowed(&s, &lists, &[], &draft));
        // a forced rows pair is the draft role's whole list
        let forced = [(Role::Draft, pair("p"))];
        assert!(crate::assistant::rows_allowed(&s, &lists, &forced, &draft));
        lists.insert(Role::Repair, vec![pair("schema")]);
        assert!(!crate::assistant::rows_allowed(&s, &lists, &forced, &draft));
        let o = AskOptions {
            dataset: "org".into(),
            question: "Whom does \"x person 1\" know?".into(),
            report_context: crate::assistant::rows_allowed(&s, &lists, &[], &draft),
            ..Default::default()
        };
        let before = f.mock.requests().len();
        let mut on = |_: &str, _: &Value| {};
        let out = ask::ask(&server(&f), &models, &Principal::local(), &o, &mut on);
        assert_eq!(out["mode"], "local", "{out}");
        for r in &f.mock.requests()[before..] {
            if system_of(r).contains("translate a person's question") {
                let prompt = mock::prompt_of(r);
                assert!(!prompt.contains("Communities of the entities"), "{prompt}");
                assert!(!prompt.contains("People who know each other."), "{prompt}");
            }
        }
    })
    .await
    .unwrap();
}

/// A map call that fails moves the summarize role to its next pair, and the batch runs
/// again there (round 1, finding 3).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_map_moves_to_the_next_pair() {
    let inner = handler("global");
    let f = build(built_with(
        move |r, i| {
            if r.body["model"] == "m"
                && system_of(r).contains("community reports about one dataset")
            {
                return (503, json!({ "error": { "message": "busy" } }));
            }
            inner(r, i)
        },
        json!([{ "provider": "p", "model": "m" }, { "provider": "p", "model": "m2" }]),
    ))
    .await;
    tokio::task::spawn_blocking(move || {
        let (out, events) = ask_with(&f, AskMode::Global, false);
        assert_eq!(out["mode"], "global", "{out}");
        assert_eq!(out["outcome"], "answered", "{out}");
        assert!(events.contains(&"escalate".to_string()), "{events:?}");
        assert_eq!(out["usage"]["answeredBy"]["model"], "m2", "{out}");
    })
    .await
    .unwrap();
}

/// The question's token budget is checked before the reduce (round 1, finding 4).
#[tokio::test(flavor = "multi_thread")]
async fn the_token_budget_stops_before_the_reduce() {
    let f = build(built("global")).await;
    tokio::task::spawn_blocking(move || {
        let models = f.st.models().unwrap();
        let o = AskOptions {
            dataset: "org".into(),
            question: "What are the main groups in this dataset?".into(),
            mode: AskMode::Global,
            // one stub answer uses 15 tokens
            max_tokens: 15,
            ..Default::default()
        };
        let before = f.mock.requests().len();
        let mut on = |_: &str, _: &Value| {};
        let out = ask::ask(&server(&f), &models, &Principal::local(), &o, &mut on);
        assert_eq!(out["error"]["code"], "budget-exceeded", "{out}");
        assert_eq!(f.mock.requests().len() - before, 1, "only the map ran");
    })
    .await
    .unwrap();
}
