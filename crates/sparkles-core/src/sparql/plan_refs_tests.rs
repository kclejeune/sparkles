//! The parts of the query that plan nodes name (spec U04 §11.3): `patterns` on the nodes
//! that read triple patterns and paths, `filterVars` on filters and `joinVars` on joins.

use super::indexjoin::FORCE_INDEX_JOIN;
use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};
use serde_json::Value;

const EX: &str = "http://ex.org/";

fn store(data: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

/// People with an age, a name, a team and a manager, teams that are part of others, in
/// the default graph and in `ex:g1`.
fn people() -> Store {
    let mut d = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..60 {
        d.push_str(&format!(
            "ex:p{i} a ex:Person ; ex:age {} ; ex:name \"n{i}\" ; ex:team ex:t{} ; ex:boss ex:p{} .\n",
            i % 90,
            i % 7,
            i / 2
        ));
    }
    for t in 0..7 {
        d.push_str(&format!("ex:t{t} ex:partOf ex:t{} .\n", t / 2));
    }
    d.push_str("ex:g1 { ex:p1 ex:name \"in g1\" . ex:p2 ex:name \"also\" }\n");
    store(&d)
}

fn opts() -> QueryOptions {
    QueryOptions {
        prefixes: vec![("ex".into(), EX.into())],
        ..Default::default()
    }
}

fn explained(s: &Store, q: &str) -> Value {
    let (_, plan) = explain(s.snapshot(), q, &opts()).unwrap();
    serde_json::to_value(&plan).unwrap()
}

fn ran(s: &Store, q: &str) -> Value {
    let r = query(s.snapshot(), q, &opts()).unwrap();
    serde_json::to_value(&r.plan).unwrap()
}

fn nodes(j: &Value) -> Vec<&Value> {
    let mut out = vec![j];
    let mut i = 0;
    while i < out.len() {
        if let Some(c) = out[i]["children"].as_array() {
            out.extend(c.iter());
        }
        i += 1;
    }
    out
}

/// Every pattern a node of the plan names, as `s p o #occurrence`, with its operator.
fn patterns(j: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for n in nodes(j) {
        for p in n["patterns"].as_array().into_iter().flatten() {
            out.push((
                n["operator"].as_str().unwrap().to_string(),
                format!(
                    "{} {} {}{} #{}",
                    p["s"].as_str().unwrap(),
                    p["p"].as_str().unwrap(),
                    p["o"].as_str().unwrap(),
                    p["graph"]
                        .as_str()
                        .map_or(String::new(), |g| format!(" @{g}")),
                    p["occurrence"]
                ),
            ));
        }
    }
    out
}

fn texts(j: &Value) -> Vec<String> {
    let mut t: Vec<String> = patterns(j).into_iter().map(|(_, p)| p).collect();
    t.sort();
    t
}

fn iri(local: &str) -> String {
    format!("<{EX}{local}>")
}

#[test]
fn every_pattern_of_three_is_named_once_by_the_operator_that_reads_it() {
    let s = people();
    for j in [
        explained(
            &s,
            "SELECT * WHERE { ?p ex:team ?t . ?p ex:name ?n . ?t ex:partOf ?u }",
        ),
        ran(
            &s,
            "SELECT * WHERE { ?p ex:team ?t . ?p ex:name ?n . ?t ex:partOf ?u }",
        ),
    ] {
        let mut want = vec![
            format!("?p {} ?t #0", iri("team")),
            format!("?p {} ?n #0", iri("name")),
            format!("?t {} ?u #0", iri("partOf")),
        ];
        want.sort();
        assert_eq!(texts(&j), want, "{j:#}");
        for (op, _) in patterns(&j) {
            assert!(
                [
                    "IndexScan",
                    "IndexRangeScan",
                    "IndexJoin",
                    "StarJoin",
                    "CountFromIndex"
                ]
                .contains(&op.as_str()),
                "{op}"
            );
        }
    }
}

#[test]
fn constants_are_n_triples_terms_and_blank_nodes_hidden() {
    let s = people();
    let j = explained(
        &s,
        "SELECT * WHERE { ?p ex:name \"n3\" ; ex:age 3 ; ex:boss [ ex:name ?bn ] }",
    );
    let t = texts(&j);
    assert!(
        t.contains(&format!("?p {} \"n3\" #0", iri("name"))),
        "{t:?}"
    );
    assert!(
        t.contains(&format!(
            "?p {} \"3\"^^<http://www.w3.org/2001/XMLSchema#integer> #0",
            iri("age")
        )),
        "{t:?}"
    );
    assert!(t.contains(&format!("?p {} ?_ #0", iri("boss"))), "{t:?}");
    assert!(t.contains(&format!("?_ {} ?bn #0", iri("name"))), "{t:?}");
}

#[test]
fn equal_patterns_count_their_occurrences_in_query_order() {
    let s = people();
    let j = explained(
        &s,
        "SELECT * WHERE { { ?p ex:name ?n } UNION { ?p ex:name ?n . ?p ex:age ?a } }",
    );
    let t = texts(&j);
    let name = iri("name");
    assert!(t.contains(&format!("?p {name} ?n #0")), "{t:?}");
    assert!(t.contains(&format!("?p {name} ?n #1")), "{t:?}");
    assert!(!t.iter().any(|x| x.ends_with("#2")), "{t:?}");
}

#[test]
fn a_graph_variable_planned_per_graph_keeps_the_occurrence_of_the_text() {
    let s = people();
    // a FILTER inside makes the group evaluate once per named graph
    let j = explained(
        &s,
        "SELECT * WHERE { GRAPH ?g { ?p ex:name ?n OPTIONAL { ?p ex:age ?a } } }",
    );
    let t = texts(&j);
    assert!(!t.is_empty(), "{j:#}");
    assert!(t.iter().all(|x| x.ends_with("#0")), "{t:?}");
}

#[test]
fn a_pattern_in_a_named_graph_names_its_graph() {
    let s = people();
    let j = explained(&s, "SELECT * WHERE { GRAPH ex:g1 { ?p ex:name ?n } }");
    let t = texts(&j);
    assert_eq!(t, vec![format!("?p {} ?n @{} #0", iri("name"), iri("g1"))]);
}

#[test]
fn paths_are_written_whole_on_the_operators_that_read_them() {
    let s = people();
    let j = explained(&s, "SELECT * WHERE { ?t ex:partOf+ ?u }");
    let p = patterns(&j);
    assert_eq!(
        p,
        vec![(
            "TransitivePath".to_string(),
            format!("?t {}+ ?u #0", iri("partOf"))
        )]
    );
    // the parser writes a sequence of IRIs as its steps joined by a blank node
    let j = explained(&s, "SELECT * WHERE { ?p ex:team/ex:partOf ?u }");
    let mut want = vec![
        format!("?p {} ?_ #0", iri("team")),
        format!("?_ {} ?u #0", iri("partOf")),
    ];
    want.sort();
    assert_eq!(texts(&j), want);
    // a sequence inside a closure stays one path, which every step's operator names
    let j = explained(&s, "SELECT * WHERE { ?p (ex:team/ex:partOf)? ?u }");
    let whole = format!("?p ({}/{})? ?u #0", iri("team"), iri("partOf"));
    let p = patterns(&j);
    assert!(!p.is_empty(), "{j:#}");
    assert!(p.iter().all(|(_, x)| *x == whole), "{p:?}");
    // the parser writes an inverse IRI as the triple the other way round
    let j = explained(&s, "SELECT * WHERE { ?u ^ex:partOf ?t }");
    assert_eq!(texts(&j), vec![format!("?t {} ?u #0", iri("partOf"))]);
    // and any outer inverse, with the path inside it kept
    let j = explained(&s, "SELECT * WHERE { ?u ^ex:partOf+ ?t }");
    assert_eq!(texts(&j), vec![format!("?t {}+ ?u #0", iri("partOf"))]);
}

#[test]
fn count_joins_name_the_variable_they_join_on() {
    let s = people();
    for q in [
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:team ?t . ?s ex:name ?n }",
        "SELECT ?t (COUNT(*) AS ?c) WHERE { ?s ex:team ?t . ?s ex:name ?n } GROUP BY ?t",
    ] {
        for j in [explained(&s, q), ran(&s, q)] {
            let joins: Vec<&Value> = nodes(&j)
                .into_iter()
                .filter(|n| {
                    n["operator"]
                        .as_str()
                        .is_some_and(|o| o.starts_with("CountJoin"))
                })
                .collect();
            for n in &joins {
                assert_eq!(n["joinVars"], serde_json::json!(["s"]), "{q}: {j:#}");
            }
            if q.contains("GROUP BY") {
                continue;
            }
            assert!(!joins.is_empty(), "a count join in {j:#}");
        }
    }
}

#[test]
fn predicate_pattern_operators_name_their_pattern() {
    let s = people();
    assert!(s.snapshot().generation.patterns.file.is_some());
    let q = "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p";
    let j = ran(&s, q);
    let p = patterns(&j);
    assert_eq!(
        p,
        vec![("PatternCount".to_string(), "?s ?p ?o #0".to_string())],
        "{j:#}"
    );
    let q = "PREFIX spk: <urn:x-sparkles:> SELECT ?s ?p WHERE { ?s spk:hasPredicate ?p }";
    let j = ran(&s, q);
    let p = patterns(&j);
    assert_eq!(
        p,
        vec![(
            "PatternScan".to_string(),
            "?s <urn:x-sparkles:hasPredicate> ?p #0".to_string()
        )],
        "{j:#}"
    );
    // without the patterns, the scan that answers it names the same triple
    let off = QueryOptions {
        optimizations: Some(Optimizations::ALL.disable("predicate_patterns").unwrap()),
        ..opts()
    };
    let r = query(s.snapshot(), q, &off).unwrap();
    let j = serde_json::to_value(&r.plan).unwrap();
    assert_eq!(
        texts(&j),
        vec!["?s <urn:x-sparkles:hasPredicate> ?p #0".to_string()],
        "{j:#}"
    );
}

#[test]
fn path_text_flattens_and_brackets_as_documented() {
    use spargebra::algebra::PropertyPathExpression as PP;
    use spargebra::term::NamedNode;
    let n = |l: &str| PP::NamedNode(NamedNode::new_unchecked(format!("{EX}{l}")));
    let b = Box::new;
    let seq = PP::Sequence(b(PP::Sequence(b(n("a")), b(n("b")))), b(n("c")));
    assert_eq!(
        exec::path_text(&seq),
        format!("{}/{}/{}", iri("a"), iri("b"), iri("c"))
    );
    let alt = PP::Alternative(b(n("a")), b(PP::Sequence(b(n("b")), b(n("c")))));
    assert_eq!(
        exec::path_text(&alt),
        format!("{}|({}/{})", iri("a"), iri("b"), iri("c"))
    );
    let star = PP::ZeroOrMore(b(PP::Alternative(b(n("a")), b(n("b")))));
    assert_eq!(
        exec::path_text(&star),
        format!("({}|{})*", iri("a"), iri("b"))
    );
    assert_eq!(
        exec::path_text(&PP::Reverse(b(n("a")))),
        format!("^{}", iri("a"))
    );
    let range = |min, max| PP::Range {
        path: b(n("a")),
        min,
        max,
    };
    assert_eq!(
        exec::path_text(&range(2, Some(2))),
        format!("{}{{2}}", iri("a"))
    );
    assert_eq!(
        exec::path_text(&range(1, Some(3))),
        format!("{}{{1,3}}", iri("a"))
    );
    assert_eq!(
        exec::path_text(&range(0, None)),
        format!("{}{{0,}}", iri("a"))
    );
    let neg = PP::NegatedPropertySet(vec![
        NamedNode::new_unchecked(format!("{EX}a")),
        NamedNode::new_unchecked(format!("{EX}b")),
    ]);
    assert_eq!(
        exec::path_text(&neg),
        format!("!({}|{})", iri("a"), iri("b"))
    );
}

#[test]
fn filters_and_joins_name_their_variables() {
    let s = people();
    let j = ran(
        &s,
        "SELECT * WHERE { ?p ex:team ?t . ?p ex:name ?n . ?t ex:partOf ?u FILTER(STRLEN(?n) > 2 && ?u != ?t) }",
    );
    let all = nodes(&j);
    let filters: Vec<&Value> = all
        .iter()
        .copied()
        .filter(|n| n["filterVars"].is_array())
        .collect();
    assert!(!filters.is_empty(), "{j:#}");
    let mut vars: Vec<&str> = filters
        .iter()
        .flat_map(|n| n["filterVars"].as_array().unwrap())
        .map(|v| v.as_str().unwrap())
        .collect();
    vars.sort();
    vars.dedup();
    assert_eq!(vars, vec!["n", "t", "u"]);
    let joins: Vec<&Value> = all
        .iter()
        .copied()
        .filter(|n| n["joinVars"].is_array())
        .collect();
    assert!(!joins.is_empty(), "{j:#}");
    for n in joins {
        for v in n["joinVars"].as_array().unwrap() {
            assert!(["p", "t"].contains(&v.as_str().unwrap()), "{n}");
        }
    }
}

#[test]
fn an_optional_join_names_the_variables_its_sides_share() {
    let s = people();
    let j = explained(
        &s,
        "SELECT * WHERE { ?p ex:name ?n OPTIONAL { ?p ex:age ?a } }",
    );
    let opt = nodes(&j)
        .into_iter()
        .find(|n| n["operator"] == "OptionalJoin")
        .expect("an optional join");
    assert_eq!(opt["joinVars"], serde_json::json!(["p"]));
}

#[test]
fn a_star_join_names_each_pattern_it_probes() {
    let s = people();
    FORCE_INDEX_JOIN.with(|f| f.set(true));
    let j = explained(
        &s,
        "SELECT * WHERE { ?p a ex:Person ; ex:age ?a ; ex:name ?n }",
    );
    FORCE_INDEX_JOIN.with(|f| f.set(false));
    let star = nodes(&j)
        .into_iter()
        .find(|n| n["operator"] == "StarJoin")
        .unwrap_or_else(|| panic!("a star join in {j:#}"));
    assert!(star["patterns"].as_array().unwrap().len() >= 2, "{star}");
    assert_eq!(star["joinVars"], serde_json::json!(["p"]));
    assert_eq!(texts(&j).len(), 3, "{j:#}");
}

#[test]
fn a_count_from_the_index_names_its_pattern() {
    let s = people();
    let j = explained(&s, "SELECT (COUNT(*) AS ?c) WHERE { ?p ex:team ?t }");
    let p = patterns(&j);
    assert_eq!(p.len(), 1, "{j:#}");
    assert_eq!(p[0].1, format!("?p {} ?t #0", iri("team")));
}

#[test]
fn redaction_keeps_the_patterns_and_drops_the_estimates() {
    let s = people();
    let (_, mut plan) = explain(
        s.snapshot(),
        "SELECT * WHERE { ?p ex:team ?t . ?p ex:name ?n }",
        &opts(),
    )
    .unwrap();
    plan.redact();
    let j = serde_json::to_value(&plan).unwrap();
    assert_eq!(texts(&j).len(), 2);
    for n in nodes(&j) {
        assert_eq!(n["estimatedRows"], -1.0);
    }
}

#[test]
fn a_plan_in_format_2_still_names_its_patterns() {
    let s = people();
    // a chain deep enough for format 2
    let mut q = String::from("SELECT * WHERE { ");
    for i in 0..300 {
        q.push_str(&format!("?v{i} ex:boss ?v{} . ", i + 1));
    }
    q.push('}');
    let j = explained(&s, &q);
    assert_eq!(j["planFormat"], 2, "not format 2");
    assert_eq!(texts(&j).len(), 300);
}

#[test]
fn nodes_that_read_no_pattern_carry_none_of_the_members() {
    let s = people();
    let j = explained(&s, "SELECT ?n WHERE { ?p ex:name ?n } ORDER BY ?n LIMIT 3");
    for n in nodes(&j) {
        let op = n["operator"].as_str().unwrap();
        if ["Project", "Limit", "TopK", "OrderBy", "Sort"].contains(&op) {
            assert!(n.get("patterns").is_none(), "{n}");
            assert!(n.get("filterVars").is_none(), "{n}");
            assert!(n.get("joinVars").is_none(), "{n}");
        }
    }
}
