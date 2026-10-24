//! `/$/mcp` as an OAuth protected resource (C11, item 67): RFC 9728 metadata, the
//! resource identifier as an audience accepted at `/$/mcp` only, and the scope
//! challenges of tool calls.

use super::oidc::{Mock, PUBLIC, start_mock};
use super::*;
use serde_json::json;

const API_AUD: &str = "https://sparql.example.org/api";
const RESOURCE: &str = "https://sparql.example.org/$/mcp";
const METADATA: &str = "https://sparql.example.org/.well-known/oauth-protected-resource/$/mcp";

fn now() -> i64 {
    jsonwebtoken::get_current_timestamp() as i64
}

/// A server with the fixture policy, an `[oidc]` provider that accepts `API_AUD`
/// access tokens, alice also reading `team-a` through a user role, and `serve --mcp`
/// with `flags` (no MCP endpoint when `None`).
async fn server(flags: Option<Vec<&'static str>>, oidc: bool) -> (AuthServer, Arc<Mock>) {
    let m = start_mock().await;
    let mut extra = String::from(
        r#"
[roles.team-readers]
datasets = { "team-a" = "read" }

[external.user_roles]
"alice@example.org" = ["team-readers"]
"#,
    );
    if oidc {
        extra.push_str(&format!(
            r#"
[oidc]
issuer = "{}"
client_id = "sparkles"
scopes = ["openid", "email", "groups"]
algorithms = ["RS256"]
api_audience = "{API_AUD}"
"#,
            m.issuer
        ));
    }
    let s = build(Fixture {
        public_url: PUBLIC,
        extra,
        mcp: flags,
        ..Default::default()
    });
    (s, m)
}

/// An access token for alice (who writes `wiki` and reads `team-a`) with audience `aud`
/// and scope `scope`.
fn token(m: &Mock, aud: J, scope: &str) -> String {
    token_for(m, "alice@example.org", aud, scope)
}

/// [`token`] for the account `email`.
fn token_for(m: &Mock, email: &str, aud: J, scope: &str) -> String {
    m.sign(&json!({
        "iss": m.issuer,
        "sub": "u-1",
        "aud": aud,
        "iat": now(),
        "exp": now() + 300,
        "scope": scope,
        "email": email,
        "groups": ["sparkles", "kg-editors"],
    }))
}

fn rpc(method: &str, params: J) -> String {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()
}

/// A modern MCP request (protocol `2026-07-28`) as `auth`.
async fn mcp(s: &AuthServer, auth: Option<&str>, method: &str, mut params: J) -> R {
    if params.get("_meta").is_none() {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        });
    }
    let name = params
        .get("name")
        .or_else(|| params.get("uri"))
        .or_else(|| params.get("taskId"))
        .and_then(J::as_str)
        .map(str::to_string);
    let mut h = vec![
        ("accept", "application/json, text/event-stream"),
        ("content-type", "application/json"),
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", method),
    ];
    if let Some(n) = &name {
        h.push(("mcp-name", n));
    }
    if let Some(a) = auth {
        h.push(("authorization", a));
    }
    call(&s.app, "POST", "/$/mcp", &h, &rpc(method, params)).await
}

/// A tool call's JSON-RPC result.
async fn tool(s: &AuthServer, auth: &str, name: &str, args: J) -> J {
    let r = mcp(
        s,
        Some(auth),
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j.get("error").is_none(), "{j}");
    j["result"].clone()
}

fn tool_error(result: &J) -> String {
    assert_eq!(result["isError"], true, "{result}");
    result["_meta"]["io.github.kclejeune.sparkles/error"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

fn dataset_names(result: &J) -> Vec<String> {
    let mut v: Vec<String> = result["structuredContent"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

/// The `insufficient_scope` challenge of a `403`, checked; returns its `scope`.
fn challenged_scope(r: &R) -> String {
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let h = r.header("www-authenticate");
    assert!(h.starts_with("Bearer error=\"insufficient_scope\""), "{h}");
    assert!(
        h.contains(&format!("resource_metadata=\"{METADATA}\"")),
        "{h}"
    );
    assert!(h.contains("error_description=\""), "{h}");
    let scope = h.split("scope=\"").nth(1).unwrap();
    scope.split('"').next().unwrap().to_string()
}

#[tokio::test]
async fn metadata_names_the_resource_and_the_provider() {
    let (s, m) = server(Some(vec![]), true).await;
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/$/mcp",
    ] {
        let r = get_as(&s.app, path, None).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
        assert!(r.header("content-type").starts_with("application/json"));
        let j = r.json();
        assert_eq!(j["resource"], RESOURCE);
        assert_eq!(j["authorization_servers"], json!([m.issuer]));
        assert_eq!(
            j["scopes_supported"],
            json!(["sparkles:read", "sparkles:update", "sparkles:admin"])
        );
        assert_eq!(j["bearer_methods_supported"], json!(["header"]));
        // a bad credential does not hide the metadata, which is public
        let r = get_as(&s.app, path, Some("Bearer spk_nope")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
}

/// Without an `[oidc]` provider, without `--mcp`, or without auth, there is no
/// protected resource: both paths answer `404`, and `/$/mcp`'s `401` names no metadata.
#[tokio::test]
async fn metadata_is_404_unless_the_endpoint_is_a_protected_resource() {
    let paths = [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/$/mcp",
    ];
    let (no_oidc, _) = server(Some(vec!["--mcp-require-auth"]), false).await;
    let (no_mcp, _) = server(None, true).await;
    let open = build(Fixture {
        enabled: false,
        mcp: Some(vec![]),
        ..Default::default()
    });
    for s in [&no_oidc, &no_mcp, &open] {
        for p in paths {
            let r = get_as(&s.app, p, None).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text());
            assert!(r.err()["error"].as_str().unwrap().contains("[oidc]"));
        }
    }
    let r = mcp(&no_oidc, None, "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let h = r.all("www-authenticate").join(" | ");
    assert!(!h.contains("resource_metadata"), "{h}");
}

/// The `401`s of `/$/mcp` name the metadata: with no token (and the scope to ask for
/// first), and with an invalid or expired one. Other routes' `401`s do not.
#[tokio::test]
async fn unauthorized_answers_name_the_metadata() {
    // anonymous callers read `public`, so they are served unless sign-in is required
    let (open, _) = server(Some(vec![]), true).await;
    assert_eq!(
        mcp(&open, None, "tools/list", json!({})).await.status,
        StatusCode::OK
    );
    let (s, m) = server(Some(vec!["--mcp-require-auth"]), true).await;
    let r = mcp(&s, None, "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
    let bearer = r
        .all("www-authenticate")
        .into_iter()
        .find(|c| c.starts_with("Bearer"))
        .unwrap();
    assert_eq!(
        bearer,
        format!(
            "Bearer realm=\"sparkles\", resource_metadata=\"{METADATA}\", scope=\"sparkles:read\""
        )
    );
    // a token of another issuer's key is invalid
    let other = jsonwebtoken::EncodingKey::from_rsa_der(super::oidc::other_key());
    let forged = m.sign_with(
        &other,
        "k1",
        &json!({"iss": m.issuer, "aud": RESOURCE, "exp": now() + 300,
                "scope": "sparkles:read", "email": "alice@example.org", "groups": ["sparkles"]}),
    );
    let expired = m.sign(&json!({
        "iss": m.issuer, "aud": RESOURCE, "iat": now() - 7200, "exp": now() - 3600,
        "scope": "sparkles:read", "email": "alice@example.org", "groups": ["sparkles"],
    }));
    for (t, why) in [
        (&forged, "invalid credentials"),
        (&expired, "token expired"),
    ] {
        let r = mcp(&s, Some(&bearer_of(t)), "tools/list", json!({})).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
        let h = r.header("www-authenticate");
        assert_eq!(
            h,
            format!(
                "Bearer realm=\"sparkles\", error=\"invalid_token\", error_description=\"{why}\", resource_metadata=\"{METADATA}\""
            )
        );
        // elsewhere the challenge has no metadata
        let r = get_as(&s.app, "/wiki/sparql?query=ASK%7B%7D", Some(&bearer_of(t))).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert!(!r.header("www-authenticate").contains("resource_metadata"));
    }
}

fn bearer_of(t: &str) -> String {
    format!("Bearer {t}")
}

/// A token for the resource works at `/$/mcp` and nowhere else; one for another
/// audience works nowhere; one for the API audience keeps working everywhere.
#[tokio::test]
async fn the_resource_is_an_audience_of_the_mcp_endpoint_only() {
    let (s, m) = server(Some(vec!["--mcp-allow-update"]), true).await;
    let mcp_only = bearer_of(&token(&m, json!(RESOURCE), "openid sparkles:update"));
    let r = tool(&s, &mcp_only, "list_datasets", json!({})).await;
    assert_eq!(dataset_names(&r), ["team-a", "wiki"]);
    let r = tool(
        &s,
        &mcp_only,
        "sparql_update",
        json!({"dataset": "wiki", "update": INSERT}),
    )
    .await;
    assert!(r.get("isError").is_none_or(|e| e == false), "{r}");
    // refused on every other route, public ones included
    for uri in ["/wiki/sparql?query=ASK%7B%7D", "/$/whoami", "/$/datasets"] {
        let r = get_as(&s.app, uri, Some(&mcp_only)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}: {}", r.text());
    }
    assert_eq!(
        update_as(&s.app, "wiki", &mcp_only, INSERT).await.status,
        StatusCode::UNAUTHORIZED
    );
    // the wrong audience, at the endpoint and elsewhere
    let wrong = bearer_of(&token(
        &m,
        json!("https://other.example.org/$/mcp"),
        "sparkles:read",
    ));
    let r = mcp(&s, Some(&wrong), "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.header("www-authenticate").contains("invalid_token"));
    assert_eq!(
        get_as(&s.app, "/$/whoami", Some(&wrong)).await.status,
        StatusCode::UNAUTHORIZED
    );
    // a resource on another path of this server is not this resource
    let near = bearer_of(&token(
        &m,
        json!("https://sparql.example.org/$/mcp/x"),
        "sparkles:read",
    ));
    assert_eq!(
        mcp(&s, Some(&near), "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );
    // the API audience without Sparkles scopes: unchanged, on every route
    let api = bearer_of(&token(&m, json!(API_AUD), "openid"));
    let r = tool(
        &s,
        &api,
        "sparql_update",
        json!({"dataset": "wiki", "update": INSERT}),
    )
    .await;
    assert!(r.get("isError").is_none_or(|e| e == false), "{r}");
    assert_eq!(
        get_as(&s.app, "/wiki/sparql?query=ASK%7B%7D", Some(&api))
            .await
            .status,
        StatusCode::OK
    );
    // both audiences: good everywhere
    let both = bearer_of(&token(&m, json!([API_AUD, RESOURCE]), "sparkles:read"));
    assert_eq!(
        get_as(&s.app, "/wiki/sparql?query=ASK%7B%7D", Some(&both))
            .await
            .status,
        StatusCode::OK
    );
}

/// Without `--mcp`, the resource is no audience at all.
#[tokio::test]
async fn the_resource_is_no_audience_without_the_endpoint() {
    let (s, m) = server(None, true).await;
    let t = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read"));
    assert_eq!(
        get_as(&s.app, "/wiki/sparql?query=ASK%7B%7D", Some(&t))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

/// A write tool with only `sparkles:read` gets the scope challenge, though the tool is
/// listed (the grants allow it); with `sparkles:update` the call goes through.
#[tokio::test]
async fn a_missing_scope_is_challenged() {
    let (s, m) = server(Some(vec!["--mcp-allow-update"]), true).await;
    let read = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read"));
    let r = mcp(&s, Some(&read), "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let names: Vec<String> = r.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"sparql_update".to_string()), "{names:?}");
    let head = super::head(&s.state, "wiki");
    let r = mcp(
        &s,
        Some(&read),
        "tools/call",
        json!({"name": "sparql_update", "arguments": {"dataset": "wiki", "update": INSERT}}),
    )
    .await;
    assert_eq!(challenged_scope(&r), "sparkles:update");
    assert!(
        r.header("www-authenticate")
            .contains("sparql_update on wiki")
    );
    assert_eq!(super::head(&s.state, "wiki"), head, "nothing was written");
    // reads go through
    let r = tool(
        &s,
        &read,
        "sparql_query",
        json!({"dataset": "wiki", "query": "ASK {}"}),
    )
    .await;
    assert!(r.get("isError").is_none_or(|e| e == false), "{r}");
    // the same scope on the API audience narrows the other routes too
    let api_read = bearer_of(&token(&m, json!(API_AUD), "sparkles:read"));
    assert_eq!(
        update_as(&s.app, "wiki", &api_read, INSERT).await.status,
        StatusCode::FORBIDDEN
    );
    // a token for the resource with no Sparkles scope at all is asked for read first
    let none = bearer_of(&token(&m, json!(RESOURCE), "openid email"));
    let r = mcp(&s, Some(&none), "tools/list", json!({})).await;
    assert_eq!(challenged_scope(&r), "sparkles:read");
    let body = rpc(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "t", "version": "1"}}),
    );
    let r = call(
        &s.app,
        "POST",
        "/$/mcp",
        &[
            ("accept", "application/json, text/event-stream"),
            ("content-type", "application/json"),
            ("authorization", &none),
        ],
        &body,
    )
    .await;
    assert_eq!(challenged_scope(&r), "sparkles:read");
}

/// A per-dataset scope covers its dataset only.
#[tokio::test]
async fn a_dataset_scope_does_not_reach_other_datasets() {
    let (s, m) = server(Some(vec!["--mcp-allow-update"]), true).await;
    let wiki = bearer_of(&token(
        &m,
        json!(RESOURCE),
        "sparkles:read:wiki sparkles:update:wiki",
    ));
    let r = tool(&s, &wiki, "list_datasets", json!({})).await;
    assert_eq!(dataset_names(&r), ["wiki"]);
    let r = mcp(
        &s,
        Some(&wiki),
        "tools/call",
        json!({"name": "sparql_query", "arguments": {"dataset": "team-a", "query": "ASK {}"}}),
    )
    .await;
    assert_eq!(challenged_scope(&r), "sparkles:read");
    assert!(r.header("www-authenticate").contains("on team-a"));
    let r = mcp(
        &s,
        Some(&wiki),
        "resources/read",
        json!({"uri": "sparkles://team-a/prefixes"}),
    )
    .await;
    assert_eq!(challenged_scope(&r), "sparkles:read");
    // read on team-a does not write wiki's neighbor either
    let team = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read:team-a"));
    let r = mcp(
        &s,
        Some(&team),
        "tools/call",
        json!({"name": "sparql_update", "arguments": {"dataset": "wiki", "update": INSERT}}),
    )
    .await;
    assert_eq!(challenged_scope(&r), "sparkles:update");
}

/// A scope never adds access: `sparkles:admin` leaves a dataset the grants hide
/// hidden, without a challenge (asking for a scope would not help), and a write the
/// grants do not allow is the tool's refusal.
#[tokio::test]
async fn a_scope_never_adds_access() {
    let (s, m) = server(Some(vec!["--mcp-allow-update"]), true).await;
    let admin = bearer_of(&token(&m, json!(RESOURCE), "sparkles:admin"));
    let r = tool(
        &s,
        &admin,
        "sparql_query",
        json!({"dataset": "secret", "query": "ASK {}"}),
    )
    .await;
    assert_eq!(tool_error(&r), "unknown-dataset");
    let r = tool(&s, &admin, "list_datasets", json!({})).await;
    assert_eq!(dataset_names(&r), ["team-a", "wiki"]);
    // alice only reads team-a
    let r = mcp(
        &s,
        Some(&admin),
        "tools/call",
        json!({"name": "sparql_update", "arguments": {"dataset": "team-a", "update": INSERT}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["result"]["isError"], true, "{}", r.text());
    // nor does a read-only scope get past a hidden dataset with a challenge
    let read = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read"));
    let r = mcp(
        &s,
        Some(&read),
        "tools/call",
        json!({"name": "sparql_update", "arguments": {"dataset": "secret", "update": INSERT}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["result"]["isError"], true);
}

/// A scope target comes from what the message acts on: a stored query's tool is bound to
/// its dataset even when a parameter is named `dataset`, and a prompt names its dataset.
#[tokio::test]
async fn scope_targets_follow_the_operation() {
    let (s, m) = server(Some(vec![]), true).await;
    let def: sparkles::stored::Definition = serde_json::from_value(json!({
        "query": "SELECT ?x WHERE { BIND(?dataset AS ?x) }",
        "parameters": { "dataset": { "type": "string" } }
    }))
    .unwrap();
    s.state.datasets()["wiki"]
        .queries
        .put("param", def, sparkles::stored::Change::default())
        .unwrap();
    let wiki = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read:wiki"));
    let r = tool(&s, &wiki, "wiki__param", json!({"dataset": "team-a"})).await;
    assert!(r.get("isError").is_none_or(|e| e == false), "{r}");
    let r = mcp(
        &s,
        Some(&wiki),
        "prompts/get",
        json!({"name": "explore_dataset", "arguments": {"dataset": "team-a"}}),
    )
    .await;
    assert_eq!(challenged_scope(&r), "sparkles:read");
}

/// A task belongs to its caller's account and scopes together: a token of the same
/// account with narrower scopes cannot read the result of a broader one's task.
#[tokio::test(flavor = "multi_thread")]
async fn tasks_do_not_cross_scopes() {
    let (s, m) = server(Some(vec!["--mcp-task-after-ms", "0"]), true).await;
    let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3000 {
        data.push_str(&format!("ex:s{i} ex:v {i} .\n"));
    }
    s.state.datasets()["team-a"]
        .store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {
            "extensions": {"io.modelcontextprotocol/tasks": {}}
        }
    });
    let broad = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read"));
    let narrow = bearer_of(&token(&m, json!(RESOURCE), "sparkles:read:wiki"));
    let r = mcp(
        &s,
        Some(&broad),
        "tools/call",
        json!({"_meta": meta, "name": "sparql_query", "arguments": {"dataset": "team-a",
               "query": "PREFIX ex: <http://ex.org/> SELECT (COUNT(*) AS ?n) WHERE { ?a ex:v ?x . ?b ex:v ?y }"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let res = r.json()["result"].clone();
    assert_eq!(res["resultType"], "task", "{res}");
    let id = res["taskId"].as_str().unwrap().to_string();
    let get = |auth: String| {
        let (meta, id) = (meta.clone(), id.clone());
        let s = &s;
        async move {
            mcp(
                s,
                Some(&auth),
                "tasks/get",
                json!({"_meta": meta, "taskId": id}),
            )
            .await
            .json()
        }
    };
    let r = get(narrow.clone()).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let mut task = J::Null;
    for _ in 0..600 {
        task = get(broad.clone()).await["result"].clone();
        if task["status"] != "working" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(task["status"], "completed", "{task}");
    // still hidden from the narrower token once it is done
    let r = get(narrow).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // and from another account whose name spells the first one's name and scopes
    let other = bearer_of(&token_for(
        &m,
        "alice@example.org#sparkles:read",
        json!(API_AUD),
        "openid",
    ));
    let r = get(other.clone()).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let other = bearer_of(&token_for(
        &m,
        "alice@example.org\",[\"sparkles:read\"]]",
        json!(API_AUD),
        "openid",
    ));
    let r = get(other).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
}
