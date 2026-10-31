//! The lint tool, over the same JSON-RPC client as the other tools.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn lints_like_sparkles_lint() {
    let mut c = Client::start(fixture_server());
    let q = "PREFIX ex: <http://ex.org/>\nPREFIX unused: <http://u/>\nSELECT ?s ?name { ?s ex:p \"x\"@en-us }";
    let r = c.structured("lint", json!({"text": q})).await;
    assert_eq!(r["language"], "sparql");
    let d = r["diagnostics"].as_array().unwrap();
    let rules: Vec<&str> = d.iter().map(|d| d["rule"].as_str().unwrap()).collect();
    assert_eq!(
        rules,
        ["unused-prefix", "unbound-variable", "language-tag-case"]
    );
    let first = &d[0];
    assert_eq!(first["severity"], "warning");
    assert_eq!(
        (first["line"].as_u64(), first["column"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(
        (first["endLine"].as_u64(), first["endColumn"].as_u64()),
        (Some(2), Some(27))
    );
    assert_eq!(first["from"], 28);
    assert_eq!(first["fix"]["edits"][0]["insert"], "");
    assert!(first["message"].as_str().unwrap().contains("unused:"));
    assert!(r.get("fixed").is_none());
    assert_eq!(
        r["counts"],
        json!({"error": 0, "warning": 3, "info": 0, "hint": 0})
    );

    // the same findings as the library
    let direct = sparkles_fmt::lint::lint(
        q,
        sparkles_fmt::Language::Sparql,
        &sparkles_fmt::lint::LintOptions::default(),
    )
    .unwrap();
    assert_eq!(direct.diagnostics.len(), d.len());

    // every language the linter takes
    for (text, lang, rule) in [
        (
            "<http://x/s> <http://x/p> \"a\"@EN .\n",
            "ntriples",
            "language-tag-case",
        ),
        (
            "<http://x/s> <http://x/p> \"a\"@EN <http://x/g> .\n",
            "nquads",
            "language-tag-case",
        ),
        (
            r#"{"@context": {"a": "http://x/a"}, "@id": "http://x/s"}"#,
            "jsonld",
            "unused-context-term",
        ),
        (
            "PREFIX u: <http://u/>\nshape <http://x/S> { <http://x/p> IRI . }",
            "shaclc",
            "unused-prefix",
        ),
        (
            "<http://x/s> <http://x/p> \"a\"@EN .\n",
            "turtle",
            "language-tag-case",
        ),
    ] {
        let r = c
            .structured("lint", json!({"text": text, "language": lang}))
            .await;
        assert_eq!(r["language"], lang);
        assert_eq!(r["diagnostics"][0]["rule"], rule, "{lang}: {r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shacl_queries_are_linted() {
    let mut c = Client::start(fixture_server());
    let ttl = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix ex: <http://ex.org/> .\nex:S sh:sparql [ sh:select \"SELECT $this { $this ex:p ?o FILTER(?bad = ?o) }\" ] .\n";
    let r = c.structured("lint", json!({"text": ttl})).await;
    assert_eq!(r["language"], "turtle");
    assert_eq!(r["diagnostics"][0]["rule"], "unbound-variable", "{r}");
    assert_eq!(r["diagnostics"][0]["line"], 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn syntax_errors_are_findings() {
    let mut c = Client::start(fixture_server());
    let r = c
        .structured("lint", json!({"text": "SELECT * WHERE { ?s ?p }"}))
        .await;
    assert_eq!(r["diagnostics"][0]["rule"], "syntax");
    assert_eq!(r["diagnostics"][0]["severity"], "error");
    assert_eq!(r["counts"]["error"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn options_set_levels_in_camel_case() {
    let mut c = Client::start(fixture_server());
    let q = "PREFIX unused: <http://u/>\nSELECT ?s { ?s ?p ?o }";
    let r = c
        .structured(
            "lint",
            json!({"text": q, "options": {"unusedPrefix": "off", "singleUseVariable": "off"}}),
        )
        .await;
    assert_eq!(r["diagnostics"], json!([]));
    let r = c
        .structured(
            "lint",
            json!({"text": q, "options": {"unusedPrefix": "error", "single-use-variable": "off"}}),
        )
        .await;
    assert_eq!(r["diagnostics"][0]["severity"], "error");
    assert_eq!(r["counts"]["error"], 1);
    for (options, needle) in [
        (json!({"noSuchRule": "off"}), "options.noSuchRule"),
        (json!({"unusedPrefix": "loud"}), "options.unusedPrefix"),
        (json!({"unusedPrefix": 3}), "options.unusedPrefix"),
        (json!({"syntax": "off"}), "options.syntax"),
        (json!([]), "`options` must be an object"),
    ] {
        let (text, meta) = c
            .error("lint", json!({"text": q, "options": options}))
            .await;
        assert_eq!(meta["code"], "bad-argument", "{text}");
        assert!(text.contains(needle), "{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fix_returns_the_fixed_text() {
    let mut c = Client::start(fixture_server());
    let q =
        "PREFIX ex: <http://x/>\nPREFIX unused: <http://u/>\nSELECT ?s { ?s ex:p \"a\"@en-us }\n";
    let r = c.structured("lint", json!({"text": q, "fix": true})).await;
    assert_eq!(
        r["fixed"],
        "PREFIX ex: <http://x/>\nSELECT ?s { ?s ex:p \"a\"@en-US }\n"
    );
    assert_eq!(r["applied"], 2);
    // the findings are those of the fixed text
    assert_eq!(r["diagnostics"], json!([]));
    // without a fix to make, the text comes back as it is
    let r = c
        .structured("lint", json!({"text": "ASK { ?s ?p ?o }", "fix": true}))
        .await;
    assert_eq!(r["fixed"], "ASK { ?s ?p ?o }");
    assert_eq!(r["applied"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_and_limits() {
    let mut c = Client::start(fixture_server());
    for args in [
        json!({"text": "x", "language": "cobol"}),
        json!({"text": "   "}),
        json!({"text": "ASK {}", "dataset": "ds"}),
        json!({"text": "ASK {}", "timeoutSeconds": 0}),
        json!({"text": "ASK {}", "fix": "yes"}),
    ] {
        let (text, meta) = c.error("lint", args.clone()).await;
        assert_eq!(meta["code"], "bad-argument", "{args}: {text}");
    }
    let (text, meta) = c
        .error("lint", json!({"text": "<?xml version=\"1.0\"?><rdf:RDF/>"}))
        .await;
    assert_eq!(meta["code"], "unsupported-language", "{text}");
    let big = format!("ASK {{ {} }}", "?s ?p ?o . ".repeat(110_000));
    let (text, meta) = c.error("lint", json!({"text": big})).await;
    assert_eq!(meta["code"], "bad-argument", "{text}");
    assert!(text.contains("at most 1048576 characters"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn results_respect_max_bytes() {
    // many findings are cut to fit, and the count says how many there were
    let cfg = McpConfig {
        max_bytes: 4096,
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[], cfg));
    let text: String = (0..300)
        .map(|i| format!("<http://x/s{i}> <http://x/p> \"v\"@EN .\n"))
        .collect();
    let r = c
        .structured("lint", json!({"text": text, "language": "ntriples"}))
        .await;
    assert_eq!(r["truncated"], true, "{r}");
    assert_eq!(r["total"], 300);
    assert_eq!(r["counts"]["warning"], 300);
    let shown = r["diagnostics"].as_array().unwrap().len();
    assert!(shown > 0 && shown < 300, "{shown}");
    assert!(r.to_string().len() <= 4096, "{}", r.to_string().len());
    // a fixed text that does not fit is refused, as the formatter's is
    let (text, meta) = c
        .error(
            "lint",
            json!({"text": text, "language": "ntriples", "fix": true}),
        )
        .await;
    assert_eq!(meta, json!({"code": "too-large", "status": 413}), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fixed_text_is_budgeted_as_serialized() {
    // the raw text fits the cap, but its JSON escapes do not
    let cfg = McpConfig {
        max_bytes: 4096,
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[], cfg));
    let text = format!("ASK {{}} #{}", "\"".repeat(3000));
    assert!(text.len() < 4096);
    let (msg, meta) = c
        .error(
            "lint",
            json!({"text": text, "language": "sparql", "fix": true}),
        )
        .await;
    assert_eq!(meta, json!({"code": "too-large", "status": 413}), "{msg}");
    // a fixed text that fits leaves the findings what is left, escapes and all
    let text = format!("PREFIX u: <http://u/>\nASK {{}} #{}", "\"".repeat(1500));
    let r = c
        .structured(
            "lint",
            json!({"text": text, "language": "sparql", "fix": true}),
        )
        .await;
    assert_eq!(r["applied"], 1, "{r}");
    assert!(r.to_string().len() <= 4096, "{}", r.to_string().len());
}
