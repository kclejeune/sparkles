//! Decorrelated EXISTS: the same answers as per-row substitution, on random data and
//! the shapes the rewrite has to get right (partial bindings after OPTIONAL, nested
//! EXISTS, filters inside the pattern over outer variables, GRAPH inside and outside,
//! NOT EXISTS beside MINUS).

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str =
    "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

fn without() -> Optimizations {
    Optimizations {
        decorrelate_exists: false,
        ..Optimizations::ALL
    }
}

fn opts(opt: Optimizations) -> QueryOptions {
    QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    }
}

/// Run `text` on `snap` (decorrelating whenever the pattern is admitted).
fn run_on(snap: Arc<Snapshot>, text: &str, o: &QueryOptions) -> QueryResult {
    let text = format!("{PREFIXES}{text}");
    super::exists::FORCE.with(|f| f.set(true));
    let r = query(snap, &text, o);
    super::exists::FORCE.with(|f| f.set(false));
    r.unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Solutions as exact RDF terms; sorted (a multiset) unless `ordered`.
fn rows(r: &QueryResult, ordered: bool) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    if !ordered {
        v.sort();
    }
    if r.kind == QueryKind::Ask {
        v.push(r.boolean.to_string());
    }
    v
}

fn decorrelated(p: &PlanInfo) -> bool {
    p.description.contains("decorrelated") || p.children.iter().any(decorrelated)
}

fn has_desc(p: &PlanInfo, needle: &str) -> bool {
    p.description.contains(needle) || p.children.iter().any(|c| has_desc(c, needle))
}

/// The answers with and without the rewrite must be equal; returns them and whether
/// the rewrite ran.
fn check(snap: &Arc<Snapshot>, text: &str) -> (Vec<String>, bool) {
    let ordered = text.contains("ORDER BY");
    let on = run_on(snap.clone(), text, &opts(Optimizations::ALL));
    let off = run_on(snap.clone(), text, &opts(without()));
    let a = rows(&on, ordered);
    assert_eq!(a, rows(&off, ordered), "{text}");
    assert!(
        !decorrelated(&off.plan),
        "{text}: switched off, yet decorrelated"
    );
    (a, decorrelated(&on.plan))
}

/// Shapes, and whether the rewrite applies to them (when the outer rows bind a key).
const SHAPES: &[(&str, bool)] = &[
    // plain correlation, one and two keys, the key in either position
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?x } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?z ex:q ?y } }",
        true,
    ),
    (
        "SELECT ?x WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?x ?pp ?y } }",
        true,
    ),
    // duplicates of the outer rows stay
    (
        "SELECT ?x WHERE { ?x ex:p ?y . ?x ex:p ?w FILTER EXISTS { ?x ex:q ?z } }",
        true,
    ),
    (
        "SELECT ?y WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:r ?z } }",
        true,
    ),
    // partial bindings after OPTIONAL: unbound keys match anything
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?y ex:q ?z } FILTER NOT EXISTS { ?z ex:r ?w } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?y ex:q ?z } FILTER EXISTS { ?x ex:r ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?y ex:q ?z } FILTER NOT EXISTS { ?x ex:r ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?x ex:q ?z } OPTIONAL { ?y ex:r ?w } FILTER EXISTS { ?z ex:p ?w } }",
        true,
    ),
    (
        "SELECT * WHERE { { ?x ex:p ?y } UNION { ?x ex:q ?z } FILTER EXISTS { ?x ex:r ?y } }",
        true,
    ),
    (
        "SELECT * WHERE { VALUES (?x ?y) { (ex:a UNDEF) (UNDEF ex:b) (ex:c ex:d) (UNDEF UNDEF) (ex:a ex:b) } FILTER EXISTS { ?x ex:p ?y } }",
        true,
    ),
    (
        "SELECT * WHERE { VALUES (?x ?y) { (ex:a UNDEF) (UNDEF ex:b) (ex:c ex:d) (UNDEF UNDEF) } FILTER NOT EXISTS { ?x ex:p ?y } }",
        true,
    ),
    // nested EXISTS: correlated with the middle pattern only (decorrelated at both
    // levels), and with the outer row (the middle is evaluated per row)
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER NOT EXISTS { ?z ex:r ?y } } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z FILTER EXISTS { ?z ex:p ?w } } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER NOT EXISTS { ?z ex:r ?x } } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER EXISTS { ?z ex:r ?x } } }",
        false,
    ),
    // FILTERs inside the pattern: over its own variables, over an outer variable it
    // binds, over an outer variable only the filter uses (per row)
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER(?z != ?y) } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?x ex:q ?z FILTER(?z != ?x) } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER(?z != ?x) } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z FILTER(BOUND(?x)) } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { { ?y ex:q ?z FILTER(?w != ?z) } ?y ex:r ?w } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?x ex:r ?w } FILTER EXISTS { ?y ex:q ?z FILTER(!BOUND(?w) || ?z != ?w) } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER(isLiteral(?z) || ?z = ex:a) } }",
        true,
    ),
    // GRAPH inside the pattern, outside it, and both with the same variable
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { GRAPH ?g { ?y ex:q ?z } } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { GRAPH ex:g1 { ?x ex:q ?y } } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { GRAPH <urn:x-arq:DefaultGraph> { ?y ex:q ?z } } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { GRAPH <urn:x-arq:UnionGraph> { ?y ex:q ?z } } }",
        true,
    ),
    (
        "SELECT * WHERE { GRAPH ?g { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z } } }",
        true,
    ),
    (
        "SELECT * WHERE { GRAPH ?g { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?x } } }",
        true,
    ),
    (
        "SELECT * WHERE { GRAPH ?g { ?x ex:p ?y } FILTER EXISTS { GRAPH ?g { ?y ex:q ?z } } }",
        true,
    ),
    (
        "SELECT * WHERE { GRAPH ?g { ?x ex:p ?y } FILTER NOT EXISTS { GRAPH ?h { ?y ex:q ?x } } }",
        true,
    ),
    (
        "SELECT * WHERE { GRAPH ex:g2 { ?x ex:p ?y FILTER EXISTS { ?x ex:q ?z } } }",
        true,
    ),
    // NOT EXISTS and MINUS over the same patterns
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z } }",
        true,
    ),
    ("SELECT * WHERE { ?x ex:p ?y MINUS { ?y ex:q ?z } }", false),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?a ex:q ?b } }",
        false,
    ),
    ("SELECT * WHERE { ?x ex:p ?y MINUS { ?a ex:q ?b } }", false),
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?y ex:r ?w } FILTER NOT EXISTS { ?x ex:q ?w } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y OPTIONAL { ?y ex:r ?w } MINUS { ?x ex:q ?w } }",
        false,
    ),
    // blank nodes in the pattern, paths
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q [] } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { [] ex:q ?y } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q/ex:r ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q+ ?x } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y (ex:q|^ex:r) ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q* ?x } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q? ?z } }",
        false,
    ),
    // outer values the pattern cannot produce (computed IRIs, literals)
    (
        "SELECT * WHERE { ?x ex:p ?y BIND(IRI(CONCAT(STR(?x), \"2\")) AS ?u) FILTER NOT EXISTS { ?u ex:q ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y BIND(?y AS ?u) FILTER EXISTS { ?z ex:r ?u } }",
        true,
    ),
    // constants substituted before the filter
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER(?x = ex:a) FILTER NOT EXISTS { ?x ex:q ?z } }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER(?y = ex:b) FILTER EXISTS { ?z ex:q ?y } }",
        true,
    ),
    // two EXISTS conjuncts, one beside an ordinary filter, EXISTS under OR / BIND
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER(EXISTS { ?y ex:q ?z } && NOT EXISTS { ?x ex:r ?y }) }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER(?x != ex:c && NOT EXISTS { ?y ex:q ?x }) }",
        true,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER(EXISTS { ?y ex:q ?z } || ?x = ex:a) }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y BIND(EXISTS { ?y ex:q ?z } AS ?e) }",
        false,
    ),
    // patterns the rewrite does not admit
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z OPTIONAL { ?z ex:r ?x } } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { { ?y ex:q ?z } UNION { ?z ex:r ?y } } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z MINUS { ?z ex:r ?x } } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z BIND(?z AS ?x) } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { SELECT ?y WHERE { ?y ex:q ?z } } }",
        false,
    ),
    (
        "SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { VALUES ?z { ex:a } ?y ex:q ?z } }",
        false,
    ),
    // modifiers above the filter
    (
        "SELECT ?x ?y WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z } } ORDER BY ?x ?y",
        true,
    ),
    (
        "SELECT ?y (COUNT(*) AS ?n) WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z } } GROUP BY ?y",
        true,
    ),
    (
        "SELECT DISTINCT ?x WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z } }",
        true,
    ),
    (
        "SELECT (COUNT(*) AS ?n) WHERE { SELECT * WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z } } LIMIT 3 }",
        true,
    ),
    ("ASK { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z } }", true),
];

/// A random dataset over a few subjects, three predicates, IRIs, blank nodes and
/// literals, in the default graph and two named graphs (the same triple often in
/// several graphs).
fn random_trig(next: &mut impl FnMut() -> u64, n: usize) -> String {
    let nodes = [
        "ex:a",
        "ex:b",
        "ex:c",
        "ex:d",
        "ex:e",
        "ex:a2",
        "_:b1",
        "_:b2",
        "1",
        "\"01\"^^xsd:integer",
        "\"x\"",
        "\"x\"@en",
    ];
    let preds = ["ex:p", "ex:q", "ex:r"];
    let mut graphs: [Vec<String>; 3] = Default::default();
    for _ in 0..n {
        // subjects are IRIs or blank nodes
        let s = nodes[(next() % 8) as usize];
        let p = preds[(next() % 3) as usize];
        let o = nodes[(next() % nodes.len() as u64) as usize];
        let t = format!("{s} {p} {o} .");
        let g = (next() % 4) as usize;
        if g == 3 {
            graphs[1].push(t.clone());
            graphs[2].push(t);
        } else {
            graphs[g].push(t);
        }
    }
    format!(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n{}\nex:g1 {{ {} }}\nex:g2 {{ {} }}\n",
        graphs[0].join("\n"),
        graphs[1].join(" "),
        graphs[2].join(" ")
    )
}

/// Random stores (default graph separate or the union of the named graphs), queried
/// as loaded, after updates still in the delta, and at the snapshot before them.
#[test]
fn decorrelated_exists_matches_substitution_on_random_data() {
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut fired = vec![0usize; SHAPES.len()];
    for round in 0..24 {
        let s = Store::in_memory(StoreOptions {
            union_default_graph: round % 3 == 2,
            ..Default::default()
        });
        let n = 10 + (next() % 50) as usize;
        let trig = random_trig(&mut next, n);
        s.load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        let before = s.snapshot();
        for (i, (q, _)) in SHAPES.iter().enumerate() {
            fired[i] += check(&before, q).1 as usize;
        }
        // updates kept in the delta: new triples, deletions, a new graph
        let mut ins = String::new();
        for _ in 0..1 + next() % 6 {
            ins.push_str(&format!(
                "ex:{} ex:{} ex:{} . ",
                ["a", "b", "c", "f"][(next() % 4) as usize],
                ["p", "q", "r"][(next() % 3) as usize],
                ["a", "b", "d", "f"][(next() % 4) as usize]
            ));
        }
        super::update::update(
            &s,
            &format!(
                "{PREFIXES}INSERT DATA {{ {ins} GRAPH ex:g3 {{ {ins} }} }} ; DELETE WHERE {{ ex:b ex:q ?o }}"
            ),
            &QueryOptions::default(),
        )
        .unwrap();
        let after = s.snapshot();
        let mut past = Vec::new();
        for (i, (q, _)) in SHAPES.iter().enumerate() {
            fired[i] += check(&after, q).1 as usize;
            past.push(check(&before, q).0);
        }
        // the past snapshot answers as it did before the update
        for ((q, _), rows) in SHAPES.iter().zip(&past) {
            let again = run_on(before.clone(), q, &opts(Optimizations::ALL));
            assert_eq!(&self::rows(&again, q.contains("ORDER BY")), rows, "{q}");
        }
    }
    for ((q, admitted), n) in SHAPES.iter().zip(&fired) {
        assert_eq!(*n > 0, *admitted, "{q}: decorrelated {n} times");
    }
}

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ttl.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

fn explain_of(s: &Store, q: &str, opt: Optimizations) -> QueryResult {
    query(s.snapshot(), &format!("{PREFIXES}{q}"), &opts(opt)).unwrap()
}

fn counter(p: &PlanInfo, name: &str) -> Option<u64> {
    p.counters
        .as_ref()
        .and_then(|c| c.get(name))
        .and_then(|v| v.as_u64())
        .or_else(|| p.children.iter().find_map(|c| counter(c, name)))
}

/// EXPLAIN shows the decorrelated filter with its counters, and the reason when the
/// EXISTS stays per row; the build happens once although the filter runs on many rows.
#[test]
fn explain_shows_decorrelation_and_why_not() {
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3000 {
        ttl.push_str(&format!("ex:s{i} ex:p ex:o{} .\n", i % 700));
    }
    // 100 objects with a q (50 of them with two)
    for j in (0..700).step_by(7) {
        ttl.push_str(&format!("ex:o{j} ex:q ex:z{} .\n", j % 5));
        if j % 14 == 0 {
            ttl.push_str(&format!("ex:o{j} ex:q ex:zz .\n"));
        }
    }
    let s = store(&ttl);
    let q = "SELECT ?x WHERE { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z } }";
    let on = explain_of(&s, q, Optimizations::ALL);
    let off = explain_of(&s, q, without());
    assert_eq!(rows(&on, false), rows(&off, false));
    assert_eq!(on.len(), 3000 - 3000 / 7 - 1);
    assert!(
        has_desc(&on.plan, "[NOT EXISTS decorrelated on ?y:"),
        "{:#?}",
        on.plan
    );
    assert!(!has_desc(&off.plan, "decorrelated"));
    // every outer row probed, none evaluated per row; one key per object with a q
    assert_eq!(counter(&on.plan, "existsProbed"), Some(3000));
    assert_eq!(counter(&on.plan, "existsPerRow"), Some(0));
    assert_eq!(counter(&on.plan, "existsKeys"), Some(100));
    assert_eq!(counter(&on.plan, "existsSolutions"), Some(150));
    // a filter over an outer variable the pattern does not bind: per row, and why
    let q = "SELECT ?x WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z FILTER(?z != ?x) } }";
    let r = explain_of(&s, q, Optimizations::ALL);
    assert!(
        has_desc(
            &r.plan,
            "[EXISTS per row: the outer solutions bind a variable"
        ),
        "{:#?}",
        r.plan
    );
    // a construct outside the admitted algebra
    let q = "SELECT ?x WHERE { ?x ex:p ?y FILTER EXISTS { ?y ex:q ?z OPTIONAL { ?z ex:r ?w } } }";
    let r = explain_of(&s, q, Optimizations::ALL);
    assert!(
        has_desc(&r.plan, "[EXISTS per row: it has an OPTIONAL]"),
        "{:#?}",
        r.plan
    );
    // a few outer rows against a large pattern: cheaper per row
    let q = "SELECT ?y WHERE { VALUES ?y { ex:o1 ex:o2 } FILTER EXISTS { ?y ex:q ?z . ?x ex:p ?y . ?x ex:p ?w } }";
    let r = explain_of(&s, q, Optimizations::ALL);
    assert!(
        has_desc(&r.plan, "costs less than the whole pattern"),
        "{:#?}",
        r.plan
    );
    // ASK runs the filter on growing prefixes of its input: the key set is built once
    let r = explain_of(
        &s,
        "ASK { ?x ex:p ?y FILTER NOT EXISTS { ?y ex:q ?z } }",
        Optimizations::ALL,
    );
    assert!(r.boolean);
    assert!(has_desc(&r.plan, "decorrelated"), "{:#?}", r.plan);
}

/// A key set over the memory budget leaves the EXISTS to per-row evaluation: the query
/// still succeeds within the budget the per-row plan needs.
#[test]
fn decorrelation_falls_back_under_the_memory_budget() {
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..2000 {
        ttl.push_str(&format!("ex:s{i} ex:p ex:o{} .\n", i % 50));
        ttl.push_str(&format!("ex:o{} ex:q ex:z{i} .\n", i % 50));
    }
    for i in 0..60 {
        ttl.push_str(&format!("ex:t{i} ex:p ex:u{i} .\n"));
    }
    let s = store(&ttl);
    let q =
        format!("{PREFIXES}SELECT ?x WHERE {{ ?x ex:p ?y FILTER NOT EXISTS {{ ?y ex:q ?z }} }}");
    let budget = |opt, limit| QueryOptions {
        max_memory_bytes: limit,
        ..opts(opt)
    };
    let off = query(s.snapshot(), &q, &budget(without(), None)).unwrap();
    let peak = off.mem_peak_bytes;
    let on = query(s.snapshot(), &q, &budget(Optimizations::ALL, None)).unwrap();
    assert!(decorrelated(&on.plan), "{:#?}", on.plan);
    assert!(on.mem_peak_bytes > peak, "{} vs {peak}", on.mem_peak_bytes);
    let tight = query(s.snapshot(), &q, &budget(Optimizations::ALL, Some(peak))).unwrap();
    assert_eq!(rows(&tight, false), rows(&off, false));
    assert!(!decorrelated(&tight.plan), "{:#?}", tight.plan);
    assert!(has_desc(&tight.plan, "memory budget"), "{:#?}", tight.plan);
}

#[test]
fn decorrelation_can_be_disabled_by_name() {
    let o = Optimizations::ALL.disable("decorrelate_exists").unwrap();
    assert!(!o.decorrelate_exists);
    assert!(o.range_pushdown);
}
