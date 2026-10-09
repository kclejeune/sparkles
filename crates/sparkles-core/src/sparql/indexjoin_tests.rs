//! Batched index joins and fused stars: random queries of the shapes they apply to over
//! random small stores (duplicates, named graphs, a union default graph, OPTIONAL
//! bindings, updates not yet compacted, past states) give the same solutions with them
//! on, forced wherever they apply, read either way, and off.

use super::indexjoin::{FORCE_INDEX_JOIN, FORCE_WALK};
use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str =
    "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

fn xorshift(mut x: u64) -> impl FnMut() -> u64 {
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

fn load(s: &Store, text: &str) {
    s.load(&[Source::from_bytes(
        text.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
}

fn update(s: &Store, text: &str) {
    super::update::update(s, &format!("{PREFIXES}{text}"), &QueryOptions::default()).unwrap();
}

/// Subjects `ex:s0…` of classes `ex:C0…ex:C3`, each with a few values of `ex:p0…ex:p4`
/// (subjects, integers, `ex:o0…`, strings; `ex:p4` links subjects), in the default
/// graph, `ex:g1`, `ex:g2` or all three; a few subjects have many values.
pub(super) fn random_trig(seed: u64, n: usize) -> String {
    let mut next = xorshift(seed);
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        let s = format!("ex:s{i}");
        let mut triples = vec![format!("{s} a ex:C{} .", next() % 4)];
        if next().is_multiple_of(5) {
            triples.push(format!("{s} a ex:C{} .", next() % 4));
        }
        let fan = if next().is_multiple_of(23) { 9 } else { 3 };
        for p in 0..5 {
            for _ in 0..next() % fan {
                let o = match (p, next() % 4) {
                    (4, _) | (_, 0) => format!("ex:s{}", next() % n as u64),
                    (_, 1) => format!("{}", next() % 7),
                    (_, 2) => format!("ex:o{}", next() % 4),
                    _ => format!("\"v{}\"", next() % 5),
                };
                triples.push(format!("{s} ex:p{p} {o} ."));
            }
        }
        for tr in triples {
            match next() % 8 {
                0..=4 => t.push_str(&format!("{tr}\n")),
                5 => t.push_str(&format!("ex:g1 {{ {tr} }}\n")),
                6 => t.push_str(&format!("ex:g2 {{ {tr} }}\n")),
                _ => t.push_str(&format!("{tr}\nex:g1 {{ {tr} }}\nex:g2 {{ {tr} }}\n")),
            }
        }
    }
    t
}

/// Updates after the base build: new subjects, values and graphs, and deletions.
const UPDATES: &[&str] = &[
    "INSERT DATA { ex:n1 a ex:C0 ; ex:p0 ex:s1 , 3 ; ex:p1 ex:o1 . ex:s2 ex:p1 ex:o2 . \
     ex:s3 a ex:C0 ; ex:p0 4 . GRAPH ex:g1 { ex:n1 ex:p0 3 . ex:n2 a ex:C1 ; ex:p2 ex:o1 } }",
    "DELETE WHERE { ex:s4 ex:p0 ?x } ; DELETE WHERE { ?s ex:p1 ex:o3 } ; \
     DELETE DATA { ex:s5 a ex:C0 . ex:s5 a ex:C1 . ex:s5 a ex:C2 . ex:s5 a ex:C3 }",
    "INSERT { ?s ex:p3 \"added\" } WHERE { ?s a ex:C1 } ; \
     INSERT DATA { GRAPH ex:g2 { ex:s7 ex:p0 ex:s8 ; ex:p1 1 } }",
];

/// Query shapes the operators apply to, with what the forced plan must show.
fn shapes() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        (
            "SELECT * WHERE { ?s a ex:C0 . ?s ex:p0 ?a }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 ; ex:p0 ?a ; ex:p1 ?b }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C1 ; ex:p0 ?a ; ex:p2 ex:o1 ; ex:p3 ?c }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { VALUES ?s { ex:s1 ex:s5 ex:s5 ex:s9 ex:nope } ?s ex:p0 ?a ; ex:p1 ?b }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { VALUES (?s ?a) { (ex:s1 UNDEF) (ex:s2 3) (ex:s2 ex:s1) (ex:s6 UNDEF) } ?s ex:p0 ?a }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p2 ?o } ?s ex:p1 ?o }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?x ex:p4 ?s . ?s ex:p0 ?a ; ex:p1 ?b FILTER(?x = ex:s3) }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 . ?s ex:p0 ?a FILTER(?a != ex:s1) ?s ex:p1 ?b FILTER(?b != 2) }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 ; ex:p0 ?a ; ex:p1 ?b FILTER(?a != ?b) }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C2 . ?s ex:p4 ?s }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { GRAPH ?g { ?s a ex:C0 . ?s ex:p0 ?a } }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { GRAPH ex:g1 { ?s a ex:C0 ; ex:p0 ?a ; ex:p1 ?b } }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s a ex:C0 ; ex:p0 ?a ; ex:p1 ?b } }",
            Some("StarJoin"),
        ),
        (
            "SELECT * FROM ex:g1 FROM ex:g2 WHERE { ?s a ex:C3 ; ex:p0 ?a ; ex:p2 ?b }",
            Some("StarJoin"),
        ),
        (
            "SELECT * FROM NAMED ex:g1 WHERE { GRAPH ?g { ?s a ex:C3 ; ex:p0 ?a } }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C2 . ?s ex:p4 ?t . ?t ex:p0 ?a }",
            Some("IndexJoin"),
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a ex:C0 ; ex:p0 ?a ; ex:p1 ?b }",
            Some("StarJoin"),
        ),
        (
            "SELECT DISTINCT ?a WHERE { ?s a ex:C0 ; ex:p0 ?a }",
            Some("IndexJoin"),
        ),
        (
            "SELECT ?s ?a ?b WHERE { ?s a ex:C0 ; ex:p0 ?a ; ex:p1 ?b } ORDER BY ?s ?a ?b LIMIT 7",
            Some("StarJoin"),
        ),
        // the same predicate twice: index joins, not one star
        (
            "SELECT * WHERE { ?s a ex:C0 . ?s ex:p0 ?a . ?s ex:p0 ?b }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C3 . ?s ?p ?o }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 . ?s ex:p0 ?a FILTER(?a > 3) }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 ; ex:p0 ?a FILTER EXISTS { ?s ex:p1 ?b } }",
            Some("IndexJoin"),
        ),
        (
            "SELECT * WHERE { { SELECT ?s WHERE { ?s a ex:C1 } ORDER BY ?s LIMIT 4 } ?s ex:p0 ?a ; ex:p3 ?c }",
            Some("StarJoin"),
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p1 ?b . ?b ex:p0 ?c } }",
            None,
        ),
        (
            "SELECT * WHERE { ?s a ex:C0 BIND(?s AS ?t) ?t ex:p0 ?a }",
            None,
        ),
        (
            "SELECT * WHERE { ?s a ex:C1 MINUS { ?s ex:p0 ?a ; ex:p2 ex:o2 } }",
            Some("IndexJoin"),
        ),
    ]
}

fn without() -> Optimizations {
    Optimizations {
        batched_join: false,
        star_fusion: false,
        ..Optimizations::ALL
    }
}

fn run(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> QueryResult {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    let text = format!("{PREFIXES}{q}");
    query(snap.clone(), &text, &opts).unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// `q` forced into index joins, with fused stars read by subject walks (`Some(true)`),
/// per pattern (`Some(false)`) or by cost.
fn forced(
    snap: &Arc<crate::store::Snapshot>,
    q: &str,
    opt: Optimizations,
    walk: Option<bool>,
) -> QueryResult {
    FORCE_INDEX_JOIN.with(|f| f.set(true));
    FORCE_WALK.with(|f| f.set(walk));
    let r = run(snap, q, opt);
    FORCE_INDEX_JOIN.with(|f| f.set(false));
    FORCE_WALK.with(|f| f.set(None));
    r
}

fn rows(r: &QueryResult) -> Vec<String> {
    r.rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The shape whose solutions are compared in order (it orders by every variable).
const ORDERED: &str = "ORDER BY ?s ?a ?b LIMIT";

/// Solutions as a multiset, or in order under a total ORDER BY.
fn answer(q: &str, r: &QueryResult) -> Vec<String> {
    let mut v = rows(r);
    if !q.contains(ORDERED) {
        v.sort();
    }
    v
}

fn has_op(p: &PlanInfo, op: &str) -> bool {
    p.operator == op || p.children.iter().any(|c| has_op(c, op))
}

fn find<'a>(p: &'a PlanInfo, op: &str) -> Option<&'a PlanInfo> {
    if p.operator == op {
        return Some(p);
    }
    p.children.iter().find_map(|c| find(c, op))
}

/// Every shape over a snapshot: the same answer with the operators off, forced (read
/// by walks and per pattern, fused or not) and chosen by cost. Returns how many forced
/// plans lack their expected operator (a pattern whose constant the store lacks is
/// planned as empty, and so is a MINUS over it).
fn check_snapshot(snap: &Arc<crate::store::Snapshot>, label: &str) -> usize {
    let mut missed = 0;
    for (q, op) in shapes() {
        let base = run(snap, q, without());
        assert!(!has_op(&base.plan, "IndexJoin") && !has_op(&base.plan, "StarJoin"));
        let want = answer(q, &base);
        let unfused = Optimizations {
            star_fusion: false,
            ..Optimizations::ALL
        };
        for (how, r) in [
            ("by cost", run(snap, q, Optimizations::ALL)),
            ("walks", forced(snap, q, Optimizations::ALL, Some(true))),
            (
                "per pattern",
                forced(snap, q, Optimizations::ALL, Some(false)),
            ),
            ("forced", forced(snap, q, Optimizations::ALL, None)),
            ("unfused", forced(snap, q, unfused, None)),
            ("generic", run(snap, q, Optimizations::NONE)),
        ] {
            assert_eq!(answer(q, &r), want, "{label}, {how}: {q}");
            if how == "unfused" {
                assert!(!has_op(&r.plan, "StarJoin"), "{label}: {q}");
            }
            if let ("walks" | "per pattern", Some(op)) = (how, op)
                && !has_op(&r.plan, op)
            {
                missed += 1;
                tracing::info!(target: "sparkles::sparql::indexjoin_tests", "{label}, {how}: {q} lacks {op}: {:#?}", r.plan);
            }
        }
    }
    missed
}

#[test]
fn index_joins_and_stars_match_the_generic_joins_on_random_data() {
    for (round, union) in [(0u64, false), (1, true), (2, false)] {
        let s = Store::in_memory(StoreOptions {
            union_default_graph: union,
            ..Default::default()
        });
        load(
            &s,
            &random_trig(0x9e37_79b9_7f4a_7c15 ^ round, 60 + 70 * round as usize),
        );
        let before = s.snapshot();
        assert_eq!(check_snapshot(&before, &format!("round {round}")), 0);
        let answers: Vec<Vec<String>> = shapes()
            .iter()
            .map(|(q, _)| answer(q, &run(&before, q, without())))
            .collect();
        for u in UPDATES {
            update(&s, u);
        }
        assert_eq!(
            check_snapshot(&s.snapshot(), &format!("round {round} after updates")),
            0
        );
        // the state before the updates still answers as before, forced or not
        for ((q, _), want) in shapes().iter().zip(&answers) {
            assert_eq!(
                &answer(q, &forced(&before, q, Optimizations::ALL, Some(true))),
                want,
                "round {round}, past state: {q}"
            );
        }
    }
}

/// A store made by updates alone (nothing but delta), and a persistent one read at a
/// past commit and after compaction.
#[test]
fn index_joins_read_the_delta_and_past_commits() {
    let s = Store::in_memory(StoreOptions::default());
    for u in UPDATES {
        update(&s, u);
    }
    update(
        &s,
        "INSERT DATA { ex:s1 a ex:C0 ; ex:p0 ex:s2 , 5 ; ex:p1 ex:o1 . ex:s2 a ex:C0 ; ex:p0 1 ; ex:p1 2 , 3 }",
    );
    // most shapes name terms these updates never wrote
    assert!(check_snapshot(&s.snapshot(), "delta only") < shapes().len());

    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    load(&s, &random_trig(0x2545_f491_4f6c_dd1d, 80));
    let loaded = s.head_commit().seq;
    for u in UPDATES {
        update(&s, u);
    }
    assert_eq!(check_snapshot(&s.snapshot(), "persistent"), 0);
    let past = |s: &Store| {
        let (snap, r) = s
            .snapshot_at(&crate::history::At::Commit(loaded + 1), &Default::default())
            .unwrap();
        assert!(r.historical);
        snap
    };
    assert_eq!(check_snapshot(&past(&s), "past commit"), 0);
    s.compact().unwrap();
    assert_eq!(check_snapshot(&s.snapshot(), "compacted"), 0);
}

/// Wide enough data that probing is chosen by cost alone, for a selective input; for an
/// input covering most subjects the patterns are scanned and merge-joined instead.
#[test]
fn probing_is_chosen_by_selectivity() {
    let s = Store::in_memory(StoreOptions::default());
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..60_000 {
        t.push_str(&format!(
            "ex:s{i} a ex:{} ; ex:name \"n{i}\" ; ex:age {} ; ex:worksFor ex:org{} .\n",
            if i % 3000 == 7 { "Rare" } else { "Common" },
            i % 80,
            i % 500
        ));
    }
    for o in 0..500 {
        t.push_str(&format!(
            "ex:org{o} ex:city \"{}\" .\n",
            if o == 42 { "Kyoto" } else { "Elsewhere" }
        ));
    }
    load(&s, &t);
    let snap = s.snapshot();
    let check = |q: &str| {
        let fast = run(&snap, q, Optimizations::ALL);
        assert_eq!(
            answer(q, &fast),
            answer(q, &run(&snap, q, without())),
            "{q}"
        );
        fast
    };
    // a rare class: one walk or probe per pattern for its 20 subjects
    let r = check("SELECT * WHERE { ?s a ex:Rare ; ex:name ?n ; ex:age ?a }");
    assert_eq!(r.len(), 20);
    let star = find(&r.plan, "StarJoin").unwrap_or_else(|| panic!("{:#?}", r.plan));
    let c = star.counters.as_ref().unwrap();
    assert_eq!(c["keys"], 20);
    assert!(c["seeks"].as_u64().unwrap() <= 40, "{c:?}");
    assert!(c["rowsRead"].as_u64().unwrap() <= 40, "{c:?}");
    // a chain from a selective constant: org → its employees → their star
    let r = check(
        "SELECT ?s ?n ?a WHERE { ?o ex:city \"Kyoto\" . ?s ex:worksFor ?o ; ex:name ?n ; ex:age ?a }",
    );
    assert_eq!(r.len(), 120);
    assert!(has_op(&r.plan, "IndexJoin") || has_op(&r.plan, "StarJoin"));
    // VALUES drive the star
    let r = check(
        "SELECT * WHERE { VALUES ?s { ex:s1 ex:s2 ex:s2 ex:s59999 ex:none } ?s ex:name ?n ; ex:age ?a }",
    );
    assert_eq!(r.len(), 4);
    assert!(has_op(&r.plan, "StarJoin"));
    // every subject: scanning the patterns is cheaper
    let r = check("SELECT (COUNT(*) AS ?c) WHERE { ?s a ex:Common ; ex:name ?n ; ex:age ?a }");
    assert!(!has_op(&r.plan, "IndexJoin") && !has_op(&r.plan, "StarJoin"));
    // switched off per query
    let off = Optimizations::ALL.disable("batched_join").unwrap();
    let r = run(
        &snap,
        "SELECT * WHERE { ?s a ex:Rare ; ex:name ?n ; ex:age ?a }",
        off,
    );
    assert!(!has_op(&r.plan, "IndexJoin") && !has_op(&r.plan, "StarJoin"));
    let off = Optimizations::ALL.disable("star_fusion").unwrap();
    let r = run(
        &snap,
        "SELECT * WHERE { ?s a ex:Rare ; ex:name ?n ; ex:age ?a }",
        off,
    );
    assert!(has_op(&r.plan, "IndexJoin") && !has_op(&r.plan, "StarJoin"));
    // EXPLAIN names the operator before running it
    let opts = QueryOptions::default();
    let (_, plan) = explain(
        snap.clone(),
        &format!("{PREFIXES}SELECT * WHERE {{ ?s a ex:Rare ; ex:name ?n ; ex:age ?a }}"),
        &opts,
    )
    .unwrap();
    let star = find(&plan, "StarJoin").unwrap_or_else(|| panic!("{plan:#?}"));
    assert!(
        star.description.contains("ex.org/name"),
        "{}",
        star.description
    );
    // LIMIT reads the input in growing steps
    let q = "SELECT * WHERE { ?s ex:worksFor ?o ; ex:name ?n } LIMIT 5";
    FORCE_INDEX_JOIN.with(|f| f.set(true));
    let r = run(&snap, q, Optimizations::ALL);
    FORCE_INDEX_JOIN.with(|f| f.set(false));
    assert_eq!(r.len(), 5);
    let all = rows(&run(
        &snap,
        "SELECT * WHERE { ?s ex:worksFor ?o ; ex:name ?n }",
        without(),
    ));
    assert!(rows(&r).iter().all(|x| all.contains(x)));
}

/// Input rows keep their order (an operator above may rely on it) and every duplicate
/// of a key gets its own copy of the key's rows.
#[test]
fn index_joins_keep_input_order_and_multiplicity() {
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        "@prefix ex: <http://ex.org/> . ex:a ex:p 1 , 2 ; ex:q 9 . ex:b ex:p 3 ; ex:q 8 , 7 . ex:c ex:q 6 .",
    );
    let snap = s.snapshot();
    let int = |n: i64| super::plan::lit_int(n).to_string();
    let q = "SELECT ?s ?x ?y WHERE { VALUES ?s { ex:b ex:a ex:c ex:b } ?s ex:p ?x ; ex:q ?y }";
    for walk in [Some(true), Some(false)] {
        let r = forced(&snap, q, Optimizations::ALL, walk);
        assert!(has_op(&r.plan, "StarJoin"), "{:#?}", r.plan);
        assert_eq!(
            rows(&r),
            // a key's rows come in index order (inline integers by value)
            [
                ("b", 3, 7),
                ("b", 3, 8),
                ("a", 1, 9),
                ("a", 2, 9),
                ("b", 3, 7),
                ("b", 3, 8)
            ]
            .map(|(s, x, y)| format!("<http://ex.org/{s}> {} {}", int(x), int(y)))
        );
    }
}

/// An input row whose key is unbound (the planner only admits keys the input always
/// binds) joins with every row of the patterns, its key taking their value.
#[test]
fn unbound_keys_join_every_row() {
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        "@prefix ex: <http://ex.org/> . ex:a ex:p 1 , 2 ; ex:q 9 . ex:b ex:p 3 ; ex:q 8 , 7 .",
    );
    let ctx = Ctx::new(s.snapshot());
    let int = |n: i64| ctx.intern_term(&super::plan::lit_int(n));
    let iri = |l: &str| {
        ctx.intern_term(&oxrdf::NamedNode::new_unchecked(format!("http://ex.org/{l}")).into())
    };
    for (q, want) in [
        (
            "SELECT * WHERE { VALUES ?s { ex:a } ?s ex:p ?x }",
            vec![
                vec![iri("a"), int(1)],
                vec![iri("a"), int(2)],
                vec![iri("b"), int(3)],
                vec![iri("b"), int(3)],
                vec![iri("a"), int(1)],
                vec![iri("a"), int(2)],
                vec![iri("b"), int(3)],
            ],
        ),
        (
            "SELECT * WHERE { VALUES ?s { ex:a } ?s ex:p ?x ; ex:q ?y }",
            vec![
                vec![iri("a"), int(1), int(9)],
                vec![iri("a"), int(2), int(9)],
                vec![iri("b"), int(3), int(7)],
                vec![iri("b"), int(3), int(8)],
                vec![iri("b"), int(3), int(7)],
                vec![iri("b"), int(3), int(8)],
                vec![iri("a"), int(1), int(9)],
                vec![iri("a"), int(2), int(9)],
                vec![iri("b"), int(3), int(7)],
                vec![iri("b"), int(3), int(8)],
            ],
        ),
    ] {
        let parsed = parse_query(&format!("{PREFIXES}{q}"), None, &[]).unwrap();
        let (pattern, _, _) = split(&parsed);
        FORCE_INDEX_JOIN.with(|f| f.set(true));
        let node = plan::Planner::new(&ctx)
            .plan(pattern, &plan::ActiveGraph::Default, Vec::new())
            .unwrap();
        FORCE_INDEX_JOIN.with(|f| f.set(false));
        fn index_join(n: &plan::Node) -> Option<&plan::Node> {
            match n.kind {
                plan::Kind::IndexJoin(_) => Some(n),
                _ => n.children.iter().find_map(index_join),
            }
        }
        let n = index_join(&node).unwrap();
        let plan::Kind::IndexJoin(spec) = &n.kind else {
            unreachable!()
        };
        let mut left = Table::new(vec![spec.key]);
        for id in [Id::UNDEF, iri("b"), Id::UNDEF] {
            left.push_row(&[id]);
        }
        let (t, stats) = super::indexjoin::run(&ctx, spec, &left, &n.vars).unwrap();
        assert_eq!(stats.mode, "all rows");
        let got: Vec<Vec<Id>> = (0..t.len()).map(|i| t.row(i)).collect();
        assert_eq!(got, want, "{q}");
    }
}

/// Keys in separate blocks are read by separate seeks, keys in one block by one, and a
/// delta change between two keys of one block splits their scan; the rows merged from
/// the delta are those a plain scan sees.
#[test]
fn keys_are_read_by_clustered_seeks() {
    let s = Store::in_memory(StoreOptions::default());
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..50_000 {
        t.push_str(&format!(
            "ex:s{i:05} a ex:C ; ex:p0 {} ; ex:p1 {} .\n",
            i % 97,
            i % 13
        ));
    }
    load(&s, &t);
    let q = "SELECT * WHERE { VALUES ?s { ex:s00010 ex:s00500 ex:s00011 ex:s25000 ex:s49990 } \
             ?s ex:p0 ?a ; ex:p1 ?b }";
    let seeks = |snap: &Arc<crate::store::Snapshot>, walk: bool, opt: Optimizations| {
        let r = forced(snap, q, opt, Some(walk));
        assert_eq!(answer(q, &r), answer(q, &run(snap, q, without())), "{walk}");
        let star = find(&r.plan, "StarJoin").unwrap_or_else(|| panic!("{:#?}", r.plan));
        let c = star.counters.clone().unwrap();
        assert_eq!(c["keys"], 5);
        (r.len(), c["seeks"].as_u64().unwrap())
    };
    let scans = Optimizations {
        gallop_index_join: false,
        ..Optimizations::ALL
    };
    let snap = s.snapshot();
    assert!(snap.perm(crate::index::Perm::Spo).blocks.len() >= 4);
    // three subject regions of SPO, read by three scans or found by three searches
    for opt in [scans, Optimizations::ALL] {
        assert_eq!(seeks(&snap, true, opt), (5, 3));
        let (n, per_pattern) = seeks(&snap, false, opt);
        assert_eq!(n, 5);
        assert!(per_pattern >= 2, "{per_pattern}");
    }
    // a change between ex:s00011 and ex:s00500 splits their scan
    update(
        &s,
        "INSERT DATA { ex:s00200 ex:p0 1000 . ex:s00500 ex:p1 1000 } ; DELETE DATA { ex:s25000 ex:p0 71 }",
    );
    let snap = s.snapshot();
    assert_eq!(seeks(&snap, true, scans), (5, 4));
    seeks(&snap, false, scans);
    // the galloping reader reads the two ranges with changes (`ex:s00500 ex:p1` and
    // `ex:s25000 ex:p0`) by scans of their own, then searches for the block of the
    // range after each
    assert_eq!(seeks(&snap, true, Optimizations::ALL), (5, 5));
    seeks(&snap, false, Optimizations::ALL);
}

/// The rows of `q` read through a cursor with `rows` per batch, planned with forced
/// index joins, with the cursor's plan and its largest batch.
fn cursor_rows(
    snap: &Arc<crate::store::Snapshot>,
    q: &str,
    rows: usize,
) -> (Vec<String>, super::cursor::CursorPlan, usize) {
    FORCE_INDEX_JOIN.with(|f| f.set(true));
    let opened = super::cursor::select_cursor(
        snap.clone(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
        &super::cursor::CursorOptions {
            batch_rows: rows,
            ..Default::default()
        },
    );
    FORCE_INDEX_JOIN.with(|f| f.set(false));
    let mut c = opened.unwrap_or_else(|e| panic!("{q}: {e}"));
    let plan = c.plan().clone();
    let mut out = Vec::new();
    let mut largest = 0;
    while let Some(b) = c.next_batch().unwrap() {
        largest = largest.max(b.len());
        for i in 0..b.len() {
            out.push(
                b.row(i)
                    .unwrap()
                    .into_iter()
                    .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    }
    out.sort();
    (out, plan, largest)
}

/// Whether the cursor plan has an `op` operator, clearing `streamed` when one of them
/// materializes.
fn cursor_plan_has(plan: &super::cursor::CursorPlan, op: &str, streamed: &mut bool) -> bool {
    let here = plan.operator.operator == op;
    if here && plan.materializes {
        *streamed = false;
    }
    plan.children
        .iter()
        .fold(here, |found, c| cursor_plan_has(c, op, streamed) || found)
}

/// Index joins and stars stream through cursors over each input batch, and match the
/// eager answers at every batch size, before and after updates.
#[test]
fn index_joins_stream_through_cursors() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &random_trig(0x51ee_d0c5_0000_0003, 120));
    let mut snapshots = vec![s.snapshot()];
    for u in UPDATES {
        update(&s, u);
    }
    snapshots.push(s.snapshot());
    let mut streamed_shapes = 0;
    for snap in &snapshots {
        for (q, op) in shapes() {
            let mut want = rows(&forced(snap, q, Optimizations::ALL, None));
            want.sort();
            for batch in [1, 2, 3, 4096] {
                let (got, plan, largest) = cursor_rows(snap, q, batch);
                assert!(largest <= batch, "{q}: a batch of {largest} rows");
                assert_eq!(got, want, "{batch} rows: {q}");
                if let Some(op) = op {
                    let mut streamed = true;
                    if cursor_plan_has(&plan, op, &mut streamed) {
                        assert!(streamed, "{q}: {plan:#?}");
                        streamed_shapes += 1;
                    }
                }
            }
        }
    }
    assert!(streamed_shapes > 0);
}

/// One input row whose key has many rows expands across many output batches, each
/// within the cap.
#[test]
fn an_index_join_resumes_a_large_expansion_across_batches() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..5000 {
        data.push_str(&format!("ex:hub ex:p {i} .\nex:s{i} ex:p {i} .\n"));
    }
    load(&s, &data);
    let snap = s.snapshot();
    let q = "SELECT ?s ?o WHERE { VALUES ?s { ex:s1 ex:hub ex:s2 } ?s ex:p ?o }";
    let mut want = rows(&forced(&snap, q, Optimizations::ALL, None));
    want.sort();
    assert_eq!(want.len(), 5002);
    for batch in [7, 4096] {
        let (got, plan, largest) = cursor_rows(&snap, q, batch);
        let mut streamed = true;
        assert!(
            cursor_plan_has(&plan, "IndexJoin", &mut streamed),
            "{plan:#?}"
        );
        assert!(streamed);
        assert!(largest <= batch);
        assert_eq!(got, want);
    }
}
