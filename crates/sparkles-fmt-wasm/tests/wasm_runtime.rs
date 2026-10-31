//! The module's entry points in a wasm runtime (node, through wasm-bindgen-test-runner),
//! where `std` has no clock: a lint or format without a deadline must never read one.
//! `mise run fmt:wasm` runs these.
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use serde_json::{Value, json};
use sparkles_fmt_wasm::{format, lint};
use wasm_bindgen_test::wasm_bindgen_test;

fn lint_json(req: Value) -> Value {
    serde_json::from_str(&lint(&req.to_string())).unwrap()
}

#[wasm_bindgen_test]
fn json_ld_lints_without_a_clock() {
    let r = lint_json(json!({"text": "{}", "language": "jsonld"}));
    assert_eq!(r["diagnostics"], json!([]), "{r}");
    let doc = r#"{"@context": {"sh": "http://www.w3.org/ns/shacl#"}, "@id": "urn:s", "sh:select": "SELECT $this { $this p:p ?o FILTER(?o) }", "http://x/p": {"@value": "a", "@language": "EN"}}"#;
    let r = lint_json(json!({"text": doc, "language": "jsonld"}));
    let rules: Vec<&str> = r["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"undefined-prefix"), "{r}");
    assert!(rules.contains(&"language-tag-case"), "{r}");
    // and the fixes, which expand the document again
    let r = lint_json(json!({"text": doc, "language": "jsonld", "fix": true}));
    assert_eq!(r["applied"], 1, "{r}");
}

#[wasm_bindgen_test]
fn every_language_lints_and_formats_without_a_clock() {
    for (text, lang) in [
        ("PREFIX u: <http://u/>\nASK {}", "sparql"),
        (
            "@prefix sh: <http://www.w3.org/ns/shacl#> .\n[ sh:select \"\"\"SELECT * { ?s ?p ?o }\"\"\" ] .\n",
            "turtle",
        ),
        ("<urn:s> <urn:p> \"a\"@EN .\n", "ntriples"),
        ("<urn:s> <urn:p> \"a\"@EN <urn:g> .\n", "nquads"),
        ("shape <http://x/S> { <http://x/p> IRI . }", "shaclc"),
    ] {
        let r = lint_json(json!({"text": text, "language": lang}));
        assert!(r["diagnostics"].is_array(), "{lang}: {r}");
        let f: Value = serde_json::from_str(&format(
            &json!({"text": text, "language": lang}).to_string(),
        ))
        .unwrap();
        assert!(f["text"].is_string(), "{lang}: {f}");
    }
}
