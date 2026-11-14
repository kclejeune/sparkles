//! The fixed-point operator against the query engine (spec F13 §4.3, §11): random graphs
//! and histories with cycles, self-loops and deletions that break cycles, paths with `+`
//! and `*` alone, under `GRAPH`, in joins and under negation, and the annotations of
//! their rows.

use super::circuit::{Advance, Circuit, DeltaSet};
use super::compile::{Kind, compile};
use super::prov::{Atom, FactKey, Provenance};
use crate::io::RdfFormat;
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use oxrdf::{Literal, NamedNode, Term};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::{BTreeMap, BTreeSet};

const NS: &str = "http://ex/";
const PREFIX: &str = "PREFIX : <http://ex/> ";
const NODES: u8 = 6;

/// A quad of the model: subject, predicate, object (`NODES` is the literal 1) and graph
/// (0 the default graph).
type Quad = (u8, u8, u8, u8);

fn node(i: u8) -> Term {
    if i == NODES {
        Term::Literal(Literal::from(1))
    } else {
        Term::NamedNode(NamedNode::new_unchecked(format!("{NS}n{i}")))
    }
}

fn pred(i: u8) -> Term {
    Term::NamedNode(NamedNode::new_unchecked(format!(
        "{NS}{}",
        ["p", "q"][i as usize]
    )))
}

fn graph(g: u8) -> Option<Term> {
    (g > 0).then(|| Term::NamedNode(NamedNode::new_unchecked(format!("{NS}g{g}"))))
}

fn text(q: &Quad) -> String {
    let t = format!("{} {} {} .", node(q.0), pred(q.1), node(q.2));
    match graph(q.3) {
        None => t,
        Some(g) => format!("GRAPH {g} {{ {t} }}"),
    }
}

fn apply(s: &Store, del: &[Quad], ins: &[Quad]) {
    let mut ops = Vec::new();
    if !del.is_empty() {
        ops.push(format!(
            "DELETE DATA {{ {} }}",
            del.iter().map(text).collect::<String>()
        ));
    }
    if !ins.is_empty() {
        ops.push(format!(
            "INSERT DATA {{ {} }}",
            ins.iter().map(text).collect::<String>()
        ));
    }
    if ops.is_empty() {
        return;
    }
    sparql::update::update(s, &ops.join(" ; "), &QueryOptions::default()).expect("update");
}

fn canon(t: &Option<Term>) -> String {
    match t {
        None => "UNDEF".into(),
        Some(t) => t.to_string(),
    }
}

fn oracle(store: &Store, q: &str, cols: &[String], kind: Kind) -> Vec<String> {
    let r = sparql::query(
        store.snapshot(),
        &format!("{PREFIX}{q}"),
        &QueryOptions {
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
            .triples()
            .iter()
            .map(|t| {
                format!(
                    "{} {} {}",
                    canon(&Some(Term::from(t.subject.clone()))),
                    t.predicate,
                    canon(&Some(t.object.clone()))
                )
            })
            .collect(),
        Kind::Select => {
            let idx: Vec<usize> = cols
                .iter()
                .map(|c| r.variables().iter().position(|v| v == c).expect("column"))
                .collect();
            r.test_application_rows()
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

fn circuit(store: &Store, q: &str, mode: Provenance) -> Circuit {
    let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).expect("parse");
    let mut c = Circuit::new(compile(&parsed, None).expect("compile"), None);
    c.set_provenance(mode).expect("provenance");
    c.build(store.snapshot(), &Control::none()).expect("build");
    c
}

fn step(store: &Store, c: &mut Circuit) {
    let d = DeltaSet::read(store, c.applied, &Control::none());
    assert!(d.complete, "the changes of a transaction are known");
    let a = c.advance(&d, &Control::none()).expect("advance");
    assert!(!matches!(a, Advance::Reset(_)), "a pass reset");
}

const QUERIES: &[&str] = &[
    "SELECT ?x ?y WHERE { ?x :p+ ?y }",
    "SELECT ?x ?y WHERE { ?x :p* ?y }",
    "SELECT ?x WHERE { ?x :p+ ?x }",
    "SELECT ?x WHERE { ?x :p* ?x }",
    "SELECT ?y WHERE { :n0 :p+ ?y }",
    "SELECT ?y WHERE { :n0 :p* ?y }",
    "SELECT ?x WHERE { ?x :p+ :n1 }",
    "SELECT ?x WHERE { ?x :p* :n1 }",
    "SELECT ?x WHERE { ?x :p* 1 }",
    "ASK { :n0 :p+ :n1 }",
    "ASK { :n0 :p+ :n0 }",
    "ASK { :n0 :p* :n0 }",
    "ASK { :n0 :p* :n1 }",
    "SELECT ?y WHERE { :zz :p* ?y }",
    "SELECT ?y WHERE { :zz :p+ ?y }",
    "SELECT ?x ?y WHERE { ?x (:p|^:q)+ ?y }",
    "SELECT ?x ?y WHERE { ?x (:p/:q)+ ?y }",
    "SELECT ?x ?y WHERE { ?x ^:p+ ?y }",
    "SELECT ?x ?y WHERE { ?x ^(:p+) ?y }",
    "SELECT ?x ?y WHERE { ?x (:p|:q)* ?y }",
    "SELECT ?g ?x ?y WHERE { GRAPH ?g { ?x :p+ ?y } }",
    "SELECT ?g ?y WHERE { GRAPH ?g { :n0 :p* ?y } }",
    "SELECT ?g ?x WHERE { GRAPH ?g { ?x :p* ?x } }",
    "SELECT ?g ?x ?y WHERE { GRAPH ?g { ?x (:p|:q)* ?y } }",
    "SELECT ?x ?y WHERE { GRAPH :g1 { ?x :p* ?y } }",
    "SELECT ?y WHERE { GRAPH :nope { :n0 :p* ?y } }",
    "SELECT ?y WHERE { GRAPH :g1 { :n0 :p* ?y } }",
    "SELECT ?x ?z WHERE { ?x :p+ ?y . ?y :q ?z }",
    "SELECT ?x ?y WHERE { ?x :q ?y FILTER NOT EXISTS { ?x :p+ ?y } }",
    "SELECT ?x ?y WHERE { ?x :q ?y FILTER EXISTS { ?y :p+ ?x } }",
    "SELECT ?x ?y WHERE { ?x :q ?y FILTER NOT EXISTS { GRAPH ?g { ?x :p+ ?y } } }",
    "SELECT ?x ?y WHERE { GRAPH :g1 { ?x :q ?y } FILTER NOT EXISTS { GRAPH :g1 { ?y :p+ ?x } } }",
    "SELECT ?x ?y WHERE { ?x :q ?y MINUS { ?x :p* ?y } }",
    "SELECT ?x ?y WHERE { ?x :q ?z OPTIONAL { ?z :p+ ?y } }",
    "SELECT ?x (COUNT(?y) AS ?n) WHERE { ?x :p+ ?y } GROUP BY ?x",
    "SELECT DISTINCT ?y WHERE { ?x :p* ?y }",
    "CONSTRUCT { ?x :reach ?y } WHERE { ?x :p+ ?y }",
];

/// A random quad, with self-loops and the literal object among them.
fn quad(rng: &mut StdRng, graphs: &[u8]) -> Quad {
    let s = rng.random_range(0..NODES);
    let o = if rng.random_range(0..8) == 0 {
        if rng.random_bool(0.5) { s } else { NODES }
    } else {
        rng.random_range(0..NODES)
    };
    (
        s,
        u8::from(rng.random_range(0..4) == 0),
        o,
        graphs[rng.random_range(0..graphs.len())],
    )
}

/// The next transaction of a history: insertions and deletions of quads the model
/// holds, often one that closes or breaks a cycle.
fn transaction(rng: &mut StdRng, data: &BTreeSet<Quad>, graphs: &[u8]) -> (Vec<Quad>, Vec<Quad>) {
    let (mut del, mut ins) = (Vec::new(), Vec::new());
    for _ in 0..rng.random_range(1..4) {
        if !data.is_empty() && rng.random_bool(0.45) {
            let held: Vec<&Quad> = data.iter().collect();
            del.push(*held[rng.random_range(0..held.len())]);
        } else {
            ins.push(quad(rng, graphs));
        }
    }
    // a transaction that removes and adds the same quad
    if rng.random_range(0..10) == 0
        && let Some(q) = del.first()
    {
        ins.push(*q);
    }
    (del, ins)
}

fn history(seed: u64, queries: &[&str], union: bool, steps: usize) -> Vec<String> {
    let mut wrong = Vec::new();
    let graphs: &[u8] = if union { &[1, 2] } else { &[0, 0, 0, 1, 2] };
    for q in queries {
        let mut rng = StdRng::seed_from_u64(seed);
        let s = Store::in_memory(StoreOptions {
            union_default_graph: union,
            ..Default::default()
        });
        let mut data: BTreeSet<Quad> = BTreeSet::new();
        let init: Vec<Quad> = (0..rng.random_range(3..10))
            .map(|_| quad(&mut rng, graphs))
            .collect();
        apply(&s, &[], &init);
        data.extend(init);
        let mut c = circuit(&s, q, Provenance::None);
        let mut log = Vec::new();
        for i in 0..=steps {
            if i > 0 {
                let (del, ins) = transaction(&mut rng, &data, graphs);
                apply(&s, &del, &ins);
                for d in &del {
                    data.remove(d);
                }
                data.extend(ins.iter().copied());
                log.push(format!("-{del:?} +{ins:?}"));
                step(&s, &mut c);
            }
            let want = oracle(&s, q, &c.columns(), c.kind);
            let got = result(&c);
            if got != want {
                wrong.push(format!(
                    "seed {seed} union {union}: {q}\n  after {log:?}\n  data {data:?}\n  circuit {got:?}\n  engine  {want:?}"
                ));
                break;
            }
        }
    }
    wrong
}

#[test]
fn paths_follow_the_oracle_through_random_histories() {
    let cases: u64 = std::env::var("SPARKLES_FIXPOINT_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let base: u64 = std::env::var("SPARKLES_FIXPOINT_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut wrong = Vec::new();
    for seed in base..base + cases {
        wrong.extend(history(seed, QUERIES, false, 14));
        if wrong.len() > 3 {
            break;
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn paths_follow_the_oracle_over_the_union_default_graph() {
    let queries: Vec<&str> = QUERIES
        .iter()
        .filter(|q| !q.contains("GRAPH"))
        .copied()
        .collect();
    let mut wrong = Vec::new();
    for seed in 100..106 {
        wrong.extend(history(seed, &queries, true, 12));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every path query of the list compiles to the fixed-point operator.
#[test]
fn paths_compile_to_the_fixed_point() {
    for q in QUERIES {
        let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).unwrap();
        let c = compile(&parsed, None).unwrap();
        assert!(c.root.has_fixpoint(), "{q}");
        assert!(!c.root.has_recompute(), "{q}: {:#?}", c.root.plan(&c.vars));
    }
}

/// A cycle that one deletion breaks, a self-loop, and a chain whose middle goes: the
/// facts that keep another walk stay, and the circuit's change lists only what goes.
#[test]
fn deletions_break_cycles_exactly() {
    let s = Store::in_memory(StoreOptions::default());
    let data = r#"@prefix : <http://ex/> .
        :n0 :p :n1 . :n1 :p :n2 . :n2 :p :n0 . :n2 :p :n3 . :n3 :p :n3 . :n1 :p :n4 . :n4 :p :n2 ."#;
    s.load(&[crate::io::Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?x ?y WHERE { ?x :p+ ?y }",
        "SELECT ?x ?y WHERE { ?x :p* ?y }",
        "SELECT ?y WHERE { :n0 :p+ ?y }",
        "SELECT ?x WHERE { ?x :p+ ?x }",
    ] {
        let st = Store::in_memory(StoreOptions::default());
        st.load(&[crate::io::Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let mut c = circuit(&st, q, Provenance::None);
        for u in [
            // n1 still reaches n2 through n4
            "DELETE DATA { :n1 :p :n2 }",
            // the cycle breaks
            "DELETE DATA { :n2 :p :n0 }",
            "DELETE DATA { :n3 :p :n3 }",
            "INSERT DATA { :n3 :p :n0 }",
            "DELETE DATA { :n4 :p :n2 } ; INSERT DATA { :n1 :p :n2 }",
            "DELETE WHERE { ?a :p ?b }",
        ] {
            sparql::update::update(&st, &format!("{PREFIX}{u}"), &QueryOptions::default()).unwrap();
            let before = result(&c);
            let d = DeltaSet::read(&st, c.applied, &Control::none());
            let Advance::Changed(ch) = c.advance(&d, &Control::none()).unwrap() else {
                panic!("{q}: {u} did not change the circuit");
            };
            let want = oracle(&st, q, &c.columns(), c.kind);
            assert_eq!(result(&c), want, "{q}: {u}");
            // the change is exactly the difference
            let removed = ch.iter().filter(|(_, w)| *w < 0).count();
            let added = ch.iter().filter(|(_, w)| *w > 0).count();
            let gone = before.iter().filter(|r| !want.contains(r)).count();
            let new = want.iter().filter(|r| !before.contains(r)).count();
            assert_eq!((removed, added), (gone, new), "{q}: {u}");
        }
    }
    drop(s);
}

/// The state survives the checkpoint encoding and a generation switch.
#[test]
fn a_fixed_point_survives_encoding_and_compaction() {
    for q in QUERIES {
        let mut rng = StdRng::seed_from_u64(7);
        let s = Store::in_memory(StoreOptions::default());
        let init: Vec<Quad> = (0..12).map(|_| quad(&mut rng, &[0, 1, 2])).collect();
        apply(&s, &[], &init);
        let mut c = circuit(&s, q, Provenance::None);
        let body = c.encode(&s.snapshot()).unwrap();
        let mut c2 = circuit(&s, q, Provenance::None);
        c2.decode(&body, &s.snapshot()).unwrap();
        assert_eq!(result(&c), result(&c2), "{q}");
        apply(&s, &init[..3], &[(5, 0, 0, 0), (0, 0, 5, 1)]);
        s.compact().unwrap();
        step(&s, &mut c);
        step(&s, &mut c2);
        let want = oracle(&s, q, &c.columns(), c.kind);
        assert_eq!(result(&c), want, "{q}: after a compaction");
        assert_eq!(result(&c2), want, "{q}: decoded, after a compaction");
        apply(&s, &init[3..8], &[]);
        step(&s, &mut c);
        assert_eq!(
            result(&c),
            oracle(&s, q, &c.columns(), c.kind),
            "{q}: after a compaction and deletions"
        );
    }
}

/// A closure that passes the evaluation budget fails the pass with `over-budget`.
#[test]
fn a_closure_over_its_budget_fails() {
    let s = Store::in_memory(StoreOptions::default());
    let chain: Vec<String> = (0..300)
        .map(|i| format!("<{NS}c{i}> <{NS}p> <{NS}c{}> .", i + 1))
        .collect();
    s.load(&[crate::io::Source::from_bytes(
        chain.join("\n").into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let parsed = sparql::parse_query(
        &format!("{PREFIX}SELECT * WHERE {{ ?x :p+ ?y }}"),
        None,
        &[],
    )
    .unwrap();
    let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
    c.max_memory_bytes = Some(256 << 10);
    let e = c.build(s.snapshot(), &Control::none()).unwrap_err();
    assert!(matches!(e, crate::Error::BudgetExceeded(_)), "{e:?}");
}

/// The walks a pass memoizes count against the budget while the pass runs and are
/// released after it, so a commit of many steps that reach nothing neither keeps
/// memory nor passes a small budget unnoticed.
#[test]
fn a_pass_releases_and_budgets_its_walks() {
    let s = Store::in_memory(StoreOptions::default());
    apply(&s, &[], &[(0, 1, 1, 0)]);
    let parsed = sparql::parse_query(
        &format!("{PREFIX}SELECT ?y WHERE {{ :n0 :p+ ?y }}"),
        None,
        &[],
    )
    .unwrap();
    let edges: String = (0..5000)
        .map(|i| format!("<{NS}c{i}> <{NS}p> <{NS}d{i}> . "))
        .collect();
    let insert = format!("INSERT DATA {{ {edges} }}");
    // without a budget, the state after the pass is the state before it
    let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
    c.build(s.snapshot(), &Control::none()).unwrap();
    let before = c.bytes();
    sparql::update::update(&s, &insert, &QueryOptions::default()).unwrap();
    step(&s, &mut c);
    assert_eq!(c.bytes(), before);
    assert!(result(&c).is_empty());
    // with a budget the state fits but the pass's walks do not
    let s = Store::in_memory(StoreOptions::default());
    apply(&s, &[], &[(0, 1, 1, 0)]);
    let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
    c.max_memory_bytes = Some(16 << 10);
    c.build(s.snapshot(), &Control::none()).unwrap();
    sparql::update::update(&s, &insert, &QueryOptions::default()).unwrap();
    let d = DeltaSet::read(&s, c.applied, &Control::none());
    let Err(e) = c.advance(&d, &Control::none()) else {
        panic!("the pass fits its budget");
    };
    assert!(matches!(e, crate::Error::BudgetExceeded(_)), "{e:?}");
}

/// The variables the compiler adds for a path's step cannot capture the query's own.
#[test]
fn step_variables_do_not_capture_query_variables() {
    let queries = [
        "SELECT ?__fp1x ?s ?o WHERE { GRAPH ?__fp1x { ?s :p+ ?o } }",
        "SELECT ?__fp1y ?s ?o WHERE { GRAPH ?__fp1y { ?s :p+ ?o } }",
        "SELECT ?__fp1x ?__fp1y WHERE { ?__fp1x :p* ?__fp1y }",
        "SELECT ?s ?__fp1y WHERE { ?s :p+ ?o . ?o :q ?__fp1y }",
    ];
    let s = Store::in_memory(StoreOptions::default());
    apply(
        &s,
        &[],
        &[
            (0, 0, 1, 1),
            (1, 0, 2, 1),
            (2, 1, 3, 1),
            (0, 0, 1, 0),
            (1, 1, 2, 0),
        ],
    );
    for q in queries {
        let c = circuit(&s, q, Provenance::None);
        let want = oracle(&s, q, &c.columns(), c.kind);
        assert!(!want.is_empty(), "{q}");
        assert_eq!(result(&c), want, "{q}");
    }
}

/// The quads of the model by fact key.
fn keys(data: &BTreeSet<Quad>) -> BTreeMap<FactKey, Quad> {
    data.iter()
        .map(|q| {
            let g = graph(q.3);
            (
                FactKey::of_terms(&node(q.0), &pred(q.1), &node(q.2), g.as_ref()),
                *q,
            )
        })
        .collect()
}

/// Annotations of path rows: each row counts one derivation per way its query derives
/// it, the supports add up to the count, and every witness is real, its quads alone
/// giving the row.
#[test]
fn path_rows_report_real_witnesses() {
    let queries = [
        "SELECT ?x ?y WHERE { ?x :p+ ?y }",
        "SELECT ?x ?y WHERE { ?x :p* ?y }",
        "SELECT ?y WHERE { :n0 :p+ ?y }",
        "SELECT ?x WHERE { ?x :p+ :n1 }",
        "SELECT ?x ?y WHERE { ?x (:p|^:q)+ ?y }",
        "SELECT ?x ?y WHERE { ?x (:p/:q)+ ?y }",
        "SELECT ?g ?x ?y WHERE { GRAPH ?g { ?x :p+ ?y } }",
        "SELECT ?x ?z WHERE { ?x :p+ ?y . ?y :q ?z }",
    ];
    let mut wrong = Vec::new();
    for mode in [
        Provenance::Count,
        Provenance::Lineage,
        Provenance::why(),
        Provenance::Why { max_witnesses: 1 },
    ] {
        for seed in 0..5u64 {
            for q in queries {
                let mut rng = StdRng::seed_from_u64(seed + 40);
                let s = Store::in_memory(StoreOptions::default());
                let mut data: BTreeSet<Quad> = BTreeSet::new();
                let graphs = [0, 0, 1, 2];
                let init: Vec<Quad> = (0..8).map(|_| quad(&mut rng, &graphs)).collect();
                apply(&s, &[], &init);
                data.extend(init);
                let mut c = circuit(&s, q, mode);
                for i in 0..=8 {
                    if i > 0 {
                        let (del, ins) = transaction(&mut rng, &data, &graphs);
                        apply(&s, &del, &ins);
                        for d in &del {
                            data.remove(d);
                        }
                        data.extend(ins.iter().copied());
                        step(&s, &mut c);
                    }
                    if let Err(e) = check_annotations(&s, &c, q, mode, &data) {
                        wrong.push(format!("{mode:?} seed {seed} step {i}: {q}: {e}"));
                        break;
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn check_annotations(
    s: &Store,
    c: &Circuit,
    q: &str,
    mode: Provenance,
    data: &BTreeSet<Quad>,
) -> Result<(), String> {
    let want = oracle(s, q, &c.columns(), c.kind);
    let got = result(c);
    if got != want {
        return Err(format!("rows {got:?}, engine {want:?}"));
    }
    let by_key = keys(data);
    for (row, w, ann) in c.raw_annotations() {
        let ann = ann.ok_or("no annotation")?;
        let line = row.iter().map(canon).collect::<Vec<_>>().join(" ");
        if ann.count() != w {
            return Err(format!("{line}: count {} for weight {w}", ann.count()));
        }
        let report = ann.report(mode.max_witnesses());
        if matches!(mode, Provenance::Why { .. }) {
            let super::prov::Ann::Why(x) = &ann else {
                return Err("not a why annotation".into());
            };
            let known: i64 = x.supports.iter().map(|(_, n)| n).sum();
            if known + x.unknown != w || known <= 0 {
                return Err(format!(
                    "{line}: supports {known} and {} unknown",
                    x.unknown
                ));
            }
            for wit in &report.witnesses {
                let mut quads = Vec::new();
                for a in wit.atoms() {
                    let Atom::Fact(k) = a else {
                        return Err(format!("{line}: a witness with a condition"));
                    };
                    quads.push(*by_key.get(k).ok_or_else(|| {
                        format!("{line}: a witness names a quad the store does not hold")
                    })?);
                }
                let alone = Store::in_memory(StoreOptions::default());
                apply(&alone, &[], &quads);
                if !oracle(&alone, q, &c.columns(), c.kind).contains(&line) {
                    return Err(format!("{line}: the witness {quads:?} does not derive it"));
                }
            }
        }
        if matches!(mode, Provenance::Lineage) {
            for a in &report.lineage {
                let Atom::Fact(k) = a else {
                    continue;
                };
                if !by_key.contains_key(k) {
                    return Err(format!(
                        "{line}: lineage names a quad the store does not hold"
                    ));
                }
            }
            if report.lineage.is_empty() {
                return Err(format!("{line}: no lineage"));
            }
        }
    }
    Ok(())
}

/// A reader of every graph whose protection hides the triples of one predicate.
fn hiding(pred: &str) -> std::sync::Arc<crate::access::GraphAccess> {
    use crate::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
    let protection = Protection {
        name: "hide".into(),
        predicates: Some(vec![pred.into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    std::sync::Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: std::sync::Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ))
}

/// Zero-length rows from a constant end depend on whether the reader can read the
/// active graph, and on the triples that protections hide in it, with the union default
/// graph and without.
#[test]
fn zero_length_rows_follow_access_and_the_union_graph() {
    use crate::access::{GraphAccess, GraphRule, Graphs};
    use std::sync::Arc;
    let readers: Vec<(&str, Option<Arc<GraphAccess>>)> = vec![
        ("everything", None),
        (
            "default and g1",
            Some(Arc::new(GraphAccess::graphs(
                Graphs::Only(GraphRule::new(["default", "http://ex/g1"], &[])),
                Graphs::none(),
            ))),
        ),
        (
            "g1 and g2",
            Some(Arc::new(GraphAccess::graphs(
                Graphs::Only(GraphRule::new(["http://ex/g1", "http://ex/g2"], &[])),
                Graphs::none(),
            ))),
        ),
        (
            "g2",
            Some(Arc::new(GraphAccess::graphs(
                Graphs::Only(GraphRule::new(["http://ex/g2"], &[])),
                Graphs::none(),
            ))),
        ),
        (
            "nothing",
            Some(Arc::new(GraphAccess::graphs(
                Graphs::none(),
                Graphs::none(),
            ))),
        ),
        ("hiding q", Some(hiding("http://ex/q"))),
        ("hiding p", Some(hiding("http://ex/p"))),
    ];
    let queries = [
        "SELECT ?y WHERE { :n0 :p* ?y }",
        "SELECT ?x WHERE { ?x :p* :n1 }",
        "SELECT ?x ?y WHERE { ?x :p* ?y }",
        "SELECT ?g ?y WHERE { GRAPH ?g { :n0 :p* ?y } }",
        "SELECT ?y WHERE { GRAPH :g1 { :n0 :p* ?y } }",
        "SELECT ?y WHERE { :n0 :p+ ?y }",
        "ASK { :n0 :p* :n0 }",
        "ASK { :zz :p* :zz }",
    ];
    let history: [(&[Quad], &[Quad]); 5] = [
        (&[], &[(0, 0, 1, 1)]),
        (&[], &[(2, 0, 3, 0)]),
        (&[], &[(1, 1, 0, 2)]),
        (&[(0, 0, 1, 1)], &[]),
        (&[(2, 0, 3, 0), (1, 1, 0, 2)], &[]),
    ];
    let mut wrong = Vec::new();
    for union in [false, true] {
        for (who, access) in &readers {
            for q in queries {
                let s = Store::in_memory(StoreOptions {
                    union_default_graph: union,
                    ..Default::default()
                });
                let parsed =
                    sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).expect("parse");
                let mut c = Circuit::new(compile(&parsed, None).unwrap(), access.clone());
                c.build(s.snapshot(), &Control::none()).unwrap();
                for i in 0..=history.len() {
                    if i > 0 {
                        let (del, ins) = history[i - 1];
                        apply(&s, del, ins);
                        step(&s, &mut c);
                    }
                    let r = sparql::query(
                        s.snapshot(),
                        &format!("{PREFIX}{q}"),
                        &QueryOptions {
                            graphs: access.clone(),
                            no_cache: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    let mut want: Vec<String> = if c.kind == Kind::Ask {
                        if r.boolean {
                            vec!["true".into()]
                        } else {
                            vec![]
                        }
                    } else {
                        let cols = c.columns();
                        let idx: Vec<usize> = cols
                            .iter()
                            .map(|c| r.variables().iter().position(|v| v == c).unwrap())
                            .collect();
                        r.test_application_rows()
                            .iter()
                            .map(|row| {
                                idx.iter()
                                    .map(|&i| canon(&row[i]))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .collect()
                    };
                    want.sort();
                    let mut fresh = Circuit::new(compile(&parsed, None).unwrap(), access.clone());
                    fresh.build(s.snapshot(), &Control::none()).unwrap();
                    // A reader of the union default graph who reads none of the named
                    // graphs that hold quads sees an empty default graph, and a circuit
                    // does not see the commits to graphs its reader cannot read, so it
                    // keeps a constant end's zero-length row until a commit it sees.
                    let hidden = union && matches!(*who, "default and g1" | "g2" | "nothing");
                    let got = if hidden { result(&fresh) } else { result(&c) };
                    if result(&fresh) != want {
                        wrong.push(format!(
                            "union {union}, {who}, step {i}, a build: {q}\n  circuit {:?}\n  engine  {want:?}",
                            result(&fresh)
                        ));
                        break;
                    }
                    if got != want {
                        wrong.push(format!(
                            "union {union}, {who}, step {i}: {q}\n  circuit {got:?}\n  engine  {want:?}"
                        ));
                        break;
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
