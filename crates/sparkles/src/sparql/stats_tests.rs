//! Counts from the index statistics: each answer equals the generic operators' on a
//! store with a delta, with named graphs, with a union default graph, at past commits,
//! in a clone, after reopening and after compaction.

use super::opt_tests::{
    PREFIXES, has_desc, has_op, load, run, same_answer, same_rows, solutions, update,
};
use super::*;
use crate::io::RdfFormat;
use crate::store::{Snapshot, Store, StoreOptions};
use std::sync::Arc;

/// Every optimization but the correction of the statistics.
fn without_delta_statistics() -> Optimizations {
    Optimizations {
        delta_statistics: false,
        ..Optimizations::ALL
    }
}

#[test]
fn class_counts_from_statistics() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..5000 {
        ttl.push_str(&format!("ex:s{i} a ex:C{} .\n", i % 13));
        if i % 4 == 0 {
            ttl.push_str(&format!("ex:s{i} a ex:Extra .\n"));
        }
    }
    load(&s, &ttl, RdfFormat::Turtle);
    let q = "SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c) ?t";
    let before = same_answer(&s, q, "GroupCountFromMetadata");
    same_answer(
        &s,
        "SELECT ?t (COUNT(*) AS ?c) WHERE { ?s rdf:type ?t } GROUP BY ?t",
        "GroupCountFromMetadata",
    );
    // the delta is corrected for: a new class, a new instance, a deleted one, a class
    // losing its only instance, and a deleted and re-inserted instance
    update(
        &s,
        "INSERT DATA { ex:new a ex:C1 , ex:Fresh . ex:s1 a ex:C0 } ; \
         DELETE DATA { ex:s2 a ex:C2 . ex:new a ex:Fresh . ex:s4 a ex:C4 } ; \
         INSERT DATA { ex:s4 a ex:C4 }",
    );
    let after = same_answer(&s, q, "desc:corrected for 3 delta quads");
    assert_ne!(before, after);
    // switched off, the index runs are counted
    let r = run(&s, q, without_delta_statistics());
    assert!(!has_op(&r.plan, "GroupCountFromMetadata"));
    assert!(has_op(&r.plan, "GroupCountFromIndex"));
    assert_eq!(solutions(&r), after);
    // after compaction the statistics are exact by themselves
    s.compact().unwrap();
    let r = run(&s, q, Optimizations::ALL);
    assert!(has_desc(&r.plan, "[from statistics]"), "{:#?}", r.plan);
    assert_eq!(same_answer(&s, q, "GroupCountFromMetadata"), after);
    // named graphs: a default-graph query leaves their quads out
    update(
        &s,
        "INSERT DATA { GRAPH ex:g { ex:s1 a ex:C1 . ex:t a ex:C2 . ex:u a ex:Only } }",
    );
    s.compact().unwrap();
    same_answer(&s, q, "desc:without 3 quads of graphs not read");
    update(
        &s,
        "INSERT DATA { GRAPH ex:g { ex:v a ex:C2 } ex:w a ex:C3 }",
    );
    same_answer(&s, q, "desc:corrected for 1 delta quads");
    // a subject typed in two graphs is one instance of their union
    for q in [
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s a ?t } } GROUP BY ?t",
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { GRAPH ex:g { ?s a ?t } } GROUP BY ?t",
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { GRAPH ?g { ?s a ?t } } GROUP BY ?t",
    ] {
        same_rows(&s, q);
    }
}

#[test]
fn predicate_counts_from_statistics() {
    let s = Store::in_memory(StoreOptions::default());
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..4000 {
        trig.push_str(&format!("ex:s{i} ex:p{} ex:o{} .\n", i % 11, i % 37));
        if i % 50 == 0 {
            trig.push_str(&format!("ex:g {{ ex:s{i} ex:p{} ex:x . }}\n", i % 3));
        }
    }
    load(&s, &trig, RdfFormat::TriG);
    let q = "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p";
    same_answer(&s, q, "desc:without 80 quads of graphs not read");
    // a delete of a triple that is not there changes nothing
    update(
        &s,
        "INSERT DATA { ex:a ex:p1 ex:b . ex:a ex:pNew ex:b . GRAPH ex:g { ex:a ex:p2 ex:b } } ; \
         DELETE DATA { ex:s1 ex:p1 ex:o1 . ex:none ex:p1 ex:o1 }",
    );
    same_answer(&s, q, "desc:corrected for 3 delta quads");
    same_answer(
        &s,
        "SELECT ?p (COUNT(?o) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
        "GroupCountFromMetadata",
    );
    same_rows(
        &s,
        "SELECT ?p (COUNT(*) AS ?c) WHERE { GRAPH ex:g { ?s ?p ?o } } GROUP BY ?p",
    );
    // the union of named graphs drops a triple's repeats across graphs, which the quad
    // counts per predicate do not tell
    let union = "SELECT ?p (COUNT(*) AS ?c) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } } GROUP BY ?p";
    assert!(!has_op(
        &run(&s, union, Optimizations::ALL).plan,
        "GroupCountFromMetadata"
    ));
    same_rows(&s, union);
}

#[test]
fn distinct_counts_from_statistics() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3000 {
        // repeated objects, objects shared between predicates, literals, a self-loop
        ttl.push_str(&format!(
            "ex:s{} ex:knows ex:s{} .\n",
            i % 700,
            (i * 7) % 900
        ));
        ttl.push_str(&format!("ex:s{i} ex:name \"n{}\" .\n", i % 1100));
        if i % 9 == 0 {
            ttl.push_str(&format!("ex:s{i} ex:likes ex:s{i} .\n"));
        }
    }
    load(&s, &ttl, RdfFormat::Turtle);
    s.compact().unwrap();
    let queries = [
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:knows ?o }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ex:knows ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:name ?o }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?p) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ?p ?o }",
    ];
    let mut before = Vec::new();
    for q in queries {
        before.push(same_answer(&s, q, "CountDistinctFromMetadata"));
    }
    // a predicate that is not in the data, and repeated variables, are counted from runs
    for q in [
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:none ?o }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ex:likes ?s }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ex:knows ex:s7 }",
    ] {
        let r = run(&s, q, Optimizations::ALL);
        assert!(!has_op(&r.plan, "CountDistinctFromMetadata"), "{q}");
        same_rows(&s, q);
    }
    // the delta is corrected for: a new subject and object, a literal that is there
    // already, the deletion of a value's only quad and of one of several
    update(
        &s,
        "INSERT DATA { ex:new ex:knows ex:other . ex:new ex:name \"n1\" } ; \
         DELETE DATA { ex:s0 ex:knows ex:s0 . ex:s1 ex:name \"n1\" . ex:s9 ex:likes ex:s9 }",
    );
    let mut after = Vec::new();
    for q in queries {
        after.push(same_answer(&s, q, "desc:delta quads]"));
        let r = run(&s, q, without_delta_statistics());
        assert!(has_op(&r.plan, "CountDistinctFromIndex"), "{q}");
    }
    assert_ne!(before, after);
    // after compaction they are exact by themselves
    s.compact().unwrap();
    for (q, a) in queries.iter().zip(&after) {
        assert_eq!(&same_answer(&s, q, "CountDistinctFromMetadata"), a);
    }
    // named graphs: a default-graph query leaves out the values only they hold
    update(
        &s,
        "INSERT DATA { GRAPH ex:g { ex:s1 ex:knows ex:elsewhere . ex:t ex:name \"other\" } }",
    );
    s.compact().unwrap();
    for q in queries {
        same_answer(&s, q, "desc:without 2 quads of graphs not read");
    }
    for q in [
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ex:knows ?o } }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { GRAPH ex:g { ?s ex:knows ?o } }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { GRAPH ?g { ?s ?p ?o } }",
    ] {
        same_rows(&s, q);
    }
}

// ------------------------------------------------------------ random updates ------

/// The count queries the statistics answer, over the default graph, one named graph,
/// two, and the union of named graphs; and whole counts and count joins over the same
/// scans, which read the delta by themselves.
fn statistics_queries() -> Vec<String> {
    let bodies = [
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?p) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:knows ?o }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ex:knows ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:name ?o }",
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t",
        "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:knows ?b . ?b ex:knows ?c }",
    ];
    let mut out = Vec::new();
    for b in bodies {
        out.push(b.to_string());
        let (head, rest) = b.split_once(" WHERE ").unwrap();
        out.push(format!("{head} FROM ex:g1 WHERE {rest}"));
        out.push(format!("{head} FROM ex:g1 FROM ex:g2 WHERE {rest}"));
        let (pattern, tail) = rest.rsplit_once('}').unwrap();
        out.push(format!(
            "{head} WHERE {{ GRAPH <urn:x-arq:UnionGraph> {pattern}}} }}{tail}"
        ));
    }
    out
}

/// A random triple over a small set of terms, so that updates meet existing triples.
fn random_triple(next: &mut dyn FnMut() -> u64) -> String {
    let s = next() % 300;
    match next() % 5 {
        0 => format!("ex:s{s} a ex:C{}", next() % 9),
        1 | 2 => format!("ex:s{s} ex:knows ex:s{}", next() % 300),
        3 => format!("ex:s{s} ex:name \"n{}\"", next() % 200),
        _ => format!("ex:s{s} ex:p{} ex:s{}", next() % 4, next() % 40),
    }
}

/// The default graph with probability `default_share` percent, else one of three named
/// graphs.
fn random_graph(next: &mut dyn FnMut() -> u64, default_share: u64) -> Option<&'static str> {
    if next() % 100 < default_share {
        None
    } else {
        Some(["ex:g1", "ex:g2", "ex:g3"][(next() % 3) as usize])
    }
}

fn in_graph(t: &str, g: Option<&str>) -> String {
    match g {
        Some(g) => format!("GRAPH {g} {{ {t} }}"),
        None => t.to_string(),
    }
}

/// The answers of every statistics query on `snap` with `opt`, and for each whether its
/// plan corrected the statistics.
fn statistics_answers(snap: &Arc<Snapshot>, opt: Optimizations) -> (Vec<Vec<String>>, Vec<bool>) {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    statistics_queries()
        .iter()
        .map(|q| {
            let text = format!("{PREFIXES}{q}");
            let r = query(snap.clone(), &text, &opts).unwrap_or_else(|e| panic!("{q}: {e}"));
            let corrected =
                has_desc(&r.plan, "corrected for") || has_desc(&r.plan, "graphs not read");
            (solutions(&r), corrected)
        })
        .unzip()
}

/// Checks that the answers on `snap` equal the generic operators': with the correction
/// within its usual work limit, without a limit, and switched off. Returns the generic
/// answers and, for each query, whether its plan without a limit corrected the
/// statistics.
fn check_statistics(snap: &Arc<Snapshot>, what: &str) -> (Vec<Vec<String>>, Vec<bool>) {
    let (slow, _) = statistics_answers(snap, Optimizations::NONE);
    let (fast, _) = statistics_answers(snap, Optimizations::ALL);
    let (plain, _) = statistics_answers(snap, without_delta_statistics());
    super::stats::UNLIMITED.set(true);
    let (unlimited, corrected) = statistics_answers(snap, Optimizations::ALL);
    super::stats::UNLIMITED.set(false);
    for (i, (q, a)) in statistics_queries().iter().zip(&slow).enumerate() {
        assert_eq!(&fast[i], a, "{what}: {q}");
        assert_eq!(&plain[i], a, "{what}, without the correction: {q}");
        assert_eq!(&unlimited[i], a, "{what}, without a work limit: {q}");
    }
    (slow, corrected)
}

type Triples = Vec<(String, Option<&'static str>)>;

/// One random update: an insert of a new triple or of one that may be there already, a
/// delete of a triple that is there or was, a re-insert of a deleted triple, a delete of
/// a triple that most likely never was, or a delete and re-insert in one commit.
fn random_update(
    s: &Store,
    next: &mut dyn FnMut() -> u64,
    known: &mut Triples,
    gone: &mut Triples,
    default_share: u64,
) {
    let pick = |v: &Triples, r: u64| v[r as usize % v.len()].clone();
    let u = match next() % 10 {
        0..=2 => {
            let (t, g) = (random_triple(next), random_graph(next, default_share));
            known.push((t.clone(), g));
            format!("INSERT DATA {{ {} }}", in_graph(&t, g))
        }
        3 => {
            let (t, g) = pick(known, next());
            format!("INSERT DATA {{ {} }}", in_graph(&t, g))
        }
        4..=6 => {
            let (t, g) = pick(known, next());
            gone.push((t.clone(), g));
            format!("DELETE DATA {{ {} }}", in_graph(&t, g))
        }
        7 if !gone.is_empty() => {
            let (t, g) = pick(gone, next());
            format!("INSERT DATA {{ {} }}", in_graph(&t, g))
        }
        8 => {
            let (t, g) = (random_triple(next), random_graph(next, default_share));
            format!("DELETE DATA {{ {} }}", in_graph(&t, g))
        }
        _ => {
            let (t, g) = pick(known, next());
            let t = in_graph(&t, g);
            format!("DELETE DATA {{ {t} }} ; INSERT DATA {{ {t} }}")
        }
    };
    update(s, &u);
}

/// Random single-triple updates on a loaded store (`default_share` percent of the
/// triples in the default graph, the others in three named graphs). The answers equal
/// the generic operators' at every step, at past commits, in a clone, after reopening
/// (which replays the delta from the log) and after compaction.
fn statistics_on_random_updates(seed: u64, union_default_graph: bool, default_share: u64) {
    let mut x = seed;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let opts = || StoreOptions {
        union_default_graph,
        ..StoreOptions::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, opts()).unwrap();
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    let mut known: Triples = Vec::new();
    let mut gone: Triples = Vec::new();
    for _ in 0..4000 {
        let t = random_triple(&mut next);
        let g = random_graph(&mut next, default_share);
        match g {
            Some(g) => trig.push_str(&format!("{g} {{ {t} . }}\n")),
            None => trig.push_str(&format!("{t} .\n")),
        }
        known.push((t, g));
    }
    load(&s, &trig, RdfFormat::TriG);
    let mut history = Vec::new();
    let mut corrected = vec![false; statistics_queries().len()];
    for round in 0..10 {
        for _ in 0..(1 + round * 3) {
            random_update(&s, &mut next, &mut known, &mut gone, default_share);
        }
        let (answers, c) = check_statistics(&s.snapshot(), &format!("round {round}"));
        for (a, b) in corrected.iter_mut().zip(c) {
            *a |= b;
        }
        history.push((s.head_commit().seq, answers));
    }
    // every query the statistics answer was corrected at some step: all but the whole
    // counts and count joins, and the quads per predicate over several graphs (the union
    // default graph among them)
    for (q, c) in statistics_queries().iter().zip(&corrected) {
        let one_graph = q.contains("FROM ex:g1") && !q.contains("ex:g2")
            || !q.contains("FROM") && !q.contains("UnionGraph") && !union_default_graph;
        let answered = !q.contains("COUNT(*)") || q.contains("GROUP BY ?p") && one_graph;
        assert_eq!(*c, answered, "{q}");
    }
    let past = |s: &Store, what: &str| {
        for (seq, want) in &history {
            let (snap, _) = s
                .snapshot_at(&crate::history::At::Commit(*seq), &Default::default())
                .unwrap();
            let (fast, _) = statistics_answers(&snap, Optimizations::ALL);
            assert_eq!(&fast, want, "{what} at commit {seq}");
        }
    };
    past(&s, "past commit");
    let head = history.last().unwrap().1.clone();
    let cloned = dir.path().join("clone");
    s.clone_to(&cloned, &Default::default()).unwrap();
    let c = Store::open(&cloned, opts()).unwrap();
    assert_eq!(check_statistics(&c.snapshot(), "clone").0, head);
    drop(c);
    drop(s);
    let s = Store::open(&root, opts()).unwrap();
    assert_eq!(check_statistics(&s.snapshot(), "reopened").0, head);
    past(&s, "reopened, past commit");
    // compaction folds the delta into the statistics, and past commits are read from the
    // generation before it, which the retention window keeps; later updates are
    // corrected again
    s.set_retention(crate::history::Retention {
        keep_commits: Some(10_000),
        keep_age_ms: None,
    })
    .unwrap();
    s.compact().unwrap();
    assert_eq!(check_statistics(&s.snapshot(), "compacted").0, head);
    past(&s, "compacted, past commit");
    for _ in 0..20 {
        random_update(&s, &mut next, &mut known, &mut gone, default_share);
    }
    check_statistics(&s.snapshot(), "updated after compaction");
}

#[test]
fn statistics_match_the_runs_on_random_updates() {
    statistics_on_random_updates(0x9e37_79b9_7f4a_7c15, false, 100);
}

#[test]
fn statistics_match_the_runs_on_random_updates_with_named_graphs() {
    statistics_on_random_updates(0x2545_f491_4f6c_dd1d, false, 95);
}

#[test]
fn statistics_match_the_runs_on_random_updates_with_a_union_default_graph() {
    statistics_on_random_updates(0xd1b5_4a32_d192_ed03, true, 5);
}

/// A store made by updates alone has empty statistics: every count comes from the delta.
#[test]
fn statistics_on_a_store_without_a_base() {
    let s = Store::in_memory(StoreOptions::default());
    update(
        &s,
        "INSERT DATA { ex:a a ex:C ; ex:knows ex:b , ex:c ; ex:name \"a\" . ex:b ex:knows ex:c . \
         GRAPH ex:g1 { ex:a a ex:D ; ex:knows ex:d } }",
    );
    update(&s, "DELETE DATA { ex:a ex:knows ex:c }");
    check_statistics(&s.snapshot(), "delta only");
}
