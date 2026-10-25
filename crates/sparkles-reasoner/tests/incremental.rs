//! Incremental materialization against full materialization on real stores: random
//! inserts and deletes, with the closure kept in memory or read back from the dataset,
//! across compaction, bulk loads, restarts and edits of the inferred graph.

use oxrdf::{GraphName, NamedNode, Quad, Term, Triple};
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, INFERRED_GRAPH, Incremental, Method, Profile, ReasonOptions, ReasonReport,
    infer, materialize_incremental,
};
use std::collections::BTreeSet;

const EX: &str = "http://ex.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// xorshift64*
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn iri(s: String) -> NamedNode {
    NamedNode::new_unchecked(s)
}

/// A random triple over a small vocabulary.
fn random_triple(r: &mut Rng, owl: bool) -> Triple {
    let c = |r: &mut Rng| iri(format!("{EX}C{}", r.below(6)));
    let p = |r: &mut Rng| iri(format!("{EX}p{}", r.below(5)));
    let i = |r: &mut Rng| iri(format!("{EX}i{}", r.below(10)));
    let v = |ns: &str, l: &str| iri(format!("{ns}{l}"));
    let k = r.below(if owl { 16 } else { 11 });
    let (s, pr, o): (NamedNode, NamedNode, Term) = match k {
        0 => (c(r), v(RDFS, "subClassOf"), c(r).into()),
        1 => (p(r), v(RDFS, "subPropertyOf"), p(r).into()),
        2 => (p(r), v(RDFS, "domain"), c(r).into()),
        3 => (p(r), v(RDFS, "range"), c(r).into()),
        4 | 5 => (i(r), v(RDF, "type"), c(r).into()),
        6..=8 => (i(r), p(r), i(r).into()),
        9 => (
            i(r),
            p(r),
            oxrdf::Literal::new_simple_literal(format!("v{}", r.below(3))).into(),
        ),
        10 => (i(r), p(r), oxrdf::Literal::from(r.below(4) as i64).into()),
        11 => (i(r), v(OWL, "sameAs"), i(r).into()),
        12 => (p(r), v(OWL, "inverseOf"), p(r).into()),
        13 => (
            p(r),
            v(RDF, "type"),
            v(
                OWL,
                [
                    "TransitiveProperty",
                    "SymmetricProperty",
                    "FunctionalProperty",
                ][r.below(3)],
            )
            .into(),
        ),
        14 => (c(r), v(OWL, "equivalentClass"), c(r).into()),
        _ => (c(r), v(OWL, "someValuesFrom"), c(r).into()),
    };
    Triple::new(s, pr, o)
}

fn quad(t: &Triple) -> Quad {
    Quad::new(
        t.subject.clone(),
        t.predicate.clone(),
        t.object.clone(),
        GraphName::DefaultGraph,
    )
}

/// Insert and delete default graph triples in one commit.
fn change(s: &Store, add: &[Triple], del: &[Triple]) {
    let mut txn = s.write();
    let mut labels = Default::default();
    for t in del {
        let q = txn.encode_quad(&quad(t), &mut labels).unwrap();
        txn.delete(q).unwrap();
    }
    for t in add {
        let q = txn.encode_quad(&quad(t), &mut labels).unwrap();
        txn.insert(q).unwrap();
    }
    txn.commit().unwrap();
}

/// The default graph's triples.
fn base(s: &Store) -> Vec<Triple> {
    let snap = s.snapshot();
    let mut out = Vec::new();
    for k in snap
        .scan_keys(Perm::Gspo, &[sparkles_core::id::Id::DEFAULT_GRAPH.0])
        .unwrap()
    {
        let q = snap.quad_to_terms(&Perm::Gspo.to_quad(&k)).unwrap();
        out.push(Triple::new(q.subject, q.predicate, q.object));
    }
    out.sort_by_key(|t| t.to_string());
    out
}

/// The inferred graph, as text.
fn inferred(s: &Store) -> BTreeSet<String> {
    let snap = s.snapshot();
    let Some(g) = snap.lookup_iri(INFERRED_GRAPH) else {
        return BTreeSet::new();
    };
    snap.scan_keys(Perm::Gspo, &[g.0])
        .unwrap()
        .iter()
        .map(|k| {
            let q = snap.quad_to_terms(&Perm::Gspo.to_quad(k)).unwrap();
            Triple::new(q.subject, q.predicate, q.object).to_string()
        })
        .collect()
}

/// What a full run would write now.
fn expected(s: &Store, p: &Profile) -> BTreeSet<String> {
    let (ts, _) = infer(s.snapshot(), p, &ReasonOptions::default()).unwrap();
    ts.iter().map(|t| t.to_string()).collect()
}

fn run(s: &Store, p: &Profile, since: Option<u64>, cache: Option<&Cache>) -> ReasonReport {
    materialize_incremental(
        s,
        p,
        &Extras::default(),
        Incremental { since, cache },
        &ReasonOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

fn check(s: &Store, p: &Profile, r: &ReasonReport, what: &str) {
    let (got, want) = (inferred(s), expected(s, p));
    if got != want {
        panic!(
            "{what} ({:?}, {:?}): extra {:#?} missing {:#?}",
            r.method,
            r.fallback,
            got.difference(&want).collect::<Vec<_>>(),
            want.difference(&got).collect::<Vec<_>>()
        );
    }
    assert_eq!(r.inferred as usize, got.len(), "{what}: inferred count");
}

/// Random changes, each followed by an incremental run checked against a full one.
fn random_changes(
    s: &Store,
    p: &Profile,
    owl: bool,
    seed: u64,
    steps: usize,
    cache: Option<&Cache>,
) -> (usize, usize, usize) {
    let mut r = Rng(seed);
    let first: Vec<Triple> = (0..30).map(|_| random_triple(&mut r, owl)).collect();
    change(s, &first, &[]);
    let rep = run(s, p, None, cache);
    check(s, p, &rep, "first run");
    let mut since = rep.receipt.unwrap().commit.seq;
    let (mut inc, mut full, mut memory) = (0, 0, 0);
    for step in 0..steps {
        let now = base(s);
        let del: Vec<Triple> = (0..r.below(4))
            .filter(|_| !now.is_empty())
            .map(|_| now[r.below(now.len())].clone())
            .collect();
        let add: Vec<Triple> = (0..r.below(5))
            .map(|_| random_triple(&mut r, owl))
            .collect();
        change(s, &add, &del);
        let rep = run(s, p, Some(since), cache);
        match rep.method {
            Method::Incremental => inc += 1,
            Method::Full => full += 1,
        }
        if rep.changes.as_ref().is_some_and(|c| c.source == "memory") {
            memory += 1;
        }
        check(s, p, &rep, &format!("seed {seed} step {step}"));
        since = rep.receipt.unwrap().commit.seq;
    }
    (inc, full, memory)
}

fn persistent() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    (dir, s)
}

fn profiles() -> Vec<(Profile, bool)> {
    vec![
        (Profile::Rdfs, false),
        (Profile::RdfsSimple, false),
        (Profile::OwlRl, true),
        (
            Profile::Rules(
                "@prefix ex: <http://ex.org/>.
                 [a: (?x ex:p0 ?y), (?y ex:p0 ?z), notEqual(?x, ?z) -> (?x ex:p1 ?z)]
                 [b: (?x ex:p1 ?y) -> (?y ex:p1 ?x)]
                 [c: (?x ex:p2 ?v), isLiteral(?v), strConcat(?v, '-x', ?w) -> (?x ex:p3 ?w)]
                 [d: (?x ex:p3 ?w), regex(?w, '(.*)-x', ?v) -> (?x ex:p4 ?v)]
                 [e: (?x ex:p4 ?v), lessThan(?v, 2) -> (?x rdf:type ex:C0)]
                 [f: (?x rdf:type ex:C0), (?x ?p ?y), notEqual(?p, rdf:type) -> (?y rdf:type ex:C1)]
                 [g: (?v ex:p2 ?x) -> (?x ex:back ?v)]"
                    .into(),
            ),
            false,
        ),
    ]
}

#[test]
fn closure_kept_in_memory() {
    for (p, owl) in profiles() {
        for seed in 1..6 {
            let s = Store::in_memory(StoreOptions::default());
            let cache = Cache::default();
            let (inc, _, memory) = random_changes(&s, &p, owl, seed, 12, Some(&cache));
            assert!(
                inc > 0 && memory == inc,
                "{p}: {inc} incremental, {memory} from memory"
            );
        }
    }
}

#[test]
fn closure_read_from_the_dataset() {
    for (p, owl) in profiles() {
        for seed in 1..4 {
            let (_d, s) = persistent();
            let (inc, full, memory) = random_changes(&s, &p, owl, seed, 10, None);
            assert!(
                inc > full && memory == 0,
                "{p}: {inc} incremental, {full} full"
            );
        }
    }
}

#[test]
fn closure_kept_for_a_persistent_dataset() {
    for (p, owl) in profiles() {
        let (_d, s) = persistent();
        let cache = Cache::default();
        let (inc, full, memory) = random_changes(&s, &p, owl, 7, 15, Some(&cache));
        assert!(
            inc > full && memory > 0,
            "{p}: {inc} incremental, {full} full, {memory} from memory"
        );
    }
}

fn ttl(s: &Store, text: &str) {
    s.load(&[Source::from_bytes(
        format!("@prefix ex: <{EX}> . @prefix rdfs: <{RDFS}> . @prefix owl: <{OWL}> .\n{text}")
            .into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
}

fn t(s: &str, p: &str, o: &str) -> Triple {
    let n = |x: &str| {
        iri(match x.split_once(':') {
            Some(("rdf", l)) => format!("{RDF}{l}"),
            Some(("rdfs", l)) => format!("{RDFS}{l}"),
            Some(("owl", l)) => format!("{OWL}{l}"),
            Some(("ex", l)) => format!("{EX}{l}"),
            _ => x.to_string(),
        })
    };
    Triple::new(n(s), n(p), n(o))
}

#[test]
fn a_type_change_does_not_remove_the_class_axioms() {
    let s = Store::in_memory(StoreOptions::default());
    ttl(
        &s,
        "ex:B rdfs:subClassOf ex:A . ex:C rdfs:subClassOf ex:B .
         ex:x a ex:C . ex:y a ex:C . ex:x ex:knows ex:y .",
    );
    let p = Profile::Rdfs;
    let cache = Cache::default();
    let r = run(&s, &p, None, Some(&cache));
    let since = r.receipt.unwrap().commit.seq;
    change(&s, &[], &[t("ex:x", "rdf:type", "ex:C")]);
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "delete");
    let c = r.changes.clone().unwrap();
    assert_eq!(
        (c.base_added, c.base_removed, c.source.as_str()),
        (0, 1, "memory")
    );
    // ex:x keeps rdfs:Resource (it has ex:knows) and loses ex:C, ex:B, ex:A; ex:C is
    // still a class through ex:y
    assert_eq!(c.removed, 3, "{c:?}");
    assert_eq!(r.inferred_removed, 2, "x a B, x a A");
    // nothing changed: nothing written, still incremental
    let since = r.receipt.unwrap().commit.seq;
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(
        (r.method, r.inferred_added, r.inferred_removed),
        (Method::Incremental, 0, 0)
    );
}

#[test]
fn edits_of_the_inferred_graph_are_undone() {
    let (_d, s) = persistent();
    ttl(&s, "ex:B rdfs:subClassOf ex:A . ex:x a ex:B .");
    let p = Profile::RdfsSimple;
    let r = run(&s, &p, None, None);
    let since = r.receipt.unwrap().commit.seq;
    let g = GraphName::NamedNode(iri(INFERRED_GRAPH.into()));
    let mut txn = s.write();
    let mut labels = Default::default();
    let bogus = Quad::new(
        iri(format!("{EX}z")),
        iri(format!("{EX}p")),
        iri(format!("{EX}q")),
        g.clone(),
    );
    let q = txn.encode_quad(&bogus, &mut labels).unwrap();
    txn.insert(q).unwrap();
    let real = Quad::new(
        iri(format!("{EX}x")),
        iri(format!("{RDF}type")),
        iri(format!("{EX}A")),
        g,
    );
    let q = txn.encode_quad(&real, &mut labels).unwrap();
    assert!(txn.delete(q).unwrap());
    txn.commit().unwrap();
    let r = run(&s, &p, Some(since), None);
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    assert_eq!((r.inferred_added, r.inferred_removed), (1, 1));
    check(&s, &p, &r, "after edits");
}

#[test]
fn compaction_restarts_and_bulk_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let opts = StoreOptions {
        bulk_threshold: 50,
        ..Default::default()
    };
    let p = Profile::OwlRl;
    let mut r = Rng(99);
    let s = Store::open(&path, opts.clone()).unwrap();
    let first: Vec<Triple> = (0..40).map(|_| random_triple(&mut r, true)).collect();
    change(&s, &first, &[]);
    let cache = Cache::default();
    let rep = run(&s, &p, None, Some(&cache));
    check(&s, &p, &rep, "first");
    let mut since = rep.receipt.unwrap().commit.seq;
    let mut methods = Vec::new();
    for step in 0..6 {
        let add: Vec<Triple> = (0..3).map(|_| random_triple(&mut r, true)).collect();
        change(&s, &add, &[]);
        match step {
            // compaction makes a new generation: the kept closure's ids are stale
            1 => s.compact().unwrap(),
            // a bulk load rebuilds the generation too
            3 => {
                let n: String = (0..60)
                    .map(|i| format!("<{EX}bulk{i}> <{RDF}type> <{EX}C{}> .\n", i % 6))
                    .collect();
                s.load(&[Source::from_bytes(
                    n.into_bytes(),
                    RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
            }
            _ => {}
        }
        let rep = run(&s, &p, Some(since), Some(&cache));
        methods.push((
            rep.method,
            rep.changes.as_ref().map(|c| c.source.clone()),
            rep.fallback.clone(),
        ));
        check(&s, &p, &rep, &format!("step {step}"));
        since = rep.receipt.unwrap().commit.seq;
    }
    // a restart loses the cache: the closure comes from the dataset
    drop(s);
    let s = Store::open(&path, opts).unwrap();
    change(&s, &[t("ex:i1", "rdf:type", "ex:C2")], &[]);
    let rep = run(&s, &p, Some(since), None);
    check(&s, &p, &rep, "after restart");
    methods.push((
        rep.method,
        rep.changes.as_ref().map(|c| c.source.clone()),
        rep.fallback.clone(),
    ));
    assert_eq!(
        methods.last().unwrap().1.as_deref(),
        Some("store"),
        "{methods:?}"
    );
    assert!(
        methods
            .iter()
            .filter(|m| m.0 == Method::Incremental)
            .count()
            >= 4,
        "{methods:?}"
    );
}

#[test]
fn large_deletions_run_in_full() {
    let s = Store::in_memory(StoreOptions::default());
    let data: String = (0..12_000)
        .map(|i| format!("<{EX}i{i}> <{RDF}type> <{EX}C{}> .\n", i % 7))
        .collect();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let p = Profile::RdfsSimple;
    let cache = Cache::default();
    let r = run(&s, &p, None, Some(&cache));
    let since = r.receipt.unwrap().commit.seq;
    let del: Vec<Triple> = (0..700)
        .map(|i| t(&format!("ex:i{i}"), "rdf:type", &format!("ex:C{}", i % 7)))
        .collect();
    change(&s, &[], &del);
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Full);
    assert!(
        r.fallback
            .as_deref()
            .unwrap()
            .starts_with("700 of 12000 explicit"),
        "{:?}",
        r.fallback
    );
    // fewer go incrementally
    let since = r.receipt.unwrap().commit.seq;
    change(&s, &[], &[t("ex:i800", "rdf:type", "ex:C2")]);
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "one deletion");
}

#[test]
fn fallbacks() {
    let s = Store::in_memory(StoreOptions::default());
    ttl(&s, "ex:B rdfs:subClassOf ex:A . ex:x a ex:B .");
    let cache = Cache::default();
    // no previous run named: full, no fallback reason
    let r = run(&s, &Profile::Rdfs, None, Some(&cache));
    assert_eq!((r.method, r.fallback.as_deref()), (Method::Full, None));
    let since = r.receipt.unwrap().commit.seq;
    // other rules
    let r = run(&s, &Profile::RdfsSimple, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Full);
    assert_eq!(
        r.fallback.as_deref(),
        Some("the rules changed since the previous run")
    );
    // a rule that is not monotonic
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [n: (?x rdf:type ex:B), noValue(?x, ex:p) -> (?x rdf:type ex:Lonely)]"
            .into(),
    );
    let r = run(&s, &rules, None, Some(&cache));
    let since = r.receipt.unwrap().commit.seq;
    change(&s, &[t("ex:y", "rdf:type", "ex:B")], &[]);
    let r = run(&s, &rules, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Full);
    assert!(r.fallback.unwrap().contains("noValue"));
    check(&s, &rules, &run(&s, &rules, None, None), "noValue");
    // owl-rl reads lists: a list change runs in full
    let p = Profile::OwlRl;
    let r = run(&s, &p, None, Some(&cache));
    let since = r.receipt.unwrap().commit.seq;
    ttl(&s, "ex:U owl:unionOf (ex:A ex:Q) .");
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Full);
    assert!(
        r.fallback.as_deref().unwrap().contains("list"),
        "{:?}",
        r.fallback
    );
    check(&s, &p, &r, "list");
    // with the list in place, other changes are incremental, and see it
    let since = r.receipt.unwrap().commit.seq;
    change(&s, &[t("ex:q", "rdf:type", "ex:Q")], &[]);
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "after the list");
    // a store with no kept closure and no files: full
    let other = Store::in_memory(StoreOptions::default());
    ttl(&other, "ex:B rdfs:subClassOf ex:A .");
    let r = run(&other, &Profile::Rdfs, None, None);
    let since = r.receipt.unwrap().commit.seq;
    let r = run(&other, &Profile::Rdfs, Some(since), None);
    assert_eq!(r.method, Method::Full);
    assert!(r.fallback.is_some());
}

/// Roadmap item 87: a closure kept in memory remembers lookups of the input graphs
/// between runs. Runs that take in several commits at once, steps that undo the previous
/// one, and runs that are cancelled or fail for memory in between still match a full
/// run, and the runs after them still continue from memory.
#[test]
fn kept_lookups_follow_commits_undos_and_failed_runs() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    for (p, owl) in profiles() {
        for seed in 1..4 {
            let s = Store::in_memory(StoreOptions::default());
            let cache = Cache::default();
            let mut r = Rng(seed);
            let first: Vec<Triple> = (0..30).map(|_| random_triple(&mut r, owl)).collect();
            change(&s, &first, &[]);
            let rep = run(&s, &p, None, Some(&cache));
            let mut since = rep.receipt.unwrap().commit.seq;
            let mut last: (Vec<Triple>, Vec<Triple>) = (Vec::new(), Vec::new());
            let mut memory = 0;
            for step in 0..12 {
                let commits = 1 + r.below(3);
                for c in 0..commits {
                    if c == 0 && step % 4 == 3 {
                        // undo the previous step's last commit
                        let (add, del) = std::mem::take(&mut last);
                        change(&s, &del, &add);
                        continue;
                    }
                    let now = base(&s);
                    let del: Vec<Triple> = (0..r.below(3))
                        .filter(|_| !now.is_empty())
                        .map(|_| now[r.below(now.len())].clone())
                        .collect();
                    let add: Vec<Triple> = (0..r.below(4))
                        .map(|_| random_triple(&mut r, owl))
                        .collect();
                    change(&s, &add, &del);
                    last = (add, del);
                }
                let what = format!("{p} seed {seed} step {step}");
                // a cancelled run and a run out of memory change nothing
                let opts = if step % 2 == 0 {
                    ReasonOptions {
                        cancel: Some(Arc::new(AtomicBool::new(true))),
                        ..Default::default()
                    }
                } else {
                    ReasonOptions {
                        max_memory: Some(4096),
                        ..Default::default()
                    }
                };
                let before = inferred(&s);
                let failed = materialize_incremental(
                    &s,
                    &p,
                    &Extras::default(),
                    Incremental {
                        since: Some(since),
                        cache: Some(&cache),
                    },
                    &opts,
                );
                assert!(failed.is_err(), "{what}: the failing run succeeded");
                assert_eq!(inferred(&s), before, "{what}: the failed run wrote");
                let rep = run(&s, &p, Some(since), Some(&cache));
                check(&s, &p, &rep, &what);
                if rep.changes.as_ref().is_some_and(|c| c.source == "memory") {
                    memory += 1;
                }
                since = rep.receipt.unwrap().commit.seq;
            }
            assert!(memory > 0, "{p} seed {seed}: no run continued from memory");
        }
    }
}

/// A kept closure over input graphs in a segmented generation, whose lookups read a
/// segment of a block at a time, and whose insertions split their work into batches of
/// bindings: random inserts and deletes, of the loaded triples among them, match a
/// full run on one thread and on six.
#[test]
fn closure_kept_over_a_segmented_generation() {
    for threads in [1, 6] {
        for (p, owl) in [(Profile::Rdfs, false), (Profile::OwlRl, true)] {
            let (_d, s) = persistent();
            // individuals of the classes `random_triple` uses, with properties of their
            // own whose domain, range and inverse are fixed, loaded in bulk so that they
            // are in the generation's blocks. No rule makes those properties transitive,
            // so the closure stays small.
            let mut r = Rng(threads as u64 * 31 + owl as u64);
            let mut nt = String::new();
            for (s, p, o) in [
                ("q0", format!("{RDFS}domain"), "C0"),
                ("q1", format!("{RDFS}range"), "C1"),
                ("q2", format!("{OWL}inverseOf"), "q3"),
            ] {
                nt.push_str(&format!("<{EX}{s}> <{p}> <{EX}{o}> .\n"));
            }
            for n in 0..4_000 {
                let f = format!("<{EX}f{n}>");
                nt.push_str(&format!("{f} <{RDF}type> <{EX}C{}> .\n", r.below(6)));
                for _ in 0..3 {
                    let o = match r.below(4) {
                        0 => format!("<{EX}i{}>", r.below(10)),
                        _ => format!("<{EX}f{}>", r.below(4_000)),
                    };
                    nt.push_str(&format!("{f} <{EX}q{}> {o} .\n", r.below(4)));
                }
            }
            s.load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
            assert!(s.snapshot().perm(Perm::Gspo).segmented());
            let opts = ReasonOptions {
                threads: Some(threads),
                ..Default::default()
            };
            let cache = Cache::default();
            let go = |since: Option<u64>| {
                materialize_incremental(
                    &s,
                    &p,
                    &Extras::default(),
                    Incremental {
                        since,
                        cache: Some(&cache),
                    },
                    &opts,
                )
                .unwrap_or_else(|e| panic!("{e:#}"))
            };
            let rep = go(None);
            check(&s, &p, &rep, "first run");
            let mut since = rep.receipt.unwrap().commit.seq;
            let mut memory = 0;
            for step in 0..5 {
                let now = base(&s);
                let del: Vec<Triple> = (0..1 + r.below(20))
                    .map(|_| now[r.below(now.len())].clone())
                    .collect();
                let add: Vec<Triple> = (0..1 + r.below(20))
                    .map(|_| random_triple(&mut r, owl))
                    .collect();
                change(&s, &add, &del);
                let rep = go(Some(since));
                if rep.changes.as_ref().is_some_and(|c| c.source == "memory") {
                    memory += 1;
                }
                check(&s, &p, &rep, &format!("{p} threads {threads} step {step}"));
                since = rep.receipt.unwrap().commit.seq;
            }
            assert!(
                memory > 0,
                "{p} threads {threads}: no run continued from memory"
            );
        }
    }
}

/// The backward search reads only a few derived rows before a long lookup of the input
/// graphs. Every derived `ex:p` fact here depends on `ex:t ex:q ex:r`, so a search that
/// read all 40,000 of them before the explicit `ex:e2 ex:p ex:o` would scan the input
/// graphs once per derived fact before it found the proof of `ex:t ex:q ex:r`.
#[test]
fn backward_search_reaches_an_explicit_proof_past_many_derived_rows() {
    let (_d, s) = persistent();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [a: (?x ex:p ex:o) -> (ex:t ex:q ex:r)]
         [b: (ex:t ex:q ex:r), (?x ex:seed ex:v) -> (?x ex:p ex:o)]"
            .into(),
    );
    let mut nt = String::new();
    for n in 0..40_000 {
        nt.push_str(&format!("<{EX}s{n}> <{EX}seed> <{EX}v> .\n"));
    }
    for e in ["e1", "e2"] {
        nt.push_str(&format!("<{EX}{e}> <{EX}p> <{EX}o> .\n"));
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let cache = Cache::default();
    let rep = run(&s, &rules, None, Some(&cache));
    check(&s, &rules, &rep, "first run");
    // the derived facts move into the generation's blocks, where they share the input
    // graphs' permutations, and a run after the compaction keeps a closure over them
    s.compact().unwrap();
    change(&s, &[t("ex:z", "ex:seed", "ex:w")], &[]);
    let rep = run(
        &s,
        &rules,
        Some(rep.receipt.unwrap().commit.seq),
        Some(&cache),
    );
    check(&s, &rules, &rep, "after the compaction");
    change(&s, &[], &[t("ex:e1", "ex:p", "ex:o")]);
    let rep = run(
        &s,
        &rules,
        Some(rep.receipt.unwrap().commit.seq),
        Some(&cache),
    );
    check(&s, &rules, &rep, "one deletion");
    assert_eq!(rep.method, Method::Incremental, "{:?}", rep.fallback);
    assert_eq!(
        rep.changes.as_ref().map(|c| c.source.as_str()),
        Some("memory")
    );
    assert!(
        rep.base_scans < 1_000,
        "{} scans of the input graphs",
        rep.base_scans
    );
}

#[test]
fn diagnostic_summary_admits_cached_literal_decode_before_allocation() {
    use sparkles_reasoner::diagnostics::DiagnosticControls;
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("<urn:item> <urn:value> \"{}\" .", "z".repeat(1_000_000)).into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let cache = Cache::default();
    let profile=Profile::Rules("[type: (?x <urn:value> ?value) -> (?value <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#string>)]".into());
    run(&s, &profile, None, Some(&cache));
    let result = sparkles_reasoner::literal_typings_controlled(
        &s,
        s.snapshot().commit,
        Some(&cache),
        &DiagnosticControls {
            max_memory: Some(1024),
            ..Default::default()
        },
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("the cached decode must be admitted before allocating a megabyte literal"),
    };
    assert!(
        error
            .downcast_ref::<sparkles_reasoner::MemoryLimitExceeded>()
            .is_some(),
        "{error:#}"
    );
}
