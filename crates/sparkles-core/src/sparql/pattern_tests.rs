//! Plans answered from the predicate patterns (X11 Phase 1): every answer equals the
//! generic operators' on the same snapshot, with a delta, in a transaction, with named
//! graphs and a union default graph, at past commits, on a branch, after full and partial
//! compactions, without patterns and under access views.

use super::opt_tests::{PREFIXES, has_op, load, solutions, update};
use super::*;
use crate::io::RdfFormat;
use crate::patterns::Scope;
use crate::store::{CompactOptions, PartialMode, Snapshot, Store, StoreOptions};
use std::sync::Arc;

const SPK: &str = "PREFIX spk: <urn:x-sparkles:> PREFIX ql: <http://qlever.cs.uni-freiburg.de/builtin-functions/> ";

fn opts(opt: Optimizations) -> QueryOptions {
    QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    }
}

fn run_on(snap: &Arc<Snapshot>, text: &str, o: &QueryOptions) -> QueryResult {
    let text = format!("{PREFIXES}{SPK}{text}");
    query(snap.clone(), &text, o).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// The answer of `text` on `snap` with every optimization, which must equal the generic
/// operators' answer; `uses` says whether the plan must read the patterns (`None`: not
/// checked).
fn same_on(snap: &Arc<Snapshot>, text: &str, uses: Option<bool>) -> Vec<String> {
    let fast = run_on(snap, text, &opts(Optimizations::ALL));
    let slow = run_on(snap, text, &opts(Optimizations::NONE));
    assert_eq!(solutions(&fast), solutions(&slow), "{text}");
    let used = has_op(&fast.plan, "PatternCount") || has_op(&fast.plan, "PatternScan");
    if let Some(u) = uses {
        assert_eq!(used, u, "{text}: {:#?}", fast.plan);
    }
    assert!(!has_op(&slow.plan, "PatternCount") && !has_op(&slow.plan, "PatternScan"));
    solutions(&fast)
}

fn same(s: &Store, text: &str, uses: Option<bool>) -> Vec<String> {
    same_on(&s.snapshot(), text, uses)
}

/// The shapes the patterns answer, and some near misses they must not change.
const QUERIES: &[(&str, bool)] = &[
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ex:Film . ?s ?p ?o } GROUP BY ?p",
        true,
    ),
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        true,
    ),
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p [] } GROUP BY ?p ORDER BY DESC(?n) ?p",
        true,
    ),
    (
        "SELECT ?p WHERE { ?s a ex:Film ; ex:year ?y . ?s ?p ?o } GROUP BY ?p",
        true,
    ),
    (
        "SELECT ?p (COUNT(?s) AS ?n) (COUNT(*) AS ?m) WHERE { ?s a ex:Film . ?s spk:hasPredicate ?p } GROUP BY ?p",
        true,
    ),
    (
        "SELECT ?p (COUNT(?s) AS ?n) WHERE { ?s ex:knows ?x . ?s spk:hasPredicate ?p } GROUP BY ?p",
        true,
    ),
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ql:has-predicate ?p } GROUP BY ?p",
        true,
    ),
    ("SELECT ?s ?p WHERE { ?s spk:hasPredicate ?p }", true),
    ("SELECT DISTINCT ?p WHERE { ?s a ex:Film . ?s ?p ?o }", true),
    ("SELECT DISTINCT ?p WHERE { ?s ?p ?o }", true),
    ("SELECT DISTINCT ?p WHERE { ex:f9 ?p ?o }", false),
    ("SELECT ?s WHERE { ?s spk:hasPredicate ex:p3 }", false),
    ("SELECT ?p WHERE { ex:f7 spk:hasPredicate ?p }", true),
    ("ASK { ex:f7 spk:hasPredicate ex:p1 }", false),
    (
        "SELECT ?s ?p WHERE { ?s a ex:Film . ?s ql:has-predicate ?p } ORDER BY ?s ?p",
        true,
    ),
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s a ex:Film . ?s ?p ?o } } GROUP BY ?p",
        true,
    ),
    (
        "SELECT ?s ?p ?g WHERE { GRAPH ?g { ?s spk:hasPredicate ?p } }",
        false,
    ),
    // triples per predicate, not subjects: the table does not know
    (
        "SELECT ?p (COUNT(*) AS ?n) WHERE { ?s a ex:Film . ?s ?p ?o } GROUP BY ?p",
        false,
    ),
    // ?o is used elsewhere
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o . ?o a ex:Film } GROUP BY ?p",
        false,
    ),
    (
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH ex:g { ?s ?p ?o } } GROUP BY ?p",
        false,
    ),
];

/// Films with subsets of five predicates, people, and a named graph holding `ex:p9`.
fn films() -> String {
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3000 {
        ttl.push_str(&format!("ex:f{i} a ex:Film .\n"));
        for b in 0..5 {
            if (i >> b) & 1 == 1 || (i % 7 == b) {
                ttl.push_str(&format!("ex:f{i} ex:p{b} {} .\n", i % 13));
            }
        }
        if i % 5 == 0 {
            ttl.push_str(&format!("ex:f{i} ex:year {} .\n", 1950 + i % 70));
        }
        if i % 3 == 0 {
            ttl.push_str(&format!("ex:u{i} ex:knows ex:f{i} ; ex:name \"u{i}\" .\n"));
        }
    }
    ttl.push_str("ex:g { ex:f1 ex:p9 1 . ex:f2 ex:p9 2 . ex:z ex:p9 3 . ex:f3 ex:p0 4 . }\n");
    ttl
}

fn check_all(s: &Store) {
    for (q, uses) in QUERIES {
        same(s, q, Some(*uses));
    }
}

#[test]
fn a1_a2_a3_pattern_counts_and_has_predicate() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    let snap = s.snapshot();
    assert!(snap.generation.patterns.file.is_some());
    check_all(&s);
    // A3: ex:p9 is only in a named graph
    let dflt = same(
        &s,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        Some(true),
    );
    assert!(!dflt.iter().any(|r| r.contains("p9")), "{dflt:?}");
    let union = same(
        &s,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } } GROUP BY ?p",
        Some(true),
    );
    assert!(union.iter().any(|r| r.contains("p9")), "{union:?}");
    // the plan says what it read
    let r = run_on(
        &snap,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ex:Film . ?s ?p ?o } GROUP BY ?p",
        &opts(Optimizations::ALL),
    );
    assert!(has_op(&r.plan, "PatternCount"), "{:#?}", r.plan);
    // switched off by name
    let off = Optimizations::ALL.disable("predicate_patterns").unwrap();
    let r = run_on(
        &snap,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        &opts(off),
    );
    assert!(!has_op(&r.plan, "PatternCount"));
}

#[test]
fn union_default_graph_reads_the_named_table() {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    load(&s, &films(), RdfFormat::TriG);
    let rows = same(
        &s,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        Some(true),
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
    check_all_unchecked(&s);
}

fn check_all_unchecked(s: &Store) {
    for (q, _) in QUERIES {
        same(s, q, None);
    }
}

/// A4: the delta, before and after full and partial compactions.
#[test]
fn a4_delta_and_compactions() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    load(&s, &films(), RdfFormat::TriG);
    s.compact().unwrap();
    let q = "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ex:Film . ?s ?p ?o } GROUP BY ?p";
    let before = same(&s, q, Some(true));
    // ex:f1 has ex:p0 and ex:p1 (1 = 0b1, and 1 % 7 = 1): delete both, and give it and
    // a new film predicates no film had, with terms the vocabulary has
    update(
        &s,
        "DELETE DATA { ex:f1 ex:p0 1 . ex:f1 ex:p1 1 } ; INSERT DATA { ex:f1 ex:knows ex:f2 . ex:u3 a ex:Film }",
    );
    let after = same(&s, q, Some(true));
    assert_ne!(before, after);
    assert!(after.iter().any(|r| r.contains("knows>")), "{after:?}");
    assert!(after.iter().any(|r| r.contains("name>")), "{after:?}");
    let r = run_on(&s.snapshot(), q, &opts(Optimizations::ALL));
    assert!(
        super::opt_tests::has_desc(&r.plan, "corrected for"),
        "{:#?}",
        r.plan
    );
    check_all(&s);
    // a partial compaction rewrites the tables
    let rep = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rep.mode, "partial", "{rep:?}");
    assert_eq!(same(&s, q, Some(true)), after);
    check_all(&s);
    let gdir = s.snapshot().generation.dir.clone().unwrap();
    let (problems, _) = crate::patterns::verify(&gdir, 10).unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    // another delta over the rewritten tables, a subject losing every triple, then again
    update(
        &s,
        "DELETE WHERE { ex:f2 ?p ?o } ; INSERT DATA { ex:f5 ex:brandNew 1 . ex:n2 ex:p0 7 }",
    );
    check_all(&s);
    let rep = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    // new terms need a full build
    assert_eq!(rep.mode, "full", "{rep:?}");
    check_all(&s);
    update(
        &s,
        "DELETE WHERE { ex:f3 ?p ?o } ; INSERT DATA { ex:f6 ex:brandNew 2 }",
    );
    let rep = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rep.mode, "partial", "{rep:?}");
    check_all(&s);
    let gdir = s.snapshot().generation.dir.clone().unwrap();
    let (problems, _) = crate::patterns::verify(&gdir, 10).unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    // reopened, the tables are read again
    drop(s);
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    assert!(s.snapshot().generation.patterns.file.is_some());
    check_all(&s);
}

/// Uncommitted changes in a transaction: a later operation of one update sees an
/// earlier one's changes through the corrected tables.
#[test]
fn a_transaction_sees_its_own_changes() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    update(
        &s,
        "INSERT DATA { ex:f10 ex:fresh 1 } ; \
         INSERT { ex:result ex:count ?n } WHERE { \
           SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { \
             { SELECT ?p (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p } \
             FILTER(?p = ex:fresh) BIND(ex:f10 AS ?s) } }",
    );
    let rows = same(&s, "SELECT ?n WHERE { ex:result ex:count ?n }", Some(false));
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains('1'), "{rows:?}");
    // and the same shape inside a write transaction's own snapshot
    let head = s.snapshot();
    let mut t = s.write();
    t.insert([
        head.lookup_iri("http://ex.org/f11").unwrap(),
        head.lookup_iri("http://ex.org/p0").unwrap(),
        crate::id::Id::from_i64(99).unwrap(),
        crate::id::Id::DEFAULT_GRAPH,
    ])
    .unwrap();
    let snap = Arc::new(t.view());
    same_on(
        &snap,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        Some(true),
    );
    drop(t);
}

/// Past commits (`at=`) and a branch.
#[test]
fn history_and_branches() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            history_max_generations: 4,
            ..Default::default()
        },
    )
    .unwrap();
    load(&s, &films(), RdfFormat::TriG);
    update(
        &s,
        "DELETE WHERE { ex:f4 ?p ?o } ; INSERT DATA { ex:f9 ex:late 1 }",
    );
    let c1 = s.head_commit().seq;
    update(&s, "INSERT DATA { ex:f9 ex:later 1 }");
    let c2 = s.head_commit().seq;
    update(&s, "DELETE WHERE { ex:f8 ?p ?o }");
    for c in [c1, c2] {
        let (snap, _) = s
            .snapshot_at(
                &crate::history::At::Commit(c),
                &crate::history::HistoryOptions::default(),
            )
            .unwrap();
        assert!(snap.historical);
        for (q, _) in QUERIES {
            same_on(&snap, q, None);
        }
    }
    s.compact().unwrap();
    s.create_branch("dev", &crate::branch::BranchOptions::default())
        .unwrap();
    let dev = s.branch("dev").unwrap();
    update(
        &dev,
        "DELETE WHERE { ex:f5 ?p ?o } ; INSERT DATA { ex:f6 ex:onDev 1 }",
    );
    update(&s, "INSERT DATA { ex:f6 ex:onMain 1 }");
    let rows = same_on(
        &dev.snapshot(),
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        Some(true),
    );
    assert!(rows.iter().any(|r| r.contains("onDev")));
    assert!(!rows.iter().any(|r| r.contains("onMain")));
    for (q, uses) in QUERIES {
        same_on(&dev.snapshot(), q, Some(*uses));
    }
}

/// A5: under a protection that hides a predicate, the plan does not read the patterns
/// and the answer does not list the predicate.
#[test]
fn a5_hidden_predicates_are_not_listed() {
    use crate::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    let protection = Protection {
        name: "hide-p3".into(),
        predicates: Some(vec!["http://ex.org/p3".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let rules = TripleRules {
        rules: vec![Rule {
            protection: Arc::new(protection),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: Caller::default(),
        limits: Limits::default(),
    };
    let o = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            rules,
        ))),
        no_cache: true,
        ..Default::default()
    };
    for q in [
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ex:Film . ?s ?p ?o } GROUP BY ?p",
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        "SELECT ?s ?p WHERE { ?s spk:hasPredicate ?p }",
    ] {
        let r = run_on(&s.snapshot(), q, &o);
        assert!(!has_op(&r.plan, "PatternCount") && !has_op(&r.plan, "PatternScan"));
        let rows = solutions(&r);
        assert!(!rows.is_empty());
        assert!(!rows.iter().any(|r| r.contains("p3>")), "{q}: {rows:?}");
    }
}

/// A6: a build over the patterns' budget has none, says why, and answers the same.
#[test]
fn a6_budget_and_stores_without_patterns() {
    let mut o = StoreOptions::default();
    o.build.pattern_bytes = 1024;
    let s = Store::in_memory(o);
    load(&s, &films(), RdfFormat::TriG);
    let snap = s.snapshot();
    assert!(snap.generation.patterns.file.is_none());
    let why = snap.generation.meta.patterns.as_ref().unwrap();
    assert!(
        why.skipped.as_deref().unwrap().contains("budget"),
        "{why:?}"
    );
    for (q, _) in QUERIES {
        same(&s, q, Some(false));
    }
    // built with patterns off
    let mut o = StoreOptions::default();
    o.build.patterns = false;
    let s = Store::in_memory(o);
    load(&s, &films(), RdfFormat::TriG);
    assert!(s.snapshot().generation.patterns.file.is_none());
    for (q, _) in QUERIES {
        same(&s, q, Some(false));
    }
}

/// A delta that touches more subjects than the correction allows runs the scan.
#[test]
fn a_large_delta_falls_back() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    let mut ins = String::new();
    for i in 0..1500 {
        ins.push_str(&format!("ex:d{i} ex:p0 {i} . "));
    }
    update(&s, &format!("INSERT DATA {{ {ins} }}"));
    // 1500 touched subjects of about 18,000 quads: past the limit of 1024
    same(
        &s,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        Some(false),
    );
    check_all_unchecked(&s);
}

/// Random data and deltas, against the generic operators.
#[test]
fn random_differential() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for round in 0..6 {
        let s = Store::in_memory(StoreOptions {
            union_default_graph: round % 3 == 2,
            ..Default::default()
        });
        let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
        for _ in 0..2500 {
            let (a, p, o) = (next(400), next(9), next(30));
            match next(6) {
                0 => trig.push_str(&format!(
                    "ex:g{} {{ ex:s{a} ex:p{p} ex:o{o} . }}\n",
                    next(3)
                )),
                1 => trig.push_str(&format!("ex:s{a} a ex:C{} .\n", next(3))),
                _ => trig.push_str(&format!("ex:s{a} ex:p{p} ex:o{o} .\n")),
            }
        }
        load(&s, &trig, RdfFormat::TriG);
        for step in 0..3 {
            let mut ins = String::new();
            let mut del = String::new();
            for _ in 0..40 {
                let (a, p, o) = (next(450), next(11), next(30));
                let t = if next(4) == 0 {
                    format!("GRAPH ex:g{} {{ ex:s{a} ex:p{p} ex:o{o} }} ", next(3))
                } else {
                    format!("ex:s{a} ex:p{p} ex:o{o} . ")
                };
                if next(2) == 0 {
                    ins.push_str(&t);
                } else {
                    del.push_str(&t);
                }
            }
            update(
                &s,
                &format!("INSERT DATA {{ {ins} }} ; DELETE DATA {{ {del} }}"),
            );
            update(&s, &format!("DELETE WHERE {{ ex:s{} ?p ?o }}", next(400)));
            for q in [
                "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
                "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ex:C1 . ?s ?p ?o } GROUP BY ?p",
                "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } } GROUP BY ?p",
                "SELECT ?p (COUNT(?s) AS ?n) WHERE { ?s ex:p1 ?x . ?s spk:hasPredicate ?p } GROUP BY ?p",
                "SELECT ?s ?p WHERE { ?s spk:hasPredicate ?p }",
                "SELECT ?s WHERE { ?s spk:hasPredicate ex:p4 }",
                "SELECT ?p WHERE { ex:s7 spk:hasPredicate ?p }",
            ] {
                same(&s, q, None);
            }
            if step == 1 {
                let r = s
                    .compact_with(&CompactOptions {
                        partial: Some(PartialMode::Always),
                        ..Default::default()
                    })
                    .unwrap();
                let _ = r;
                if let Some(d) = s.snapshot().generation.dir.clone()
                    && s.snapshot().generation.patterns.file.is_some()
                {
                    let (problems, _) = crate::patterns::verify(&d, 10).unwrap();
                    assert!(problems.is_empty(), "round {round}: {problems:?}");
                }
            }
        }
    }
}

#[test]
fn both_tables_are_built() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    let snap = s.snapshot();
    let f = snap.generation.patterns.file.clone().unwrap();
    let info = snap.generation.meta.patterns.clone().unwrap();
    assert_eq!(info.format, crate::patterns::VERSION);
    assert_eq!(info.subjects[1], 4, "{info:?}");
    assert_eq!(f.table(Scope::Named).unwrap().subjects(), 4);
    assert!(f.table(Scope::Default).unwrap().patterns() > 10);
}

/// Two counts that differ only in what they count are cached apart.
#[test]
fn the_result_cache_keeps_the_measures_apart() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    let cached = QueryOptions::default();
    let rows =
        "SELECT ?p (COUNT(?s) AS ?n) WHERE { ?s ?q ?v . ?s spk:hasPredicate ?p } GROUP BY ?p";
    let subjects = "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?q ?v . ?s spk:hasPredicate ?p } GROUP BY ?p";
    let a = solutions(&run_on(&s.snapshot(), rows, &cached));
    let b = solutions(&run_on(&s.snapshot(), subjects, &cached));
    assert_ne!(a, b);
    assert_eq!(a, same(&s, rows, Some(true)));
    assert_eq!(b, same(&s, subjects, Some(true)));
    assert_eq!(a, solutions(&run_on(&s.snapshot(), rows, &cached)));
}

/// Review round 1: shapes the rewrite must refuse or keep exact.
#[test]
fn near_misses_of_the_review() {
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        "@prefix ex: <http://ex.org/> .\n\
         ex:a ex:p 1 ; ex:q <<( ex:z ex:r ex:v )>> . ex:p ex:p 2 . ex:b ex:q 2 .\n\
         ex:a ex:p ex:z .",
        RdfFormat::Turtle,
    );
    // the subject is the grouped predicate: only ex:p ex:p 2 matches
    let rows = same(
        &s,
        "SELECT ?p (COUNT(DISTINCT ?p) AS ?n) WHERE { ?p ?p ?o } GROUP BY ?p",
        Some(false),
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    // ?o inside a triple term elsewhere joins
    let rows = same(
        &s,
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o . ?s ex:q <<( ?o ex:r ex:v )>> } GROUP BY ?p",
        Some(false),
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    // a triple term is never a predicate nor a subject
    for q in [
        "SELECT ?x WHERE { ex:a spk:hasPredicate <<( ?x ex:r ex:v )>> }",
        "SELECT ?x WHERE { <<( ?x ex:r ex:v )>> spk:hasPredicate ?p }",
        "SELECT ?p WHERE { ex:a spk:hasPredicate <<( ex:z ex:r ex:v )>> }",
    ] {
        assert!(same(&s, q, Some(false)).is_empty(), "{q}");
    }
    // a substituted object restricts the triples
    let text = format!(
        "{PREFIXES}{SPK}SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s ?p ?o }} GROUP BY ?p"
    );
    let bound = |opt| QueryOptions {
        initial_bindings: vec![("o".into(), oxrdf::Term::Literal(oxrdf::Literal::from(1)))],
        ..opts(opt)
    };
    let fast = query(s.snapshot(), &text, &bound(Optimizations::ALL)).unwrap();
    let slow = query(s.snapshot(), &text, &bound(Optimizations::NONE)).unwrap();
    assert_eq!(solutions(&fast), solutions(&slow));
    assert_eq!(solutions(&fast).len(), 1, "{:?}", solutions(&fast));
    assert!(!has_op(&fast.plan, "PatternCount"));
}

/// A pattern scan admits its output against the memory budget, and stops at a LIMIT.
#[test]
fn pattern_scans_are_budgeted_and_limited() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    let text = format!("{PREFIXES}{SPK}SELECT ?s ?p WHERE {{ ?s spk:hasPredicate ?p }}");
    let tight = QueryOptions {
        max_memory_bytes: Some(64 * 1024),
        ..opts(Optimizations::ALL)
    };
    match query(s.snapshot(), &text, &tight) {
        Err(crate::error::Error::BudgetExceeded(b)) => {
            assert_eq!(b.kind, crate::error::BudgetKind::Memory, "{b:?}")
        }
        other => panic!("{:?}", other.map(|r| solutions(&r).len())),
    }
    let limited = format!("{text} LIMIT 3");
    let r = query(s.snapshot(), &limited, &opts(Optimizations::ALL)).unwrap();
    assert_eq!(solutions(&r).len(), 3);
    assert!(has_op(&r.plan, "PatternScan"), "{:#?}", r.plan);
    assert!(
        super::opt_tests::has_desc(&r.plan, "stopped early"),
        "{:#?}",
        r.plan
    );
}

/// Strict cursors run the counts natively and keep the streaming scan for pairs.
#[test]
fn cursors_run_pattern_plans_without_materializing() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &films(), RdfFormat::TriG);
    update(&s, "INSERT DATA { ex:f1 ex:knows ex:f2 }");
    let rows = |text: &str, opt: Optimizations, fallback| -> (Vec<String>, String) {
        let text = format!("{PREFIXES}{SPK}{text}");
        let c = CursorOptions {
            batch_rows: 7,
            fallback,
            ..Default::default()
        };
        let mut cur = select_cursor(s.snapshot(), &text, &opts(opt), &c)
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        let mut out = Vec::new();
        while let Some(b) = cur.next_batch().unwrap() {
            for i in 0..b.len() {
                out.push(format!("{:?}", b.row(i).unwrap()));
            }
        }
        out.sort();
        (out, cur.plan_json().unwrap())
    };
    for q in [
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p",
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ex:Film . ?s ?p ?o } GROUP BY ?p",
        "SELECT DISTINCT ?p WHERE { ?s a ex:Film . ?s ?p ?o }",
    ] {
        let (fast, plan) = rows(q, Optimizations::ALL, FallbackPolicy::RejectMaterialization);
        assert!(plan.contains("PatternCount"), "{q}: {plan}");
        let (slow, _) = rows(q, Optimizations::NONE, FallbackPolicy::AllowMaterialization);
        assert_eq!(fast, slow, "{q}");
    }
    let q = "SELECT ?s ?p WHERE { ?s spk:hasPredicate ?p } LIMIT 5";
    let (fast, plan) = rows(q, Optimizations::ALL, FallbackPolicy::AllowMaterialization);
    assert!(!plan.contains("PatternScan"), "{plan}");
    assert_eq!(fast.len(), 5);
}

/// A partial compaction with patterns turned off writes none.
#[test]
fn partial_compaction_honours_patterns_off() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let s = Store::open(&db, StoreOptions::default()).unwrap();
    load(&s, &films(), RdfFormat::TriG);
    s.compact().unwrap();
    assert!(s.snapshot().generation.patterns.file.is_some());
    drop(s);
    let mut o = StoreOptions::default();
    o.build.patterns = false;
    let s = Store::open(&db, o).unwrap();
    update(&s, "INSERT DATA { ex:f1 ex:knows ex:f2 }");
    let rep = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rep.mode, "partial", "{rep:?}");
    let snap = s.snapshot();
    assert!(snap.generation.patterns.file.is_none());
    let gdir = snap.generation.dir.clone().unwrap();
    assert!(!gdir.join(crate::patterns::FILE).exists());
    let why = snap.generation.meta.patterns.as_ref().unwrap();
    assert!(
        why.skipped.as_deref().unwrap().contains("turned off"),
        "{why:?}"
    );
    check_all_unchecked(&s);
}

/// Review round 2: row limits count output rows, not reserved capacity, and a native
/// pattern count in a cursor checks its rows as the eager executor does.
#[test]
fn row_limits_count_rows() {
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..5000 {
        nt.push_str(&format!("<urn:s{i}> <urn:p0> 1 .\n"));
    }
    for i in 0..5000 {
        nt.push_str(&format!("<urn:one> <urn:q{i}> 1 .\n"));
    }
    nt.push_str("<urn:one> <urn:p0> 2 .\n");
    load(&s, &nt, RdfFormat::Turtle);
    // 5,001 subjects with ex:p0 or more: the pairs of the p0 subjects alone are 5,001
    let text = format!("{SPK}SELECT ?s WHERE {{ ?s spk:hasPredicate ?p }} LIMIT 5000");
    let limited = QueryOptions {
        max_rows: Some(5000),
        ..opts(Optimizations::ALL)
    };
    let r = query(s.snapshot(), &text, &limited).unwrap();
    assert_eq!(solutions(&r).len(), 5000);
    assert!(has_op(&r.plan, "PatternScan"), "{:#?}", r.plan);
    // one subject with 5,001 predicates under a limit of 100 rows
    let text = format!(
        "{SPK}SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s <urn:p0> 2 . ?s ?p ?o }} GROUP BY ?p LIMIT 1"
    );
    for opt in [Optimizations::ALL, Optimizations::NONE] {
        let o = QueryOptions {
            max_rows: Some(100),
            ..opts(opt)
        };
        let c = CursorOptions {
            fallback: FallbackPolicy::AllowMaterialization,
            ..Default::default()
        };
        let res = select_cursor(s.snapshot(), &text, &o, &c).and_then(|mut cur| {
            while cur.next_batch()?.is_some() {}
            cur.plan_json()
        });
        match res {
            Err(crate::error::Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, crate::error::BudgetKind::Rows, "{b:?}")
            }
            other => panic!("{opt:?}: {other:?}"),
        }
    }
}

/// Review round 3: a limited pattern scan checks its final row count, so an OFFSET
/// cannot hide its rows past the row limit.
#[test]
fn a_limited_scan_checks_its_final_rows() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::new();
    for i in 0..6000 {
        ttl.push_str(&format!("<urn:s{i}> <urn:p> 1 .\n"));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    let text =
        format!("{SPK}SELECT ?s ?p WHERE {{ ?s spk:hasPredicate ?p }} OFFSET 4000 LIMIT 1000");
    // the scan's 5,000 rows pass the limit of 4,500 (the generic plan's intermediates
    // differ, and it answers within the limit)
    let o = QueryOptions {
        max_rows: Some(4500),
        ..opts(Optimizations::ALL)
    };
    match query(s.snapshot(), &text, &o) {
        Err(crate::error::Error::BudgetExceeded(b)) => {
            assert_eq!(b.kind, crate::error::BudgetKind::Rows, "{b:?}")
        }
        other => panic!("{:?}", other.map(|r| solutions(&r).len())),
    }
    // and within the limit it answers as the generic plan does
    let r = same(&s, &text, Some(true));
    assert_eq!(r.len(), 1000);
}
