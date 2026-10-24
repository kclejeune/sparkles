//! Differential tests: after every commit, a circuit's result equals the query
//! evaluated at the commit's snapshot (spec F13 §11).

use super::circuit::{Advance, Circuit, DeltaSet};
use super::compile::{Kind, compile};
use crate::io::RdfFormat;
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use oxrdf::Term;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

const PREFIX: &str = "PREFIX : <http://ex/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

fn canon(t: &Option<Term>) -> String {
    match t {
        None => "UNDEF".into(),
        Some(Term::BlankNode(_)) => "_:b".into(),
        Some(t) => t.to_string(),
    }
}

/// The query's result at the store's head as sorted rows of the circuit's columns.
fn oracle(store: &Store, q: &str, cols: &[String], kind: Kind) -> Vec<String> {
    let r = sparql::query(
        store.snapshot(),
        &format!("{PREFIX}{q}"),
        &QueryOptions::default(),
    )
    .expect("oracle query");
    let mut out: Vec<String> = match kind {
        Kind::Ask => {
            if r.boolean {
                vec!["true".into()]
            } else {
                vec![]
            }
        }
        Kind::Construct => r
            .triples
            .iter()
            .map(|t| {
                let s = canon(&Some(Term::from(t.subject.clone())));
                let o = canon(&Some(t.object.clone()));
                format!("{s} {} {o}", t.predicate)
            })
            .collect(),
        Kind::Select => {
            let idx: Vec<usize> = cols
                .iter()
                .map(|c| r.vars.iter().position(|v| v == c).expect("column"))
                .collect();
            r.rows()
                .iter()
                .map(|row| {
                    idx.iter()
                        .map(|&i| canon(&row[i]))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect()
        }
    };
    out.sort();
    out
}

fn result(c: &Circuit) -> Vec<String> {
    let mut out = Vec::new();
    if c.kind == Kind::Ask {
        if c.result.total() > 0 {
            out.push("true".into());
        }
        return out;
    }
    for (row, w) in c.rows() {
        assert!(w > 0, "a negative weight in the result");
        let s = row.iter().map(canon).collect::<Vec<_>>().join(" ");
        for _ in 0..w {
            out.push(s.clone());
        }
    }
    out.sort();
    out
}

fn circuit(store: &Store, q: &str) -> Circuit {
    let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).expect("parse");
    let mut c = Circuit::new(compile(&parsed, None).expect("compile"), None);
    c.build(store.snapshot(), &Control::none()).expect("build");
    c
}

fn step(store: &Store, c: &mut Circuit) -> Advance {
    let d = DeltaSet::read(store, c.applied, &Control::none());
    assert!(d.complete, "the changes of a transaction are known");
    let a = c.advance(&d, &Control::none()).expect("advance");
    assert!(!matches!(a, Advance::Reset(_)), "a pass reset");
    a
}

fn check(store: &Store, c: &Circuit, q: &str, what: &str) {
    let want = oracle(store, q, &c.columns(), c.kind);
    let got = result(c);
    assert_eq!(got, want, "{what}: {q}\nplan: {:#?}", c.plan());
}

const DATA: &str = r#"
@prefix : <http://ex/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
:a :memberOf :t1 . :b :memberOf :t1 . :c :memberOf :t2 .
:a :age 30 . :b :age 40 . :c :age 50 .
:a :knows :b . :b :knows :c .
"#;

fn upd(s: &Store, u: &str) -> crate::Result<crate::sparql::update::UpdateStats> {
    crate::sparql::update::update(s, u, &QueryOptions::default())
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[crate::io::Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load");
    s
}

const QUERIES: &[&str] = &[
    "SELECT ?team (COUNT(?p) AS ?n) WHERE { ?p :memberOf ?team } GROUP BY ?team",
    "SELECT * WHERE { ?a :knows ?b . ?b :knows ?c }",
    "SELECT * WHERE { ?p :memberOf ?t OPTIONAL { ?t rdfs:label ?l } }",
    "SELECT * WHERE { ?p :memberOf ?t MINUS { ?p :left true } }",
    "SELECT * WHERE { ?p :memberOf ?t FILTER NOT EXISTS { ?p :left true } }",
    "SELECT * WHERE { ?p :memberOf ?t FILTER EXISTS { ?p :age ?x } }",
    "SELECT ?x WHERE { { ?x :memberOf :t1 } UNION { ?x :knows ?y } }",
    "SELECT ?p ?y WHERE { ?p :age ?a FILTER(?a > 35) BIND(?a + 1 AS ?y) }",
    "SELECT DISTINCT ?t WHERE { ?p :memberOf ?t }",
    "SELECT ?p ?t WHERE { VALUES ?t { :t1 :t3 } ?p :memberOf ?t }",
    "SELECT ?t (SUM(?a) AS ?s) (AVG(?a) AS ?m) (COUNT(DISTINCT ?a) AS ?d) WHERE { ?p :memberOf ?t . ?p :age ?a } GROUP BY ?t",
    "SELECT (COUNT(*) AS ?n) WHERE { ?p :memberOf ?t }",
    "SELECT ?g ?s WHERE { GRAPH ?g { ?s :memberOf ?t } }",
    "SELECT ?s ?o WHERE { ?s :knows/:memberOf ?o }",
    "SELECT ?s ?o WHERE { ?s :knows|:memberOf ?o }",
    "SELECT ?s ?o WHERE { ?s :knows+ ?o }",
    "SELECT ?t ?n ?l WHERE { { SELECT ?t (COUNT(*) AS ?n) WHERE { ?p :memberOf ?t } GROUP BY ?t } OPTIONAL { ?t rdfs:label ?l } }",
    "SELECT * WHERE { ?p :memberOf ?t OPTIONAL { ?t rdfs:label ?l } ?p :age ?a }",
    "SELECT ?t ?n WHERE { ?p :memberOf ?t } ORDER BY ?t LIMIT 1",
    "ASK { ?p :left true }",
    "CONSTRUCT { ?p :inTeam ?t . ?t :hasMember [ :who ?p ] } WHERE { ?p :memberOf ?t }",
    "SELECT * WHERE { ?p :memberOf ?t OPTIONAL { ?p :age ?a FILTER(?a > 35) } }",
    "SELECT ?t (COUNT(?l) AS ?n) WHERE { ?p :memberOf ?t OPTIONAL { ?p rdfs:label ?l } } GROUP BY ?t HAVING (COUNT(?l) > 0)",
    "SELECT * WHERE { GRAPH :g1 { ?s ?p ?o } }",
    "SELECT ?s (CONCAT(STR(?s), \"-x\") AS ?k) WHERE { ?s :memberOf ?t }",
];

const UPDATES: &[&str] = &[
    "INSERT DATA { :d :memberOf :t2 }",
    "INSERT DATA { :x rdfs:label \"X\" }",
    "INSERT DATA { :t1 rdfs:label \"One\" }",
    "DELETE DATA { :a :memberOf :t1 } ; INSERT DATA { :a :memberOf :t1 }",
    "INSERT DATA { :b :left true }",
    "DELETE DATA { :b :memberOf :t1 }",
    "INSERT DATA { :c :knows :a . :d :age 60 }",
    "INSERT DATA { GRAPH :g1 { :e :memberOf :t1 } }",
    "DELETE DATA { :b :left true }",
    "INSERT DATA { :a rdfs:label \"A\" . :d :age 25.5 }",
    "DELETE DATA { :t1 rdfs:label \"One\" }",
    "DELETE DATA { GRAPH :g1 { :e :memberOf :t1 } }",
    "INSERT DATA { :b :memberOf :t1 . :b :knows :d }",
    "DELETE WHERE { ?s :age ?a }",
];

#[test]
fn circuits_follow_the_oracle_through_a_history() {
    for q in QUERIES {
        let s = store();
        let mut c = circuit(&s, q);
        check(&s, &c, q, "after the build");
        for u in UPDATES {
            upd(&s, &format!("{PREFIX}{u}")).expect("update");
            step(&s, &mut c);
            check(&s, &c, q, u);
        }
    }
}

#[test]
fn several_commits_apply_as_one_pass() {
    for q in QUERIES {
        let s = store();
        let mut c = circuit(&s, q);
        for u in UPDATES.chunks(3) {
            for x in u {
                upd(&s, &format!("{PREFIX}{x}")).expect("update");
            }
            step(&s, &mut c);
            check(&s, &c, q, "a run of commits");
        }
    }
}

#[test]
fn a_commit_outside_the_footprint_runs_no_pass() {
    let s = store();
    let q = QUERIES[0];
    let mut c = circuit(&s, q);
    upd(
        &s,
        &format!("{PREFIX}INSERT DATA {{ :x <http://xmlns.com/foaf/0.1/name> \"X\" }}"),
    )
    .expect("update");
    assert!(matches!(step(&s, &mut c), Advance::Skipped));
    upd(&s, &format!("{PREFIX}INSERT DATA {{ :d :memberOf :t2 }}")).expect("update");
    match step(&s, &mut c) {
        Advance::Changed(d) => assert_eq!(d.len(), 2, "removed (t2, 1) and added (t2, 2)"),
        _ => panic!("expected a change"),
    }
    // no net change, no diff
    upd(
        &s,
        &format!("{PREFIX}DELETE DATA {{ :a :memberOf :t1 }} ; INSERT DATA {{ :a :memberOf :t1 }}"),
    )
    .expect("update");
    match step(&s, &mut c) {
        Advance::Skipped => {}
        Advance::Changed(d) => assert!(d.is_empty()),
        Advance::Reset(_) => panic!("no reset expected"),
    }
}

#[test]
fn plans_mark_their_modes() {
    let parse = |q: &str| sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).unwrap();
    let recomputed: Vec<&str> = QUERIES
        .iter()
        .filter(|q| compile(&parse(q), None).unwrap().root.has_recompute())
        .copied()
        .collect();
    assert_eq!(
        recomputed,
        vec![
            "SELECT ?s ?o WHERE { ?s :knows+ ?o }",
            "SELECT ?t ?n WHERE { ?p :memberOf ?t } ORDER BY ?t LIMIT 1",
        ]
    );
}

#[test]
fn refused_and_recomputed_features() {
    let parse = |q: &str| sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).unwrap();
    let e = compile(
        &parse("SELECT ?x WHERE { ?x :p ?y FILTER(?y < NOW()) }"),
        None,
    )
    .unwrap_err();
    assert!(e.to_string().contains("NOW"), "{e}");
    let c = compile(&parse("SELECT ?p ?q WHERE { ?p :knows+ ?q }"), None).unwrap();
    assert!(c.root.has_recompute());
    let c = compile(&parse(QUERIES[0]), None).unwrap();
    assert!(!c.root.has_recompute());
}

#[test]
fn a_state_survives_encoding_and_a_generation_switch() {
    let s = store();
    for q in QUERIES {
        let mut c = circuit(&s, q);
        let body = c.encode(&s.snapshot()).unwrap();
        let mut c2 = circuit(&s, q);
        c2.decode(&body, &s.snapshot()).unwrap();
        assert_eq!(result(&c), result(&c2), "{q}");
        upd(
            &s,
            &format!("{PREFIX}INSERT DATA {{ :z :memberOf :t9 . :z :age 7 }}"),
        )
        .unwrap();
        s.compact().unwrap();
        step(&s, &mut c);
        check(&s, &c, q, "after a compaction");
        upd(
            &s,
            &format!("{PREFIX}DELETE DATA {{ :z :memberOf :t9 . :z :age 7 }}"),
        )
        .unwrap();
        step(&s, &mut c);
        check(&s, &c, q, "after a compaction and a delete");
    }
}

#[test]
fn random_histories_follow_the_oracle() {
    let subjects = [":a", ":b", ":c", ":d", ":e"];
    let preds = [":memberOf", ":knows", ":age", "rdfs:label", ":left"];
    let objects = [":t1", ":t2", ":a", ":b", "1", "2", "\"x\"", "true"];
    let graphs = ["", "GRAPH :g1", "GRAPH :g2"];
    for seed in 0..6u64 {
        let mut rng = StdRng::seed_from_u64(seed);
        for q in QUERIES {
            let s = store();
            let mut c = circuit(&s, q);
            for _ in 0..12 {
                let mut ops = Vec::new();
                for _ in 0..rng.random_range(1..4) {
                    let t = format!(
                        "{} {} {}",
                        subjects[rng.random_range(0..subjects.len())],
                        preds[rng.random_range(0..preds.len())],
                        objects[rng.random_range(0..objects.len())]
                    );
                    let g = graphs[rng.random_range(0..graphs.len())];
                    let body = if g.is_empty() {
                        t
                    } else {
                        format!("{g} {{ {t} }}")
                    };
                    let verb = if rng.random_bool(0.6) {
                        "INSERT"
                    } else {
                        "DELETE"
                    };
                    ops.push(format!("{verb} DATA {{ {body} }}"));
                }
                upd(&s, &format!("{PREFIX}{}", ops.join(" ; "))).expect("update");
                if rng.random_bool(0.7) {
                    step(&s, &mut c);
                    check(&s, &c, q, &format!("seed {seed}: {}", ops.join(" ; ")));
                }
            }
            step(&s, &mut c);
            check(&s, &c, q, &format!("seed {seed} at the end"));
        }
    }
}

/// Queries whose circuits once missed changes or disagreed with the engine: each runs
/// from an empty store through its updates and is checked after every commit.
#[test]
fn edge_cases_follow_the_oracle() {
    let cases: &[(&str, &[&str])] = &[
        // a decimal sum past the decimal range is unbound
        (
            "SELECT (SUM(?v) AS ?s) WHERE { ?x :v ?v }",
            &[
                "INSERT DATA { :a :v 99999999999999999999.0 }",
                "INSERT DATA { :b :v 99999999999999999999.0 }",
                "DELETE DATA { :b :v 99999999999999999999.0 }",
                "INSERT DATA { :c :v 9223372036854775807 . :d :v 9223372036854775807 }",
            ],
        ),
        // the base IRI reaches expressions
        (
            "BASE <http://example.org/> SELECT (IRI(\"relative\") AS ?i) WHERE {}",
            &["INSERT DATA { :a :p 1 }"],
        ),
        // a property function's footprint covers the triples it reads
        (
            "PREFIX list: <http://jena.apache.org/ARQ/list#> SELECT ?x WHERE { :l list:member ?x }",
            &[
                "INSERT DATA { :l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> 1 ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> }",
                "DELETE DATA { :l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> 1 } ; INSERT DATA { :l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> 2 }",
            ],
        ),
        // the empty path joins every node of the graph to itself
        (
            "SELECT ?s ?o WHERE { ?s :p* ?o }",
            &[
                "INSERT DATA { :a :other :b }",
                "DELETE DATA { :a :other :b }",
            ],
        ),
        // ORDER BY expressions are part of the footprint
        (
            "SELECT ?s WHERE { ?s :r ?x } ORDER BY DESC(EXISTS { ?s :q ?y }) LIMIT 1",
            &[
                "INSERT DATA { :a :r 1 . :b :r 1 . :a :q 1 }",
                "DELETE DATA { :a :q 1 } ; INSERT DATA { :b :q 1 }",
            ],
        ),
        // an empty group under a named graph needs the graph to exist
        (
            "SELECT ?n WHERE { GRAPH :g { BIND(1 AS ?n) } }",
            &[
                "INSERT DATA { GRAPH :g { :a :p 1 } }",
                "DELETE DATA { GRAPH :g { :a :p 1 } }",
            ],
        ),
        // graph enumeration follows the named graphs
        (
            "SELECT ?g WHERE { GRAPH ?g {} }",
            &[
                "INSERT DATA { GRAPH :g { :a :p 1 } }",
                "DELETE DATA { GRAPH :g { :a :p 1 } }",
            ],
        ),
        // a template's blank node is one node per occurrence of a solution
        (
            "CONSTRUCT { [] :p ?o } WHERE { ?s :v ?o }",
            &[
                "INSERT DATA { :a :v 1 . :b :v 1 }",
                "DELETE DATA { :a :v 1 }",
                "INSERT DATA { :c :v 1 . :d :v 2 }",
            ],
        ),
    ];
    for (q, updates) in cases {
        let s = Store::in_memory(StoreOptions::default());
        let mut c = circuit(&s, q);
        check(&s, &c, q, "after the build");
        for u in *updates {
            upd(&s, &format!("{PREFIX}{u}")).expect("update");
            step(&s, &mut c);
            check(&s, &c, q, u);
        }
    }
    // the two occurrences of one solution give two triples
    let s = Store::in_memory(StoreOptions::default());
    let q = "CONSTRUCT { [] :p ?o } WHERE { VALUES ?o { 1 1 } }";
    let c = circuit(&s, q);
    check(&s, &c, q, "after the build");
    assert_eq!(result(&c).len(), 2);
}

/// A rebuild starts the operators' state from nothing.
#[test]
fn a_rebuild_clears_the_operators_state() {
    for q in [
        "SELECT ?s WHERE { VALUES ?k { 1 } ?s :p ?k }",
        "SELECT ?s WHERE { ?s :p ?k MINUS { VALUES ?k { 2 } } }",
        "CONSTRUCT { [] :p ?k } WHERE { ?s :p ?k }",
    ] {
        let s = Store::in_memory(StoreOptions::default());
        upd(&s, &format!("{PREFIX}INSERT DATA {{ :a :p 1 }}")).unwrap();
        let mut c = circuit(&s, q);
        c.reset(&DeltaSet::at(s.snapshot()), &Control::none())
            .unwrap();
        check(&s, &c, q, "after the rebuild");
        upd(&s, &format!("{PREFIX}INSERT DATA {{ :b :p 1 }}")).unwrap();
        step(&s, &mut c);
        check(&s, &c, q, "after an insert");
        upd(&s, &format!("{PREFIX}DELETE DATA {{ :a :p 1 }}")).unwrap();
        step(&s, &mut c);
        check(&s, &c, q, "after a delete");
    }
}
