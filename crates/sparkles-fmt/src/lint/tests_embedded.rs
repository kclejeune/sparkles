//! The SPARQL in `sh:select`, `sh:ask`, `sh:construct` and `sh:update` literals: the
//! prefixes and pre-bound variables of SHACL, and the mapping of findings back into the
//! literal.

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

/// Like `found`, without the hints about unused context terms.
fn found_q(text: &str, lang: Language) -> Vec<String> {
    diags(text, lang)
        .into_iter()
        .filter(|d| d.rule != "unused-context-term")
        .map(|d| format!("{}@{}", d.rule, &text[d.start..d.end]))
        .collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

const HEAD: &str = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
                    @prefix ex: <http://example.org/> .\n\
                    @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
                    ex:Decl sh:declare [ sh:prefix \"d\" ; sh:namespace \"http://declared.example/\"^^xsd:anyURI ] .\n";

fn ttl(body: &str) -> String {
    format!("{HEAD}{body}")
}

#[test]
fn a_long_select_is_linted_and_mapped() {
    let doc = ttl(
        "ex:S a sh:NodeShape ;\n  sh:sparql [\n    sh:prefixes ex:Decl ;\n    sh:select \"\"\"\n      SELECT $this ?value\n      WHERE {\n        $this ex:p ?value .\n        FILTER(?value = ?missing)\n      }\"\"\" ;\n  ] .\n",
    );
    let d = diags(&doc, Language::Turtle);
    assert_eq!(
        d.iter()
            .map(|d| (d.rule, &doc[d.start..d.end]))
            .collect::<Vec<_>>(),
        vec![("unbound-variable", "?missing")]
    );
    // lines and columns are those of the document
    assert_eq!((d[0].line, d[0].column), (12, 25));
    assert_eq!(d[0].severity, Severity::Warning);
}

#[test]
fn pre_bound_variables_count_as_bound() {
    let doc = ttl("ex:S sh:sparql [ sh:prefixes ex:Decl ; sh:select \"\"\"\n\
        SELECT $this ?value\n\
        WHERE {\n\
          $this $PATH ?value .\n\
          FILTER(?value != $currentShape && BOUND($shapesGraph))\n\
        }\"\"\" ] .\n");
    assert_eq!(found(&doc, Language::Turtle), Vec::<String>::new());
    // a variable that is not pre-bound is still reported
    let doc = ttl(
        "ex:S sh:sparql [ sh:select \"\"\"SELECT $this WHERE { $this a ex:C . FILTER($other) }\"\"\" ] .\n",
    );
    assert_eq!(
        found(&doc, Language::Turtle),
        strs(&["unbound-variable@$other"])
    );
    // the parameters of a constraint component are pre-bound in its validators
    let doc = ttl(
        "ex:MinLen a sh:ConstraintComponent ;\n  sh:parameter [ sh:path ex:minLen ] ;\n  sh:validator [ a sh:SPARQLAskValidator ; sh:ask \"ASK { FILTER(STRLEN(STR($value)) >= $minLen && $maxLen > 0) }\" ] .\n",
    );
    assert_eq!(
        found(&doc, Language::Turtle),
        strs(&["unbound-variable@$maxLen"])
    );
}

#[test]
fn prefixes_come_from_sh_declare_and_the_document() {
    let doc = ttl("ex:S sh:sparql [ sh:prefixes ex:Decl ; sh:select \"\"\"\n\
        SELECT $this WHERE { $this d:p ex:o . $this nope:q ?x . FILTER(?x) }\"\"\" ] .\n");
    assert_eq!(
        found(&doc, Language::Turtle),
        strs(&["undefined-prefix@nope:q"])
    );
    // the prefixes of the query itself win, and an unused one is a finding that stays in
    // the literal; the injected prefixes are never reported
    let doc = ttl("ex:S sh:sparql [ sh:prefixes ex:Decl ; sh:select \"\"\"\n\
        PREFIX u: <http://u.example/>\n\
        SELECT $this WHERE { $this d:p ?x . FILTER(?x) }\"\"\" ] .\n");
    assert_eq!(
        found(&doc, Language::Turtle),
        strs(&["unused-prefix@PREFIX u: <http://u.example/>"])
    );
    // prefixes of an owl:imports closure
    let doc = ttl(
        "ex:Imp <http://www.w3.org/2002/07/owl#imports> ex:Decl .\nex:S sh:sparql [ sh:prefixes ex:Imp ; sh:select \"SELECT $this WHERE { $this d:p ?x . FILTER(?x) }\" ] .\n",
    );
    assert_eq!(found(&doc, Language::Turtle), Vec::<String>::new());
}

#[test]
fn escapes_and_short_strings_map_exactly() {
    // newlines and a unicode escape before the finding, in a short string
    let doc = ttl(
        "ex:S sh:sparql [ sh:select \"SELECT $this\\nWHERE {\\n  $this ex:p \\\"\\u00e9\\\" .\\n  FILTER(?zz > 1)\\n}\" ] .\n",
    );
    let d = diags(&doc, Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        (d[0].rule, &doc[d[0].start..d[0].end]),
        ("unbound-variable", "?zz")
    );
    assert!(d[0].fix.is_none());
    // a single-quoted long string
    let doc = ttl(
        "ex:S sh:sparql [ sh:select '''SELECT $this { $this ex:p ?x FILTER(?q && ?x) }''' ] .\n",
    );
    let d = diags(&doc, Language::Turtle);
    assert_eq!(
        d.iter().map(|d| &doc[d.start..d.end]).collect::<Vec<_>>(),
        ["?q"]
    );
    // a finding that covers an escape covers all of it
    let doc =
        ttl("ex:S sh:sparql [ sh:select \"SELECT $this { $this ex:p \\\"a\\\"@en-us }\" ] .\n");
    let d = diags(&doc, Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&doc[d[0].start..d[0].end], "@en-us");
}

#[test]
fn unicode_before_a_finding_is_mapped_by_bytes() {
    let doc = ttl(
        "ex:S sh:sparql [ sh:select \"\"\"SELECT $this { $this ex:p \"é😀ü\" . FILTER(?ü > 1) }\"\"\" ] .\n",
    );
    let d = diags(&doc, Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&doc[d[0].start..d[0].end], "?ü");
    // columns count characters
    let line = doc.lines().nth(4).unwrap();
    assert_eq!(
        line.chars()
            .take(d[0].column as usize - 1)
            .collect::<String>(),
        line[..line.find("?ü").unwrap()]
    );
}

#[test]
fn syntax_errors_are_mapped_into_the_literal() {
    let doc =
        ttl("ex:S sh:sparql [ sh:select \"\"\"SELECT $this\nWHERE { $this ex:p }\"\"\" ] .\n");
    let d = diags(&doc, Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].rule, d[0].severity), ("syntax", Severity::Error));
    // where the SPARQL lint puts the same error in the query alone
    let query = "SELECT $this\nWHERE { $this ex:p }";
    let prelude = "PREFIX ex: <http://example.org/>\n";
    let alone = diags(&format!("{prelude}{query}"), Language::Sparql);
    assert_eq!(alone.len(), 1, "{alone:?}");
    let at = doc.find(query).unwrap() + alone[0].start - prelude.len();
    assert_eq!(d[0].start, at, "{d:?}");
    assert_eq!(d[0].line, 6, "{d:?}");
}

#[test]
fn every_query_predicate_is_linted() {
    for (pred, query) in [
        (
            "sh:select",
            "SELECT $this { $this ex:p ?o FILTER(?bad = ?o) }",
        ),
        ("sh:ask", "ASK { $this ex:p ?o FILTER(?bad = ?o) }"),
        (
            "sh:construct",
            "CONSTRUCT { $this ex:q ?bad } WHERE { $this ex:p ?o FILTER(?o) }",
        ),
        (
            "sh:update",
            "DELETE { $this ex:p ?o } INSERT { $this ex:q ?bad } WHERE { $this ex:p ?o }",
        ),
    ] {
        let doc = ttl(&format!(
            "ex:S sh:sparql [ {pred} \"\"\"{query}\"\"\" ] .\n"
        ));
        assert_eq!(
            found(&doc, Language::Turtle),
            strs(&["unbound-variable@?bad"]),
            "{pred}"
        );
    }
    // the full IRI, and TriG
    let doc = ttl(
        "ex:S sh:sparql [ <http://www.w3.org/ns/shacl#select> \"SELECT $this { $this ex:p ?o FILTER(?bad = ?o) }\" ] .\n",
    );
    assert_eq!(
        found(&doc, Language::Turtle),
        strs(&["unbound-variable@?bad"])
    );
    let doc =
        ttl("ex:G { ex:S sh:sparql [ sh:ask \"ASK { $this ex:p ?o FILTER(?bad = ?o) }\" ] . }\n");
    assert_eq!(
        found(&doc, Language::TriG),
        strs(&["unbound-variable@?bad"])
    );
}

#[test]
fn only_plain_strings_are_queries() {
    for object in [
        "\"SELECT $this { FILTER(?bad = ?o) }\"@en",
        "ex:NotAQuery",
        "42",
        "\"not sparql\"^^xsd:token",
    ] {
        let doc = ttl(&format!("ex:S sh:sparql [ sh:select {object} ] .\n"));
        assert_eq!(
            found(&doc, Language::Turtle),
            Vec::<String>::new(),
            "{object}"
        );
    }
    // an explicit xsd:string is a plain string
    let doc = ttl(
        "ex:S sh:sparql [ sh:select \"SELECT $this { $this ex:p ?o FILTER(?bad = ?o) }\"^^xsd:string ] .\n",
    );
    assert!(found(&doc, Language::Turtle).contains(&"unbound-variable@?bad".to_string()));
    // other predicates are not queries
    let doc = ttl("ex:S ex:select \"SELECT $this { FILTER(?bad = ?o) }\" .\n");
    assert_eq!(found(&doc, Language::Turtle), Vec::<String>::new());
}

#[test]
fn rules_apply_inside_the_literal() {
    let mut o = LintOptions::default();
    o.set("unbound-variable", "off").unwrap();
    o.set("language-tag-case", "error").unwrap();
    let doc = ttl(
        "ex:S sh:sparql [ sh:select \"\"\"SELECT $this { $this ex:p \"a\"@en-us FILTER(?bad = ?o) }\"\"\" ] .\n",
    );
    let d = lint(&doc, Language::Turtle, &o).unwrap().diagnostics;
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        (d[0].rule, d[0].severity),
        ("language-tag-case", Severity::Error)
    );
}

#[test]
fn fixes_only_in_long_strings_without_escapes() {
    let query = "PREFIX u: <http://u.example/>\nSELECT $this {\n  $this ex:p \"a\"@en-us ;\n    ex:q \"b\"^^xsd:string .\n}";
    let long = ttl(&format!(
        "ex:S sh:sparql [ sh:select \"\"\"{query}\"\"\" ] .\n"
    ));
    let d = diags(&long, Language::Turtle);
    assert_eq!(
        d.iter().map(|d| d.rule).collect::<Vec<_>>(),
        ["unused-prefix", "language-tag-case", "redundant-datatype"]
    );
    assert!(d.iter().all(|d| d.fix.is_some()), "{d:?}");
    let f = fix(&long, Language::Turtle, &LintOptions::default()).unwrap();
    assert!(f.text.contains("\"\"\"SELECT $this {"), "{}", f.text);
    assert!(f.text.contains("\"a\"@en-US ;"), "{}", f.text);
    assert!(f.text.contains("ex:q \"b\" ."), "{}", f.text);
    assert!(!f.text.contains("PREFIX u:"), "{}", f.text);
    assert_eq!(f.applied, 3);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    // the same query in a short string with escapes, or in a short string
    let short =
        ttl("ex:S sh:sparql [ sh:select \"SELECT $this { $this ex:p \\\"a\\\"@en-us }\" ] .\n");
    let d = diags(&short, Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].fix.is_none());
    let f = fix(&short, Language::Turtle, &LintOptions::default()).unwrap();
    assert_eq!((f.text.as_str(), f.applied), (short.as_str(), 0));
    let short_plain =
        ttl("ex:S sh:sparql [ sh:select \"SELECT $this { $this ex:p 'a'@en-us }\" ] .\n");
    let d = diags(&short_plain, Language::Turtle);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].fix.is_none());
}

// --------------------------------------------------------------------- JSON-LD ------

const JSON_HEAD: &str = r#""@context": {"sh": "http://www.w3.org/ns/shacl#", "ex": "http://example.org/", "d": "http://declared.example/"}"#;

#[test]
fn json_ld_queries_are_linted() {
    let doc = format!(
        "{{\n  {JSON_HEAD},\n  \"@id\": \"ex:S\",\n  \"sh:sparql\": {{\n    \"sh:select\": \"SELECT $this ?v\\nWHERE {{\\n  $this d:p ?v .\\n  FILTER(?missing)\\n}}\"\n  }}\n}}"
    );
    // the context's IRIs are the document's prefixes
    let d: Vec<_> = diags(&doc, Language::JsonLd)
        .into_iter()
        .filter(|d| d.rule != "unused-context-term")
        .collect();
    assert_eq!(
        d.iter()
            .map(|d| (d.rule, &doc[d.start..d.end]))
            .collect::<Vec<_>>(),
        vec![("unbound-variable", "?missing")]
    );
    assert_eq!(d[0].line, 5);
    assert!(d[0].fix.is_none());
    // a value object, the full IRI as the key, and an array
    let doc = format!(
        "{{{JSON_HEAD}, \"@id\": \"ex:S\", \"sh:sparql\": [{{\"http://www.w3.org/ns/shacl#ask\": {{\"@value\": \"ASK {{ $this ex:p ?o FILTER(?bad = ?o) }}\"}}}}]}}"
    );
    assert_eq!(
        found_q(&doc, Language::JsonLd),
        strs(&["unbound-variable@?bad"])
    );
    // sh:prefixes and sh:declare in the same document
    let doc = r#"{"@context": {"sh": "http://www.w3.org/ns/shacl#"}, "@graph": [
      {"@id": "http://e/Decl", "sh:declare": {"sh:prefix": "q", "sh:namespace": "http://q.example/"}},
      {"@id": "http://e/S", "sh:sparql": {"sh:prefixes": {"@id": "http://e/Decl"}, "sh:select": "SELECT $this { $this q:p ?o . $this r:p ?o }"}}]}"#;
    assert_eq!(
        found_q(doc, Language::JsonLd),
        strs(&["undefined-prefix@r:p"])
    );
    // pre-bound variables
    let doc = format!(
        "{{{JSON_HEAD}, \"@id\": \"ex:S\", \"sh:sparql\": {{\"sh:select\": \"SELECT $this {{ $this $PATH $value }}\"}}}}"
    );
    assert_eq!(found_q(&doc, Language::JsonLd), Vec::<String>::new());
    // a syntax error lands in the string
    let doc = format!(
        "{{{JSON_HEAD}, \"@id\": \"ex:S\", \"sh:sparql\": {{\"sh:select\": \"SELECT $this {{ $this ex:p \"}}}}"
    );
    let d: Vec<_> = diags(&doc, Language::JsonLd)
        .into_iter()
        .filter(|d| d.rule != "unused-context-term")
        .collect();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].rule, "syntax");
}

#[test]
fn document_prefixes_used_by_queries_are_used() {
    let doc = ttl(
        "@prefix qq: <http://qq.org/> .\n<urn:s> sh:select \"\"\"SELECT $this { $this qq:p ?o FILTER(?o) }\"\"\" .\n",
    );
    assert_eq!(found_q(&doc, Language::Turtle), Vec::<String>::new());
    let f = fix(&doc, Language::Turtle, &LintOptions::default()).unwrap();
    assert_eq!(f.text, doc);
    assert!(f.diagnostics.iter().all(|d| d.rule != "undefined-prefix"));
    // in TriG too, and a prefix that nothing uses still goes
    let doc = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix ex: <http://ex.org/> .\n@prefix u: <http://u/> .\n<urn:g> { <urn:s> sh:ask \"ASK { ex:a ex:b ex:c }\" }\n";
    assert_eq!(
        found_q(doc, Language::TriG),
        strs(&["unused-prefix@@prefix u: <http://u/> ."])
    );
}

#[test]
fn each_holder_has_its_own_prefixes_and_parameters() {
    // the same query text on two nodes, and only the first declares p:
    let q = "SELECT $this { $this p:p ?o FILTER(?o) }";
    let doc = ttl(&format!(
        "<urn:a> sh:prefixes <urn:decl> ; sh:select \"{q}\" .\n<urn:b> sh:select \"{q}\" .\n<urn:decl> sh:declare [ sh:prefix \"p\" ; sh:namespace \"http://p/\" ] .\n"
    ));
    let d = diags(&doc, Language::Turtle);
    let undefined: Vec<_> = d.iter().filter(|d| d.rule == "undefined-prefix").collect();
    assert_eq!(undefined.len(), 1, "{d:?}");
    assert!(undefined[0].start > doc.find("<urn:b>").unwrap(), "{d:?}");
    // parameters of a component reach its own validator only
    let q = "ASK { FILTER(?limit > 1) }";
    let doc = ttl(&format!(
        "<urn:c> sh:parameter [ sh:path <http://x/limit> ] ; sh:validator <urn:v1> .\n<urn:v1> sh:ask \"{q}\" .\n<urn:v2> sh:ask \"{q}\" .\n"
    ));
    let d = diags(&doc, Language::Turtle);
    let unbound: Vec<_> = d.iter().filter(|d| d.rule == "unbound-variable").collect();
    assert_eq!(unbound.len(), 1, "{d:?}");
    assert!(unbound[0].start > doc.find("<urn:v2>").unwrap(), "{d:?}");
    // blank nodes and named graphs keep their holders apart as well
    let doc = format!(
        "@prefix sh: <http://www.w3.org/ns/shacl#> .\n<urn:g1> {{ [ sh:prefixes <urn:decl> ; sh:select \"{q2}\" ] . <urn:decl> sh:declare [ sh:prefix \"p\" ; sh:namespace \"http://p/\" ] . }}\n<urn:g2> {{ [ sh:prefixes <urn:decl> ; sh:select \"{q2}\" ] . }}\n",
        q2 = "SELECT $this { $this p:p ?o FILTER(?o) }"
    );
    let d = diags(&doc, Language::TriG);
    let undefined: Vec<_> = d.iter().filter(|d| d.rule == "undefined-prefix").collect();
    assert_eq!(undefined.len(), 1, "{d:?}");
    assert!(undefined[0].start > doc.find("<urn:g2>").unwrap(), "{d:?}");
}

#[test]
fn json_ld_holders_have_their_own_prefixes() {
    let q = "SELECT $this { $this p:p ?o FILTER(?o) }";
    let doc = format!(
        r#"{{"@context": {{"sh": "http://www.w3.org/ns/shacl#"}}, "@graph": [
      {{"@id": "http://e/Decl", "sh:declare": {{"sh:prefix": "p", "sh:namespace": "http://p/"}}}},
      {{"sh:select": "{q}", "@id": "http://e/A", "sh:prefixes": {{"@id": "http://e/Decl"}}}},
      {{"@id": "http://e/B", "sh:select": "{q}"}},
      {{"sh:select": "{q}"}}]}}"#
    );
    let d = diags(&doc, Language::JsonLd);
    let undefined: Vec<_> = d.iter().filter(|d| d.rule == "undefined-prefix").collect();
    assert_eq!(undefined.len(), 2, "{d:?}");
    assert!(
        undefined[0].start > doc.find("http://e/B").unwrap(),
        "{d:?}"
    );
}
