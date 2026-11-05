//! Item 653 through the real Streamable HTTP router, SDK adapter and caller context.
use super::*;
use crate::auth::hash_password_with;
use crate::http::router_tests::auth::{AuthServer, Fixture, b, build, call};
use sparkles::evidence::{ClaimKind, ConflictMode, MatchingPolicy, Policy, Settings};
use sparkles::task::Control;

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const ACCEPTED: &str = "urn:ev:accepted";
const IDENTITIES: &str = "urn:ev:identities";
const MAPS: &str = "urn:ev:maps";
const MAP: &str = "urn:ev:map";
const NAMES: [&str; 6] = [
    "propose_claims",
    "review_claims",
    "withdraw_claims",
    "resolve_evidence",
    "resolve_identities",
    "find_evidence_dependents",
];

fn configured(flags: Vec<&'static str>, read_only: bool) -> AuthServer {
    let hash = |u: &str| hash_password_with(&format!("{u}-pw"), 8, 1, 1).unwrap();
    let s = build(Fixture {
        mcp: Some(flags),
        read_only,
        extra: format!(
            r#"
[[users]]
name = "evw"
password = "{}"
datasets = {{ ev = "write" }}
[[users]]
name = "evr"
password = "{}"
datasets = {{ ev = "read" }}
[[users]]
name = "evp"
password = "{}"
[[users.grants]]
dataset = "ev"
level = "write"
graphs = ["urn:ev:claims"]
"#,
            hash("evw"),
            hash("evr"),
            hash("evp")
        ),
        ..Default::default()
    });
    let ds = s.state.attach("ev", DbType::Persistent, None).unwrap();
    sparkles::evidence::configure(
        &ds.dataset,
        &Settings {
            enabled: true,
            source_records: vec![SOURCES.into()],
            proposals: vec![CLAIMS.into()],
            decisions: vec![DECISIONS.into()],
            supports: vec![SUPPORTS.into()],
            identity: vec![IDENTITIES.into(), MAPS.into()],
            projections: vec![ACCEPTED.into(), MAP.into()],
            policies: [(
                "main".into(),
                Policy {
                    version: 1,
                    admitted_kinds: vec![ClaimKind::Reported],
                    required_scope: vec![],
                    conflicts: ConflictMode::Refuse,
                    boundary: vec![SOURCES.into(), CLAIMS.into()],
                    decisions: DECISIONS.into(),
                    supports: SUPPORTS.into(),
                    output: ACCEPTED.into(),
                },
            )]
            .into_iter()
            .collect(),
            matching_policies: [(
                "people".into(),
                MatchingPolicy {
                    version: 1,
                    boundary: vec![IDENTITIES.into()],
                    decisions: DECISIONS.into(),
                    maps: MAPS.into(),
                    output: MAP.into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        &Control::none(),
    )
    .unwrap();
    s
}

fn head(s: &AuthServer) -> u64 {
    s.state.get("ev").unwrap().store.head_commit().seq
}
fn digest() -> String {
    format!("sha256:{}", "0".repeat(64))
}
fn proposal() -> J {
    json!({"revisions": [{"key":"rev", "source":"urn:bulletin", "sourceKey":"bulletin", "content":{"digest":digest()}}],
        "claims":[{"key":"claim", "occurrenceKey":"c1", "revision":{"key":"rev"}, "proposition":"<urn:release> <urn:affectedBy> <urn:cve>", "kind":"reported", "polarity":"positive", "author":"urn:publisher"}]})
}
fn record<'a>(out: &'a J, key: &str) -> &'a J {
    out["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap()
}
async fn invoke(s: &AuthServer, user: &str, name: &str, mut args: J) -> J {
    args["dataset"] = json!("ev");
    tool(&s.app, name, args, &[("authorization", &b(user))]).await
}
async fn success(s: &AuthServer, name: &str, args: J) -> J {
    let result = invoke(s, "evw", name, args).await;
    assert_eq!(result["isError"], false, "{result}");
    let mirror: J = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(mirror, result["structuredContent"]);
    result["structuredContent"].clone()
}
async fn plain(
    s: &AuthServer,
    path: &str,
    user: &str,
    args: J,
) -> crate::http::router_tests::auth::R {
    call(
        &s.app,
        "POST",
        &format!("/ev{path}"),
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &args.to_string(),
    )
    .await
}
fn review(s: &AuthServer, policy: &str, target: &J, disposition: &str) -> J {
    json!({"policy":policy,"policyVersion":1,"ifHead":head(s),"decisions":[{"key":"d", "target":target["iri"], "targetDigest":target["recordDigest"], "disposition":disposition}]})
}
fn no_projection(s: &AuthServer) {
    assert!(
        !s.state
            .get("ev")
            .unwrap()
            .dataset
            .ask(&format!(
                "ASK {{ GRAPH <{ACCEPTED}> {{ <urn:release> <urn:affectedBy> <urn:cve> }} }}"
            ))
            .unwrap()
    );
}

#[tokio::test]
async fn proposals_reviews_withdrawals_and_http_read_parity() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let before = head(&s);
    let mut input = proposal();
    input["dryRun"] = json!(true);
    let preview = success(&s, "propose_claims", input).await;
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["dryRun"]["wouldCommit"], true);
    assert_eq!(head(&s), before);
    no_projection(&s);
    let mut input = proposal();
    input["ifHead"] = json!(before);
    input["idempotencyKey"] = json!("proposal-1");
    let proposed = success(&s, "propose_claims", input.clone()).await;
    assert_eq!(proposed["committed"], true);
    no_projection(&s);
    assert!(s.state.get("ev").unwrap().dataset.ask(&format!("ASK {{ GRAPH <{CLAIMS}> {{ <{}> <http://www.w3.org/ns/prov#wasAssociatedWith> <urn:x-sparkles:principal:user:evw> }} }}", proposed["operation"].as_str().unwrap())).unwrap());
    let replay = success(&s, "propose_claims", input).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["operation"], proposed["operation"]);
    let target = record(&proposed, "claim");
    let references =
        json!({"references":[{"key":"c", "record":{"iri":target["iri"],"kinds":["Claim"]}}]});
    let resolved = success(&s, "resolve_evidence", references.clone()).await;
    let http = plain(&s, "/claims/resolve", "evw", references).await;
    assert_eq!(http.status, StatusCode::OK);
    assert_eq!(resolved, http.json());
    let h = head(&s);
    let mut decision = review(&s, "main", target, "accepted");
    decision["dryRun"] = json!(true);
    let preview = success(&s, "review_claims", decision).await;
    assert_eq!(preview["committed"], false);
    assert_eq!(head(&s), h);
    no_projection(&s);
    let mut bad = review(&s, "main", target, "accepted");
    bad["policyVersion"] = json!(2);
    assert_eq!(
        tool_error(&invoke(&s, "evw", "review_claims", bad).await),
        "policy-changed"
    );
    let mut bad = review(&s, "main", target, "accepted");
    bad["decisions"][0]["targetDigest"] = json!(digest());
    assert_eq!(
        tool_error(&invoke(&s, "evw", "review_claims", bad).await),
        "target-changed"
    );
    assert_eq!(head(&s), h);
    no_projection(&s);
    let accepted = success(&s, "review_claims", review(&s, "main", target, "accepted")).await;
    assert_eq!(accepted["changed"]["inserted"].as_array().unwrap().len(), 1);
    assert!(
        s.state
            .get("ev")
            .unwrap()
            .dataset
            .ask(&format!(
                "ASK {{ GRAPH <{ACCEPTED}> {{ <urn:release> <urn:affectedBy> <urn:cve> }} }}"
            ))
            .unwrap()
    );
    let mut stale_request = review(&s, "main", target, "accepted");
    stale_request["ifHead"] = json!(h);
    let stale = invoke(&s, "evw", "review_claims", stale_request).await;
    assert_eq!(tool_error(&stale), "head-changed");
    let withdrawal = json!({"ifHead":head(&s),"withdrawals":[{"key":"w","target":target["iri"],"targetDigest":target["recordDigest"]}]});
    let mut dry = withdrawal.clone();
    dry["dryRun"] = json!(true);
    let h = head(&s);
    let preview = success(&s, "withdraw_claims", dry).await;
    assert_eq!(preview["committed"], false);
    assert_eq!(head(&s), h);
    let withdrawn = success(&s, "withdraw_claims", withdrawal).await;
    assert_eq!(withdrawn["changed"]["removed"].as_array().unwrap().len(), 1);
    no_projection(&s);
    let mut withdrawn_review = review(&s, "main", target, "accepted");
    withdrawn_review["decisions"][0]["supersedes"] = accepted["decisions"][0]["iri"].clone();
    let failed = invoke(&s, "evw", "review_claims", withdrawn_review).await;
    assert_eq!(tool_error(&failed), "target-withdrawn");
}

#[tokio::test]
async fn identity_proposals_reviews_reversals_and_dependents() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let proposed = success(&s, "propose_claims", json!({"identityGraph":IDENTITIES,"identities":[{"key":"match","occurrenceKey":"m1","entities":["urn:person:a","urn:person:b"],"relation":"possibleMatch","author":"urn:matcher"}]})).await;
    let target = record(&proposed, "match");
    let accepted = success(
        &s,
        "review_claims",
        review(&s, "people", target, "accepted"),
    )
    .await;
    assert_eq!(accepted["changed"]["map"]["number"], 1);
    let request = json!({"policy":"people","entities":["urn:person:b","urn:person:unknown"]});
    let mapped = success(&s, "resolve_identities", request.clone()).await;
    assert_eq!(mapped["entities"][0]["representative"], "urn:person:a");
    assert_eq!(
        mapped["entities"][0]["members"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        mapped["entities"][1]["representative"],
        "urn:person:unknown"
    );
    assert_eq!(
        plain(&s, "/claims/identities", "evw", request.clone())
            .await
            .json(),
        mapped
    );
    let assessed = success(&s, "propose_claims", json!({"assessments":[
        {"key":"a","occurrenceKey":"a1","target":{"iri":target["iri"]},"conclusion":"\"holds\"","method":{"method":"urn:analysis","version":"1"},"configurationDigest":digest(),"evaluatedAt":"2026-01-01T00:00:00Z","evaluator":"urn:analyst","inputs":[{"iri":accepted["mapRevision"]}]},
        {"key":"b","occurrenceKey":"b1","target":{"key":"a"},"conclusion":"\"holds\"","method":{"method":"urn:analysis","version":"1"},"configurationDigest":digest(),"evaluatedAt":"2026-01-01T00:00:00Z","evaluator":"urn:analyst"}
    ]})).await;
    let dep_request = json!({"changed":[accepted["mapRevision"]],"transitive":true});
    let dependents = success(&s, "find_evidence_dependents", dep_request.clone()).await;
    let found = dependents["dependents"].as_array().unwrap();
    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .any(|d| d["assessment"] == record(&assessed, "a")["iri"]
                && d["depth"] == 0
                && d["dependence"] == "used")
    );
    assert!(
        found
            .iter()
            .any(|d| d["assessment"] == record(&assessed, "b")["iri"] && d["depth"] == 1)
    );
    assert_eq!(
        plain(&s, "/claims/dependents", "evw", dep_request.clone())
            .await
            .json(),
        dependents
    );
    let mut reverse = review(&s, "people", target, "rejected");
    reverse["decisions"][0]["supersedes"] = accepted["decisions"][0]["iri"].clone();
    let reversed = success(&s, "review_claims", reverse).await;
    assert_eq!(reversed["changed"]["map"]["number"], 2);
    let mapped = success(&s, "resolve_identities", request).await;
    assert_eq!(mapped["entities"][0]["representative"], "urn:person:b");
    // Only the claim graph is visible to evp, so identity resolution is refused.
    let refused = invoke(
        &s,
        "evp",
        "resolve_identities",
        json!({"policy":"people","entities":["urn:person:a"]}),
    )
    .await;
    assert_eq!(tool_error(&refused), "forbidden");
}

#[tokio::test]
async fn caller_grants_hidden_records_and_http_refusals_match() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let h = head(&s);
    for user in ["evr", "evp"] {
        let denied = invoke(&s, user, "propose_claims", proposal()).await;
        assert_eq!(tool_error(&denied), "forbidden");
    }
    assert_eq!(head(&s), h);
    let proposed = success(&s, "propose_claims", proposal()).await;
    let rev = record(&proposed, "rev")["iri"].as_str().unwrap();
    let resolve = |iri: &str| json!({"references":[{"key":"hidden","record":{"iri":iri}}]});
    let hidden = invoke(&s, "evp", "resolve_evidence", resolve(rev)).await;
    let absent = invoke(&s, "evp", "resolve_evidence", resolve("urn:missing")).await;
    assert_eq!(tool_error(&hidden), "evidence-unavailable");
    let meta = "io.github.kclejeune.sparkles/error";
    assert_eq!(hidden["_meta"][meta], absent["_meta"][meta]);
    let http = plain(&s, "/claims/resolve", "evp", resolve(rev)).await;
    assert_eq!(http.status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut body = http.json();
    body.as_object_mut().unwrap().remove("requestId");
    assert_eq!(hidden["_meta"][meta]["data"], body);
    assert_eq!(hidden["_meta"][meta]["status"], 422);
    // A source revision hidden from a proposal cannot be used to mint a new claim.
    let mut batch = proposal();
    batch.as_object_mut().unwrap().remove("revisions");
    batch["claims"][0]["revision"] = json!({"iri":rev});
    let h = head(&s);
    let hidden = invoke(&s, "evp", "propose_claims", batch).await;
    assert_eq!(tool_error(&hidden), "evidence-unavailable");
    assert_eq!(head(&s), h);
    // Raw updates remain outside the protected evidence writer.
    let raw=tool(&s.app,"sparql_update",json!({"dataset":"ev","update":format!("INSERT DATA {{ GRAPH <{CLAIMS}> {{ <urn:s> <urn:p> <urn:o> }} }}")}),&[("authorization",&b("evw"))]).await;
    assert_eq!(tool_error(&raw), "profile-managed-graph");
    assert_eq!(head(&s), h);
    let unknown = invoke(&s, "bob", "resolve_evidence", json!({"references":[]})).await;
    assert_eq!(tool_error(&unknown), "unknown-dataset");
}

#[tokio::test]
async fn advertised_payloads_update_switch_and_disable_controls() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let writer = b("evw");
    let reply = modern(
        &s.app,
        "tools/list",
        json!({}),
        &[("authorization", &writer)],
    )
    .await;
    let tools = reply.rpc()["result"]["tools"].as_array().unwrap().clone();
    for name in ["propose_claims", "review_claims", "withdraw_claims"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], false, "{name}");
        assert_eq!(tool["annotations"]["destructiveHint"], false, "{name}");
        assert_eq!(
            tool["annotations"]["idempotentHint"], false,
            "idempotentHint for {name}"
        );
    }
    for name in NAMES {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
        assert!(t["inputSchema"]["properties"]["branch"].is_object());
        assert!(t["inputSchema"]["properties"]["timeoutSeconds"].is_object());
        for schema in [&t["inputSchema"], &t["outputSchema"]] {
            assert!(schema.is_object());
            for keyword in ["$schema", "$ref", "oneOf", "anyOf", "allOf"] {
                assert!(!schema.to_string().contains(keyword), "{name}: {keyword}");
            }
        }
    }
    let resolve = tools
        .iter()
        .find(|t| t["name"] == "resolve_evidence")
        .unwrap();
    assert_eq!(
        resolve["inputSchema"]["properties"]["references"]["items"]["properties"]["storedFact"]["properties"]
            ["graph"]["type"],
        json!(["string", "null"])
    );
    assert_eq!(
        resolve["outputSchema"]["properties"]["resolved"]["items"]["properties"]["storedFact"]["properties"]
            ["graph"]["type"],
        json!(["string", "null"])
    );
    let propose = tools
        .iter()
        .find(|t| t["name"] == "propose_claims")
        .unwrap();
    assert!(
        propose["inputSchema"]["properties"]["identities"]["items"]["properties"]["entities"]
            .is_object()
    );
    assert!(
        propose["inputSchema"]["properties"]["assessments"]["items"]["properties"]["inputs"]
            .is_object()
    );
    assert_eq!(propose["annotations"]["readOnlyHint"], false);
    assert_eq!(propose["annotations"]["destructiveHint"], false);
    let readers = tool_names(&s.app, &[("authorization", &b("evr"))]).await;
    assert!(!readers.contains(&"propose_claims".into()));
    assert!(readers.contains(&"resolve_evidence".into()));
    let s = configured(vec![], false);
    let names = tool_names(&s.app, &[("authorization", &b("evw"))]).await;
    assert!(!names.contains(&"review_claims".into()));
    assert!(names.contains(&"resolve_identities".into()));
    let s = configured(vec!["--mcp-allow-update"], true);
    assert!(
        !tool_names(&s.app, &[("authorization", &b("evw"))])
            .await
            .contains(&"withdraw_claims".into())
    );
    let s = configured(vec!["--mcp-disable-tool", "resolve_evidence"], false);
    assert!(
        !tool_names(&s.app, &[("authorization", &b("evw"))])
            .await
            .contains(&"resolve_evidence".into())
    );
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name":"resolve_evidence","arguments":{"dataset":"ev","references":[]}}),
        &[("authorization", &b("evw"))],
    )
    .await;
    assert_eq!(r.rpc()["error"]["code"], -32602);
}

#[tokio::test]
async fn branch_selection_and_invalid_requests_write_nothing() {
    let s = configured(vec!["--mcp-allow-update"], false);
    s.state
        .get("ev")
        .unwrap()
        .store
        .create_branch("work", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let h = head(&s);
    let mut args = proposal();
    args["branch"] = json!("work");
    let proposed = success(&s, "propose_claims", args).await;
    assert_eq!(proposed["committed"], true);
    assert_eq!(head(&s), h);
    let record = record(&proposed, "claim");
    let read = json!({"references":[{"key":"r","record":{"iri":record["iri"]}}]});
    assert_eq!(
        tool_error(&invoke(&s, "evw", "resolve_evidence", read.clone()).await),
        "evidence-unavailable"
    );
    let mut on_branch = read.clone();
    on_branch["branch"] = json!("work");
    let resolved = success(&s, "resolve_evidence", on_branch).await;
    let http = plain(&s, "@work/claims/resolve", "evw", read).await;
    // The path form chooses the same branch and exact record.
    assert_eq!(http.status, StatusCode::OK, "{}", http.text());
    assert_eq!(resolved, http.json());
    let invalid = invoke(
        &s,
        "evw",
        "propose_claims",
        json!({"claims":[],"unknown":true}),
    )
    .await;
    assert_eq!(tool_error(&invalid), "invalid-proposal");
    let too_many = invoke(
        &s,
        "evw",
        "find_evidence_dependents",
        json!({"changed":vec!["urn:x";1001]}),
    )
    .await;
    assert_eq!(tool_error(&too_many), "invalid-reference");
    let invalid_timeout = invoke(
        &s,
        "evw",
        "resolve_evidence",
        json!({"references":[],"timeoutSeconds":0}),
    )
    .await;
    assert_eq!(tool_error(&invalid_timeout), "bad-argument");
    let expired = invoke(&s, "evw", "propose_claims", json!({"timeoutSeconds":1e-16})).await;
    assert_eq!(tool_error(&expired), "timeout");
    assert_eq!(head(&s), h);
}

#[tokio::test]
async fn passage_batches_and_exact_retained_fact_resolution() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let sha = |text: &str| {
        sparkles::evidence::Sha256Digest::of_bytes(text.as_bytes(), &Control::none())
            .unwrap()
            .to_string()
    };
    let mut batch = proposal();
    batch["revisions"][0]["content"] = json!({"text":"évidence"});
    batch["renditions"] = json!([{"key":"text","revision":{"key":"rev"},"occurrenceKey":"text","conversion":{"method":"urn:identity","version":"1"},"mediaType":"text/plain","content":{"text":"évidence"}}]);
    batch["passages"] = json!([{"key":"span","revision":{"key":"rev"},"rendition":{"key":"text"},"occurrenceKey":"span","start":0,"end":2,"textDigest":sha("év"),"quote":"év"}]);
    batch["references"] = json!([{"key":"ref","occurrenceKey":"ref","passage":{"revision":{"key":"rev"},"rendition":{"key":"text"},"passage":{"key":"span"}}}]);
    batch["links"] = json!([{"key":"link","occurrenceKey":"link","target":{"key":"claim"},"evidence":{"key":"ref"},"relation":"supports","author":"urn:reviewer"}]);
    let mut mismatch = batch.clone();
    mismatch["passages"][0]["quote"] = json!("wrong");
    let h = head(&s);
    assert_eq!(
        tool_error(&invoke(&s, "evw", "propose_claims", mismatch).await),
        "span-mismatch"
    );
    assert_eq!(head(&s), h);
    let out = success(&s, "propose_claims", batch).await;
    assert_eq!(out["records"].as_array().unwrap().len(), 6);
    let read = json!({"references":[{"key":"span","record":{"iri":record(&out,"span")["iri"],"kinds":["Passage"]}},{"key":"ref","record":{"iri":record(&out,"ref")["iri"],"kinds":["EvidenceRef"]}}]});
    let resolved = success(&s, "resolve_evidence", read).await;
    assert_eq!(resolved["resolved"].as_array().unwrap().len(), 2);
    let ds = s.state.get("ev").unwrap();
    ds.dataset
        .update("INSERT DATA { <urn:fact> <urn:p> <urn:old> }")
        .unwrap();
    let at = head(&s);
    ds.dataset.update("DELETE DATA { <urn:fact> <urn:p> <urn:old> }; INSERT DATA { <urn:fact> <urn:p> <urn:new> }").unwrap();
    let reference = |commit| json!({"references":[{"key":"f","storedFact":{"datasetId":ds.store.dataset_id(),"branchId":ds.store.dataset_id(),"commit":commit,"graph":null,"triple":"<urn:fact> <urn:p> <urn:old>"}}]});
    assert_eq!(
        success(&s, "resolve_evidence", reference(at)).await["resolved"][0]["storedFact"]["commit"],
        at
    );
    assert_eq!(
        tool_error(&invoke(&s, "evw", "resolve_evidence", reference(head(&s))).await),
        "evidence-unavailable"
    );
    let hidden = s.state.get("secret").unwrap();
    let reference = |id| json!({"references":[{"key":"f","storedFact":{"datasetId":id,"branchId":hidden.store.dataset_id(),"commit":hidden.store.head_commit().seq,"triple":"<http://ex.org/secret> <http://ex.org/p> \"1\""}}]});
    let hidden = invoke(
        &s,
        "evw",
        "resolve_evidence",
        reference(hidden.store.dataset_id()),
    )
    .await;
    let missing = invoke(
        &s,
        "evw",
        "resolve_evidence",
        reference(uuid::Uuid::new_v4()),
    )
    .await;
    assert_eq!(hidden["_meta"], missing["_meta"]);
}

#[tokio::test]
async fn a_revoked_caller_cannot_publish_over_mcp() {
    use crate::http::router_tests::auth::{bearer, password_session};
    let s = configured(vec!["--mcp-allow-update"], false);
    let (cookie, csrf) = password_session(&s, "evw").await;
    let minted = call(
        &s.app,
        "POST",
        "/$/auth/tokens",
        &[
            ("cookie", &cookie),
            ("x-sparkles-csrf", &csrf),
            ("content-type", "application/json"),
        ],
        r#"{"name":"evidence-importer","datasets":{"ev":"write"},"expiresIn":"7d"}"#,
    )
    .await;
    assert_eq!(minted.status, StatusCode::CREATED, "{}", minted.text());
    let token = minted.json()["token"].as_str().unwrap().to_string();
    let id = minted.json()["id"].as_str().unwrap().to_string();
    let ds = s.state.get("ev").unwrap();
    ds.dataset
        .update("INSERT DATA { <urn:a> <urn:p> <urn:o> }")
        .unwrap();
    let h = head(&s);
    let mut input = proposal();
    input["dataset"] = json!("ev");
    input["references"] = json!([{"key":"f","occurrenceKey":"f","storedFact":{"datasetId":ds.store.dataset_id(),"branchId":ds.store.dataset_id(),"commit":h,"triple":"<urn:a> <urn:p> <urn:o>"}}]);
    let auth = s.auth();
    ds.store.set_failpoint(
        "evidence-held",
        Some(Arc::new(move |_| {
            assert_eq!(auth.tokens.remove(std::slice::from_ref(&id)).unwrap(), 1);
        })),
    );
    let out = tool(
        &s.app,
        "propose_claims",
        input,
        &[("authorization", &bearer(&token))],
    )
    .await;
    ds.store.set_failpoint("evidence-held", None);
    assert_eq!(tool_error(&out), "authority-changed");
    assert_eq!(
        out["_meta"]["io.github.kclejeune.sparkles/error"]["status"],
        503
    );
    assert_eq!(head(&s), h);
    assert_eq!(
        sparkles::evidence::reconcile_operation_holds(&ds.dataset, 0)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn keyless_retries_publish_operations_while_reusing_record_state() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let first = success(&s, "propose_claims", proposal()).await;
    let first_head = head(&s);
    let second = success(&s, "propose_claims", proposal()).await;
    assert_eq!(first["committed"], true);
    assert_eq!(second["committed"], true);
    assert_eq!(second["replayed"], false);
    assert_ne!(second["operation"], first["operation"]);
    assert!(head(&s) > first_head);
    for key in ["rev", "claim"] {
        assert_eq!(record(&second, key)["iri"], record(&first, key)["iri"]);
        assert_eq!(
            record(&second, key)["recordDigest"],
            record(&first, key)["recordDigest"]
        );
        assert_eq!(record(&second, key)["existing"], true);
    }
    let target = record(&first, "claim");
    let reference =
        json!({"references":[{"key":"claim","record":{"iri":target["iri"],"kinds":["Claim"]}}]});
    let resolved = success(&s, "resolve_evidence", reference).await;
    assert_eq!(
        resolved["resolved"][0]["record"]["receipt"]["iri"],
        first["operation"]
    );
    assert_eq!(
        resolved["resolved"][0]["record"]["receipt"]["commit"],
        first["commit"]
    );

    let decision = review(&s, "main", target, "accepted");
    let accepted = success(&s, "review_claims", decision.clone()).await;
    let reviewed_head = head(&s);
    assert_eq!(accepted["committed"], true);
    let repeat = invoke(&s, "evw", "review_claims", decision).await;
    assert_eq!(tool_error(&repeat), "head-changed");
    assert_eq!(head(&s), reviewed_head);

    let request = json!({"withdrawals":[{"key":"w","target":target["iri"],"targetDigest":target["recordDigest"]}]});
    let withdrawn = success(&s, "withdraw_claims", request.clone()).await;
    let withdrawn_head = head(&s);
    assert_eq!(withdrawn["committed"], true);
    assert_eq!(withdrawn["withdrawals"][0]["existing"], false);
    assert_eq!(withdrawn["changed"]["removed"].as_array().unwrap().len(), 1);
    no_projection(&s);
    let repeated = success(&s, "withdraw_claims", request).await;
    assert_eq!(repeated["committed"], true);
    assert_eq!(repeated["replayed"], false);
    assert_eq!(repeated["withdrawals"][0]["existing"], true);
    assert!(repeated["withdrawals"][0]["iri"].is_null());
    assert_ne!(repeated["operation"], withdrawn["operation"]);
    assert!(head(&s) > withdrawn_head);
    for field in ["supportsAdded", "supportsEnded", "inserted", "removed"] {
        assert!(repeated["changed"][field].as_array().unwrap().is_empty());
    }
    no_projection(&s);
    for operation in [&second["operation"], &repeated["operation"]] {
        assert!(
            s.state
                .get("ev")
                .unwrap()
                .dataset
                .ask(&format!(
                    "ASK {{ GRAPH ?graph {{ <{}> a <urn:x-sparkles:evidence:Operation> }} }}",
                    operation.as_str().unwrap()
                ))
                .unwrap()
        );
    }
    let original = success(&s, "resolve_evidence", json!({"references":[{"key":"withdrawal","record":{"iri":withdrawn["withdrawals"][0]["iri"],"kinds":["Withdrawal"]}}]})).await;
    assert_eq!(
        original["resolved"][0]["record"]["receipt"]["iri"],
        withdrawn["operation"]
    );
    assert_eq!(
        original["resolved"][0]["record"]["receipt"]["commit"],
        withdrawn["commit"]
    );
}

#[tokio::test]
async fn explicit_keys_replay_each_write_receipt_without_moving_the_head() {
    let s = configured(vec!["--mcp-allow-update"], false);
    let mut proposal = proposal();
    proposal["idempotencyKey"] = json!("proposal-retry");
    let proposed = success(&s, "propose_claims", proposal.clone()).await;
    let published_head = head(&s);
    let replay = success(&s, "propose_claims", proposal).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["operation"], proposed["operation"]);
    assert_eq!(replay["commit"], proposed["commit"]);
    assert_eq!(head(&s), published_head);

    let target = record(&proposed, "claim");
    let mut decision = review(&s, "main", target, "accepted");
    decision["idempotencyKey"] = json!("review-retry");
    let accepted = success(&s, "review_claims", decision.clone()).await;
    let reviewed_head = head(&s);
    let replay = success(&s, "review_claims", decision).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["operation"], accepted["operation"]);
    assert_eq!(replay["commit"], accepted["commit"]);
    assert_eq!(head(&s), reviewed_head);

    let withdrawal = json!({"ifHead":reviewed_head,"idempotencyKey":"withdrawal-retry","withdrawals":[{"key":"w","target":target["iri"],"targetDigest":target["recordDigest"]}]});
    let withdrawn = success(&s, "withdraw_claims", withdrawal.clone()).await;
    let withdrawn_head = head(&s);
    let replay = success(&s, "withdraw_claims", withdrawal).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["operation"], withdrawn["operation"]);
    assert_eq!(replay["commit"], withdrawn["commit"]);
    assert_eq!(head(&s), withdrawn_head);
    for field in ["supportsAdded", "supportsEnded", "inserted", "removed"] {
        assert!(replay["changed"][field].as_array().unwrap().is_empty());
    }
    no_projection(&s);
}
