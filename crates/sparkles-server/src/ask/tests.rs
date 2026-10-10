//! The asking pipeline against mock providers: grounding, the check, the run with the
//! added limit, repair from the check and from `why_empty`, refusal of updates,
//! clarification, the plain-text level, the trimming of A27, and the demo question set.

use super::*;
use crate::models::ModelsConfig;
use crate::models::mock::{self, MockModel, Received};
use crate::state::{AppState, DbType};
use parking_lot::Mutex;
use sparkles::io::{RdfFormat, Source};
use sparkles::store::StoreOptions;
use std::collections::VecDeque;

const ORG: &str = r#"@prefix ex:   <http://example.org/ontology#> .
@prefix res:  <http://example.org/resource/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Person rdfs:label "Person"@en .
ex:Team rdfs:label "Team"@en .
ex:Organization rdfs:label "Organization"@en .
ex:memberOf rdfs:label "member of"@en .
res:acme a ex:Organization ; rdfs:label "Acme"@en .
res:payments a ex:Team ; rdfs:label "Payments team"@en ; ex:partOf res:acme .
res:platform a ex:Team ; rdfs:label "Platform team"@en ; ex:partOf res:acme .
res:ana a ex:Person ; foaf:name "Ana Lima"@en ; ex:memberOf res:payments .
res:bo a ex:Person ; foaf:name "Bo Chen"@en ; ex:memberOf res:platform .
"#;

const PREFIXES: &str = "PREFIX ex: <http://example.org/ontology#>\nPREFIX res: <http://example.org/resource/>\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\n";

fn server(ttl: &str) -> McpServer {
    let mut st = AppState::standalone(StoreOptions::default(), Duration::from_secs(30));
    st.read_only = true;
    st.limits.max_result_bytes = None;
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    McpServer::new(st, crate::mcp::McpConfig::default())
}

fn models(url: &str, model: Value) -> Models {
    let pair = json!([{ "provider": "mock", "model": "m" }]);
    let cfg = json!({
        "providers": { "mock": { "kind": "openai", "endpoint": url, "models": { "m": model } } },
        "roles": { "draft": pair, "repair": pair, "summarize": pair }
    });
    let policy = sparkles::outbound::OutboundPolicy {
        allow_private: true,
        ..Default::default()
    };
    Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        policy,
    )
}

fn draft(q: &str) -> String {
    json!({
        "query": q,
        "explanation": "It lists what was asked.",
        "assumptions": ["'payments' is the team res:payments"],
        "clarify": { "question": "", "choices": [] },
        "graph": { "subject": "", "predicate": "", "object": "" }
    })
    .to_string()
}

fn is_summary(r: &Received) -> bool {
    r.body.to_string().contains("rows of a query result")
}

/// A mock that answers drafts from `drafts` in order (repeating the last) and summaries
/// with `summary`.
fn scripted(drafts: Vec<String>, summary: Value) -> MockModel {
    let drafts = Mutex::new(drafts.into_iter().collect::<VecDeque<_>>());
    MockModel::start(move |r, _| {
        if is_summary(r) {
            return (200, mock::openai(&summary.to_string()));
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

fn opts(question: &str) -> AskOptions {
    AskOptions {
        dataset: "org".into(),
        question: question.into(),
        deadline: Duration::from_secs(60),
        ..AskOptions::default()
    }
}

/// Run an ask, collecting the event names.
fn run(server: &McpServer, m: &Models, o: &AskOptions) -> (Value, Vec<String>) {
    let mut events = Vec::new();
    let mut on = |e: &str, _: &Value| events.push(e.to_string());
    let out = ask(server, m, &Principal::local(), o, &mut on);
    (out, events)
}

fn quads(s: &McpServer) -> u64 {
    s.state.datasets()["org"].store.snapshot().len()
}

/// The steps in order, the added limit, the rows in the summary prompt, and a summary
/// marker for a row that was not sent removed (A24).
#[test]
fn answers_and_summarizes() {
    let s = server(ORG);
    let mock = scripted(
        vec![draft(&format!(
            "{PREFIXES}SELECT ?p ?name WHERE {{ ?p ex:memberOf res:payments ; foaf:name ?name }}"
        ))],
        json!({ "text": "Ana Lima is on the payments team [1]. So is someone else [90].", "citations": [1, 90] }),
    );
    let m = models(&mock.url(), json!({}));
    let (out, events) = run(&s, &m, &opts("Who is on the payments team?"));
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(
        events,
        [
            "ground", "draft", "check", "run", "result", "summary", "usage"
        ]
    );
    let r = &out["result"];
    assert_eq!(r["limitAdded"], true);
    assert!(r["query"].as_str().unwrap().ends_with("LIMIT 1000"));
    assert_eq!(r["results"]["total"], 1);
    assert_eq!(r["attempt"], 1);
    assert!(
        r["terms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["term"] == "ex:memberOf"),
        "{r:#}"
    );
    assert_eq!(
        out["summary"],
        json!({ "text": "Ana Lima is on the payments team [1]. So is someone else.", "citations": [1] })
    );
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 2);
    let d = mock::prompt_of(&reqs[0]);
    assert!(d.contains("<data>") && d.contains("ex:memberOf"), "{d}");
    // linking found the team's label
    assert!(out["usage"]["modelCalls"] == 2, "{out:#}");
    let sm = mock::prompt_of(&reqs[1]);
    assert!(
        sm.contains("[1] ?p = res:ana ; ?name = \"Ana Lima\"@en"),
        "{sm}"
    );
    assert_eq!(out["usage"]["inputTokens"], 20);
    assert_eq!(out["usage"]["complexity"]["score"], 1);
}

/// A13 in Phase 1: a draft with an unknown class gets a repair request naming
/// `unknown-class` and the suggested class, and the repaired query answers.
#[test]
fn repairs_from_the_check() {
    let s = server(ORG);
    let mock = scripted(
        vec![
            draft("SELECT ?o WHERE { ?o a ex:Organisation }"),
            draft("SELECT ?o WHERE { ?o a ex:Organization }"),
        ],
        json!({}),
    );
    let m = models(&mock.url(), json!({}));
    let o = AskOptions {
        summary: false,
        ..opts("Which organizations are there?")
    };
    let (out, events) = run(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(out["result"]["attempt"], 2);
    assert!(events.contains(&"diagnosis".to_string()));
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 2);
    let repair = mock::prompt_of(&reqs[1]);
    assert!(repair.contains("unknown-class"), "{repair}");
    assert!(repair.contains("ex:Organization"), "{repair}");
    assert!(repair.contains("ex:Organisation"), "{repair}");
    assert_eq!(out["usage"]["steps"][1]["role"], "repair");
}

/// An empty result is diagnosed with `why_empty`, repaired at most twice, then shown
/// empty with the diagnosis; the model answered three times and no summary was asked.
#[test]
fn empty_results_are_diagnosed() {
    let s = server(ORG);
    let mock = scripted(
        vec![draft(r#"SELECT ?p WHERE { ?p foaf:name "Ana Lima" }"#)],
        json!({}),
    );
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("Who is Ana Lima?"));
    assert_eq!(out["outcome"], "empty", "{out:#}");
    assert_eq!(out["result"]["empty"], true);
    let diag = out["result"]["diagnosis"].as_str().unwrap();
    assert!(diag.contains("language-tag"), "{diag}");
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 3);
    assert!(reqs.iter().all(|r| !is_summary(r)));
    assert!(mock::prompt_of(&reqs[1]).contains("language-tag"));
    assert_eq!(out["attempts"].as_array().unwrap().len(), 3);
}

/// A17 in Phase 1: a draft that is SPARQL Update gets `not-a-query`, is repaired, and
/// nothing is ever written.
#[test]
fn updates_are_never_run() {
    let s = server(ORG);
    let before = quads(&s);
    let mock = scripted(
        vec![draft(
            "INSERT DATA { <http://example.org/resource/x> <http://example.org/ontology#memberOf> <http://example.org/resource/payments> }",
        )],
        json!({}),
    );
    let m = models(&mock.url(), json!({}));
    let (out, events) = run(&s, &m, &opts("Add x to payments"));
    assert_eq!(out["outcome"], "failed", "{out:#}");
    assert!(!events.contains(&"run".to_string()));
    assert_eq!(out["result"]["issues"][0]["code"], "not-a-query");
    assert!(mock::prompt_of(&mock.requests()[1]).contains("not-a-query"));
    assert_eq!(quads(&s), before);
}

/// A27: with `contextTokens: 4096` the draft request holds at most 30 classes, 60
/// predicates and two stored examples, and there is no summary call.
#[test]
fn a27_small_context_trims_the_grounding() {
    let mut ttl = String::from("@prefix ex: <http://example.org/ontology#> .\n");
    for i in 0..40 {
        ttl.push_str(&format!("ex:i{i} a ex:Class{i} .\n"));
    }
    for i in 0..80 {
        ttl.push_str(&format!("ex:i0 ex:pred{i} {i} .\n"));
    }
    let s = server(&ttl);
    let ds = s.state.datasets()["org"].clone();
    for i in 0..4 {
        let d: sparkles::stored::Definition = serde_json::from_value(json!({
            "query": format!("SELECT ?x WHERE {{ ?x a <http://example.org/ontology#Class{i}> }} LIMIT 10"),
            "description": format!("Instances of class {i}"),
            "questions": [format!("Which things are of class {i}?")]
        }))
        .unwrap();
        ds.queries
            .put(
                &format!("class_{i}"),
                d,
                sparkles::stored::Change::default(),
            )
            .unwrap();
    }
    let mock = scripted(
        vec![draft("SELECT ?x WHERE { ?x a ex:Class1 }")],
        json!({ "text": "One [1].", "citations": [1] }),
    );
    let m = models(&mock.url(), json!({ "contextTokens": 4096 }));
    let (out, _) = run(&s, &m, &opts("Which things are of class 1?"));
    assert_eq!(out["outcome"], "answered", "{out:#}");
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1, "no summary call");
    let p = mock::prompt_of(&reqs[0]);
    let section = |head: &str| -> usize {
        let Some(i) = p.find(head) else { return 0 };
        p[i..]
            .lines()
            .skip(1)
            .take_while(|l| !l.trim().is_empty())
            .count()
    };
    let classes = section("Classes (");
    let predicates = section("Predicates (");
    assert!(classes > 0 && classes <= 30, "{classes}\n{p}");
    assert!(predicates > 0 && predicates <= 60, "{predicates}");
    let examples = p.lines().filter(|l| l.starts_with("# class_")).count();
    assert_eq!(examples, 2, "{p}");
    // the class the question names ranks into the trimmed list
    assert!(p.contains("ex:Class1 "), "{p}");
    assert_eq!(out["usage"]["grounding"]["trimmed"], true);
    assert!(
        out["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("without a summary"))
    );
}

/// §4.4: an ambiguous mention is asked about before any model call; the answer to it
/// reaches the draft.
#[test]
fn ambiguous_mentions_ask_first() {
    let ttl = format!(
        "{ORG}\nres:ana2 a ex:Person ; rdfs:label \"Ana\"@en ; ex:memberOf res:platform .\nres:ana rdfs:label \"Ana\"@en .\n"
    );
    let s = server(&ttl);
    let mock = scripted(
        vec![draft("SELECT ?t WHERE { res:ana2 ex:memberOf ?t }")],
        json!({ "text": "Platform [1].", "citations": [1] }),
    );
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("Which team is Ana on?"));
    assert_eq!(out["outcome"], "clarify", "{out:#}");
    let choices = out["clarify"]["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2, "{out:#}");
    assert!(mock.requests().is_empty());
    let o = AskOptions {
        clarification: Some("res:ana2".into()),
        ..opts("Which team is Ana on?")
    };
    let (out, _) = run(&s, &m, &o);
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert!(mock::prompt_of(&mock.requests()[0]).contains("res:ana2"));
}

/// A draft's own clarification ends the ask with its choices; an empty query is an
/// answer that the data cannot answer the question.
#[test]
fn draft_clarification_and_unanswerable() {
    let s = server(ORG);
    let clarify = json!({
        "query": "SELECT ?t WHERE { ?t a ex:Team }",
        "explanation": "Teams.",
        "assumptions": [],
        "clarify": { "question": "Teams or organizations?", "choices": ["teams", "organizations"] },
        "graph": { "subject": "", "predicate": "", "object": "" }
    });
    let mock = scripted(vec![clarify.to_string()], json!({}));
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("List the units"));
    assert_eq!(out["outcome"], "clarify");
    assert_eq!(out["clarify"]["choices"][1]["value"], "organizations");
    let mock = scripted(
        vec![
            json!({
                "query": "",
                "explanation": "The data holds no phone numbers.",
                "assumptions": [],
                "clarify": { "question": "", "choices": [] },
                "graph": { "subject": "", "predicate": "", "object": "" }
            })
            .to_string(),
        ],
        json!({}),
    );
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("What is Ana's phone number?"));
    assert_eq!(out["outcome"], "unanswerable", "{out:#}");
    assert_eq!(
        out["result"]["explanation"],
        "The data holds no phone numbers."
    );
}

/// A26: a provider that rejects structured output answers in plain text; the fenced
/// query is used, with a note that the model is limited and no clarification.
#[test]
fn plain_text_level() {
    let s = server(ORG);
    let mock = MockModel::start(|r, _| {
        if r.body.get("response_format").is_some() {
            return (
                400,
                json!({ "error": { "message": "response_format is not supported" } }),
            );
        }
        if is_summary(r) {
            return (200, mock::openai("Two teams [1] [2]."));
        }
        (
            200,
            mock::openai("```sparql\nSELECT ?t WHERE { ?t a ex:Team }\n```\nThe teams."),
        )
    });
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("Which teams are there?"));
    assert_eq!(out["outcome"], "answered", "{out:#}");
    assert_eq!(out["usage"]["level"], "text");
    assert_eq!(out["result"]["explanation"], "The teams.");
    assert!(
        out["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("plain text"))
    );
    assert_eq!(out["summary"]["citations"], json!([1, 2]));
}

/// A provider that is down ends the ask with `provider-unavailable`.
#[test]
fn provider_unavailable() {
    let s = server(ORG);
    let mock = MockModel::start(|_, _| (503, json!({ "error": "down" })));
    let m = models(&mock.url(), json!({ "requestTimeoutSecs": 2 }));
    let (out, events) = run(&s, &m, &opts("Which teams are there?"));
    assert_eq!(out["outcome"], "error");
    assert_eq!(out["error"]["code"], "provider-unavailable", "{out:#}");
    assert!(events.contains(&"error".to_string()));
}

#[test]
fn mentions_and_words() {
    assert_eq!(mentions("Who is on the Storage team?"), ["Storage"]);
    assert_eq!(
        mentions("What does Guido van Rossum work on?"),
        ["Guido van Rossum"]
    );
    assert_eq!(
        mentions("Which projects does Ada Lovelace's team work on?"),
        ["Ada Lovelace"]
    );
    assert_eq!(
        mentions("Members of \"Query Engine team\" in Berlin"),
        ["Query Engine team", "Members of Query Engine", "Berlin"]
    );
    assert!(mentions("how many people are there?").is_empty());
    assert!(words("startDate of Teams").contains("team"));
    assert!(words("startDate").contains("date"));
    assert!(has_service("SELECT * { SERVICE <http://x/> { ?s ?p ?o } }"));
    assert!(!has_service(
        "SELECT * { ?s <http://x/service> \"service\" }"
    ));
}

/// The demo question set: every gold query parses, runs over the organisation graph,
/// answers the questions marked answerable, and has the complexity stored with it.
#[test]
fn demo_set_gold_queries() {
    let ttl = include_str!("../../../../testsuite/ask/org.ttl");
    let set: Value =
        serde_json::from_str(include_str!("../../../../testsuite/ask/demo.json")).unwrap();
    let s = server(ttl);
    let qs = set["questions"].as_array().unwrap();
    assert!(qs.len() >= 55, "{}", qs.len());
    let mut ids = BTreeSet::new();
    let mut wrong = Vec::new();
    for q in qs {
        let id = q["id"].as_str().unwrap();
        assert!(ids.insert(id.to_string()), "duplicate id {id}");
        let kind = q["kind"].as_str().unwrap();
        let Some(gold) = q["gold"].as_str() else {
            assert!(
                matches!(kind, "unanswerable" | "clarify"),
                "{id} has no gold query"
            );
            continue;
        };
        let call = Call {
            arrived: Instant::now(),
            cancel: Arc::new(AtomicBool::new(false)),
            request_id: "t".into(),
            principal: Principal::local(),
            headers: None,
            held: None,
        };
        let mut a = Map::new();
        a.insert("query".into(), gold.into());
        a.insert("format".into(), "json".into());
        a.insert("maxRows".into(), 1000.into());
        let doc = match s.run_now("sparql_query", a, &call) {
            Ok(Outcome::Text(t)) => serde_json::from_str::<Value>(&t).unwrap(),
            Ok(_) => panic!("{id}"),
            Err(e) => panic!("{id}: {}", e.message),
        };
        let rows = doc["total"].as_u64().unwrap_or(1);
        if kind == "answerable" {
            assert!(
                rows > 0 || doc["boolean"].is_boolean(),
                "{id} answers nothing"
            );
        }
        let ds = s.state.datasets()["org"].clone();
        let pv: Vec<(String, String)> = crate::mcp::tools::dataset_prefixes(&ds)
            .into_iter()
            .collect();
        let parsed = sparkles::sparql::parse_query(gold, None, &pv).unwrap();
        let c = complexity::of(&parsed);
        if q["complexity"].as_u64() != Some(u64::from(c.score())) {
            wrong.push(format!("{id} {}", c.score()));
        }
    }
    assert!(
        wrong.is_empty(),
        "complexity differs:\n{}",
        wrong.join("\n")
    );
}
