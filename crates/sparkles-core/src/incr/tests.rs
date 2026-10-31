//! Differential tests: after every commit, a circuit's result equals the query
//! evaluated at the commit's snapshot (spec F13 §11).

use super::circuit::{Advance, Circuit, DeltaSet};
use super::compile::{Kind, compile};
use super::r#gen::{self as generator, QueryGen, Step};
use crate::access::GraphAccess;
use crate::io::RdfFormat;
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use oxrdf::Term;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::sync::Arc;

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
    oracle_as(store, q, cols, kind, None)
}

/// [`oracle`] for a reader with `access`, over its masked snapshot.
fn oracle_as(
    store: &Store,
    q: &str,
    cols: &[String],
    kind: Kind,
    access: Option<&Arc<GraphAccess>>,
) -> Vec<String> {
    let r = sparql::query(
        store.snapshot(),
        &format!("{PREFIX}{q}"),
        &QueryOptions {
            graphs: access.cloned(),
            no_cache: true,
            ..Default::default()
        },
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
    generated(generator::cases(160), false);
}

/// The generated cases of [`random_histories_follow_the_oracle`] read by random readers
/// over their masked snapshots: graphs left out, and protections by predicate and by
/// the data.
#[test]
fn random_histories_follow_the_masked_oracle() {
    generated(generator::cases(120), true);
}

/// A generated case: its query, starting data, reader and history.
struct Case {
    seed: u64,
    query: String,
    data: String,
    access: Option<(Arc<GraphAccess>, String)>,
    steps: Vec<Step>,
}

impl Case {
    /// The case of `seed`; masked cases draw from seeds of their own.
    fn new(seed: u64, masked: bool) -> Case {
        let mut rng = StdRng::seed_from_u64(if masked { seed | 1 << 32 } else { seed });
        let query = QueryGen::new(&mut rng).query();
        let data = generator::data(&mut rng);
        let access = masked.then(|| generator::access(&mut rng));
        let n = rng.random_range(4..12);
        let steps = generator::history(&mut rng, n);
        Case {
            seed,
            query,
            data,
            access,
            steps,
        }
    }

    fn describe(&self, steps: &[Step], why: &str) -> String {
        let mut out = format!(
            "seed {} (SPARKLES_INCR_GEN_SEED={} SPARKLES_INCR_GEN_CASES=1)\nquery: {}\ndata: {}\n",
            self.seed, self.seed, self.query, self.data
        );
        if let Some((_, d)) = &self.access {
            out.push_str(&format!("reader: {d}\n"));
        }
        for s in steps {
            out.push_str(&format!("  {s:?}\n"));
        }
        out.push_str(why);
        out
    }
}

/// What running a case found.
enum Ran {
    /// the circuit followed the oracle; `true` when it was reset at some commit
    Followed(bool),
    /// the compiler refused the query, with the reason
    Refused(String),
}

/// Rows of a circuit's result, or the reason they are not a result.
fn try_result(c: &Circuit) -> Result<Vec<String>, String> {
    if let Some((_, w)) = c.rows().iter().find(|(_, w)| *w <= 0) {
        return Err(format!("a weight {w} in the result"));
    }
    Ok(result(c))
}

/// Run `case`'s query through `steps`, comparing the circuit with the oracle at every
/// check. A panic counts as a failure, so that a failing case can be shrunk.
fn run_case(case: &Case, steps: &[Step]) -> Result<Ran, String> {
    let access = case.access.as_ref().map(|(a, _)| a.clone());
    let run = || -> Result<Ran, String> {
        let s = Store::in_memory(StoreOptions::default());
        upd(&s, &format!("{PREFIX}{}", case.data)).map_err(|e| format!("data: {e}"))?;
        let parsed = sparql::parse_query(&format!("{PREFIX}{}", case.query), None, &[])
            .map_err(|e| format!("the generator wrote a query that does not parse: {e}"))?;
        let whole = super::runtime::whole_reason(access.as_ref());
        let compiled = match compile(&parsed, whole) {
            Ok(c) => c,
            Err(e) => return Ok(Ran::Refused(e.to_string())),
        };
        let mut c = Circuit::new(compiled, access.clone());
        c.build(s.snapshot(), &Control::none())
            .map_err(|e| format!("build: {e}"))?;
        let compare = |c: &Circuit, at: &str| -> Result<(), String> {
            let mut want = oracle_as(&s, &case.query, &c.columns(), c.kind, access.as_ref());
            let mut got = try_result(c)?;
            // REDUCED may drop some duplicates or none, and the circuit drops all; the
            // generator writes it only on the outer query, so the sets compare
            if case.query.contains("REDUCED") {
                want.dedup();
                got.dedup();
            }
            if got != want {
                return Err(format!(
                    "{at}: the circuit has\n    {got:?}\nthe oracle has\n    {want:?}\nplan: {:#?}",
                    c.plan()
                ));
            }
            Ok(())
        };
        compare(&c, "after the build")?;
        let mut reset = false;
        for (i, step) in steps.iter().enumerate() {
            match step {
                Step::Update(u) => {
                    upd(&s, &format!("{PREFIX}{u}")).map_err(|e| format!("update {u}: {e}"))?;
                }
                Step::Compact => {
                    s.compact().map_err(|e| format!("compact: {e}"))?;
                }
                Step::Check => {
                    let d = DeltaSet::read(&s, c.applied, &Control::none());
                    match c.advance(&d, &Control::none()) {
                        Ok(Advance::Reset(_)) => reset = true,
                        Ok(_) => {}
                        Err(e) => return Err(format!("step {i}: advance: {e}")),
                    }
                    compare(&c, &format!("step {i}"))?;
                }
            }
        }
        Ok(Ran::Followed(reset))
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(r) => r,
        Err(p) => Err(format!(
            "panicked: {}",
            p.downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| p.downcast_ref::<&str>().copied())
                .unwrap_or("?")
        )),
    }
}

/// The shortest history found by removing one step at a time that still fails, with
/// its failure. The final check stays.
fn shrink(case: &Case, mut steps: Vec<Step>, mut why: String) -> (Vec<Step>, String) {
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i + 1 < steps.len() {
            let mut fewer = steps.clone();
            fewer.remove(i);
            match run_case(case, &fewer) {
                Err(e) => {
                    steps = fewer;
                    why = e;
                    changed = true;
                }
                Ok(_) => i += 1,
            }
        }
    }
    (steps, why)
}

/// Run `n` generated cases, read by every graph or (`masked`) by random readers, and
/// fail with every divergence, each shrunk to a short history.
fn generated(n: usize, masked: bool) {
    let first = generator::first_seed();
    let (mut followed, mut resets, mut refused) = (0usize, 0usize, Vec::new());
    let mut failures = Vec::new();
    for seed in first..first + n as u64 {
        let case = Case::new(seed, masked);
        match run_case(&case, &case.steps) {
            Ok(Ran::Followed(reset)) => {
                followed += 1;
                resets += reset as usize;
            }
            Ok(Ran::Refused(why)) => refused.push(why),
            Err(why) => {
                let (steps, why) = shrink(&case, case.steps.clone(), why);
                failures.push(case.describe(&steps, &why));
            }
        }
    }
    eprintln!(
        "generated cases{}: {followed} followed the oracle ({resets} with a reset), {} refused, {} diverged",
        if masked { " (masked)" } else { "" },
        refused.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {n} generated cases diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    // the generator stays inside the fragment: most of its queries compile
    assert!(
        followed * 4 >= n * 3,
        "only {followed} of {n} generated queries compiled; refused: {refused:#?}"
    );
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
        // decimal values that cancel out sum exactly
        (
            "SELECT (SUM(?v) AS ?s) (AVG(?v) AS ?m) WHERE { ?x :v ?v }",
            &[
                "INSERT DATA { :a :v 100000000000000000000.0 . :b :v -100000000000000000000.0 }",
                "INSERT DATA { :c :v 100000000000000000001.0 . :d :v -100000000000000000001.0 }",
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
        // a GRAPH inside GRAPH ?g: ?g ranges over every named graph, whatever the
        // inner GRAPH reads, so a graph the inner pattern never reads changes it
        (
            "SELECT ?g ?h ?o WHERE { GRAPH ?g { GRAPH ?h { ?s :p ?o } } }",
            &[
                "INSERT DATA { GRAPH :h { :a :p 1 } }",
                "INSERT DATA { GRAPH :x { :a :other 1 } }",
                "INSERT DATA { GRAPH :x { :b :p 2 } }",
                "DELETE DATA { GRAPH :x { :a :other 1 . :b :p 2 } }",
            ],
        ),
        (
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s :q ?o GRAPH :h { ?s :p ?x } } }",
            &[
                "INSERT DATA { GRAPH :h { :a :p 1 } }",
                "INSERT DATA { GRAPH :x { :a :q 1 } }",
                "INSERT DATA { :a :q 2 }",
                "DELETE DATA { GRAPH :x { :a :q 1 } }",
            ],
        ),
        // a GRAPH inside a constant GRAPH reads only while the outer graph exists
        (
            "SELECT ?h ?o WHERE { GRAPH :x { GRAPH ?h { ?s :p ?o } } }",
            &[
                "INSERT DATA { GRAPH :h { :a :p 1 } }",
                "INSERT DATA { GRAPH :x { :b :q 1 } }",
                "DELETE DATA { GRAPH :x { :b :q 1 } }",
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

/// A zero-length path from a term an outer pattern binds matches that term, even when
/// it is no node of the active graph. `EXISTS` substitutes the outer row, so its
/// answer differs from a join of the pattern evaluated alone. `OPTIONAL` joins, so a
/// term that is no node of the graph finds no match. The expected rows come from
/// `arq --engine=ref` (Jena 6.2) over the data after the third update, and the
/// generator found the first case at seed 5897.
#[test]
fn zero_length_paths_follow_the_oracle() {
    let updates = [
        "INSERT DATA { GRAPH :g2 { :a :knows :d } . :a :left false . :d :left true . :d rdfs:label \"y\" . :d rdfs:label 1 . GRAPH :g2 { :d :knows :b } }",
        "INSERT DATA { :d rdfs:label \"x\"@en }",
        "INSERT DATA { :a :knows :e } ; INSERT DATA { :a rdfs:label 1 }",
        "DELETE DATA { GRAPH :g2 { :a :knows :d . :d :knows :b } }",
        "INSERT DATA { GRAPH :g3 { :e :other 1 } }",
    ];
    let cases: &[(&str, &[&str])] = &[
        (
            "CONSTRUCT { ?v1 :out ?v1 } WHERE { ?v0 :knows? ?v1 . ?v2 rdfs:label \"x\"@en . FILTER NOT EXISTS { GRAPH ?v3 { ?v1 :knows* ?v4 } } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER NOT EXISTS { GRAPH ?g { ?x :knows* ?y } } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER NOT EXISTS { GRAPH ?g { ?x :knows? ?y } } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER EXISTS { GRAPH ?g { ?x :knows* ?y } } }",
            &["<http://ex/e>"],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER EXISTS { GRAPH ?g { ?x :knows? ?y } } }",
            &["<http://ex/e>"],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER NOT EXISTS { GRAPH :g2 { ?x :knows* ?y } } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER NOT EXISTS { GRAPH :nope { ?x :knows* ?y } } }",
            &["<http://ex/e>"],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER EXISTS { GRAPH :nope { ?x :knows? ?y } } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { GRAPH :g2 { ?a :knows ?x } FILTER NOT EXISTS { ?x :knows* ?y } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { GRAPH :g2 { ?a :knows ?x } FILTER EXISTS { ?x :knows? ?y } }",
            &["<http://ex/b>", "<http://ex/d>"],
        ),
        (
            "SELECT ?x WHERE { VALUES ?x { :zz } FILTER NOT EXISTS { ?x :knows? ?y } }",
            &[],
        ),
        (
            "SELECT ?x WHERE { ?a :knows ?x FILTER NOT EXISTS { GRAPH ?g { ?y :knows* ?x } } }",
            &[],
        ),
        (
            "SELECT ?x ?g ?y WHERE { ?a :knows ?x OPTIONAL { GRAPH ?g { ?x :knows* ?y } } }",
            &["<http://ex/e> UNDEF UNDEF"],
        ),
        (
            "SELECT ?x ?y WHERE { GRAPH :g2 { ?a :knows ?x } OPTIONAL { ?x :knows? ?y } }",
            &["<http://ex/b> UNDEF", "<http://ex/d> <http://ex/d>"],
        ),
        (
            "SELECT ?g ?y WHERE { GRAPH ?g { :e :knows* ?y } }",
            &["<http://ex/g2> <http://ex/e>"],
        ),
        ("SELECT ?y WHERE { GRAPH :nope { :e :knows* ?y } }", &[]),
        (
            "SELECT ?g ?s ?o WHERE { GRAPH ?g { ?s :knows? ?o } }",
            &[
                "<http://ex/g2> <http://ex/a> <http://ex/a>",
                "<http://ex/g2> <http://ex/a> <http://ex/d>",
                "<http://ex/g2> <http://ex/b> <http://ex/b>",
                "<http://ex/g2> <http://ex/d> <http://ex/b>",
                "<http://ex/g2> <http://ex/d> <http://ex/d>",
            ],
        ),
    ];
    let mut wrong = Vec::new();
    for (q, want) in cases {
        let s = Store::in_memory(StoreOptions::default());
        let mut c = circuit(&s, q);
        let mut what = vec!["after the build"];
        what.extend(updates);
        for (i, u) in what.iter().enumerate() {
            if i > 0 {
                upd(&s, &format!("{PREFIX}{u}")).expect("update");
                step(&s, &mut c);
            }
            let got = result(&c);
            let oracle = oracle(&s, q, &c.columns(), c.kind);
            if got != oracle {
                wrong.push(format!(
                    "{q}\n  {u}: the circuit has {got:?}, the engine {oracle:?}"
                ));
            }
            if i == 3 && oracle != *want {
                wrong.push(format!(
                    "{q}\n  the engine has {oracle:?}, the reference {want:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
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

/// Divergences the generator found where the engine, the oracle, drops rows that the
/// SPARQL semantics give and the circuits keep, shrunk to a few quads. With
/// `SPARKLES_INCR_GEN_CASES` above 2451 the generated cases reach the first (seed
/// 2451), and the masked ones above 389 reach the second (seed 389).
#[test]
#[ignore = "the engine drops these rows; the circuits answer them correctly"]
fn generator_found_engine_divergences() {
    let cases: &[(&str, &str, &[&str])] = &[
        // an equality between a variable of each UNION branch, over a join that binds
        // both in one row
        (
            "INSERT DATA { :a :p 1 . :a :q 1 . :c :r :a }",
            "SELECT ?x ?y ?z WHERE { { ?x :p 1 } UNION { ?y :q 1 } ?z :r ?x FILTER(sameTerm(?x, ?y)) }",
            &["<http://ex/a> <http://ex/a> <http://ex/c>"],
        ),
        // a filter on a variable that VALUES leaves unbound and a path binds
        (
            "INSERT DATA { :c :knows :e }",
            "SELECT ?x ?y WHERE { VALUES ?x { UNDEF } ?x :knows+ ?y FILTER(BOUND(?x)) }",
            &["<http://ex/c> <http://ex/e>"],
        ),
    ];
    for (data, q, want) in cases {
        let s = Store::in_memory(StoreOptions::default());
        upd(&s, &format!("{PREFIX}{data}")).unwrap();
        let c = circuit(&s, q);
        assert_eq!(result(&c), *want, "the circuit: {q}");
        check(&s, &c, q, "the engine");
    }
}
