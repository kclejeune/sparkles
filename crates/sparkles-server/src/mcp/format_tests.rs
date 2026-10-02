//! The format tool, over the same JSON-RPC client as the other tools.

use super::*;

/// What `sparkles fmt` and `POST /$/format` make of `text`.
fn formatted(text: &str, lang: sparkles_fmt::Language) -> String {
    sparkles_fmt::format(text, lang, &sparkles_fmt::Options::default())
        .unwrap()
        .text
}

#[tokio::test(flavor = "multi_thread")]
async fn formats_like_sparkles_fmt() {
    let mut c = Client::start(fixture_server());
    let q = "prefix ex: <http://ex.org/> select * where{?s ex:p ?o . FILTER(?o>1)}";
    let r = c.structured("format", json!({"text": q})).await;
    assert_eq!(r["language"], "sparql");
    assert_eq!(r["changed"], true);
    assert_eq!(r["warnings"], json!([]));
    let text = r["text"].as_str().unwrap();
    assert_eq!(text, formatted(q, sparkles_fmt::Language::Sparql));
    // formatted text is a fixpoint
    let again = c.structured("format", json!({"text": text})).await;
    assert_eq!(again["changed"], false);
    assert_eq!(again["text"], text);

    // Turtle is detected; N-Quads is named, and sorted on request
    let ttl = "@prefix ex: <http://ex.org/> . ex:a ex:b ex:c ; ex:d \"e\" .";
    let r = c.structured("format", json!({"text": ttl})).await;
    assert_eq!(r["language"], "turtle");
    assert_eq!(r["text"], formatted(ttl, sparkles_fmt::Language::Turtle));
    let nq = "<urn:b> <urn:p> <urn:o> <urn:g> .\n<urn:a>   <urn:p> <urn:o> .\n";
    let r = c
        .structured(
            "format",
            json!({"text": nq, "language": "nquads", "options": {"sort": true}}),
        )
        .await;
    assert_eq!(r["language"], "nquads");
    assert_eq!(
        r["text"],
        "<urn:a> <urn:p> <urn:o> .\n<urn:b> <urn:p> <urn:o> <urn:g> .\n"
    );
    // JSON-LD
    let r = c
        .structured(
            "format",
            json!({"text": r#"{"@id":"urn:a","urn:p":{"@id":"urn:o"}}"#, "language": "jsonld"}),
        )
        .await;
    assert_eq!(r["language"], "jsonld");
    assert!(
        r["text"].as_str().unwrap().contains("\"@id\": \"urn:a\""),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn options_and_warnings() {
    let mut c = Client::start(fixture_server());
    // a prefix declared elsewhere is formatted, with a warning that names its position
    let r = c
        .structured(
            "format",
            json!({"text": "SELECT * WHERE { ?s ex:p ?o }", "language": "sparql"}),
        )
        .await;
    let w = &r["warnings"][0];
    assert_eq!(w["code"], "undeclared-prefix", "{r}");
    assert_eq!(
        (w["line"].as_u64(), w["column"].as_u64()),
        (Some(1), Some(21))
    );
    // style options use the camelCase names of POST /$/format
    let q = "SELECT ?s WHERE { ?s ?p ?o }";
    let r = c
        .structured(
            "format",
            json!({"text": q, "options": {"indentWidth": 4, "lineWidth": null}}),
        )
        .await;
    assert_eq!(r["text"], "SELECT ?s\nWHERE {\n    ?s ?p ?o .\n}\n");
    for (options, needle) in [
        (json!({"lineWidth": 5}), "options.lineWidth"),
        (json!({"bogus": 1}), "options.bogus: unknown option"),
        (json!([]), "`options` must be an object"),
    ] {
        let (text, meta) = c
            .error("format", json!({"text": q, "options": options}))
            .await;
        assert_eq!(meta["code"], "bad-argument", "{text}");
        assert!(text.contains(needle), "{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_and_limits() {
    let mut c = Client::start(fixture_server());
    let (text, meta) = c
        .error("format", json!({"text": "SELECT * WHERE { ?s ?p }"}))
        .await;
    assert_eq!(meta, json!({"code": "syntax", "status": 400}), "{text}");
    assert!(
        text.starts_with("SPARQL syntax error at line 1, column"),
        "{text}"
    );
    assert!(text.contains("Hint: the text was read as sparql"), "{text}");
    let (text, meta) = c
        .error(
            "format",
            json!({"text": "<?xml version=\"1.0\"?><rdf:RDF/>"}),
        )
        .await;
    assert_eq!(meta["code"], "unsupported-language", "{text}");
    for args in [
        json!({"text": "x", "language": "cobol"}),
        json!({"text": "   "}),
        json!({"text": "ASK {}", "dataset": "ds"}),
        json!({"text": "ASK {}", "timeoutSeconds": 0}),
    ] {
        let (text, meta) = c.error("format", args.clone()).await;
        assert_eq!(meta["code"], "bad-argument", "{args}: {text}");
    }
    let big = format!("ASK {{ {} }}", "?s ?p ?o . ".repeat(110_000));
    let (text, meta) = c.error("format", json!({"text": big})).await;
    assert_eq!(meta["code"], "bad-argument", "{text}");
    assert!(text.contains("at most 1048576 characters"), "{text}");

    // the output is capped like other results
    let cfg = McpConfig {
        max_bytes: 1024,
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[], cfg));
    let q = format!("ASK {{ {} }}", "?s <urn:p> ?o . ".repeat(100));
    let (text, meta) = c.error("format", json!({"text": q})).await;
    assert_eq!(meta, json!({"code": "too-large", "status": 413}), "{text}");
    let r = c.structured("format", json!({"text": "ASK{}"})).await;
    assert_eq!(r["language"], "sparql");
}
