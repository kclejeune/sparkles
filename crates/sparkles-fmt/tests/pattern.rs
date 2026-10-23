//! The pattern of a query for the visual query builder (spec U08 §3.2, §4.1): the subset
//! classification of each construct, the ranges of `;` and `,` abbreviations, and, over
//! the W3C suites and the golden inputs, the round-trip property A3: every query that
//! parses has a pattern, every locked block's text is the source at its range, and the
//! items of every group cover its tokens, so a splice at the reported ranges can leave
//! every other byte as it was.

mod corpus;

use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::pattern::{
    Form, Group, GroupKind, Item, Locked, Path, Pattern, PatternError, PredKind, TermKind, Triple,
    pattern,
};

fn ok(text: &str) -> Pattern {
    pattern(text).unwrap_or_else(|e| panic!("{text}: {e:?}"))
}

fn items(p: &Pattern) -> &[Item] {
    &p.root.as_ref().expect("a WHERE group").items
}

fn triples(items: &[Item]) -> Vec<&Triple> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Triple(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn locked(items: &[Item]) -> Vec<&Locked> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Locked(l) => Some(l),
            _ => None,
        })
        .collect()
}

fn at(text: &str, r: &std::ops::Range<usize>) -> String {
    text[r.clone()].to_string()
}

const A3: &str = "PREFIX ex: <http://example.org/>
SELECT ?person ?org WHERE {
  ?person a ex:Person ; ex:worksFor ?org .
  ?org a ex:Org ; ex:locatedIn ex:lyon .
}
LIMIT 1000
";

#[test]
fn a1_draws_the_query_of_u03_a3() {
    let p = ok(A3);
    assert_eq!(p.form, Form::Select);
    assert_eq!(p.prefixes.len(), 1);
    assert_eq!(
        (p.prefixes[0].prefix.as_str(), p.prefixes[0].iri.as_str()),
        ("ex", "http://example.org/")
    );
    let pr = p.projection.as_ref().unwrap();
    assert_eq!(pr.vars, ["person", "org"]);
    assert!(!pr.star && !pr.distinct && !pr.locked);
    assert_eq!(at(A3, &pr.range), "SELECT ?person ?org");
    assert_eq!(p.limit.as_ref().map(|l| l.value), Some(1000));
    assert_eq!(at(A3, &p.limit.as_ref().unwrap().range), "LIMIT 1000");
    let ts = triples(items(&p));
    assert_eq!(ts.len(), 4);
    let spo: Vec<(String, String, String)> = ts
        .iter()
        .map(|t| (t.s.value.clone(), t.p.value.clone(), t.o.value.clone()))
        .collect();
    let ty = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string();
    assert_eq!(
        spo,
        [
            (
                "person".into(),
                ty.clone(),
                "http://example.org/Person".into()
            ),
            (
                "person".into(),
                "http://example.org/worksFor".into(),
                "org".into()
            ),
            ("org".into(), ty, "http://example.org/Org".into()),
            (
                "org".into(),
                "http://example.org/locatedIn".into(),
                "http://example.org/lyon".into()
            ),
        ]
    );
    // each triple of a `;` list has its own predicate and object ranges
    let t = ts[1];
    assert_eq!(at(A3, &t.p.range), "ex:worksFor");
    assert_eq!(at(A3, &t.o.range), "?org");
    assert_eq!(at(A3, &t.s.range), "?person");
    assert_eq!(at(A3, &t.range), "?person a ex:Person ; ex:worksFor ?org .");
    assert_eq!(at(A3, &ts[0].entry_range), "a ex:Person ;");
    assert_eq!(at(A3, &t.entry_range), "ex:worksFor ?org");
    assert_eq!((t.stmt, t.entry, t.object), (0, 1, 0));
    assert_eq!((ts[3].stmt, ts[3].entry, ts[3].object), (1, 1, 0));
    assert_eq!(ts[3].o.kind, TermKind::Iri);
    let root = p.root.as_ref().unwrap();
    assert_eq!(&A3[root.inner.start - 1..root.inner.start], "{");
    assert_eq!(&A3[root.inner.end..root.inner.end + 1], "}");
}

#[test]
fn object_lists_give_a_triple_per_object() {
    let q = "SELECT * { ?s <http://x/p> ?a, \"b\"@en , 3 ; <http://x/q> _:b . }";
    let p = ok(q);
    let ts = triples(items(&p));
    assert_eq!(ts.len(), 4);
    let objects: Vec<String> = ts.iter().map(|t| at(q, &t.o.range)).collect();
    assert_eq!(objects, ["?a", "\"b\"@en", "3", "_:b"]);
    assert_eq!(at(q, &ts[0].object_range), "?a,");
    assert_eq!(at(q, &ts[1].object_range), "\"b\"@en ,");
    assert_eq!(at(q, &ts[2].object_range), "3");
    assert_eq!(ts[1].o.kind, TermKind::Literal);
    assert_eq!(ts[3].o.kind, TermKind::BNode);
    assert_eq!(ts[3].o.value, "b");
    assert_eq!((ts[2].entry, ts[2].object), (0, 2));
    assert_eq!((ts[3].entry, ts[3].object), (1, 0));
    assert!(ts.iter().all(|t| t.stmt == 0));
}

#[test]
fn a2_a_bind_is_a_locked_block_with_its_comment() {
    let q = "PREFIX ex: <http://example.org/>
SELECT * WHERE {
  ?person ex:born ?b .
  BIND(YEAR(NOW()) AS ?y) # year
  FILTER(?y > 2000)
}";
    let p = ok(q);
    let l = locked(items(&p));
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].kind, "BIND");
    assert_eq!(l[0].text, "BIND(YEAR(NOW()) AS ?y) # year");
    assert_eq!(l[0].vars, ["y"]);
    assert_eq!(at(q, &l[0].range), l[0].text);
    let Item::Filter(f) = &items(&p)[2] else {
        panic!("a filter")
    };
    let form = f.form.as_ref().unwrap();
    assert_eq!((form.op.as_str(), form.var.as_str()), (">", "y"));
    assert_eq!(form.args, ["2000"]);
}

#[test]
fn every_construct_of_the_subset_table() {
    // (query, the kinds of the root's items: a triple `t`, a filter `f` with or
    // without a form, an optional group `o`, or a locked block's kind)
    let cases: &[(&str, &[&str])] = &[
        ("SELECT * { ?s ?p ?o }", &["t"]),
        ("SELECT * { ?s a <http://x/C> }", &["t"]),
        ("SELECT * { _:b <http://x/p> ?o }", &["t"]),
        ("SELECT * { ?s <http://x/p> 'x' }", &["t"]),
        // literal subjects, blank node lists, collections, anonymous nodes
        ("SELECT * { 'x' <http://x/p> ?o }", &["triples"]),
        (
            "SELECT * { ?s <http://x/p> [ <http://x/q> 1 ] }",
            &["triples"],
        ),
        ("SELECT * { [ <http://x/q> 1 ] }", &["triples"]),
        ("SELECT * { ?s <http://x/p> (1 2) }", &["triples"]),
        ("SELECT * { ?s <http://x/p> [] }", &["triples"]),
        ("SELECT * { ?s <http://x/p> () }", &["triples"]),
        // RDF 1.2 triple terms, reifiers and annotations
        (
            "SELECT * { ?s <http://x/p> <<( ?a <http://x/b> ?c )>> }",
            &["triples"],
        ),
        (
            "SELECT * { << ?a <http://x/b> ?c >> <http://x/p> ?o }",
            &["triples"],
        ),
        ("SELECT * { ?s <http://x/p> ?o ~ ?r }", &["triples"]),
        (
            "SELECT * { ?s <http://x/p> ?o {| <http://x/q> 1 |} }",
            &["triples"],
        ),
        // simple paths
        ("SELECT * { ?s <http://x/p>+ ?o }", &["t"]),
        ("SELECT * { ?s <http://x/p>* ?o }", &["t"]),
        ("SELECT * { ?s <http://x/p>? ?o }", &["t"]),
        ("SELECT * { ?s ^<http://x/p> ?o }", &["t"]),
        ("SELECT * { ?s ^<http://x/p>+ ?o }", &["t"]),
        ("SELECT * { ?s <http://x/p>/<http://x/q> ?o }", &["t"]),
        ("SELECT * { ?s <http://x/p>|a ?o }", &["t"]),
        ("SELECT * { ?s <http://x/p>{1,3} ?o }", &["t"]),
        // negated sets, nested groups and mixed paths lock the statement
        ("SELECT * { ?s !<http://x/p> ?o }", &["triples"]),
        ("SELECT * { ?s (<http://x/p>) ?o }", &["triples"]),
        (
            "SELECT * { ?s <http://x/p>/^<http://x/q> ?o }",
            &["triples"],
        ),
        (
            "SELECT * { ?s (<http://x/p>|<http://x/q>)+ ?o }",
            &["triples"],
        ),
        // filters: a form or a locked chip, never a locked block
        ("SELECT * { ?s ?p ?o FILTER(?o = 1) }", &["t", "f="]),
        ("SELECT * { ?s ?p ?o FILTER(1 < ?o) }", &["t", "f>"]),
        (
            "SELECT * { ?s ?p ?o FILTER(?o != <http://x/a>) }",
            &["t", "f!="],
        ),
        (
            "SELECT * { ?s ?p ?o FILTER(CONTAINS(?o, 'a')) }",
            &["t", "fcontains"],
        ),
        (
            "SELECT * { ?s ?p ?o FILTER STRSTARTS(?o, \"A\") }",
            &["t", "fstrstarts"],
        ),
        (
            "SELECT * { ?s ?p ?o FILTER(regex(?o, 'a', 'i')) }",
            &["t", "fregex"],
        ),
        (
            "SELECT * { ?s ?p ?o FILTER(LANGMATCHES(LANG(?o), 'en')) }",
            &["t", "flangmatches"],
        ),
        ("SELECT * { ?s ?p ?o FILTER(BOUND(?o)) }", &["t", "fbound"]),
        ("SELECT * { ?s ?p ?o FILTER(isIRI(?o)) }", &["t", "fisIRI"]),
        (
            "SELECT * { ?s ?p ?o FILTER(isLiteral(?o)) }",
            &["t", "fisLiteral"],
        ),
        ("SELECT * { ?s ?p ?o FILTER((?o > 1)) }", &["t", "f>"]),
        (
            "SELECT * { ?s ?p ?o FILTER(?o > 1 && ?o < 9) }",
            &["t", "f"],
        ),
        ("SELECT * { ?s ?p ?o FILTER(?o = ?s) }", &["t", "f"]),
        (
            "SELECT * { ?s ?p ?o FILTER NOT EXISTS { ?o ?p ?s } }",
            &["t", "f"],
        ),
        ("SELECT * { ?s ?p ?o FILTER(STRLEN(?o) > 2) }", &["t", "f"]),
        // one level of OPTIONAL with triples and filters
        (
            "SELECT * { ?s ?p ?o OPTIONAL { ?o ?q ?r FILTER(?r > 1) } }",
            &["t", "o"],
        ),
        (
            "SELECT * { ?s ?p ?o OPTIONAL { ?o ?q ?r OPTIONAL { ?r ?x ?y } } }",
            &["t", "OPTIONAL"],
        ),
        (
            "SELECT * { ?s ?p ?o OPTIONAL { ?o ?q ?r BIND(1 AS ?z) } }",
            &["t", "OPTIONAL"],
        ),
        (
            "SELECT * { ?s ?p ?o OPTIONAL { ?o ?q [] } }",
            &["t", "OPTIONAL"],
        ),
        // everything else is locked
        (
            "SELECT * { ?s ?p ?o MINUS { ?s a <http://x/C> } }",
            &["t", "MINUS"],
        ),
        ("SELECT * { { ?s ?p ?o } UNION { ?o ?p ?s } }", &["UNION"]),
        ("SELECT * { { ?s ?p ?o } }", &["group"]),
        ("SELECT * { GRAPH ?g { ?s ?p ?o } }", &["GRAPH"]),
        (
            "SELECT * { SERVICE <http://x/s> { ?s ?p ?o } }",
            &["SERVICE"],
        ),
        ("SELECT * { BIND(1 AS ?x) }", &["BIND"]),
        ("SELECT * { VALUES ?x { 1 2 } }", &["VALUES"]),
        ("SELECT * { { SELECT ?s { ?s ?p ?o } } }", &["subquery"]),
        ("SELECT * { SELECT ?s { ?s ?p ?o } }", &["subquery"]),
        (
            "SELECT * { ?s ?p ?o LATERAL { ?o ?q ?r } }",
            &["t", "LATERAL"],
        ),
    ];
    for (q, want) in cases {
        let p = ok(q);
        let got: Vec<String> = items(&p)
            .iter()
            .map(|i| match i {
                Item::Triple(_) => "t".to_string(),
                Item::Filter(f) => format!("f{}", f.form.as_ref().map_or("", |f| f.op.as_str())),
                Item::Group(g) => {
                    assert_eq!(g.kind, GroupKind::Optional);
                    "o".to_string()
                }
                Item::Locked(l) => l.kind.clone(),
            })
            .collect();
        assert_eq!(got, *want, "{q}");
    }
}

#[test]
fn paths_and_predicates() {
    let q = "PREFIX ex: <http://example.org/>
SELECT * { ?s ex:partOf+ ?o . ?s ^ex:in ?o . ?s ex:a/ex:b ?o . ?s ex:a|ex:b ?o . ?s ?p ?o . ?s a ?o }";
    let p = ok(q);
    let ts = triples(items(&p));
    let ex = |l: &str| Path::Iri {
        text: format!("ex:{l}"),
        iri: format!("http://example.org/{l}"),
    };
    assert_eq!(ts[0].p.kind, PredKind::Path);
    assert_eq!(ts[0].p.text, "ex:partOf+");
    assert_eq!(
        ts[0].p.path,
        Some(Path::Mod {
            op: "+".into(),
            item: Box::new(ex("partOf"))
        })
    );
    assert_eq!(ts[1].p.path, Some(Path::Inv(Box::new(ex("in")))));
    assert_eq!(ts[2].p.path, Some(Path::Seq(vec![ex("a"), ex("b")])));
    assert_eq!(ts[3].p.path, Some(Path::Alt(vec![ex("a"), ex("b")])));
    assert_eq!((ts[4].p.kind, ts[4].p.value.as_str()), (PredKind::Var, "p"));
    assert_eq!(ts[5].p.text, "a");
    assert_eq!(ts[5].p.kind, PredKind::Iri);
}

#[test]
fn projection_modifiers_and_other_forms() {
    let p = ok(
        "SELECT DISTINCT ?a (COUNT(*) AS ?n) { ?a ?b ?c } GROUP BY ?a ORDER BY ?n LIMIT 5 OFFSET 10",
    );
    let pr = p.projection.as_ref().unwrap();
    assert_eq!(pr.vars, ["a", "n"]);
    assert!(pr.distinct && pr.locked);
    let kinds: Vec<&str> = p.modifiers.iter().map(|m| m.kind.as_str()).collect();
    assert_eq!(kinds, ["GROUP BY", "ORDER BY"]);
    assert_eq!((p.limit.unwrap().value, p.offset.unwrap().value), (5, 10));
    assert!(
        ok("SELECT REDUCED ?a { ?a ?b ?c }")
            .projection
            .unwrap()
            .locked
    );
    assert!(ok("SELECT * { ?a ?b ?c }").projection.unwrap().star);
    let p = ok("SELECT * { ?a ?b ?c } VALUES ?a { 1 }");
    assert_eq!(p.modifiers[0].kind, "VALUES");

    let p = ok("ASK FROM <http://g> { ?a ?b ?c }");
    assert_eq!(p.form, Form::Ask);
    assert!(p.projection.is_none());
    assert_eq!(p.locked[0].kind, "FROM");
    let p = ok("CONSTRUCT { ?a ?b ?c } WHERE { ?a ?b ?c }");
    assert_eq!(p.form, Form::Construct);
    assert_eq!(p.locked[0].kind, "CONSTRUCT");
    assert_eq!(p.locked[0].text, "{ ?a ?b ?c }");
    assert_eq!(triples(items(&p)).len(), 1);
    let p = ok("CONSTRUCT WHERE { ?a ?b ?c }");
    assert_eq!(triples(items(&p)).len(), 1);
    let p = ok("DESCRIBE <http://x/a>");
    assert_eq!(p.form, Form::Describe);
    assert!(p.root.is_none());
    assert_eq!(p.locked[0].text, "DESCRIBE <http://x/a>");
    let p = ok("SELECT * { ?s ?p ?o } LIMIT 99999999999999999999999");
    assert!(p.limit.is_none());
    assert_eq!(p.modifiers[0].kind, "LIMIT");
}

#[test]
fn subqueries_share_their_projection() {
    let p = ok("SELECT * { ?a ?b ?c { SELECT ?a WHERE { ?a ?x ?y } } }");
    let l = locked(items(&p));
    assert_eq!(
        (l[0].kind.as_str(), l[0].vars.clone()),
        ("subquery", vec!["a".to_string()])
    );
}

#[test]
fn errors_and_updates() {
    match pattern("SELECT * {") {
        Err(PatternError::Syntax { line, column, .. }) => assert_eq!((line, column), (1, 11)),
        other => panic!("{other:?}"),
    }
    match pattern("PREFIX ex: <http://x/>\nSELECT * { ?s ex:p }") {
        Err(PatternError::Syntax { line, .. }) => assert_eq!(line, 2),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        pattern("INSERT DATA { <a:a> <a:b> <a:c> }"),
        Err(PatternError::Update)
    );
    assert_eq!(pattern("CLEAR ALL"), Err(PatternError::Update));
    assert!(matches!(
        pattern(""),
        Err(PatternError::Update | PatternError::Syntax { .. })
    ));
    // too deep to recurse over: refused, not a crash
    let deep = format!(
        "SELECT * {{ FILTER({}1{}) }}",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    assert!(matches!(pattern(&deep), Err(PatternError::Syntax { .. })));
}

#[test]
fn base_resolves_iris_and_namespaces() {
    let q =
        "BASE <http://x/>\nPREFIX r: <rel/>\nSELECT * { <thing> <p> ?o . <http://x/thing> r:q ?o }";
    let p = ok(q);
    assert_eq!(p.base.as_deref(), Some("http://x/"));
    assert_eq!(p.prefixes[0].iri, "http://x/rel/");
    let ts = triples(items(&p));
    assert_eq!(ts[0].s.value, "http://x/thing");
    assert_eq!(ts[0].s.value, ts[1].s.value);
    assert_eq!(ts[0].p.value, "http://x/p");
    assert_eq!(ts[1].p.value, "http://x/rel/q");
    // a later BASE resolves against the one before
    let p = ok("BASE <http://x/a/> BASE <b/> SELECT * { <c> ?p ?o }");
    assert_eq!(triples(items(&p))[0].s.value, "http://x/a/b/c");
}

#[test]
fn separator_dots_and_token_ends() {
    let q = "SELECT * { OPTIONAL { ?s <http://p> ?o } # note\n . FILTER(?o > 1) . BIND(1 AS ?x) # one\n . ?s <http://p> [ <http://q> 1 ] # c\n }";
    let p = ok(q);
    let its = items(&p);
    let Item::Group(g) = &its[0] else { panic!() };
    assert_eq!(at(q, g.dot.as_ref().unwrap()), ".");
    assert!(g.dot.as_ref().unwrap().start > q.find("# note").unwrap());
    let Item::Filter(f) = &its[1] else { panic!() };
    assert_eq!(at(q, f.dot.as_ref().unwrap()), ".");
    let Item::Locked(b) = &its[2] else { panic!() };
    assert_eq!(b.text, "BIND(1 AS ?x) # one");
    assert_eq!(&q[b.range.start..b.end], "BIND(1 AS ?x)");
    assert!(b.dot.is_some());
    let Item::Locked(t) = &its[3] else { panic!() };
    assert_eq!(t.kind, "triples");
    assert_eq!(&q[t.range.start..t.end], "?s <http://p> [ <http://q> 1 ]");
    assert_eq!(t.text, "?s <http://p> [ <http://q> 1 ] # c");
    assert!(t.dot.is_none());
}

#[test]
fn the_strict_check_adds_the_server_parser() {
    use sparkles_fmt::pattern::check;
    assert_eq!(check("SELECT * { ?s ?p ?o }"), Ok(()));
    // the lossless parser takes what fails only on scoping
    let scope = "SELECT * { ?s ?p ?o BIND(1 AS ?o) }";
    assert!(pattern(scope).is_ok());
    assert!(matches!(check(scope), Err(PatternError::Syntax { .. })));
    assert!(matches!(
        check("SELECT * {"),
        Err(PatternError::Syntax { .. })
    ));
    assert_eq!(check("CLEAR ALL"), Err(PatternError::Update));
    // undeclared prefixes pass: the query page declares them on Run
    assert_eq!(check("SELECT * { ?s ex:p ?o }"), Ok(()));
}

#[test]
fn ranges_for_the_edits_of_phase_2() {
    let q = "PREFIX ex: <http://x/>\nSELECT DISTINCT ?a (COUNT(?b) AS ?n) WHERE {\n  ?a ex:p ?b .\n  FILTER(?b > 1 && ?a != ?b)\n  BIND(?b + 1 AS ?c)\n  VALUES ?v { 1 }\n  MINUS { ?a ex:q ?d }\n}\nORDER BY ?a";
    let p = ok(q);
    assert_eq!(&q[p.query_start..p.query_start + 6], "SELECT");
    let pr = p.projection.as_ref().unwrap();
    assert_eq!(at(q, &pr.keyword), "SELECT");
    assert_eq!(at(q, pr.distinct_range.as_ref().unwrap()), "DISTINCT");
    assert!(pr.star_range.is_none());
    let list: Vec<_> = pr
        .items
        .iter()
        .map(|i| (i.var.clone(), i.expression, at(q, &i.range)))
        .collect();
    let want = |v: &str, e: bool, t: &str| (v.to_string(), e, t.to_string());
    assert_eq!(
        list,
        [want("a", false, "?a"), want("n", true, "(COUNT(?b) AS ?n)")]
    );
    let star = ok("SELECT * { ?s ?p ?o }");
    assert_eq!(
        at(
            "SELECT * { ?s ?p ?o }",
            star.projection.unwrap().star_range.as_ref().unwrap()
        ),
        "*"
    );
    let its = items(&p);
    let Item::Filter(f) = &its[1] else { panic!() };
    let refs: Vec<_> = f
        .var_refs
        .iter()
        .map(|v| format!("{}={}", v.name, at(q, &v.range)))
        .collect();
    assert_eq!(refs, ["b=?b", "a=?a", "b=?b"]);
    let binds = |i: usize| match &its[i] {
        Item::Locked(l) => l.binds.clone(),
        _ => panic!(),
    };
    assert_eq!(binds(2), ["c"]);
    assert_eq!(binds(3), ["v"]);
    assert!(binds(4).is_empty());
    let Item::Locked(b) = &its[2] else { panic!() };
    assert_eq!(b.var_refs.len(), 2);
    assert!(p.modifiers[0].binds.is_empty());
    assert_eq!(at(q, &p.modifiers[0].var_refs[0].range), "?a");
}

#[test]
fn unicode_ranges_are_bytes() {
    let q = "SELECT * { ?s <http://x/p> \"é😀\" . ?s <http://x/q> ?o }";
    let p = ok(q);
    let ts = triples(items(&p));
    assert_eq!(at(q, &ts[0].o.range), "\"é😀\"");
    assert_eq!(at(q, &ts[1].o.range), "?o");
}

// ------------------------------------------------------------ the corpus (A3) ------

/// Check the pattern of `text` (whose pattern is `p`): locked texts and ranges, and the
/// coverage of every group's tokens by its items.
fn check(name: &str, text: &str, p: &Pattern) {
    let tokens = lex(text, LexMode::Sparql);
    let sig: Vec<(usize, usize, TokenKind)> = tokens
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
        .map(|t| (t.start as usize, t.end(), t.kind))
        .collect();
    let locked_ok = |l: &Locked| {
        assert_eq!(&text[l.range.clone()], l.text, "{name}: {} block", l.kind);
    };
    p.locked.iter().for_each(locked_ok);
    p.modifiers.iter().for_each(locked_ok);
    if let Some(root) = &p.root {
        group(name, text, &sig, root);
    }
}

fn group(name: &str, text: &str, sig: &[(usize, usize, TokenKind)], g: &Group) {
    assert!(
        g.range.start <= g.inner.start && g.inner.end <= g.range.end,
        "{name}"
    );
    let mut end = g.inner.start;
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for item in &g.items {
        let r = item.range();
        assert!(
            r.start <= r.end && r.end <= g.inner.end,
            "{name}: {r:?} outside {g:?}"
        );
        match item {
            Item::Triple(t) => {
                for sub in [
                    &t.s.range,
                    &t.p.range,
                    &t.o.range,
                    &t.entry_range,
                    &t.object_range,
                ] {
                    assert!(
                        t.range.start <= sub.start && sub.end <= t.range.end,
                        "{name}"
                    );
                }
                assert_eq!(&text[t.s.range.clone()], t.s.text, "{name}");
                assert_eq!(&text[t.p.range.clone()], t.p.text, "{name}");
                assert_eq!(&text[t.o.range.clone()], t.o.text, "{name}");
                // the triples of one statement share its range
                if covered.last() == Some(&(r.start, r.end)) {
                    continue;
                }
            }
            Item::Filter(f) => assert_eq!(&text[f.range.clone()], f.text, "{name}"),
            Item::Group(inner) => group(name, text, sig, inner),
            Item::Locked(l) => assert_eq!(&text[l.range.clone()], l.text, "{name}"),
        }
        assert!(r.start >= end, "{name}: items overlap at {r:?}");
        end = r.end;
        covered.push((r.start, r.end));
    }
    // every significant token between the braces is in one item, or is a `.`
    for &(s, e, k) in sig
        .iter()
        .filter(|(s, _, _)| *s >= g.inner.start && *s < g.inner.end)
    {
        let inside = covered.iter().any(|&(a, b)| a <= s && e <= b);
        assert!(
            inside || k == TokenKind::Dot,
            "{name}: token {:?} at {s} is in no item",
            &text[s..e]
        );
    }
}

#[test]
fn a3_every_positive_query_has_a_pattern() {
    let mut queries = 0;
    let mut locked_blocks = 0;
    let mut triples_seen = 0;
    for (name, text) in corpus::positive_texts() {
        let p = match pattern(text) {
            Ok(p) => p,
            Err(PatternError::Update) => continue,
            Err(e) => panic!("{name}: {e:?}"),
        };
        queries += 1;
        check(name, text, &p);
        fn count(items: &[Item], l: &mut usize, t: &mut usize) {
            for i in items {
                match i {
                    Item::Locked(_) => *l += 1,
                    Item::Triple(_) => *t += 1,
                    Item::Group(g) => count(&g.items, l, t),
                    Item::Filter(_) => {}
                }
            }
        }
        if let Some(root) = &p.root {
            count(&root.items, &mut locked_blocks, &mut triples_seen);
        }
    }
    eprintln!("{queries} queries: {triples_seen} triples, {locked_blocks} locked blocks");
    // the golden inputs alone hold queries; the W3C suites add hundreds
    assert!(queries > 0);
    if corpus::suite_dir().is_some() {
        assert!(queries > 500, "only {queries} queries");
    }
}
