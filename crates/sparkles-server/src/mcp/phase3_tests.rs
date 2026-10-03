//! The tools and protocol features of C11 Phase 3 over the same JSON-RPC client as the
//! other tools: path search, DESCRIBE modes, GraphQL, completions, subscriptions and
//! tasks.

use super::*;

/// A chain `ex:a → ex:b → ex:c → ex:d` of `ex:next`, with a shortcut `ex:a ex:jump ex:c`.
const CHAIN: &str = r#"@prefix ex: <http://ex.org/> .
ex:a ex:next ex:b . ex:b ex:next ex:c . ex:c ex:next ex:d .
ex:a ex:jump ex:c .
"#;

#[tokio::test(flavor = "multi_thread")]
async fn find_paths() {
    let mut c = Client::start(server_with(&[("t", &[CHAIN])], McpConfig::default()));
    let s = c
        .structured(
            "find_paths",
            json!({"source": "ex:a", "target": "ex:d", "predicates": ["ex:next"]}),
        )
        .await;
    assert_eq!(s["algorithm"], "shortest");
    assert_eq!(s["commit"], 1);
    assert_eq!(
        s["paths"],
        json!([{"source": "ex:a", "target": "ex:d", "length": 3, "cost": 3,
            "edges": ["ex:a ex:next ex:b", "ex:b ex:next ex:c", "ex:c ex:next ex:d"]}])
    );
    assert_eq!(s["prefixes"], json!({"ex": "http://ex.org/"}));
    // every predicate: the shortcut makes it shorter
    let s = c
        .structured("find_paths", json!({"source": "ex:a", "target": "ex:d"}))
        .await;
    assert_eq!(s["paths"][0]["length"], 2);
    // all paths up to 3 edges
    let s = c
        .structured(
            "find_paths",
            json!({"source": "ex:a", "target": "ex:d", "algorithm": "all", "maxLength": 3}),
        )
        .await;
    assert_eq!(s["paths"].as_array().unwrap().len(), 2, "{s}");
    // one end only: the nodes reachable from it, at most `limit`
    let s = c
        .structured(
            "find_paths",
            json!({"source": "ex:a", "predicates": ["ex:next"], "limit": 2}),
        )
        .await;
    let ends: Vec<&str> = s["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["target"].as_str().unwrap())
        .collect();
    assert_eq!(ends, ["ex:b", "ex:c"]);
    assert_eq!(s["limited"], true);
    // backwards from the target
    let s = c
        .structured(
            "find_paths",
            json!({"target": "ex:c", "predicates": ["ex:next"], "direction": "forward"}),
        )
        .await;
    let starts: Vec<&str> = s["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["source"].as_str().unwrap())
        .collect();
    assert_eq!(starts, ["ex:b", "ex:a"]);
    // errors
    let (t, e) = c.error("find_paths", json!({})).await;
    assert!(t.starts_with("source or target is required"), "{t}");
    assert_eq!(e["code"], "bad-argument");
    let (t, _) = c
        .error(
            "find_paths",
            json!({"source": "ex:a", "target": "ex:d", "algorithm": "all"}),
        )
        .await;
    assert!(t.contains("path:maxLength"), "{t}");
    let (_, e) = c
        .error(
            "find_paths",
            json!({"source": "ex:a", "target": "ex:d", "predicates": ["not an iri"]}),
        )
        .await;
    assert_eq!(e["code"], "bad-argument");
}

#[tokio::test(flavor = "multi_thread")]
async fn describe_modes() {
    let ttl = r#"@prefix ex: <http://ex.org/> .
ex:alice ex:knows ex:bob ; ex:address [ ex:city "Paris" ] .
ex:carol ex:knows ex:alice .
"#;
    let mut c = Client::start(server_with(&[("t", &[FIXTURE, ttl])], McpConfig::default()));
    let s = c
        .structured(
            "describe_resource",
            json!({"iri": "ex:alice", "mode": "cbd", "direction": "outgoing"}),
        )
        .await;
    let d = &s["description"];
    assert_eq!(d["mode"], "cbd");
    let triples: Vec<&str> = d["triples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    // the concise bounded description follows the blank node
    assert!(triples.contains(&"ex:alice ex:knows ex:bob"), "{triples:?}");
    assert!(
        triples.iter().any(|t| t.ends_with("ex:city \"Paris\"")),
        "{triples:?}"
    );
    assert!(!triples.iter().any(|t| t.starts_with("ex:carol")));
    assert_eq!(d["truncated"], false);
    // a blank node of an earlier result
    let address = triples
        .iter()
        .find_map(|t| t.strip_prefix("ex:alice ex:address "))
        .unwrap()
        .to_string();
    let s = c
        .structured(
            "describe_resource",
            json!({"iri": address, "mode": "outgoing", "direction": "outgoing"}),
        )
        .await;
    assert_eq!(
        s["description"]["triples"],
        json!([format!("{address} ex:city \"Paris\"")])
    );
    // the symmetric one adds the incoming triples
    let s = c
        .structured(
            "describe_resource",
            json!({"iri": "ex:alice", "mode": "scbd"}),
        )
        .await;
    assert!(
        s["description"]["triples"]
            .as_array()
            .unwrap()
            .contains(&json!("ex:carol ex:knows ex:alice")),
        "{s}"
    );
    // capped by maxTriples
    let s = c
        .structured(
            "describe_resource",
            json!({"iri": "ex:alice", "mode": "outgoing", "maxTriples": 1}),
        )
        .await;
    assert_eq!(s["description"]["triples"].as_array().unwrap().len(), 1);
    assert_eq!(s["description"]["truncated"], true);
    // without mode there is no description
    let s = c
        .structured("describe_resource", json!({"iri": "ex:alice"}))
        .await;
    assert!(s.get("description").is_none());
    let (_, e) = c
        .error(
            "describe_resource",
            json!({"iri": "ex:alice", "mode": "all"}),
        )
        .await;
    assert_eq!(e["code"], "bad-argument");
}

#[cfg(feature = "graphql")]
#[tokio::test(flavor = "multi_thread")]
async fn graphql_query() {
    let server = fixture_server();
    let mut c = Client::start(server.clone());
    let names = |r: &Value| -> Vec<String> {
        r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    // no schema installed: the tool is neither listed nor callable
    let r = c.request(1, "tools/list", json!({"_meta": m()})).await;
    assert!(!names(&r).contains(&"graphql_query".to_string()));
    let r = c
        .request(
            2,
            "tools/call",
            json!({"_meta": m(), "name": "graphql_query", "arguments": {"query": "{ x }"}}),
        )
        .await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let ds = server.state.get("t").unwrap();
    let sdl = "extend schema @rdf(vocab: \"http://ex.org/\") @prefix(name: \"ex\", iri: \"http://ex.org/\")\ntype Person { age: Int knows: [Person!]! }\n";
    ds.graphql
        .put(
            sparkles_graphql::Config::new(sdl.to_string()),
            Default::default(),
            &|_, _, _| true,
        )
        .unwrap();
    let r = c.request(3, "tools/list", json!({"_meta": m()})).await;
    let listed = names(&r);
    assert!(listed.contains(&"graphql_query".to_string()), "{listed:?}");
    let d = c.structured("list_datasets", json!({})).await;
    assert_eq!(d["datasets"][0]["graphql"], true);
    // without query: the API schema
    let sdl = c.text("graphql_query", json!({})).await;
    assert!(sdl.contains("type Person"), "{sdl}");
    let out: Value = serde_json::from_str(
        &c.text(
            "graphql_query",
            json!({"query": "{ allPerson(orderBy: [AGE_ASC]) { totalCount nodes { id age knows { id } } } }"}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(out["dataset"], "t");
    assert_eq!(out["commit"], 1);
    let all = &out["data"]["allPerson"];
    assert_eq!(all["totalCount"], 2, "{out}");
    assert_eq!(all["nodes"][0]["id"], "http://ex.org/bob");
    assert_eq!(all["nodes"][1]["knows"][0]["id"], "http://ex.org/bob");
    // variables
    let out: Value = serde_json::from_str(
        &c.text(
            "graphql_query",
            json!({"query": "query($a: Int) { allPerson(filter: { age: { gt: $a } }) { totalCount } }", "variables": {"a": 26}}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(out["data"]["allPerson"]["totalCount"], 1, "{out}");
    let (_, e) = c.error("graphql_query", json!({"query": "{ nope }"})).await;
    assert_eq!(e["code"], "graphql-error");
}
