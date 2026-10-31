//! The term rules over N-Triples and N-Quads, and the rules over SHACLC.

use super::*;

fn diags(text: &str, lang: Language) -> Vec<Diagnostic> {
    lint(text, lang, &LintOptions::default())
        .unwrap()
        .diagnostics
}

/// `rule@covered-text` for each diagnostic, in order.
fn found(text: &str, lang: Language) -> Vec<String> {
    diags(text, lang)
        .into_iter()
        .map(|d| format!("{}@{}", d.rule, &text[d.start..d.end]))
        .collect()
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

#[test]
fn every_language_lints() {
    for lang in Language::ALL {
        assert!(lints(lang), "{lang:?}");
    }
}

#[test]
fn ntriples_term_rules() {
    let nt = format!(
        "<http://x/s> <http://x/p> \"a\"@en-us .\n\
         <http://x/s> <http://x/p> \"abc\"^^<{XSD}integer> . # \"c\"@EN\n\
         <http://x/s> <http://x/p> \"x\"^^<{XSD}string> .\n\
         <http://x/s> <http://x/p> \"b\"@iw .\n\
         <http://x/s> <http://x/p> \"2020-13-01\"^^<{XSD}date> .\n\
         _:b <http://x/p> \"x y\"^^<{XSD}anyURI> .\n"
    );
    assert_eq!(
        found(&nt, Language::NTriples),
        [
            "language-tag-case@@en-us".to_string(),
            format!("suspicious-datatype@\"abc\"^^<{XSD}integer>"),
            format!("redundant-datatype@^^<{XSD}string>"),
            "deprecated-language-tag@@iw".to_string(),
            format!("suspicious-datatype@\"2020-13-01\"^^<{XSD}date>"),
            format!("iri-space@\"x y\"^^<{XSD}anyURI>"),
        ]
    );
    // clean input, a BOM, CRLF line breaks and comments
    let clean = "\u{feff}# c\r\n<http://x/s> <http://x/p> \"a\"@en-GB .\r\n\r\n";
    assert_eq!(found(clean, Language::NTriples), Vec::<String>::new());
}

#[test]
fn nquads_term_rules_and_positions() {
    let nq = "<http://x/s> <http://x/p> \"a\"@EN <http://x/g> .\n<http://x/s> <http://x/p> \"b\"@zh-hant <http://x/g> .\n";
    let d = diags(nq, Language::NQuads);
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d.iter().all(|d| d.rule == "language-tag-case"));
    assert_eq!((d[0].line, d[0].column), (1, 30));
    assert_eq!((d[1].line, d[1].column), (2, 30));
    // RDF 1.2 triple terms and reified triples are walked too
    let nt = "<http://x/s> <http://x/p> <<( <http://x/a> <http://x/b> \"c\"@EN )>> .\n";
    assert_eq!(
        found(nt, Language::NTriples),
        ["language-tag-case@@EN".to_string()]
    );
}

#[test]
fn line_format_syntax_errors_stand_alone() {
    let d = diags("<http://x/s> <http://x/p> .\n", Language::NTriples);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].rule, d[0].severity), ("syntax", Severity::Error));
    assert_eq!(d[0].line, 1);
    let d = diags(
        "<http://x/s> <http://x/p> \"x\" .\n<a b> <http://x/p> <http://x/o> .\n",
        Language::NQuads,
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].line, 2);
    // an IRI with a space does not parse: the lint says why
    let d = diags(
        "<http://x/a b> <http://x/p> <http://x/o> .\n",
        Language::NTriples,
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].rule, d[0].severity), ("iri-space", Severity::Error));
}

#[test]
fn line_format_fixes() {
    let nt = format!(
        "<http://x/s> <http://x/p> \"a\"@en-us .\n<http://x/s> <http://x/p> \"x\"^^<{XSD}string> .\n"
    );
    let f = fix(&nt, Language::NTriples, &LintOptions::default()).unwrap();
    assert_eq!(
        f.text,
        "<http://x/s> <http://x/p> \"a\"@en-US .\n<http://x/s> <http://x/p> \"x\" .\n"
    );
    assert_eq!(f.applied, 2);
    assert!(f.diagnostics.is_empty());
    let nq = "<http://x/s> <http://x/p> \"a\"@EN <http://x/g> .\n";
    let f = fix(nq, Language::NQuads, &LintOptions::default()).unwrap();
    assert_eq!(
        f.text,
        "<http://x/s> <http://x/p> \"a\"@en <http://x/g> .\n"
    );
}

#[test]
fn prefix_rules_do_not_apply_to_line_formats() {
    let mut o = LintOptions::default();
    o.set("unused-prefix", "error").unwrap();
    let l = lint(
        "<http://x/s> <http://x/p> <http://x/o> .\n",
        Language::NTriples,
        &o,
    )
    .unwrap();
    assert!(l.diagnostics.is_empty());
}

// ------------------------------------------------------------------- SHACLC ------

fn shaclc(text: &str) -> Vec<String> {
    found(text, Language::Shaclc)
}

#[test]
fn shaclc_prefixes() {
    let doc = "PREFIX ex: <http://example.org/>\n\
               PREFIX unused: <http://u/>\n\
               shape ex:S -> ex:C {\n  ex:name xsd:string [1..1] .\n  @ex:T .\n}\n";
    assert_eq!(
        shaclc(doc),
        ["unused-prefix@PREFIX unused: <http://u/>".to_string()]
    );
    // the standard prefixes are bound without a declaration and are never unused
    assert_eq!(
        shaclc("shape <http://x/S> { <http://x/p> sh:Literal [0..1] . <http://x/q> rdfs:Class . }"),
        Vec::<String>::new()
    );
    // an undeclared prefix is a finding of the lint, not a syntax error, and the rest
    // still runs
    let doc = "PREFIX ex: <http://example.org/>\nshape ex:S { nope:p IRI . @alsonot:T . }\n";
    assert_eq!(
        shaclc(doc),
        [
            "undefined-prefix@nope:p".to_string(),
            "undefined-prefix@@alsonot:T".to_string()
        ]
    );
    // a prefix used only in a shape reference is used
    let doc = "PREFIX ex: <http://example.org/>\nPREFIX o: <http://o/>\nshape <http://x/S> { <http://x/p> @o:T . }\n";
    assert_eq!(
        shaclc(doc),
        ["unused-prefix@PREFIX ex: <http://example.org/>".to_string()]
    );
}

#[test]
fn shaclc_terms() {
    let doc = format!(
        "shape <http://x/S> {{\n  <http://x/p> in=[\"a\"@en-us \"b\"^^xsd:integer \"c\"^^<{XSD}string> \"d\"@iw] .\n  <http://x/q> hasValue=\"x y\"^^xsd:anyURI .\n}}\n"
    );
    assert_eq!(
        shaclc(&doc),
        [
            "language-tag-case@@en-us".to_string(),
            "suspicious-datatype@\"b\"^^xsd:integer".to_string(),
            format!("redundant-datatype@^^<{XSD}string>"),
            "deprecated-language-tag@@iw".to_string(),
            "iri-space@\"x y\"^^xsd:anyURI".to_string(),
        ]
    );
    let owl =
        "shape <http://x/S> { <http://x/p> class=<http://www.w3.org/2002/07/owl#DataRange> . }";
    assert_eq!(
        shaclc(owl),
        ["deprecated-syntax@<http://www.w3.org/2002/07/owl#DataRange>".to_string()]
    );
}

#[test]
fn shaclc_syntax_errors_and_positions() {
    let d = diags("shape <http://x/S> { <http://x/p> ", Language::Shaclc);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].rule, d[0].severity), ("syntax", Severity::Error));
    // an undeclared prefix and a syntax error elsewhere: the syntax error is reported
    // at its own place, not shifted by the lint's bookkeeping
    let doc = "shape <http://x/S> { nope:p IRI .\n  <http://x/q> [1..] . }";
    let d = diags(doc, Language::Shaclc);
    let s = d.iter().find(|d| d.rule == "syntax").unwrap();
    assert_eq!(s.line, 2, "{d:?}");
    assert_eq!(&doc[s.start..s.start + 1], "]", "{d:?}");
}

#[test]
fn shaclc_fixes() {
    let doc = "PREFIX ex: <http://example.org/>\nPREFIX unused: <http://u/>\nshape ex:S { ex:p in=[\"a\"@en-us] . }\n";
    let f = fix(doc, Language::Shaclc, &LintOptions::default()).unwrap();
    assert_eq!(
        f.text,
        "PREFIX ex: <http://example.org/>\nshape ex:S { ex:p in=[\"a\"@en-US] . }\n"
    );
    assert_eq!(f.applied, 2);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
}

#[test]
fn fixing_a_document_with_an_undefined_prefix() {
    // the undefined prefix is a finding, and the other fixes still apply
    let ttl = "@prefix u: <http://u/> .\nnope:a <http://x/p> \"c\"@EN .\n";
    let f = fix(ttl, Language::Turtle, &LintOptions::default()).unwrap();
    assert_eq!(f.text, "nope:a <http://x/p> \"c\"@en .\n");
    let doc = "PREFIX u: <http://u/>\nshape nope:S { <http://x/p> in=[\"a\"@EN] . }\n";
    let f = fix(doc, Language::Shaclc, &LintOptions::default()).unwrap();
    assert_eq!(f.text, "shape nope:S { <http://x/p> in=[\"a\"@en] . }\n");
}
