//! The rules over JSON-LD: the term rules over value objects and coerced terms, the
//! `@context` rules, and the fixes.

use super::*;

fn diags(text: &str) -> Vec<Diagnostic> {
    lint(text, Language::JsonLd, &LintOptions::default())
        .unwrap()
        .diagnostics
}

/// `rule@covered-text` for each diagnostic, in order.
fn found(text: &str) -> Vec<String> {
    diags(text)
        .into_iter()
        .map(|d| format!("{}@{}", d.rule, &text[d.start..d.end]))
        .collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn json_ld_is_linted() {
    assert!(lints(Language::JsonLd));
    assert_eq!(found("{}"), Vec::<String>::new());
    assert_eq!(found("[]"), Vec::<String>::new());
}

#[test]
fn syntax_errors_stand_alone() {
    let d = diags("{\"a\": }");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].rule, d[0].severity), ("syntax", Severity::Error));
    let d = diags("{\"a\": 1,}");
    assert_eq!(d[0].rule, "syntax");
}

const TERMS: &str = r#"{
  "@context": {
    "xsd": "http://www.w3.org/2001/XMLSchema#",
    "ex": "http://example.org/",
    "age": { "@id": "ex:age", "@type": "xsd:integer" },
    "label": "ex:label"
  },
  "@id": "ex:a",
  "label": [
    { "@value": "a", "@language": "en-us" },
    { "@value": "abc", "@type": "xsd:integer" },
    { "@value": "x", "@type": "xsd:string" },
    { "@value": "b", "@language": "iw" },
    { "@value": "2020-13-01", "@type": "http://www.w3.org/2001/XMLSchema#date" },
    { "@value": "12", "@type": "xsd:interger" },
    { "@value": 5, "@type": "xsd:integer" }
  ],
  "age": "abc"
}"#;

#[test]
fn term_rules_over_value_objects() {
    assert_eq!(
        found(TERMS),
        strs(&[
            "language-tag-case@\"en-us\"",
            "suspicious-datatype@\"abc\"",
            "redundant-datatype@\"@type\": \"xsd:string\"",
            "deprecated-language-tag@\"iw\"",
            "suspicious-datatype@\"2020-13-01\"",
            "suspicious-datatype@\"xsd:interger\"",
            "suspicious-datatype@\"abc\"",
        ])
    );
}

#[test]
fn iris_in_json_ld() {
    let doc = r#"{"@context": {"owl": "http://www.w3.org/2002/07/owl#",
        "arq": "http://jena.hpl.hp.com/ARQ/function#"},
      "@id": "http://x/a b", "@type": "owl:DataRange", "arq:f": 1}"#;
    assert_eq!(
        found(doc),
        strs(&[
            "deprecated-syntax@\"http://jena.hpl.hp.com/ARQ/function#\"",
            "iri-space@\"http://x/a b\"",
            "deprecated-syntax@\"owl:DataRange\"",
        ])
    );
    let doc = r#"{"@id": "http://x/a", "http://x/p": {"@value": "a b", "@type": "http://www.w3.org/2001/XMLSchema#anyURI"}}"#;
    assert_eq!(found(doc), strs(&["iri-space@\"a b\""]));
}

#[test]
fn unused_context_terms() {
    let doc = r#"{
  "@context": {
    "name": "http://schema.org/name",
    "unused": "http://schema.org/unused",
    "ex": "http://example.org/",
    "xsd": "http://www.w3.org/2001/XMLSchema#",
    "born": { "@id": "ex:born", "@type": "xsd:date" },
    "Person": "ex:Person",
    "alias": "name"
  },
  "@id": "ex:a",
  "@type": "Person",
  "name": "A",
  "ex:other": 1,
  "born": "2000-01-01"
}"#;
    let d = diags(doc);
    assert_eq!(
        d.iter()
            .map(|d| (d.rule, &doc[d.start..d.end], d.severity))
            .collect::<Vec<_>>(),
        vec![
            (
                "unused-context-term",
                "\"unused\": \"http://schema.org/unused\"",
                Severity::Hint
            ),
            ("unused-context-term", "\"alias\": \"name\"", Severity::Hint),
        ]
    );
    assert!(d.iter().all(|d| d.fix.is_some()));
    assert_eq!(d[0].line, 4);
}

#[test]
fn unused_context_term_uses() {
    // a prefix used only inside a compact IRI value, a term used as a coerced @vocab value,
    // a term used by another definition, and one used as a key of a scoped context
    let doc = r#"{
  "@context": {
    "ex": "http://example.org/",
    "kind": { "@id": "ex:kind", "@type": "@vocab" },
    "Thing": "ex:Thing",
    "p": { "@id": "ex:p", "@context": { "q": "ex:q" } }
  },
  "kind": "Thing",
  "p": { "q": 1 }
}"#;
    assert_eq!(found(doc), Vec::<String>::new());
    // terms of a nested context count too
    let doc = r#"{"@context": {"a": "http://x/a"}, "@graph": [{"@context": {"b": "http://x/b", "c": "http://x/c"}, "b": 1}]}"#;
    assert_eq!(
        found(doc),
        strs(&[
            "unused-context-term@\"a\": \"http://x/a\"",
            "unused-context-term@\"c\": \"http://x/c\""
        ])
    );
    // keywords and non-term entries are not terms
    let doc =
        r#"{"@context": {"@vocab": "http://x/", "@version": 1.1, "@base": "http://b/"}, "p": 1}"#;
    assert_eq!(found(doc), Vec::<String>::new());
}

#[test]
fn unmapped_keys() {
    let doc = r#"{
  "@context": { "name": "http://schema.org/name", "knows": "http://schema.org/knows" },
  "@id": "http://x/a",
  "name": "A",
  "nmae": "B",
  "ex:fine": 1,
  "http://x/p": 2,
  "knows": { "@id": "http://x/b", "nope": 1, "name": "B" }
}"#;
    assert_eq!(
        found(doc),
        strs(&["unmapped-key@\"nmae\"", "unmapped-key@\"nope\""])
    );
    let d = diags(doc);
    assert!(
        d.iter()
            .all(|d| d.severity == Severity::Warning && d.fix.is_none())
    );
    // @graph, and a nested @context that defines the term
    let doc = r#"{"@context": {"p": "http://x/p"}, "p": 0, "@graph": [{"zz": 1}, {"@context": {"zz": "http://x/zz"}, "zz": 2}]}"#;
    assert_eq!(found(doc), strs(&["unmapped-key@\"zz\""]));
}

#[test]
fn keys_that_are_not_dropped() {
    // @vocab maps everything
    let doc = r#"{"@context": {"@vocab": "http://x/"}, "anything": 1}"#;
    assert!(found(doc).is_empty());
    // a context the linter cannot read may define anything
    let doc = r#"{"@context": "https://example.org/context.jsonld", "anything": 1}"#;
    assert!(found(doc).is_empty());
    let doc =
        r#"{"@context": ["https://example.org/c", {"a": "http://x/a"}], "anything": 1, "a": 2}"#;
    assert!(found(doc).is_empty());
    // no context at all: it may come from an HTTP header
    assert!(found(r#"{"anything": 1}"#).is_empty());
    // a term set to null is dropped on purpose
    let doc = r#"{"@context": {"p": "http://x/p", "skipped": null}, "p": 1, "skipped": 2}"#;
    assert!(found(doc).is_empty());
    // map containers, JSON literals and value objects hold data, not properties
    let doc = r#"{
  "@context": {
    "labels": { "@id": "http://x/l", "@container": "@language" },
    "byId": { "@id": "http://x/b", "@container": "@index" },
    "raw": { "@id": "http://x/r", "@type": "@json" },
    "p": "http://x/p"
  },
  "labels": { "en": "a", "de": "b" },
  "byId": { "k1": { "p": 1 }, "k2": { "p": 2 } },
  "raw": { "any": { "thing": true } },
  "p": { "@value": { "also": "json" }, "@type": "@json" }
}"#;
    assert_eq!(found(doc), Vec::<String>::new());
    // but the values of an index map are node objects
    let doc = r#"{"@context": {"byId": { "@id": "http://x/b", "@container": "@index" }}, "byId": { "k1": { "nope": 1 } }}"#;
    assert_eq!(found(doc), strs(&["unmapped-key@\"nope\""]));
    // a scoped context of a term applies to its value
    let doc = r#"{"@context": {"p": {"@id": "http://x/p", "@context": {"q": "http://x/q"}}}, "p": {"q": 1, "z": 2}}"#;
    assert_eq!(found(doc), strs(&["unmapped-key@\"z\""]));
    // keys are compared decoded
    let doc = r#"{"@context": {"age": "http://x/age"}, "age": 1}"#;
    assert!(found(doc).is_empty());
}

#[test]
fn json_ld_fixes() {
    let f = fix(TERMS, Language::JsonLd, &LintOptions::default()).unwrap();
    assert_eq!(f.applied, 2, "{:?}", f.diagnostics);
    assert!(f.text.contains("\"@language\": \"en-US\""), "{}", f.text);
    assert!(f.text.contains("{ \"@value\": \"x\" },\n"), "{}", f.text);
    assert!(!f.text.contains("xsd:string"), "{}", f.text);
    // unused terms go, and so do their commas and lines
    let doc = r#"{
  "@context": {
    "a": "http://x/a",
    "b": "http://x/b",
    "c": "http://x/c"
  },
  "b": 1
}"#;
    let f = fix(doc, Language::JsonLd, &LintOptions::default()).unwrap();
    assert_eq!(
        f.text,
        "{\n  \"@context\": {\n    \"b\": \"http://x/b\"\n  },\n  \"b\": 1\n}"
    );
    assert_eq!(f.applied, 2);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    // a context with one term left over
    let f = fix(
        r#"{"@context": {"a": "http://x/a"}, "@id": "http://x/s"}"#,
        Language::JsonLd,
        &LintOptions::default(),
    )
    .unwrap();
    assert_eq!(f.text, r#"{"@context": {}, "@id": "http://x/s"}"#);
}

#[test]
fn no_fix_without_the_whole_context() {
    // a remote context may use the term, and expansion cannot be checked
    let doc =
        r#"{"@context": ["https://example.org/c", {"a": "http://x/a"}], "@id": "http://x/s"}"#;
    let d = diags(doc);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].rule, "unused-context-term");
    assert!(d[0].fix.is_none());
    let f = fix(doc, Language::JsonLd, &LintOptions::default()).unwrap();
    assert_eq!((f.text.as_str(), f.applied), (doc, 0));
    // language tags are still fixed there? Not without an expansion to compare.
    let doc = r#"{"@context": "https://example.org/c", "@id": "http://x/s", "http://x/p": {"@value": "a", "@language": "EN"}}"#;
    let f = fix(doc, Language::JsonLd, &LintOptions::default()).unwrap();
    assert_eq!((f.text.as_str(), f.applied), (doc, 0));
}

#[test]
fn fix_keeps_the_dataset() {
    let f = fix(TERMS, Language::JsonLd, &LintOptions::default()).unwrap();
    let quads = |t: &str| -> Vec<oxrdf::Quad> {
        oxjsonld::JsonLdParser::new()
            .with_base_iri("http://sparkles-fmt.invalid/base/")
            .unwrap()
            .for_slice(t)
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(crate::check::graph::isomorphic(
        &quads(TERMS),
        &quads(&f.text)
    ));
}

#[test]
fn odd_documents_are_linted_without_a_panic() {
    for doc in [
        "null",
        "7",
        "\"s\"",
        r#"{"@context": null, "a": 1}"#,
        r#"{"@context": [null, {"a": "http://x/a"}], "a": 1, "b": 2}"#,
        r#"{"@context": {"a": null, "b": {"@id": null}, "c": 5, "d": [], "e": {"@type": 3}}}"#,
        r#"{"@context": {"a": "a", "b": "c", "c": "b"}, "a": 1}"#,
        r#"{"@context": {"": "http://x/"}, "": 1}"#,
        r#"{"@context": {"a": {"@id": "http://x/a", "@container": ["@set", "@language"]}}, "a": 3}"#,
        r#"{"@id": {"@value": 1}, "@type": [1, "a b"]}"#,
        r#"{"@context": {"@vocab": null}, "k": {"@value": "x", "@language": 4}}"#,
        r#"{"@value": "x", "@type": 5, "@language": "😀"}"#,
        r#"{"@context": {"x": "http://x/"}, "x:y": {"@value": "z", "@type": "x:t"}}"#,
        "\u{feff}{\"@context\": {}}",
    ] {
        let _ = diags(doc);
        let _ = fix(doc, Language::JsonLd, &LintOptions::default());
    }
    // decoded keys and surrogate pairs
    assert_eq!(
        super::jsonld::json_string("\"\\ud83d\\ude00\\u00e9\""),
        "😀é"
    );
}

#[test]
fn json_ld_positions_are_in_the_input() {
    let doc = "{\n  \"@context\": {\"a\": \"http://x/a\"},\n  \"@id\": \"http://x/s\",\n  \"http://x/p\": {\"@value\": \"v\", \"@language\": \"EN\"}\n}";
    let d = diags(doc);
    let unused = &d[0];
    assert_eq!(
        (unused.rule, unused.line, unused.column),
        ("unused-context-term", 2, 16)
    );
    let lang = &d[1];
    assert_eq!(
        (lang.rule, lang.line, lang.column),
        ("language-tag-case", 4, 46)
    );
}
