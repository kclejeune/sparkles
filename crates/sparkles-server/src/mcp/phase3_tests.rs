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
async fn list_changes() {
    let server = fixture_server();
    let ds = server.state.get("t").unwrap();
    let update = |u: &str| {
        let opts = sparkles::sparql::QueryOptions {
            write: sparkles::guard::WriteOptions {
                message: Some("rename".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        sparkles::sparql::update::update(
            &ds.store,
            &format!("PREFIX ex: <http://ex.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {u}"),
            &opts,
        )
        .unwrap();
    };
    update(
        "DELETE DATA { ex:bob rdfs:label \"Bob\" } ; INSERT DATA { ex:bob rdfs:label \"Robert\" }",
    );
    update("INSERT DATA { ex:carol a ex:Person }");
    let mut c = Client::start(server);
    // the labels bob has had
    let s = c
        .structured(
            "list_changes",
            json!({"subjects": ["ex:bob"], "predicates": ["rdfs:label"]}),
        )
        .await;
    let rows: Vec<(String, String, u64)> = s["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["op"].as_str().unwrap().to_string(),
                c["quad"].as_str().unwrap().to_string(),
                c["commit"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "add".to_string(),
                "ex:bob rdfs:label \"Bob\"".to_string(),
                1
            ),
            (
                "remove".to_string(),
                "ex:bob rdfs:label \"Bob\"".to_string(),
                2
            ),
            (
                "add".to_string(),
                "ex:bob rdfs:label \"Robert\"".to_string(),
                2
            ),
        ],
        "{s}"
    );
    assert_eq!(s["changes"][1]["message"], "rename");
    assert_eq!(s["head"], 3);
    // newest first, additions only, from commit 2 on, by object
    let s = c
        .structured(
            "list_changes",
            json!({"objects": ["ex:Person"], "op": "add", "from": 2, "order": "desc", "limit": 1}),
        )
        .await;
    assert_eq!(
        s["changes"][0]["quad"], "ex:carol rdf:type ex:Person",
        "{s}"
    );
    assert_eq!(s["truncated"], false);
    let s = c
        .structured("list_changes", json!({"objects": ["\"Robert\""]}))
        .await;
    assert_eq!(s["changes"].as_array().unwrap().len(), 1, "{s}");
    let (_, e) = c.error("list_changes", json!({"from": "yesterday"})).await;
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

/// Save a stored query of `ds`.
fn put_query(ds: &crate::state::Dataset, name: &str, def: Value) {
    let d: sparkles::stored::Definition = serde_json::from_value(def).unwrap();
    ds.queries
        .put(name, d, sparkles::stored::Change::default())
        .unwrap();
}

/// `completion/complete` of argument `arg` of `reference`.
async fn complete(c: &mut Client, reference: Value, arg: &str, value: &str, ctx: Value) -> Value {
    let r = c
        .request(
            40,
            "completion/complete",
            json!({"_meta": m(), "ref": reference, "argument": {"name": arg, "value": value},
                   "context": {"arguments": ctx}}),
        )
        .await;
    r.get("result")
        .map_or(r.clone(), |res| res["completion"].clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn completions() {
    let server = server_with(
        &[
            ("alpha", &[FIXTURE]),
            ("beta", &[FIXTURE]),
            ("other", &[CHAIN]),
        ],
        McpConfig::default(),
    );
    let alpha = server.state.get("alpha").unwrap();
    sparkles::sparql::update::update(
        &alpha.store,
        "INSERT DATA { GRAPH <http://ex.org/g/one> { <http://ex.org/x> <http://ex.org/y> 1 } GRAPH <http://ex.org/g/two> { <http://ex.org/x> <http://ex.org/y> 2 } GRAPH <http://other.org/g> { <http://ex.org/x> <http://ex.org/y> 3 } }",
        &sparkles::sparql::QueryOptions::default(),
    )
    .unwrap();
    put_query(
        &alpha,
        "older",
        json!({"query": "SELECT ?p WHERE { ?p <http://ex.org/age> ?a FILTER(?a >= ?min && ?a <= ?max) OPTIONAL { ?p <http://ex.org/label> ?label } }",
               "parameters": {"min": {"type": "integer"}, "max": {"type": "integer"}, "label": {"type": "string", "required": false}}}),
    );
    put_query(&alpha, "everything", json!({"query": "ASK { ?s ?p ?o }"}));
    let mut c = Client::start(server);
    let prompt = |name: &str| json!({"type": "ref/prompt", "name": name});
    // dataset names
    let r = complete(&mut c, prompt("explore_dataset"), "dataset", "", json!({})).await;
    assert_eq!(
        r,
        json!({"values": ["alpha", "beta", "other"], "total": 3, "hasMore": false})
    );
    let r = complete(&mut c, prompt("answer_question"), "dataset", "b", json!({})).await;
    assert_eq!(r["values"], json!(["beta"]));
    // resource templates
    let r = complete(
        &mut c,
        json!({"type": "ref/resource", "uri": "sparkles://{dataset}/schema"}),
        "dataset",
        "o",
        json!({}),
    )
    .await;
    assert_eq!(r["values"], json!(["other"]));
    // stored queries: only the datasets that have some, then their names
    let r = complete(&mut c, prompt("run_stored_query"), "dataset", "", json!({})).await;
    assert_eq!(r["values"], json!(["alpha"]));
    let r = complete(
        &mut c,
        json!({"type": "ref/resource", "uri": "sparkles://{dataset}/queries/{query}"}),
        "query",
        "",
        json!({"dataset": "alpha"}),
    )
    .await;
    assert_eq!(r["values"], json!(["everything", "older"]));
    // parameter names, leaving out those already given
    let ctx = json!({"dataset": "alpha", "query": "older"});
    let r = complete(
        &mut c,
        prompt("run_stored_query"),
        "arguments",
        "",
        ctx.clone(),
    )
    .await;
    assert_eq!(r["values"], json!(["label=", "max=", "min="]));
    let r = complete(
        &mut c,
        prompt("run_stored_query"),
        "arguments",
        "min=3, m",
        ctx,
    )
    .await;
    assert_eq!(r["values"], json!(["min=3, max="]));
    // named graphs, by the typed start of their IRI
    let ctx = json!({"dataset": "alpha"});
    let r = complete(
        &mut c,
        prompt("explore_dataset"),
        "graph",
        "http://ex.org/",
        ctx.clone(),
    )
    .await;
    assert_eq!(
        r["values"],
        json!(["http://ex.org/g/one", "http://ex.org/g/two"])
    );
    let r = complete(&mut c, prompt("explore_dataset"), "graph", "", ctx.clone()).await;
    assert_eq!(r["total"], 3);
    // prefixes
    let r = complete(&mut c, prompt("explain_term"), "term", "rd", ctx).await;
    assert_eq!(r["values"], json!(["rdf:", "rdfs:"]));
    // free text and unknown references
    let r = complete(
        &mut c,
        prompt("answer_question"),
        "question",
        "Who",
        json!({"dataset": "alpha"}),
    )
    .await;
    assert_eq!(r["values"], json!([]));
    let r = complete(&mut c, prompt("nope"), "dataset", "", json!({})).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let r = complete(&mut c, prompt("explore_dataset"), "query", "", json!({})).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");

    // the new prompts and the stored-query resource
    let r = c
        .request(
            41,
            "prompts/get",
            json!({"_meta": m(), "name": "run_stored_query",
                   "arguments": {"dataset": "alpha", "query": "older", "arguments": "min=3"}}),
        )
        .await;
    let text = r["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("with the tool alpha__older"), "{text}");
    assert!(text.contains("- max (integer, required)"), "{text}");
    assert!(text.contains("Use these arguments: min=3"), "{text}");
    let r = c
        .request(
            42,
            "resources/read",
            json!({"_meta": m(), "uri": "sparkles://alpha/queries/older"}),
        )
        .await;
    let q: Value =
        serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (q["tool"].clone(), q["version"].clone()),
        (json!("alpha__older"), json!(1))
    );
    let r = c.request(43, "resources/list", json!({"_meta": m()})).await;
    let uris: Vec<&str> = r["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"sparkles://alpha/queries/older"), "{uris:?}");
}

/// A server whose subscriptions look for changes every 20 ms.
fn quick_server() -> McpServer {
    server_with(
        &[("t", &[FIXTURE])],
        McpConfig {
            watch_interval: Duration::from_millis(20),
            ..McpConfig::default()
        },
    )
}

/// The next message that is not `notifications/subscriptions/acknowledged`.
async fn next_change(c: &mut Client) -> Value {
    loop {
        let v = c
            .recv_within(Duration::from_secs(10))
            .await
            .expect("no notification");
        if v["method"] != "notifications/subscriptions/acknowledged" {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_listen() {
    let server = quick_server();
    let ds = server.state.get("t").unwrap();
    let mut c = Client::start(server.clone());
    c.send(
        json!({"jsonrpc": "2.0", "id": 9, "method": "subscriptions/listen", "params": {
        "_meta": m(),
        "notifications": {"toolsListChanged": true, "resourcesListChanged": true,
            "resourceSubscriptions": ["sparkles://t/schema", "sparkles://elsewhere/schema"]}}}),
    )
    .await;
    let ack = c.recv().await;
    assert_eq!(
        ack["method"], "notifications/subscriptions/acknowledged",
        "{ack}"
    );
    // the acknowledgment comes before the subscription takes its first look
    tokio::time::sleep(Duration::from_millis(200)).await;
    // a stored query added changes the tool list
    put_query(&ds, "everything", json!({"query": "ASK { ?s ?p ?o }"}));
    let n = next_change(&mut c).await;
    assert_eq!(n["method"], "notifications/tools/list_changed", "{n}");
    // a commit updates the schema resource
    sparkles::sparql::update::update(
        &ds.store,
        "INSERT DATA { <http://ex.org/carol> a <http://ex.org/Person> }",
        &sparkles::sparql::QueryOptions::default(),
    )
    .unwrap();
    let n = next_change(&mut c).await;
    assert_eq!(n["method"], "notifications/resources/updated", "{n}");
    assert_eq!(n["params"]["uri"], "sparkles://t/schema");
    // a dataset created changes the resource list
    server.state.attach("u", DbType::Mem, None).unwrap();
    let n = next_change(&mut c).await;
    assert_eq!(n["method"], "notifications/resources/list_changed", "{n}");
    // the stored query removed: the tool list again
    ds.queries.delete("everything", None).unwrap();
    let n = next_change(&mut c).await;
    assert_eq!(n["method"], "notifications/tools/list_changed", "{n}");
    // cancelled: no more notifications, and the stream's slot is free again
    c.send(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 9}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    put_query(&ds, "again", json!({"query": "ASK { ?s ?p ?o }"}));
    while let Some(v) = c.recv_within(Duration::from_millis(300)).await {
        assert_ne!(v["method"], "notifications/tools/list_changed", "{v}");
    }
    assert_eq!(
        server.shared.subscriptions.available_permits(),
        super::notify::MAX_SUBSCRIPTIONS
    );
    // the capabilities announce the notifications
    let r = c
        .request(10, "server/discover", json!({"_meta": m()}))
        .await;
    let caps = &r["result"]["capabilities"];
    assert_eq!(caps["tools"]["listChanged"], true, "{caps}");
    assert_eq!(caps["resources"]["subscribe"], true, "{caps}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_get_list_changes() {
    let server = quick_server();
    let ds = server.state.get("t").unwrap();
    let mut c = Client::start(server);
    let r = c
        .request(
            1,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "t", "version": "1"}}),
        )
        .await;
    let caps = &r["result"]["capabilities"];
    assert_eq!(caps["tools"]["listChanged"], true, "{caps}");
    // the legacy era has no subscriptions to resources
    assert!(caps["resources"].get("subscribe").is_none(), "{caps}");
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    put_query(&ds, "everything", json!({"query": "ASK { ?s ?p ?o }"}));
    let n = c
        .recv_within(Duration::from_secs(10))
        .await
        .expect("no notification");
    assert_eq!(n["method"], "notifications/tools/list_changed", "{n}");
}

/// `_meta` of a modern request from a client that supports tasks.
fn m_tasks() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {
            "extensions": {"io.modelcontextprotocol/tasks": {}}
        }
    })
}

/// Poll `tasks/get` until the task ends.
async fn finished(c: &mut Client, id: &str) -> Value {
    for i in 0..600 {
        let r = c
            .request(
                500 + i,
                "tasks/get",
                json!({"_meta": m_tasks(), "taskId": id}),
            )
            .await;
        let task = r["result"].clone();
        if ["completed", "failed", "cancelled"].contains(&task["status"].as_str().unwrap_or("")) {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("task {id} did not end");
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks() {
    let server = server_with(
        &[("t", &[&numbers(3000)])],
        McpConfig {
            task_after: Duration::from_millis(0),
            ..McpConfig::default()
        },
    );
    let mut c = Client::start(server.clone());
    // the extension is announced
    let r = c.request(1, "server/discover", json!({"_meta": m()})).await;
    assert!(
        r["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"].is_object(),
        "{r}"
    );
    // a slow call becomes a task
    let slow = "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:v ?x . ?b ex:v ?y }";
    let r = c
        .request(
            2,
            "tools/call",
            json!({"_meta": m_tasks(), "name": "sparql_query", "arguments": {"query": slow}}),
        )
        .await;
    let res = &r["result"];
    assert_eq!(res["resultType"], "task", "{r}");
    let id = res["taskId"].as_str().unwrap().to_string();
    assert_eq!(res["status"], "working");
    let task = finished(&mut c, &id).await;
    assert_eq!(task["status"], "completed", "{task}");
    let text = task["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.ends_with("9000000"), "{text}");
    // a task stopped with tasks/cancel
    let slower = "SELECT * WHERE { ?a ex:v ?x . ?b ex:v ?y . ?c ex:v ?z }";
    let r = c
        .request(
            3,
            "tools/call",
            json!({"_meta": m_tasks(), "name": "sparql_query",
                   "arguments": {"query": slower, "timeoutSeconds": 60}}),
        )
        .await;
    let id = r["result"]["taskId"].as_str().unwrap().to_string();
    let r = c
        .request(4, "tasks/cancel", json!({"_meta": m_tasks(), "taskId": id}))
        .await;
    assert!(r.get("error").is_none(), "{r}");
    let task = finished(&mut c, &id).await;
    assert_eq!(task["status"], "cancelled", "{task}");
    // the engine stopped: every slot is free again
    for _ in 0..100 {
        if server.shared.slots.available_permits() == server.cfg().max_concurrent {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        server.shared.slots.available_permits(),
        server.cfg().max_concurrent
    );
    // an unknown task
    let r = c
        .request(
            5,
            "tasks/get",
            json!({"_meta": m_tasks(), "taskId": "nope"}),
        )
        .await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // a client without the extension gets the result itself
    let r = c
        .request(
            6,
            "tools/call",
            json!({"_meta": m(), "name": "sparql_query", "arguments": {"query": slow}}),
        )
        .await;
    assert_eq!(r["result"]["resultType"], "complete", "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks_belong_to_their_caller() {
    let tasks = super::tasks::Tasks::default();
    let work = tokio::spawn(async { rmcp::model::CallToolResult::success(Vec::new()) });
    let task = tasks.start("alice".into(), work, Arc::default());
    assert!(tasks.get("alice", &task.task_id).is_ok());
    assert!(tasks.get("bob", &task.task_id).is_err());
    assert!(tasks.cancel("bob", &task.task_id).is_err());
}
