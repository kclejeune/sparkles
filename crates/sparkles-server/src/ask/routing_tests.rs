//! Escalation along the role lists (C18 §5.5) against three mock providers, `cheap`,
//! `mid` and `top`, each of which logs the requests it receives (A13, A16, A35 to A40).

use super::tests::{ORG, PREFIXES, draft, is_summary, opts, server};
use super::*;
use crate::models::ModelsConfig;
use crate::models::mock::{self, MockModel};
use parking_lot::Mutex;
use std::collections::VecDeque;

/// A mock that answers drafts and repairs from `drafts` in order (repeating the last)
/// and summaries with a one-row summary.
fn scripted(drafts: Vec<String>) -> MockModel {
    let drafts = Mutex::new(drafts.into_iter().collect::<VecDeque<_>>());
    MockModel::start(move |r, _| {
        if is_summary(r) {
            return (
                200,
                mock::openai(&json!({ "text": "One row [1].", "citations": [1] }).to_string()),
            );
        }
        let mut d = drafts.lock();
        let text = if d.len() > 1 {
            d.pop_front().unwrap()
        } else {
            d.front().cloned().unwrap_or_default()
        };
        (200, mock::openai(&text))
    })
}

/// Models with the providers `cheap`, `mid` and `top` at the mocks' URLs and `roles`.
fn three(urls: [&str; 3], roles: Value, extra: Value) -> Models {
    let mut providers = Map::new();
    for (name, url) in ["cheap", "mid", "top"].into_iter().zip(urls) {
        let mut p = json!({ "kind": "openai", "endpoint": url, "requestTimeoutSecs": 2 });
        if let Some(x) = extra.get(name) {
            for (k, v) in x.as_object().unwrap() {
                p[k] = v.clone();
            }
        }
        providers.insert(name.into(), p);
    }
    let cfg = json!({ "providers": providers, "roles": roles });
    Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )
}

fn pair(p: &str) -> Value {
    json!({ "provider": p, "model": "m" })
}

/// Run an ask, collecting the events with their data.
fn run_events(s: &McpServer, m: &Models, o: &AskOptions) -> (Value, Vec<(String, Value)>) {
    let mut events = Vec::new();
    let mut on = |e: &str, v: &Value| events.push((e.to_string(), v.clone()));
    let out = ask(s, m, &Principal::local(), o, &mut on);
    (out, events)
}

fn step_pairs(out: &Value) -> Vec<(String, String)> {
    out["usage"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["role"].as_str().unwrap().to_string(),
                s["provider"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn escalations(events: &[(String, Value)]) -> Vec<Value> {
    events
        .iter()
        .filter(|(e, _)| e == "escalate")
        .map(|(_, v)| v.clone())
        .collect()
}

const EASY: &str = "SELECT ?t WHERE { ?t a ex:Team }";

/// A35: an easy question is drafted and summarized by `cheap` alone.
#[test]
fn a35_cheap_answers_easy_questions() {
    let s = server(ORG);
    let (c, mi, t) = (
        scripted(vec![draft(EASY)]),
        scripted(vec![]),
        scripted(vec![]),
    );
    let m = three(
        [&c.url(), &mi.url(), &t.url()],
        json!({ "draft": [pair("cheap"), pair("mid"), pair("top")], "summarize": [pair("cheap")] }),
        json!({}),
    );
    let (out, events) = run_events(&s, &m, &opts("Which teams are there?"));
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(
        step_pairs(&out),
        [
            ("draft".into(), "cheap".into()),
            ("summarize".into(), "cheap".into())
        ]
    );
    assert!(mi.requests().is_empty() && t.requests().is_empty());
    assert!(escalations(&events).is_empty());
    assert_eq!(out["usage"]["answeredBy"]["provider"], "cheap");
    assert_eq!(out["usage"]["tryHarder"], true);
}

/// A36 (and A13): `mid` repairs a draft with an unknown class; when its repair still has
/// the error, `check-failed` moves the repair role to `top`, and `mid` repairs no more.
#[test]
fn a36_check_failed_escalates_the_repair() {
    let s = server(ORG);
    let bad = draft("SELECT ?o WHERE { ?o a ex:Organisation }");
    let good = draft("SELECT ?o WHERE { ?o a ex:Organization }");
    let (c, mi, t) = (
        scripted(vec![bad.clone()]),
        scripted(vec![bad]),
        scripted(vec![good]),
    );
    let m = three(
        [&c.url(), &mi.url(), &t.url()],
        json!({ "draft": [pair("cheap")], "repair": [pair("mid"), pair("top")] }),
        json!({}),
    );
    let o = AskOptions {
        summary: false,
        ..opts("Which organizations are there?")
    };
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(out["result"]["attempt"], 3);
    // A13: the repair request names the issue and the suggested class
    let repair = mock::prompt_of(&mi.requests()[0]);
    assert!(
        repair.contains("unknown-class") && repair.contains("ex:Organization"),
        "{repair}"
    );
    let e = escalations(&events);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0]["signal"], "check-failed");
    assert_eq!(e[0]["role"], "repair");
    assert_eq!(e[0]["from"]["provider"], "mid");
    assert_eq!(e[0]["to"]["provider"], "top");
    assert_eq!(mi.requests().len(), 1);
    assert_eq!(
        step_pairs(&out),
        [
            ("draft".into(), "cheap".into()),
            ("repair".into(), "mid".into()),
            ("repair".into(), "top".into())
        ]
    );
    assert_eq!(out["usage"]["steps"][2]["signal"], "check-failed");
    assert_eq!(out["usage"]["answeredBy"]["role"], "repair");
    assert_eq!(out["usage"]["answeredBy"]["provider"], "top");
}

/// A37: a repaired query that is empty with the verdict `query` moves the repair role on;
/// an empty join with the verdict `data` stops at once with no further model call.
#[test]
fn a37_empty_query_escalates_and_data_stops() {
    let s = server(ORG);
    let untagged = draft(r#"SELECT ?p WHERE { ?p foaf:name "Ana Lima" }"#);
    let tagged = draft(r#"SELECT ?p WHERE { ?p foaf:name "Ana Lima"@en }"#);
    let (c, mi, t) = (
        scripted(vec![untagged.clone()]),
        scripted(vec![untagged]),
        scripted(vec![tagged]),
    );
    let m = three(
        [&c.url(), &mi.url(), &t.url()],
        json!({ "draft": [pair("cheap")], "repair": [pair("mid"), pair("top")] }),
        json!({}),
    );
    let o = AskOptions {
        summary: false,
        ..opts("Who is Ana Lima?")
    };
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    let e = escalations(&events);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0]["signal"], "empty-query");
    assert_eq!(out["attempts"][0]["verdict"], "query");
    assert_eq!(t.requests().len(), 1);
    // the data holds no match: one call, and the empty result
    let c = scripted(vec![draft(&format!(
        "{PREFIXES}SELECT ?p WHERE {{ ?p ex:memberOf res:payments . ?p foaf:name \"Bo Chen\"@en }}"
    ))]);
    let m = three(
        [&c.url(), &mi.url(), &t.url()],
        json!({ "draft": [pair("cheap")], "repair": [pair("mid"), pair("top")] }),
        json!({}),
    );
    let before = (mi.requests().len(), t.requests().len());
    let (out, _) = run_events(&s, &m, &opts("Is Bo Chen on the payments team?"));
    assert_eq!(out["outcome"], "empty", "{out:#}");
    assert_eq!(out["result"]["verdict"], "data");
    assert_eq!(c.requests().len(), 1, "no summary and no repair");
    assert_eq!((mi.requests().len(), t.requests().len()), before);
}

/// A38: a timeout, a refusal or invalid output twice runs the same step on the next
/// pair; when every pair fails the ask ends with `provider-unavailable` after at most
/// two failed calls.
#[test]
fn a38_provider_failures_move_the_step() {
    let s = server(ORG);
    let slow = MockModel::start(|_, _| {
        std::thread::sleep(Duration::from_secs(4));
        (200, mock::openai(""))
    });
    let mid = scripted(vec![draft(EASY)]);
    let idle = scripted(vec![]);
    let m = three(
        [&slow.url(), &mid.url(), &idle.url()],
        json!({ "draft": [pair("cheap"), pair("mid"), pair("top")] }),
        json!({ "cheap": { "requestTimeoutSecs": 1 } }),
    );
    let o = AskOptions {
        summary: false,
        ..opts("Which teams are there?")
    };
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    let e = escalations(&events);
    assert_eq!(e[0]["signal"], "provider-failure", "{e:?}");
    assert_eq!(out["usage"]["answeredBy"]["provider"], "mid");
    // an anthropic refusal
    let refusing = MockModel::start(|_, _| {
        (
            200,
            json!({ "content": [], "stop_reason": "refusal", "usage": { "input_tokens": 3, "output_tokens": 0 } }),
        )
    });
    let m = three(
        [&refusing.url(), &mid.url(), &idle.url()],
        json!({ "draft": [pair("cheap"), pair("mid")] }),
        json!({ "cheap": { "kind": "anthropic" } }),
    );
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(escalations(&events)[0]["signal"], "provider-failure");
    assert_eq!(out["usage"]["steps"][0]["outcome"], "refusal");
    // invalid JSON twice at the schema level
    let invalid = MockModel::start(|_, _| (200, mock::openai("{\"query\": 1}")));
    let m = three(
        [&invalid.url(), &mid.url(), &idle.url()],
        json!({ "draft": [pair("cheap"), pair("mid")] }),
        json!({ "cheap": { "structuredOutput": "json-schema" } }),
    );
    let (out, _) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(invalid.requests().len(), 2);
    assert_eq!(out["usage"]["steps"][0]["outcome"], "invalid-output");
    // every pair fails: two failed calls, then provider-unavailable
    let down = MockModel::start(|_, _| (400, json!({ "error": { "message": "bad model" } })));
    let m = three(
        [&down.url(), &down.url(), &down.url()],
        json!({ "draft": [pair("cheap"), pair("mid"), pair("top")] }),
        json!({}),
    );
    let (out, _) = run_events(&s, &m, &o);
    assert_eq!(out["error"]["code"], "provider-unavailable", "{out:#}");
    assert_eq!(out["usage"]["failedCalls"], 2);
    assert_eq!(out["usage"]["modelCalls"], 2);
}

/// A39: a complex draft is repaired by the second pair of `repair`, a simple one by the
/// first, and a follow-up to a complex query is drafted by `mid`.
#[test]
fn a39_complexity_routes() {
    let s = server(ORG);
    let complex = "SELECT ?t (COUNT(?p) AS ?n) (MAX(?x) AS ?m) WHERE { { SELECT ?t WHERE { ?t a ex:Teem } } ?p ex:memberOf ?t . OPTIONAL { ?p foaf:name ?x } MINUS { ?p a ex:Robot } } GROUP BY ?t";
    let parsed = s.state.datasets()["org"].clone();
    let pv: Vec<(String, String)> = crate::mcp::tools::dataset_prefixes(&parsed)
        .into_iter()
        .collect();
    let score = complexity::of(&sparkles::sparql::parse_query(complex, None, &pv).unwrap()).score();
    assert!(score >= 8, "{score}");
    let fixed = draft(EASY);
    let (c, mi, t) = (
        scripted(vec![draft(complex)]),
        scripted(vec![fixed.clone()]),
        scripted(vec![fixed.clone()]),
    );
    let roles = json!({ "draft": [pair("cheap"), pair("mid"), pair("top")], "repair": [pair("mid"), pair("top")] });
    let m = three([&c.url(), &mi.url(), &t.url()], roles.clone(), json!({}));
    let o = AskOptions {
        summary: false,
        ..opts("How many people are in each team?")
    };
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(escalations(&events)[0]["signal"], "complexity");
    assert!(mi.requests().is_empty());
    assert_eq!(t.requests().len(), 1);
    // a simple draft's repair comes from the first pair
    let c = scripted(vec![draft("SELECT ?o WHERE { ?o a ex:Organisation }")]);
    let m = three([&c.url(), &mi.url(), &t.url()], roles.clone(), json!({}));
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert!(escalations(&events).is_empty());
    assert_eq!(mi.requests().len(), 1);
    // a follow-up to a complex query
    let c = scripted(vec![draft(EASY)]);
    let mi2 = scripted(vec![draft(EASY)]);
    let m = three([&c.url(), &mi2.url(), &t.url()], roles, json!({}));
    let o = AskOptions {
        context: vec![Turn {
            question: "How many people are in each team?".into(),
            query: complex.into(),
        }],
        ..o
    };
    let (out, _) = run_events(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert!(c.requests().is_empty());
    assert_eq!(out["usage"]["answeredBy"]["provider"], "mid");
    // the earlier turn reaches the prompt as data
    let p = mock::prompt_of(&mi2.requests()[0]);
    assert!(
        p.contains("Earlier question") && p.contains("ex:Teem"),
        "{p}"
    );
}

/// A40 in the library: Try harder starts the draft at the next pair, and the last pair
/// offers no more.
#[test]
fn a40_try_harder() {
    let s = server(ORG);
    let (c, mi, t) = (
        scripted(vec![draft(EASY)]),
        scripted(vec![draft(EASY)]),
        scripted(vec![draft(EASY)]),
    );
    let m = three(
        [&c.url(), &mi.url(), &t.url()],
        json!({ "draft": [pair("cheap"), pair("mid"), pair("top")] }),
        json!({}),
    );
    let o = AskOptions {
        summary: false,
        draft_start: Some(1),
        ..opts("Which teams are there?")
    };
    let (out, events) = run_events(&s, &m, &o);
    assert_eq!(out["usage"]["answeredBy"]["provider"], "mid", "{out:#}");
    assert_eq!(escalations(&events)[0]["signal"], "try-harder");
    assert_eq!(out["usage"]["tryHarder"], true);
    assert!(c.requests().is_empty());
    let o = AskOptions {
        draft_start: Some(2),
        ..o
    };
    let (out, _) = run_events(&s, &m, &o);
    assert_eq!(out["usage"]["answeredBy"]["provider"], "top");
    assert_eq!(out["usage"]["tryHarder"], false);
    let o = AskOptions {
        draft_start: Some(3),
        ..o
    };
    let (out, _) = run_events(&s, &m, &o);
    assert_eq!(out["error"]["code"], "no-later-pair");
}

/// A16: a principal whose grants show one graph asks about another. The provider's
/// requests hold no term of the hidden graph, and the empty result's diagnosis says
/// the constant does not occur in the view.
#[test]
fn a16_hidden_graphs_stay_hidden() {
    use crate::auth::{Access, Grants, Kind, Level as L, Restricted, Scheme};
    let s = server(ORG);
    let ds = s.state.datasets()["org"].clone();
    let hidden = "@prefix ex: <http://example.org/ontology#> . @prefix res: <http://example.org/resource/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\nres:secret a ex:Project ; rdfs:label \"Project Nightjar\"@en ; ex:salaryBand \"B7\" ; ex:memberOf res:payments .";
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            hidden.as_bytes().to_vec(),
            sparkles::io::RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new("https://example.org/hr").unwrap()),
        )])
        .unwrap();
    let visible = "@prefix ex: <http://example.org/ontology#> . @prefix res: <http://example.org/resource/> .\nres:ana ex:memberOf res:payments .";
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            visible.as_bytes().to_vec(),
            sparkles::io::RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new("https://example.org/public").unwrap()),
        )])
        .unwrap();
    let access = Access::of(Grants {
        restricted: vec![Restricted {
            dataset: "org".into(),
            level: L::Read,
            graphs: Some(vec!["https://example.org/public".into()]),
            endpoints: None,
            lifts: Vec::new(),
            branches: None,
        }],
        ..Default::default()
    });
    let p = Principal::new(Kind::User, "ana", Scheme::Basic, access);
    let c = scripted(vec![draft(
        "SELECT ?t WHERE { <http://example.org/resource/secret> <http://example.org/ontology#memberOf> ?t }",
    )]);
    let m = three(
        [&c.url(), &c.url(), &c.url()],
        json!({ "draft": [pair("cheap")] }),
        json!({}),
    );
    let o = AskOptions {
        summary: false,
        ..opts("Which team is Project Nightjar part of?")
    };
    let mut on = |_: &str, _: &Value| {};
    let out = ask(&s, &m, &p, &o, &mut on);
    assert_eq!(out["outcome"], "empty", "{out:#}");
    let d = out["result"]["diagnosis"].as_str().unwrap();
    assert!(d.contains("does not occur"), "{d}");
    assert_eq!(out["result"]["verdict"], "data");
    // one call: the verdict `data` stops the repairs
    let reqs = c.requests();
    assert_eq!(reqs.len(), 1);
    let body = reqs[0].body.to_string();
    assert!(
        !body.contains("salaryBand")
            && !body.contains("res:secret")
            && !body.contains("ex:Project"),
        "a hidden term reached the provider: {body}"
    );
}
