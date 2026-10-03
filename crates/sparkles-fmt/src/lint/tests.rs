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

fn sparql(text: &str) -> Vec<String> {
    found(text, Language::Sparql)
}

fn turtle(text: &str) -> Vec<String> {
    found(text, Language::Turtle)
}

fn rules(text: &str, lang: Language) -> Vec<&'static str> {
    diags(text, lang).into_iter().map(|d| d.rule).collect()
}

#[test]
fn clean_documents_have_nothing() {
    let q = "PREFIX ex: <http://example.org/>\n\
             SELECT ?s ?n WHERE { ?s ex:name ?n ; ex:age ?a FILTER(?a > 18) }\n";
    assert_eq!(sparql(q), Vec::<String>::new());
    let ttl = "@prefix ex: <http://example.org/> .\nex:a ex:b \"x\"@en-GB , 3 .\n";
    assert_eq!(turtle(ttl), Vec::<String>::new());
}

#[test]
fn syntax_errors_stand_alone() {
    let d = diags("SELECT * WHERE { ?s ?p }", Language::Sparql);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].rule, "syntax");
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(rules("<a> <b> .", Language::Turtle), ["syntax"]);
}

#[test]
fn prefixes() {
    let q = "PREFIX ex: <http://example.org/>\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT ?s { ?s ex:p ?o . ?o dc:title ?t }";
    assert_eq!(
        sparql(q),
        [
            "unused-prefix@PREFIX foaf: <http://xmlns.com/foaf/0.1/>",
            "undefined-prefix@dc:title",
            "single-use-variable@?t",
        ]
    );
    // Turtle: an undeclared prefix is the lint's finding, and the rest still runs
    let ttl =
        "@prefix ex: <http://example.org/> .\n@prefix unused: <http://u/> .\nex:a ex:b dc:c .\n";
    assert_eq!(
        turtle(ttl),
        [
            "unused-prefix@@prefix unused: <http://u/> .",
            "undefined-prefix@dc:c"
        ]
    );
    // a prefix redefined before any use: the first declaration is unused
    let ttl = "PREFIX ex: <http://a/>\nPREFIX ex: <http://b/>\nex:s ex:p ex:o .\n";
    assert_eq!(turtle(ttl), ["unused-prefix@PREFIX ex: <http://a/>"]);
    // used before its declaration in Turtle
    let ttl = "ex:s ex:p ex:o .\n@prefix ex: <http://a/> .\n";
    let r = rules(ttl, Language::Turtle);
    assert!(r.contains(&"undefined-prefix"), "{r:?}");
}

#[test]
fn variables() {
    // projected but never bound, and bound but never used
    let q = "SELECT ?s ?name { ?s <http://x/p> ?o ; <http://x/q> ?nmae }";
    assert_eq!(
        sparql(q),
        [
            "unbound-variable@?name",
            "single-use-variable@?o",
            "single-use-variable@?nmae"
        ]
    );
    // SELECT *, ASK, names starting with _, and EXISTS patterns are exempt
    assert!(sparql("SELECT * { ?s ?p ?o }").is_empty());
    assert!(sparql("ASK { ?s ?p ?o }").is_empty());
    assert!(sparql("SELECT ?s ?p { ?s ?p ?_any }").is_empty());
    assert!(
        sparql("SELECT ?s { ?s a ?c FILTER NOT EXISTS { ?s <http://x/q> ?any } }")
            .iter()
            .all(|d| d == "single-use-variable@?c")
    );
    // VALUES nothing joins with, and GRAPH ?g as a wildcard
    assert_eq!(
        sparql("SELECT ?s ?o { ?s <http://x/p> ?o VALUES ?x { 1 2 } }"),
        ["cartesian-product@VALUES ?x { 1 2 }", "unused-variable@?x"]
    );
    assert_eq!(
        sparql("SELECT ?s { GRAPH ?g { ?s a <http://x/C> } }"),
        ["single-use-variable@?g"]
    );
    // a BIND whose result nothing reads
    assert_eq!(
        sparql("SELECT ?s { ?s <http://x/p> ?o BIND(?o + 1 AS ?next) }"),
        ["unused-variable@?next"]
    );
    // a subquery's projection is bound outside it, and its own variables are its own
    assert_eq!(
        sparql("SELECT ?s ?c { { SELECT ?s (COUNT(?o) AS ?c) { ?s ?p ?o } GROUP BY ?s } }"),
        ["single-use-variable@?p"]
    );
    // updates: the templates read the WHERE clause's bindings
    let u = "DELETE { ?s <http://x/p> ?o } INSERT { ?s <http://x/q> ?new } WHERE { ?s <http://x/p> ?o }";
    assert_eq!(sparql(u), ["unbound-variable@?new"]);
    assert!(sparql("DELETE WHERE { ?s <http://x/p> ?o }").is_empty());
    // a FILTER on a variable bound nowhere
    assert_eq!(
        sparql("SELECT ?s ?o { ?s <http://x/p> ?o FILTER(?x > 3) }"),
        ["unbound-variable@?x"]
    );
}

#[test]
fn cartesian_products() {
    let q = "SELECT ?a ?b { ?a <http://x/p> 1 . ?b <http://x/q> 2 }";
    assert_eq!(sparql(q), ["cartesian-product@?b <http://x/q> 2"]);
    // joined through a shared variable, a blank node label, a BIND or VALUES
    assert!(sparql("SELECT ?a ?b { ?a <http://x/p> ?x . ?b <http://x/q> ?x }").is_empty());
    assert!(sparql("SELECT ?a ?b { ?a <http://x/p> _:n . ?b <http://x/q> _:n }").is_empty());
    assert!(
        sparql("SELECT ?a ?b { ?a <http://x/p> ?x BIND(?x AS ?y) ?b <http://x/q> ?y }").is_empty()
    );
    // a FILTER that relates them does not make it a join
    assert_eq!(
        rules(
            "SELECT ?a ?b { ?a <http://x/p> ?x . ?b <http://x/q> ?y FILTER(?x = ?y) }",
            Language::Sparql
        ),
        ["cartesian-product"]
    );
    // ground patterns are existence tests
    assert!(sparql("SELECT ?a { ?a <http://x/p> 1 . <http://x/s> <http://x/p> 2 }").is_empty());
}

#[test]
fn grouping() {
    let d = diags("SELECT * { ?s ?p ?o } GROUP BY ?s", Language::Sparql);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].rule, "select-star-group-by");
    assert_eq!(
        sparql("SELECT ?o { ?s ?p ?o } GROUP BY ?s"),
        ["ungrouped-variable@?o"]
    );
    assert_eq!(
        sparql("SELECT ?s (?o + 1 AS ?x) (COUNT(?p) AS ?n) { ?s ?p ?o } GROUP BY ?s"),
        ["ungrouped-variable@?o"]
    );
    let q = "SELECT ?s (COUNT(?o) AS ?n) ((?n * 2) AS ?m) { ?s ?_p ?o } GROUP BY ?s";
    assert!(sparql(q).is_empty(), "{:?}", diags(q, Language::Sparql));
}

#[test]
fn filters() {
    let q = "SELECT ?s ?o { ?s <http://x/p> ?o . { ?s <http://x/q> ?r FILTER(?o > 3) } }";
    assert_eq!(sparql(q), ["single-use-variable@?r", "filter-scope@?o"]);
    // OPTIONAL's FILTER is its join condition, and EXISTS sees the outer row
    assert!(
        sparql(
            "SELECT ?s ?o ?r { ?s <http://x/p> ?o OPTIONAL { ?s <http://x/q> ?r FILTER(?o > ?r) } }"
        )
        .is_empty()
    );
    assert!(
        sparql("SELECT ?s ?o { ?s <http://x/p> ?o FILTER EXISTS { ?s <http://x/q> ?r FILTER(?r > ?o) } }")
            .is_empty()
    );
    let q = "PREFIX ex: <http://x/>\nSELECT ?s ?p { ?s ?p ?o FILTER(?p = ex:name) }";
    let d = diags(q, Language::Sparql);
    let eq: Vec<_> = d.iter().filter(|d| d.rule == "filter-equality").collect();
    assert_eq!(eq.len(), 1, "{d:?}");
    assert_eq!(eq[0].severity, Severity::Hint);
    assert!(eq[0].message.contains("ex:name"));
    assert_eq!(
        rules(
            "SELECT ?s ?o { ?s ?p ?o FILTER(sameTerm(<http://x/a>, ?p)) }",
            Language::Sparql
        ),
        ["filter-equality"]
    );
    // a literal compares by value: no finding
    assert!(
        sparql("SELECT ?s ?o { ?s ?p ?o FILTER(?o = 1) }")
            .iter()
            .all(|d| !d.starts_with("filter-equality"))
    );
}

#[test]
fn language_tags() {
    let ttl = "<http://x/s> <http://x/p> \"a\"@en-us , \"b\"@ZH-hant-tw , \"c\"@SR-latn--rtl , \"d\"@x-private-AB .\n";
    assert_eq!(
        turtle(ttl),
        [
            "language-tag-case@@en-us",
            "language-tag-case@@ZH-hant-tw",
            "language-tag-case@@SR-latn--rtl",
            "language-tag-case@@x-private-AB",
        ]
    );
    assert_eq!(terms::canonical_case("ZH-hant-tw"), "zh-Hant-TW");
    assert_eq!(terms::canonical_case("x-private-AB"), "x-private-ab");
    assert_eq!(terms::canonical_case("de-ch-1996"), "de-CH-1996");
    assert_eq!(terms::canonical_case("en-a-bbb-x-CC"), "en-a-bbb-x-cc");
    let ttl = "<http://x/s> <http://x/p> \"a\"@iw , \"b\"@i-klingon , \"c\"@de-DD .\n";
    assert_eq!(
        rules(ttl, Language::Turtle),
        [
            "deprecated-language-tag",
            "deprecated-language-tag",
            "deprecated-language-tag"
        ]
    );
    // directives are not language tags
    assert!(
        turtle("@prefix ex: <http://x/> .\n@base <http://y/> .\nex:a ex:b ex:c .\n").is_empty()
    );
}

#[test]
fn datatypes() {
    let ttl = "@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
               <http://x/s> <http://x/p> \"abc\"^^xsd:integer , \"300\"^^xsd:byte , \"2020-13-01\"^^xsd:date ,\n\
               \"12\"^^xsd:interger , \"x\"^^xsd:string , \" 1\"^^xsd:int ,\n\
               \"1\"^^<http://www.w3.org/2001/XMLSchema/integer> , \"99999999999999999999999\"^^xsd:integer ,\n\
               \"2020-01-01T00:00:00Z\"^^xsd:dateTime , \"1e3\"^^xsd:double , \"P1D\"^^xsd:duration , \"true\"^^xsd:boolean .\n";
    assert_eq!(
        turtle(ttl),
        [
            "suspicious-datatype@\"abc\"^^xsd:integer",
            "suspicious-datatype@\"300\"^^xsd:byte",
            "suspicious-datatype@\"2020-13-01\"^^xsd:date",
            "suspicious-datatype@xsd:interger",
            "redundant-datatype@^^xsd:string",
            "suspicious-datatype@\" 1\"^^xsd:int",
            "suspicious-datatype@<http://www.w3.org/2001/XMLSchema/integer>",
        ]
    );
    let q = "SELECT ?s { ?s ?p \"x\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#langString> }";
    assert!(
        rules(q, Language::Sparql).contains(&"suspicious-datatype")
            || rules(q, Language::Sparql) == ["syntax"]
    );
}

#[test]
fn iris_with_spaces() {
    let q = "SELECT ?i { BIND(IRI(\"http://x/a b\") AS ?i) }";
    assert_eq!(sparql(q), ["iri-space@\"http://x/a b\""]);
    let ttl =
        "<http://x/s> <http://x/p> \"http://x/a b\"^^<http://www.w3.org/2001/XMLSchema#anyURI> .\n";
    assert_eq!(rules(ttl, Language::Turtle), ["iri-space"]);
    // an IRI written with a space does not parse: the lint says why
    let d = diags("<http://x/a b> <http://x/p> 1 .\n", Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].rule, "iri-space");
    assert_eq!(d[0].severity, Severity::Error);
    let d = diags("SELECT * { <http://x/a b> ?p ?o }", Language::Sparql);
    assert_eq!(d[0].rule, "iri-space", "{d:?}");
}

#[test]
fn deprecated_names() {
    let q = "PREFIX afn: <http://jena.hpl.hp.com/ARQ/function#>\nSELECT ?l { BIND(afn:localname(<http://x/a>) AS ?l) }";
    assert_eq!(
        rules(q, Language::Sparql),
        ["deprecated-syntax"],
        "{:?}",
        diags(q, Language::Sparql)
    );
    let ttl = "@prefix owl: <http://www.w3.org/2002/07/owl#> .\n<http://x/d> a owl:DataRange .\n";
    assert_eq!(turtle(ttl), ["deprecated-syntax@owl:DataRange"]);
}

#[test]
fn levels_turn_rules_off_and_change_severity() {
    let mut o = LintOptions::default();
    o.set("unused-prefix", "error").unwrap();
    o.set("cartesian-product", "off").unwrap();
    let q = "PREFIX ex: <http://x/>\nSELECT ?a ?b { ?a <http://x/p> 1 . ?b <http://x/q> 2 }";
    let d = lint(q, Language::Sparql, &o).unwrap().diagnostics;
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        (d[0].rule, d[0].severity),
        ("unused-prefix", Severity::Error)
    );
    assert!(o.set("no-such-rule", "error").is_err());
    assert!(o.set("unused-prefix", "loud").is_err());
    assert!(o.set("syntax", "off").is_err());
    assert_eq!(Severity::parse("WARN").unwrap(), Some(Severity::Warning));
}

#[test]
fn positions() {
    let q = "PREFIX ex: <http://x/>\nSELECT ?s {\n  ?s ?p ?o .\n  é:a ?q ?r }";
    let d = diags(q, Language::Sparql);
    let u = d.iter().find(|d| d.rule == "undefined-prefix").unwrap();
    assert_eq!((u.line, u.column, u.end_line, u.end_column), (4, 3, 4, 6));
}

#[test]
fn fixes() {
    let q = "PREFIX ex: <http://x/>\nPREFIX unused: <http://u/>\nSELECT ?s { ?s ex:p \"a\"@en-us , \"b\"^^<http://www.w3.org/2001/XMLSchema#string> }\n";
    let f = fix(q, Language::Sparql, &LintOptions::default()).unwrap();
    assert_eq!(
        f.text,
        "PREFIX ex: <http://x/>\nSELECT ?s { ?s ex:p \"a\"@en-US , \"b\" }\n"
    );
    assert_eq!(f.applied, 3);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);

    let ttl =
        "@prefix ex: <http://x/> .\n@prefix u: <http://u/> . # unused\nex:a ex:b \"c\"@EN .\n";
    let f = fix(ttl, Language::Turtle, &LintOptions::default()).unwrap();
    assert_eq!(
        f.text,
        "@prefix ex: <http://x/> .\n# unused\nex:a ex:b \"c\"@en .\n"
    );

    // a rule turned off is not fixed, and unsafe rules never are
    let mut o = LintOptions::default();
    o.set("unused-prefix", "off").unwrap();
    let f = fix(ttl, Language::Turtle, &o).unwrap();
    assert!(f.text.contains("@prefix u:"));
    let f = fix(
        "SELECT ?s { ?s ?p ?o }",
        Language::Sparql,
        &LintOptions::default(),
    )
    .unwrap();
    assert_eq!(f.applied, 0);
    // a syntax error: nothing to fix
    let f = fix("SELECT * {", Language::Sparql, &LintOptions::default()).unwrap();
    assert_eq!((f.applied, f.diagnostics[0].rule), (0, "syntax"));
}

#[test]
fn other_languages_are_refused() {
    assert_eq!(
        lint("<a> <b> <c> .", Language::NTriples, &LintOptions::default()),
        Err(LintError::UnsupportedLanguage("N-Triples"))
    );
}

#[test]
fn every_rule_is_documented_once() {
    let mut ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), RULES.len());
    assert!(RULES.iter().all(|r| !r.summary.is_empty()));
}
