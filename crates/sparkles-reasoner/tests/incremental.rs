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
fn large_deletions_use_cost_estimate_not_fixed_ratio() {
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
    // Deleting 700 of 12,000 explicit triples (> 5%) with zero rule fan-out stays
    // incremental now that LARGE_DELETION is replaced by the cost estimate (F16 §7.3).
    let del: Vec<Triple> = (0..700)
        .map(|i| t(&format!("ex:i{i}"), "rdf:type", &format!("ex:C{}", i % 7)))
        .collect();
    change(&s, &[], &del);
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "low-fanout deletion above 5%");
    // A deletion whose estimated rule-instance visits exceed item 798's visit bound falls
    // back before running backward searches.
    let r = delete_from_a_product("(ex:s ex:r ex:o)", 32, Some(32 << 20));
    assert_eq!(r.method, Method::Full);
    assert!(
        r.fallback
            .as_deref()
            .unwrap_or_default()
            .starts_with("the deletion is estimated to visit"),
        "{:?}",
        r.fallback
    );
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
    // owl-rl reads lists: changing an explicit list stays incremental (F16 §7.3)
    let p = Profile::OwlRl;
    let r = run(&s, &p, None, Some(&cache));
    let since = r.receipt.unwrap().commit.seq;
    ttl(&s, "ex:U owl:unionOf (ex:A ex:Q) .");
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "list");
    // with the list in place, other changes are incremental, and see it
    let since = r.receipt.unwrap().commit.seq;
    change(&s, &[t("ex:q", "rdf:type", "ex:Q")], &[]);
    let r = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "after the list");
    // lists derived by rules still fall back to a full run (F16 §7.3)
    let derived_list_rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [make: (?x ex:head ?m) -> (?x rdf:first ?m), (?x rdf:rest rdf:nil)]
         [read: (?c ex:uses ?l), listMember(?l, ?m) -> (?c ex:has ?m)]"
            .into(),
    );
    let ds = Store::in_memory(StoreOptions::default());
    ttl(&ds, "ex:l0 ex:head ex:v1 . ex:c0 ex:uses ex:l0 .");
    let dcache = Cache::default();
    let r = run(&ds, &derived_list_rules, None, Some(&dcache));
    let since = r.receipt.unwrap().commit.seq;
    change(&ds, &[t("ex:l1", "ex:head", "ex:v2")], &[]);
    let r = run(&ds, &derived_list_rules, Some(since), Some(&dcache));
    assert_eq!(r.method, Method::Full);
    assert!(
        r.fallback.as_deref().unwrap_or_default().contains("list"),
        "{:?}",
        r.fallback
    );
    check(&ds, &derived_list_rules, &r, "derived list fallback");
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

#[test]
fn diagnostic_summary_accounts_for_cumulative_retained_language_tags() {
    use sparkles_reasoner::diagnostics::DiagnosticControls;
    let s = Store::in_memory(StoreOptions::default());
    let mut text = String::new();
    for i in 0..30 {
        let tag = format!("x-{}{:08x}", "abcdefgh-".repeat(100), i);
        text.push_str(&format!("<urn:item{i}> <urn:value> \"bad\"@{tag} .\n"));
    }
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let cache = Cache::default();
    let profile=Profile::Rules("[type: (?x <urn:value> ?value) -> (?value <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#integer>)]".into());
    run(&s, &profile, None, Some(&cache));
    let result = sparkles_reasoner::literal_typings_controlled(
        &s,
        s.snapshot().commit,
        Some(&cache),
        &DiagnosticControls {
            max_memory: Some(32768),
            ..Default::default()
        },
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("cumulative retained language tags must exhaust the summary allowance"),
    };
    assert!(
        error
            .downcast_ref::<sparkles_reasoner::MemoryLimitExceeded>()
            .is_some(),
        "{error:#}"
    );
}

#[test]
fn analysis_cancellation_interrupts_geometry_during_its_input_scan() {
    use sparkles_reasoner::analysis::{AnalysisContext, WitnessMap};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(b"<urn:a> <http://www.opengis.net/ont/geosparql#hasGeometry> <urn:g> . <urn:b> <http://www.opengis.net/ont/geosparql#hasGeometry> <urn:h> .".to_vec(),RdfFormat::Turtle,None)]).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let context = AnalysisContext::new(
        WitnessMap::new(1, None, cancel.clone(), None),
        None,
        "geometry control regression".into(),
    )
    .unwrap();
    let seen = cancel.clone();
    let options = ReasonOptions {
        analysis: Some(context),
        progress: Some(Arc::new(move |_, message| {
            if message == "finding default geometries" {
                seen.store(true, Ordering::Relaxed);
            }
        })),
        ..Default::default()
    };
    let result = materialize_incremental(
        &s,
        &Profile::Rules(String::new()),
        &Extras {
            geo_default_geometry: true,
            ..Default::default()
        },
        Incremental::default(),
        &options,
    );
    assert!(
        cancel.load(Ordering::Relaxed),
        "cancellation must happen inside the geometry scan"
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("geometry construction must stop at the next control check"),
    };
    assert!(
        error
            .to_string()
            .contains("stopping an entailment analysis"),
        "{error:#}"
    );
    assert_eq!(s.snapshot().commit, 1);
}

/// The edges of a symmetric and transitive `ex:near` among `n` people, whose closure is
/// one clique, and the Turtle that states them.
fn clique(n: usize) -> (Vec<Triple>, String) {
    let mut text = String::from("ex:near a owl:SymmetricProperty, owl:TransitiveProperty .\n");
    let mut edges = Vec::new();
    for i in 0..n {
        edges.push(t(
            &format!("ex:p{i}"),
            "ex:near",
            &format!("ex:p{}", (i + 1) % n),
        ));
        edges.push(t(
            &format!("ex:p{i}"),
            "ex:near",
            &format!("ex:p{}", (i * 7 + 3) % n),
        ));
    }
    for e in &edges {
        text += &format!("{} {} {} .\n", e.subject, e.predicate, e.object);
    }
    (edges, text)
}

/// Roadmap item 798. Deleting a few edges of a symmetric and transitive property whose
/// closure is one clique must stay within the memory of a full run of the same data.
/// Every fact of the clique follows through every member, and the searches for other
/// proofs of the removed edges reach most of the closure through each of them, so the
/// deletion passes its bound and the run materializes in full. The full run of 600 people
/// fits in 256 MiB. Without the bound, the run after deleting 24 of the 1,200 edges
/// exceeded it, and without a limit it visited 184 million rule instances and held 5 GB.
/// OWL2Bench(1) shows the case at scale. Its `hasSameHomeTownWith` closure is a clique of
/// about 1,100 people, a full run needs less than 800 MiB, and deleting 1% of the data
/// triples grew to 14.1 GiB before the out-of-memory killer ended it.
#[test]
fn deleting_edges_of_a_clique_stays_within_a_full_runs_memory() {
    const LIMIT: usize = 256 << 20;
    let (edges, text) = clique(600);
    // the closure is read back from the dataset, as `sparkles infer` reads it
    let (_d, s) = persistent();
    ttl(&s, &text);
    let p = Profile::OwlRl;
    let opts = ReasonOptions {
        max_memory: Some(LIMIT),
        ..ReasonOptions::default()
    };
    let go = |since| {
        materialize_incremental(
            &s,
            &p,
            &Extras::default(),
            Incremental { since, cache: None },
            &opts,
        )
    };
    let full = go(None).unwrap_or_else(|e| panic!("the full run exceeds {LIMIT} bytes: {e:#}"));
    check(&s, &p, &full, "the full run");
    let since = Some(full.receipt.as_ref().unwrap().commit.seq);
    let del: Vec<Triple> = edges.iter().step_by(50).cloned().collect();
    change(&s, &[], &del);
    let r = go(since).unwrap_or_else(|e| panic!("the run after the deletion: {e:#}"));
    check(&s, &p, &r, "after deleting edges of the clique");
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
}

/// The bound of roadmap item 798 leaves the deletion of an edge of a small clique to
/// incremental maintenance, whose searches cost far less than a full run there.
#[test]
fn deleting_an_edge_of_a_small_clique_stays_incremental() {
    let (edges, text) = clique(40);
    let (_d, s) = persistent();
    ttl(&s, &text);
    let p = Profile::OwlRl;
    let cache = Cache::default();
    let full = run(&s, &p, None, Some(&cache));
    check(&s, &p, &full, "the full run");
    change(&s, &[], &edges[..2]);
    let r = run(&s, &p, Some(full.receipt.unwrap().commit.seq), Some(&cache));
    check(&s, &p, &r, "after deleting edges of the clique");
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
}

/// Delete the `ex:p` facts of `ps` facts beside 100,000 `ex:q` facts, under the rule
/// `(?x ex:p ex:on), (?y ex:q ex:on) -> head`, within `limit` bytes, and return the run
/// after the deletion, checked against a full run.
fn delete_from_a_product(head: &str, ps: usize, limit: Option<usize>) -> ReasonReport {
    let (_d, s) = persistent();
    let rules = Profile::Rules(format!(
        "@prefix ex: <http://ex.org/>.
         [a: (?x ex:p ex:on), (?y ex:q ex:on) -> {head}]"
    ));
    let mut nt = String::new();
    for n in 0..100_000 {
        nt.push_str(&format!("<{EX}q{n}> <{EX}q> <{EX}on> .\n"));
    }
    let ps: Vec<Triple> = (0..ps)
        .map(|n| t(&format!("ex:p{n}"), "ex:p", "ex:on"))
        .collect();
    for p in &ps {
        nt.push_str(&format!("{} {} {} .\n", p.subject, p.predicate, p.object));
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let opts = ReasonOptions {
        max_memory: limit,
        ..ReasonOptions::default()
    };
    let go = |since| {
        materialize_incremental(
            &s,
            &rules,
            &Extras::default(),
            Incremental { since, cache: None },
            &opts,
        )
    };
    let full = go(None).unwrap_or_else(|e| panic!("the full run: {e:#}"));
    check(&s, &rules, &full, "the full run");
    change(&s, &[], &ps);
    let r = go(Some(full.receipt.unwrap().commit.seq))
        .unwrap_or_else(|e| panic!("the run after the deletion: {e:#}"));
    check(&s, &rules, &r, "after the deletion");
    r
}

/// Roadmap item 798. The consequences of deleted facts are the heads of the rule instances
/// that used them, and a rule whose head does not depend on the deleted atom's bindings
/// has one head for many instances. Deleting 32 `ex:p` facts matches 3.2 million
/// instances of one head, which the deletion kept once each, past the memory of a full
/// run, before it counted them. The evaluations now keep each head once and count the
/// instances as they match them, so the deletion passes its bound on visits and the run
/// materializes in full.
#[test]
fn the_consequences_of_a_deletion_count_their_instances() {
    let r = delete_from_a_product("(ex:s ex:r ex:o)", 32, Some(32 << 20));
    assert_eq!(r.method, Method::Full);
    let why = r.fallback.as_deref().unwrap_or_default();
    assert!(why.starts_with("the deletion "), "{why}");
}

/// Even when the upfront cost estimate stays below the visit bound (because average
/// per-key fan-out is small across many distinct keys), item 798's runtime visit counter
/// still stops a skewed deletion that visits past the bound.
#[test]
fn the_runtime_visit_bound_stops_skewed_consequences() {
    let (_d, s) = persistent();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [a: (?x ex:p ?k), (?y ex:q ?k) -> (ex:s ex:r ex:o)]"
            .into(),
    );
    let mut nt = String::new();
    // 50,000 singleton keys keep average fan-out near 2, while key ex:hot has 50,000 rows
    for n in 0..50_000 {
        nt.push_str(&format!("<{EX}q{n}> <{EX}q> <{EX}k{n}> .\n"));
        nt.push_str(&format!("<{EX}h{n}> <{EX}q> <{EX}hot> .\n"));
    }
    let ps: Vec<Triple> = (0..32)
        .map(|n| t(&format!("ex:p{n}"), "ex:p", "ex:hot"))
        .collect();
    for p in &ps {
        nt.push_str(&format!("{} {} {} .\n", p.subject, p.predicate, p.object));
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let opts = ReasonOptions {
        max_memory: Some(64 << 20),
        ..ReasonOptions::default()
    };
    let go = |since| {
        materialize_incremental(
            &s,
            &rules,
            &Extras::default(),
            Incremental { since, cache: None },
            &opts,
        )
    };
    let full = go(None).unwrap();
    check(&s, &rules, &full, "skewed full");
    change(&s, &[], &ps);
    let r = go(Some(full.receipt.unwrap().commit.seq)).unwrap();
    check(&s, &rules, &r, "skewed deletion");
    assert_eq!(r.method, Method::Full);
    let why = r.fallback.as_deref().unwrap_or_default();
    assert!(why.starts_with("the deletion visited"), "{why}");
}

/// The same with heads that depend on the other atom: the evaluations of the deleted
/// facts, 256 at a time, each find the same 100,000 heads, and the bound counts the
/// instances of every evaluation as they match.
#[test]
fn the_consequences_of_a_deletion_stop_across_evaluations() {
    let r = delete_from_a_product("(?y ex:r ex:on)", 512, None);
    assert_eq!(r.method, Method::Full);
    let why = r.fallback.as_deref().unwrap_or_default();
    assert!(why.starts_with("the deletion "), "{why}");
}

/// A run with the change of the inferred graph reported.
fn run_reporting(s: &Store, p: &Profile, since: Option<u64>, cache: &Cache) -> ReasonReport {
    materialize_incremental(
        s,
        p,
        &Extras::default(),
        Incremental {
            since,
            cache: Some(cache),
        },
        &ReasonOptions {
            inferred_change: true,
            ..ReasonOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

/// The reported change of the inferred graph, as text, against the difference of the
/// inferred graph before and after the run.
fn check_change(
    s: &Store,
    r: &ReasonReport,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    what: &str,
) {
    let Some(change) = &r.inferred_change else {
        assert_eq!(
            r.method,
            Method::Full,
            "{what}: an incremental run reports its change"
        );
        return;
    };
    assert_eq!(
        r.method,
        Method::Incremental,
        "{what}: a full run reports no change"
    );
    let snap = s.snapshot();
    assert_eq!(
        r.receipt.as_ref().unwrap().commit.seq,
        snap.commit,
        "{what}: the change is in the ids of the committed snapshot"
    );
    let g = sparkles_core::id::Id::DEFAULT_GRAPH;
    let (mut added, mut removed) = (BTreeSet::new(), BTreeSet::new());
    for (t, w) in change {
        let q = snap.quad_to_terms(&[t[0], t[1], t[2], g]).unwrap();
        let text = Triple::new(q.subject, q.predicate, q.object).to_string();
        let fresh = match w {
            1 => added.insert(text),
            -1 => removed.insert(text),
            _ => panic!("{what}: weight {w}"),
        };
        assert!(fresh, "{what}: a triple twice in the change");
    }
    let want_added: BTreeSet<String> = after.difference(before).cloned().collect();
    let want_removed: BTreeSet<String> = before.difference(after).cloned().collect();
    assert_eq!(added, want_added, "{what}: added");
    assert_eq!(removed, want_removed, "{what}: removed");
    assert_eq!(
        (r.inferred_added, r.inferred_removed),
        (added.len() as u64, removed.len() as u64),
        "{what}: counts"
    );
}

#[test]
fn the_reported_change_is_the_inferred_graph_s_difference() {
    for (p, owl) in profiles() {
        for seed in 1..5 {
            let s = Store::in_memory(StoreOptions::default());
            let cache = Cache::default();
            let mut r = Rng(seed);
            let first: Vec<Triple> = (0..30).map(|_| random_triple(&mut r, owl)).collect();
            change(&s, &first, &[]);
            let rep = run_reporting(&s, &p, None, &cache);
            assert_eq!(rep.inferred_change, None, "{p}: a first run is a full one");
            let mut since = rep.receipt.unwrap().commit.seq;
            let mut inc = 0;
            for step in 0..10 {
                let now = base(&s);
                let del: Vec<Triple> = (0..r.below(4))
                    .filter(|_| !now.is_empty())
                    .map(|_| now[r.below(now.len())].clone())
                    .collect();
                // some insertions make derived triples explicit
                let derived: Vec<String> = inferred(&s).into_iter().collect();
                let mut add: Vec<Triple> = (0..r.below(5))
                    .map(|_| random_triple(&mut r, owl))
                    .collect();
                if !derived.is_empty() && r.below(2) == 0 {
                    let pick = &derived[r.below(derived.len())];
                    if let Some(t) = expected_triples(&s, &p)
                        .into_iter()
                        .find(|t| &t.to_string() == pick)
                    {
                        add.push(t);
                    }
                }
                let before = inferred(&s);
                change(&s, &add, &del);
                let rep = run_reporting(&s, &p, Some(since), &cache);
                let what = format!("{p} seed {seed} step {step}");
                check(&s, &p, &rep, &what);
                check_change(&s, &rep, &before, &inferred(&s), &what);
                if rep.method == Method::Incremental {
                    inc += 1;
                }
                since = rep.receipt.unwrap().commit.seq;
            }
            assert!(inc > 0, "{p} seed {seed}: no incremental run");
        }
    }
}

/// What a full run would write now, as triples.
fn expected_triples(s: &Store, p: &Profile) -> Vec<Triple> {
    infer(s.snapshot(), p, &ReasonOptions::default()).unwrap().0
}

#[test]
fn a_derived_triple_made_explicit_leaves_the_reported_change() {
    let s = Store::in_memory(StoreOptions::default());
    ttl(
        &s,
        "ex:B rdfs:subClassOf ex:A . ex:x a ex:B . ex:y a ex:B .",
    );
    let p = Profile::Rdfs;
    let cache = Cache::default();
    let r = run_reporting(&s, &p, None, &cache);
    let mut since = r.receipt.unwrap().commit.seq;
    let a = t("ex:x", "rdf:type", "ex:A");
    assert!(inferred(&s).contains(&a.to_string()));
    // the derived triple becomes explicit: it leaves the inferred graph
    let before = inferred(&s);
    change(&s, std::slice::from_ref(&a), &[]);
    let r = run_reporting(&s, &p, Some(since), &cache);
    check_change(&s, &r, &before, &inferred(&s), "made explicit");
    let change_text = |r: &ReasonReport| r.inferred_change.as_ref().map(Vec::len);
    assert_eq!(change_text(&r), Some(1), "{:?}", r.inferred_change);
    since = r.receipt.unwrap().commit.seq;
    // and comes back when it stops being explicit
    let before = inferred(&s);
    change(&s, &[], &[a]);
    let r = run_reporting(&s, &p, Some(since), &cache);
    check_change(&s, &r, &before, &inferred(&s), "explicit again derived");
    assert_eq!(change_text(&r), Some(1), "{:?}", r.inferred_change);
}

/// A maintained OWL 2 RL closure over lists equals a full run after each step of a
/// history of list edits (item 839's plan, §2): a cell added, removed and moved, and a
/// tail that two lists share, beside ordinary insertions and deletions, with the closure
/// kept in memory and read back from the dataset.
#[test]
fn a_maintained_history_of_list_edits_equals_full_runs() {
    let rdf_nil = format!("{RDF}nil");
    let nil = rdf_nil.as_str();
    let first = "rdf:first";
    let rest = "rdf:rest";
    // ex:C0 is the intersection of the list ex:l0 = (C1 C2), ex:C5 the union of ex:m0, a
    // chain ex:q0 is the list ex:k0 = (p0 p1), and a key of ex:C3 is the list ex:h0 = (p2)
    let setup = vec![
        t("ex:C0", "owl:intersectionOf", "ex:l0"),
        t("ex:l0", first, "ex:C1"),
        t("ex:l0", rest, "ex:l1"),
        t("ex:l1", first, "ex:C2"),
        t("ex:l1", rest, nil),
        t("ex:C5", "owl:unionOf", "ex:m0"),
        t("ex:m0", first, "ex:C4"),
        t("ex:m0", rest, "ex:l1"),
        t("ex:q0", "owl:propertyChainAxiom", "ex:k0"),
        t("ex:k0", first, "ex:p0"),
        t("ex:k0", rest, "ex:k1"),
        t("ex:k1", first, "ex:p1"),
        t("ex:k1", rest, nil),
        t("ex:C3", "owl:hasKey", "ex:h0"),
        t("ex:h0", first, "ex:p2"),
        t("ex:h0", rest, nil),
        t("ex:i0", "rdf:type", "ex:C1"),
        t("ex:i0", "rdf:type", "ex:C2"),
        t("ex:i1", "rdf:type", "ex:C4"),
        t("ex:i0", "ex:p0", "ex:i1"),
        t("ex:i1", "ex:p1", "ex:i2"),
        t("ex:i3", "rdf:type", "ex:C3"),
        t("ex:i4", "rdf:type", "ex:C3"),
        t("ex:i3", "ex:p2", "ex:i5"),
        t("ex:i4", "ex:p2", "ex:i5"),
    ];
    let steps: Vec<(&str, Vec<Triple>, Vec<Triple>)> = vec![
        // a cell added at the end of the shared tail: (C1 C2 C6) and (C4 C2 C6)
        (
            "add a cell",
            vec![
                t("ex:l1", rest, "ex:l2"),
                t("ex:l2", first, "ex:C6"),
                t("ex:l2", rest, nil),
                t("ex:i0", "rdf:type", "ex:C6"),
            ],
            vec![t("ex:l1", rest, nil)],
        ),
        (
            "an ordinary insertion",
            vec![t("ex:i6", "rdf:type", "ex:C0")],
            vec![],
        ),
        // the cell removed again
        (
            "remove a cell",
            vec![t("ex:l1", rest, nil)],
            vec![
                t("ex:l1", rest, "ex:l2"),
                t("ex:l2", first, "ex:C6"),
                t("ex:l2", rest, nil),
            ],
        ),
        // the chain's second cell moved to a new node with another property
        (
            "move a cell",
            vec![
                t("ex:k0", rest, "ex:k2"),
                t("ex:k2", first, "ex:p3"),
                t("ex:k2", rest, nil),
                t("ex:i1", "ex:p3", "ex:i7"),
            ],
            vec![t("ex:k0", rest, "ex:k1")],
        ),
        // the key grows a second property, which the individuals share
        (
            "grow a key",
            vec![
                t("ex:h0", rest, "ex:h1"),
                t("ex:h1", first, "ex:p4"),
                t("ex:h1", rest, nil),
                t("ex:i3", "ex:p4", "ex:i8"),
                t("ex:i4", "ex:p4", "ex:i8"),
            ],
            vec![t("ex:h0", rest, nil)],
        ),
        (
            "an ordinary deletion",
            vec![],
            vec![t("ex:i0", "rdf:type", "ex:C2")],
        ),
    ];
    let p = Profile::OwlRl;
    for kept in [true, false] {
        let (_d, s) = persistent();
        let cache = Cache::default();
        let cache = kept.then_some(&cache);
        change(&s, &setup, &[]);
        let rep = run(&s, &p, None, cache);
        check(&s, &p, &rep, "first run");
        let mut since = rep.receipt.unwrap().commit.seq;
        for (what, add, del) in &steps {
            change(&s, add, del);
            let rep = run(&s, &p, Some(since), cache);
            assert_eq!(
                rep.method,
                Method::Incremental,
                "{what} (kept {kept}): {:?}",
                rep.fallback
            );
            check(&s, &p, &rep, &format!("{what} (kept {kept})"));
            since = rep.receipt.unwrap().commit.seq;
        }
    }
}

/// Operator-maintained closure relations (F23 Phase 4, item 837 / item 521 second half):
/// splitting a symmetric+transitive clique by deleting its bridge edge, and deleting the
/// `owl:TransitiveProperty` guard triple, both stay incremental and match full runs and
/// emit exact Z-set changes.
#[test]
fn operator_maintained_clique_bridge_split_and_guard_deletion() {
    let (_d, s) = persistent();
    let mut text = String::from("ex:near a owl:SymmetricProperty, owl:TransitiveProperty .\n");
    for base_i in [0usize, 100] {
        for i in 0..100 {
            let a = base_i + i;
            let b = base_i + (i + 1) % 100;
            text += &format!("ex:p{a} ex:near ex:p{b} .\n");
        }
    }
    let bridge = t("ex:p0", "ex:near", "ex:p100");
    text += "ex:p0 ex:near ex:p100 .\n";
    ttl(&s, &text);
    let p = Profile::OwlRl;
    let cache = Cache::default();
    let full = run_reporting(&s, &p, None, &cache);
    check(&s, &p, &full, "clique bridge initial");
    let before = inferred(&s);
    change(&s, &[], &[bridge]);
    let split = run_reporting(&s, &p, Some(full.receipt.unwrap().commit.seq), &cache);
    assert_eq!(split.method, Method::Incremental, "{:?}", split.fallback);
    check(&s, &p, &split, "clique bridge split");
    let after = inferred(&s);
    check_change(&s, &split, &before, &after, "clique bridge split change");
    // Deleting the TransitiveProperty guard removes all multi-hop pairs while keeping
    // 1-hop symmetric pairs, incrementally.
    let before = after;
    change(
        &s,
        &[],
        &[t("ex:near", "rdf:type", "owl:TransitiveProperty")],
    );
    let guard_del = run_reporting(&s, &p, Some(split.receipt.unwrap().commit.seq), &cache);
    assert_eq!(
        guard_del.method,
        Method::Incremental,
        "{:?}",
        guard_del.fallback
    );
    check(&s, &p, &guard_del, "guard deletion");
    let after = inferred(&s);
    check_change(&s, &guard_del, &before, &after, "guard deletion change");
}

/// Operator-maintained closure relations with mixed rules: a subproperty feeding a
/// transitive property (`prp-spo1` + `prp-trp`), an `owl:sameAs` equivalence class split
/// and re-merge, and a directed `rdfs:subClassOf` diamond all stay incremental and match
/// full runs.
#[test]
fn operator_maintained_mixed_subproperty_and_equivalence() {
    let (_d, s) = persistent();
    ttl(
        &s,
        "ex:sub rdfs:subPropertyOf ex:tr .
         ex:tr a owl:TransitiveProperty .
         ex:tr rdfs:domain ex:Dom .
         ex:a ex:sub ex:b .
         ex:b ex:tr ex:c .
         ex:a ex:tr ex:c .
         ex:x owl:sameAs ex:y .
         ex:y owl:sameAs ex:z .
         ex:x ex:tag ex:v .
         ex:C3 rdfs:subClassOf ex:C2 .
         ex:C2 rdfs:subClassOf ex:C1 .
         ex:C3 rdfs:subClassOf ex:C4 .
         ex:C4 rdfs:subClassOf ex:C1 .
         ex:inst a ex:C3 .",
    );
    let p = Profile::OwlRl;
    let cache = Cache::default();
    let full = run_reporting(&s, &p, None, &cache);
    check(&s, &p, &full, "mixed initial");
    let mut since = full.receipt.unwrap().commit.seq;
    let steps: Vec<(&str, Vec<Triple>, Vec<Triple>)> = vec![
        // Deleting explicit `a tr c` while `a sub b` + `b tr c` still derives it
        (
            "delete shortcut kept via subproperty",
            vec![],
            vec![t("ex:a", "ex:tr", "ex:c")],
        ),
        // Deleting `a sub b` now kills `a tr b`, `a tr c`, and `a a ex:Dom`
        (
            "delete subproperty edge",
            vec![],
            vec![t("ex:a", "ex:sub", "ex:b")],
        ),
        // Split sameAs class: `z` loses `ex:tag ex:v`
        (
            "split sameAs class",
            vec![],
            vec![t("ex:y", "owl:sameAs", "ex:z")],
        ),
        // Re-merge sameAs class via late-edge operator mode
        (
            "re-merge sameAs class",
            vec![t("ex:x", "owl:sameAs", "ex:z")],
            vec![],
        ),
        // Delete one branch of subClassOf diamond: `C3 subClassOf C1` survives via C4
        (
            "delete diamond branch",
            vec![],
            vec![t("ex:C2", "rdfs:subClassOf", "ex:C1")],
        ),
        // Delete second branch: `C3 subClassOf C1` and `inst a C1` disappear
        (
            "delete second branch",
            vec![],
            vec![t("ex:C4", "rdfs:subClassOf", "ex:C1")],
        ),
    ];
    for (what, add, del) in &steps {
        let before = inferred(&s);
        change(&s, add, del);
        let rep = run_reporting(&s, &p, Some(since), &cache);
        assert_eq!(
            rep.method,
            Method::Incremental,
            "{what}: {:?}",
            rep.fallback
        );
        check(&s, &p, &rep, what);
        let after = inferred(&s);
        check_change(&s, &rep, &before, &after, what);
        since = rep.receipt.unwrap().commit.seq;
    }
}

/// Per-list tracking (F16 §7.3) with `listLength`, `listNotContains`, `listEntry` and
/// `listForAll`: adding and removing cells stays incremental and matches full runs.
#[test]
fn changed_lists_with_nonmonotonic_list_builtins_stay_incremental() {
    let rdf_nil = format!("{RDF}nil");
    let nil = rdf_nil.as_str();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [len: (?c ex:items ?l), listLength(?l, ?n) -> (?c ex:count ?n)]
         [notc: (?c ex:items ?l), listNotContains(?l, ex:bad) -> (?c ex:ok ex:yes)]
         [ent: (?c ex:items ?l), listEntry(?l, 1, ?x) -> (?c ex:second ?x)]"
            .into(),
    );
    let (_d, s) = persistent();
    let cache = Cache::default();
    change(
        &s,
        &[
            t("ex:c0", "ex:items", "ex:l0"),
            t("ex:l0", "rdf:first", "ex:a"),
            t("ex:l0", "rdf:rest", "ex:l1"),
            t("ex:l1", "rdf:first", "ex:b"),
            t("ex:l1", "rdf:rest", nil),
        ],
        &[],
    );
    let full = run(&s, &rules, None, Some(&cache));
    check(&s, &rules, &full, "list builtins initial");
    let mut since = full.receipt.unwrap().commit.seq;
    let steps: Vec<(&str, Vec<Triple>, Vec<Triple>)> = vec![
        // Append ex:bad -> count changes 2->3, ex:ok is removed!
        (
            "append ex:bad",
            vec![
                t("ex:l1", "rdf:rest", "ex:l2"),
                t("ex:l2", "rdf:first", "ex:bad"),
                t("ex:l2", "rdf:rest", nil),
            ],
            vec![t("ex:l1", "rdf:rest", nil)],
        ),
        // Remove ex:bad -> count changes 3->2, ex:ok returns!
        (
            "remove ex:bad",
            vec![t("ex:l1", "rdf:rest", nil)],
            vec![
                t("ex:l1", "rdf:rest", "ex:l2"),
                t("ex:l2", "rdf:first", "ex:bad"),
                t("ex:l2", "rdf:rest", nil),
            ],
        ),
        // Replace second element ex:b -> ex:c
        (
            "replace second element",
            vec![t("ex:l1", "rdf:first", "ex:c")],
            vec![t("ex:l1", "rdf:first", "ex:b")],
        ),
    ];
    for (what, add, del) in &steps {
        change(&s, add, del);
        let rep = run(&s, &rules, Some(since), Some(&cache));
        assert_eq!(
            rep.method,
            Method::Incremental,
            "{what}: {:?}",
            rep.fallback
        );
        check(&s, &rules, &rep, what);
        since = rep.receipt.unwrap().commit.seq;
    }
}

/// Derived `owl:TransitiveProperty` guards (through `owl:sameAs` + `eq-rep-s`, through a
/// custom fixed transitive predicate `ex:r`, and through `rdfs:subClassOf` + `cax-sco`)
/// must keep the predicate governed for retractions and saturation without poisoning
/// `Search::unprovable`.
#[test]
fn derived_transitive_guard_retractions_and_shortcuts_match_full_runs() {
    // 1. OWL 2 RL: `ex:p` derives `owl:TransitiveProperty` via `owl:sameAs` + `eq-rep-s`
    {
        let (_d, s) = persistent();
        ttl(
            &s,
            "ex:p owl:sameAs ex:q .
             ex:q a owl:TransitiveProperty .
             ex:a ex:p ex:c .
             ex:c ex:p ex:d .",
        );
        let p = Profile::OwlRl;
        let cache = Cache::default();
        let full = run_reporting(&s, &p, None, &cache);
        check(&s, &p, &full, "sameAs derived guard initial");
        // Delete chain edge `ex:a ex:p ex:c` -> `ex:a ex:p ex:d` must be retracted!
        let before = inferred(&s);
        change(&s, &[], &[t("ex:a", "ex:p", "ex:c")]);
        let r1 = run_reporting(&s, &p, Some(full.receipt.unwrap().commit.seq), &cache);
        assert_eq!(r1.method, Method::Incremental, "{:?}", r1.fallback);
        check(&s, &p, &r1, "sameAs derived guard delete chain edge");
        let after = inferred(&s);
        assert!(
            !after.contains(&t("ex:a", "ex:p", "ex:d").to_string()),
            "stale transitive edge remained"
        );
        check_change(&s, &r1, &before, &after, "sameAs derived guard chain delta");

        // Add shortcut + detour, then delete explicit shortcut `ex:a ex:p ex:c`:
        // `ex:a ex:p ex:c` and `ex:a ex:p ex:d` must survive via `dirty_preds` saturation.
        change(
            &s,
            &[
                t("ex:a", "ex:p", "ex:b"),
                t("ex:b", "ex:p", "ex:c"),
                t("ex:a", "ex:p", "ex:c"),
            ],
            &[],
        );
        let r2 = run_reporting(&s, &p, Some(r1.receipt.unwrap().commit.seq), &cache);
        assert_eq!(r2.method, Method::Incremental, "{:?}", r2.fallback);
        check(&s, &p, &r2, "sameAs derived guard insert detour");
        let before = inferred(&s);
        change(&s, &[], &[t("ex:a", "ex:p", "ex:c")]);
        let r3 = run_reporting(&s, &p, Some(r2.receipt.unwrap().commit.seq), &cache);
        assert_eq!(r3.method, Method::Incremental, "{:?}", r3.fallback);
        check(&s, &p, &r3, "sameAs derived guard delete shortcut");
        let after = inferred(&s);
        assert!(
            after.contains(&t("ex:a", "ex:p", "ex:c").to_string()),
            "shortcut lost"
        );
        assert!(
            after.contains(&t("ex:a", "ex:p", "ex:d").to_string()),
            "chained fact lost"
        );
        check_change(
            &s,
            &r3,
            &before,
            &after,
            "sameAs derived guard shortcut delta",
        );
    }

    // 2. Custom rule deriving `owl:TransitiveProperty` from a fixed transitive predicate `ex:r`
    {
        let rules = Profile::Rules(
            "@prefix ex: <http://ex.org/>.
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>.
             @prefix owl: <http://www.w3.org/2002/07/owl#>.
             [r-tr: (?x ex:r ?y), (?y ex:r ?z) -> (?x ex:r ?z)]
             [decl: (ex:u ex:r ex:w) -> (ex:q rdf:type owl:TransitiveProperty)]
             [tr: (?p rdf:type owl:TransitiveProperty), (?x ?p ?y), (?y ?p ?z) -> (?x ?p ?z)]"
                .into(),
        );
        let (_d, s) = persistent();
        ttl(
            &s,
            "ex:u ex:r ex:v .
             ex:v ex:r ex:w .
             ex:a ex:q ex:b .
             ex:b ex:q ex:c .",
        );
        let cache = Cache::default();
        let full = run_reporting(&s, &rules, None, &cache);
        check(&s, &rules, &full, "fixed-op derived guard initial");
        let before = inferred(&s);
        change(&s, &[], &[t("ex:a", "ex:q", "ex:b")]);
        let r = run_reporting(&s, &rules, Some(full.receipt.unwrap().commit.seq), &cache);
        assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
        check(&s, &rules, &r, "fixed-op derived guard delete edge");
        let after = inferred(&s);
        assert!(
            !after.contains(&t("ex:a", "ex:q", "ex:c").to_string()),
            "stale transitive fact remained"
        );
        check_change(&s, &r, &before, &after, "fixed-op derived guard delta");
    }

    // 3. OWL 2 RL: `rdfs:subClassOf` + `cax-sco` deriving `owl:TransitiveProperty`
    {
        let (_d, s) = persistent();
        ttl(
            &s,
            "ex:MyTrans rdfs:subClassOf owl:TransitiveProperty .
             ex:p a ex:MyTrans .
             ex:a ex:p ex:b .
             ex:b ex:p ex:c .",
        );
        let p = Profile::OwlRl;
        let cache = Cache::default();
        let full = run_reporting(&s, &p, None, &cache);
        check(&s, &p, &full, "subClassOf derived guard initial");
        let before = inferred(&s);
        change(&s, &[], &[t("ex:a", "ex:p", "ex:b")]);
        let r1 = run_reporting(&s, &p, Some(full.receipt.unwrap().commit.seq), &cache);
        assert_eq!(r1.method, Method::Incremental, "{:?}", r1.fallback);
        check(&s, &p, &r1, "subClassOf derived guard delete edge");
        check_change(
            &s,
            &r1,
            &before,
            &inferred(&s),
            "subClassOf derived guard edge delta",
        );
        // Re-add edge and delete the subclass declaration
        change(&s, &[t("ex:a", "ex:p", "ex:b")], &[]);
        let r2 = run_reporting(&s, &p, Some(r1.receipt.unwrap().commit.seq), &cache);
        let before = inferred(&s);
        change(&s, &[], &[t("ex:p", "rdf:type", "ex:MyTrans")]);
        let r3 = run_reporting(&s, &p, Some(r2.receipt.unwrap().commit.seq), &cache);
        assert_eq!(r3.method, Method::Incremental, "{:?}", r3.fallback);
        check(&s, &p, &r3, "subClassOf derived guard delete typing");
        check_change(
            &s,
            &r3,
            &before,
            &inferred(&s),
            "subClassOf derived guard typing delta",
        );
    }
}

/// Overlapping closure operators on the same predicate: an irreflexive transitive rule
/// listed before an unfiltered transitive rule must still retract reflexive pairs produced
/// by the unfiltered rule.
#[test]
fn overlapping_transitive_operators_retract_reflexive_pairs() {
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [tr-irref: (?x ex:q ?y), (?y ex:q ?z), notEqual(?x, ?z) -> (?x ex:q ?z)]
         [tr-full: (?x ex:q ?y), (?y ex:q ?z) -> (?x ex:q ?z)]"
            .into(),
    );
    let (_d, s) = persistent();
    ttl(&s, "ex:a ex:q ex:b . ex:b ex:q ex:a .");
    let cache = Cache::default();
    let full = run_reporting(&s, &rules, None, &cache);
    check(&s, &rules, &full, "overlapping ops initial");
    let before = inferred(&s);
    assert!(before.contains(&t("ex:a", "ex:q", "ex:a").to_string()));
    assert!(before.contains(&t("ex:b", "ex:q", "ex:b").to_string()));
    change(&s, &[], &[t("ex:a", "ex:q", "ex:b")]);
    let r = run_reporting(&s, &rules, Some(full.receipt.unwrap().commit.seq), &cache);
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &rules, &r, "overlapping ops delete");
    let after = inferred(&s);
    assert!(!after.contains(&t("ex:a", "ex:q", "ex:a").to_string()));
    assert!(!after.contains(&t("ex:b", "ex:q", "ex:b").to_string()));
    check_change(&s, &r, &before, &after, "overlapping ops delta");
}

/// Multi-graph ordered RDF list edits: both removing an earlier graph's copy of a
/// duplicated `rdf:first` (`base_moved`) and removing two competing objects from different
/// graphs must agree with full runs for cached and dataset-backed closures.
#[test]
fn ordered_multi_graph_list_edits_match_full_runs() {
    use sparkles_reasoner::{GraphRef, Inputs};
    let g1 = format!("{EX}g1");
    let g2 = format!("{EX}g2");
    let g3 = format!("{EX}g3");
    let inputs = Inputs {
        data_graphs: vec![
            GraphRef::Named(g1.clone()),
            GraphRef::Named(g2.clone()),
            GraphRef::Named(g3.clone()),
        ],
        ..Default::default()
    };
    let ropts = ReasonOptions {
        inputs,
        ..Default::default()
    };
    let extras = Extras::default();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [m: (?c ex:items ?l), listMember(?l, ?x) -> (?c ex:has ?x)]"
            .into(),
    );
    let q_in = |tr: &Triple, g: &str| {
        Quad::new(
            tr.subject.clone(),
            tr.predicate.clone(),
            tr.object.clone(),
            GraphName::NamedNode(iri(g.to_string())),
        )
    };
    let commit_quads = |s: &Store, add: &[Quad], del: &[Quad]| {
        let mut txn = s.write();
        let mut labels = Default::default();
        for q in del {
            let enc = txn.encode_quad(q, &mut labels).unwrap();
            txn.delete(enc).unwrap();
        }
        for q in add {
            let enc = txn.encode_quad(q, &mut labels).unwrap();
            txn.insert(enc).unwrap();
        }
        txn.commit().unwrap();
    };
    let full_expected = |s: &Store| -> BTreeSet<String> {
        let (ts, _) = infer(s.snapshot(), &rules, &ropts).unwrap();
        ts.iter().map(|t| t.to_string()).collect()
    };
    let rdf_nil = format!("{RDF}nil");
    let nil = rdf_nil.as_str();

    for kept in [true, false] {
        // Case 1: removing G1's copy of `(l rdf:first a)` while G3 also holds it (`base_moved`)
        // changes the earliest object from `a` to `b` (in G2).
        let (_d, s) = persistent();
        let cache = Cache::default();
        let cache_opt = kept.then_some(&cache);
        commit_quads(
            &s,
            &[
                q_in(&t("ex:l", "rdf:first", "ex:a"), &g1),
                q_in(&t("ex:l", "rdf:first", "ex:b"), &g2),
                q_in(&t("ex:l", "rdf:first", "ex:a"), &g3),
                q_in(&t("ex:l", "rdf:rest", nil), &g3),
                q_in(&t("ex:c", "ex:items", "ex:l"), &g3),
            ],
            &[],
        );
        let r0 = materialize_incremental(
            &s,
            &rules,
            &extras,
            Incremental {
                since: None,
                cache: cache_opt,
            },
            &ropts,
        )
        .unwrap();
        assert_eq!(
            inferred(&s),
            full_expected(&s),
            "case 1 initial (kept {kept})"
        );
        commit_quads(&s, &[], &[q_in(&t("ex:l", "rdf:first", "ex:a"), &g1)]);
        let r1 = materialize_incremental(
            &s,
            &rules,
            &extras,
            Incremental {
                since: Some(r0.receipt.unwrap().commit.seq),
                cache: cache_opt,
            },
            &ropts,
        )
        .unwrap();
        assert_eq!(r1.method, Method::Full);
        assert!(
            r1.fallback
                .as_deref()
                .is_some_and(|f| f.contains("moved across input graphs")),
            "case 1 unexpected fallback (kept {kept}): {:?}",
            r1.fallback
        );
        assert_eq!(
            inferred(&s),
            full_expected(&s),
            "case 1 base_moved list edit (kept {kept})"
        );
        assert!(inferred(&s).contains(&t("ex:c", "ex:has", "ex:b").to_string()));
        assert!(!inferred(&s).contains(&t("ex:c", "ex:has", "ex:a").to_string()));

        // Case 2: remove both competing `rdf:first` objects from G1 and G2 where G2's object
        // has a lower term id than G1's object.
        let (_d2, s2) = persistent();
        let cache2 = Cache::default();
        let cache2_opt = kept.then_some(&cache2);
        // Intern `ex:a_low` first so its term ID is strictly lower than `ex:z_high`.
        commit_quads(
            &s2,
            &[
                q_in(&t("ex:l", "rdf:first", "ex:a_low"), &g2),
                q_in(&t("ex:l", "rdf:first", "ex:z_high"), &g1),
                q_in(&t("ex:l", "rdf:rest", nil), &g3),
                q_in(&t("ex:c", "ex:items", "ex:l"), &g3),
            ],
            &[],
        );
        let r0 = materialize_incremental(
            &s2,
            &rules,
            &extras,
            Incremental {
                since: None,
                cache: cache2_opt,
            },
            &ropts,
        )
        .unwrap();
        assert_eq!(
            inferred(&s2),
            full_expected(&s2),
            "case 2 initial (kept {kept})"
        );
        commit_quads(
            &s2,
            &[],
            &[
                q_in(&t("ex:l", "rdf:first", "ex:z_high"), &g1),
                q_in(&t("ex:l", "rdf:first", "ex:a_low"), &g2),
            ],
        );
        let r1 = materialize_incremental(
            &s2,
            &rules,
            &extras,
            Incremental {
                since: Some(r0.receipt.unwrap().commit.seq),
                cache: cache2_opt,
            },
            &ropts,
        )
        .unwrap();
        assert_eq!(r1.method, Method::Full);
        assert!(
            r1.fallback
                .as_deref()
                .is_some_and(|f| f.contains("branching RDF list")),
            "case 2 unexpected fallback (kept {kept}): {:?}",
            r1.fallback
        );
        assert_eq!(
            inferred(&s2),
            full_expected(&s2),
            "case 2 competing multi-graph removal (kept {kept})"
        );
        assert!(inferred(&s2).is_empty());
    }
}

/// Deleting ordinary instance `rdf:type` triples under `Profile::OwlRl` must respect
/// constant object slots on `prp-symp`/`prp-fp`/`prp-ifp` in `estimate_deletion_visits`
/// and remain incremental instead of triggering a false full-run fallback.
#[test]
fn owl_rl_instance_type_deletions_stay_incremental() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    // 10 predicates (~1,000 facts each -> avg_p ~ 1,000) plus 1,200 ordinary rdf:type rows.
    for p_idx in 0..9 {
        for i in 0..1_000 {
            data += &format!("<{EX}s{i}> <{EX}p{p_idx}> <{EX}o{}> .\n", i % 50);
        }
    }
    for i in 0..1_200 {
        data += &format!("<{EX}ind{i}> <{RDF}type> <{EX}Student> .\n");
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let p = Profile::OwlRl;
    let cache = Cache::default();
    let full = run(&s, &p, None, Some(&cache));
    let mut since = full.receipt.unwrap().commit.seq;

    // Small deletion (50 rdf:type triples)
    let small: Vec<Triple> = (0..50)
        .map(|i| t(&format!("ex:ind{i}"), "rdf:type", "ex:Student"))
        .collect();
    change(&s, &[], &small);
    let r1 = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r1.method, Method::Incremental, "{:?}", r1.fallback);
    check(&s, &p, &r1, "owl-rl small rdf:type deletion");
    since = r1.receipt.unwrap().commit.seq;

    // Larger deletion (600 rdf:type triples, > 5% of total facts)
    let large: Vec<Triple> = (50..650)
        .map(|i| t(&format!("ex:ind{i}"), "rdf:type", "ex:Student"))
        .collect();
    change(&s, &[], &large);
    let r2 = run(&s, &p, Some(since), Some(&cache));
    assert_eq!(r2.method, Method::Incremental, "{:?}", r2.fallback);
    check(&s, &p, &r2, "owl-rl >5% rdf:type deletion");
}

/// `revive_overdeleted` path: a non-operator rule (`cls-svf1`) reads a surviving 2-hop
/// mixed transitive fact (`ex:a ex:tr ex:c`, derived from `prp-spo1` + `prp-trp`, not
/// explicit `ex:tr` edges alone) while an explicit copy of the head (`ex:a a ex:Reach`)
/// is deleted.
#[test]
fn revive_overdeleted_surviving_mixed_transitive_support() {
    let (_d, s) = persistent();
    ttl(
        &s,
        "ex:sub rdfs:subPropertyOf ex:tr .
         ex:tr a owl:TransitiveProperty .
         ex:Reach owl:someValuesFrom ex:Target ; owl:onProperty ex:tr .
         ex:c a ex:Target .
         ex:a ex:sub ex:b .
         ex:b ex:tr ex:c .
         ex:a a ex:Reach .",
    );
    let p = Profile::OwlRl;
    let cache = Cache::default();
    let full = run_reporting(&s, &p, None, &cache);
    check(&s, &p, &full, "revive_overdeleted initial");
    let before = inferred(&s);
    change(&s, &[], &[t("ex:a", "rdf:type", "ex:Reach")]);
    let r = run_reporting(&s, &p, Some(full.receipt.unwrap().commit.seq), &cache);
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    check(&s, &p, &r, "revive_overdeleted delete explicit head");
    let after = inferred(&s);
    assert!(
        after.contains(&t("ex:a", "rdf:type", "ex:Reach").to_string()),
        "head supported by surviving mixed transitive fact must be revived"
    );
    check_change(&s, &r, &before, &after, "revive_overdeleted delta");
}

/// List replay (`eval_list_rules_new`) and revival (`revive_overdeleted`) enforce
/// `max_inferred` across all derived facts admitted by the incremental pass.
#[test]
fn list_replay_and_revival_enforce_max_inferred() {
    let rdf_nil = format!("{RDF}nil");
    let nil = rdf_nil.as_str();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [r1: (?c ex:items ?l), listMember(?l, ex:x) -> (?c ex:hit1 ex:ok)]
         [r2: (?c ex:items ?l), listMember(?l, ex:x) -> (?c ex:hit2 ex:ok)]"
            .into(),
    );
    let (_d, s) = persistent();
    let cache = Cache::default();
    change(
        &s,
        &[
            t("ex:c0", "ex:items", "ex:l0"),
            t("ex:l0", "rdf:first", "ex:y"),
            t("ex:l0", "rdf:rest", nil),
        ],
        &[],
    );
    let r0 = run(&s, &rules, None, Some(&cache));
    let since = r0.receipt.unwrap().commit.seq;
    // Edit list so both rules derive 1 distinct triple each (2 total) while max_inferred = 1
    change(
        &s,
        &[t("ex:l0", "rdf:first", "ex:x")],
        &[t("ex:l0", "rdf:first", "ex:y")],
    );
    let opts = ReasonOptions {
        max_inferred: 1,
        ..ReasonOptions::default()
    };
    let err = materialize_incremental(
        &s,
        &rules,
        &Extras::default(),
        Incremental {
            since: Some(since),
            cache: Some(&cache),
        },
        &opts,
    )
    .expect_err("list replay producing 2 triples must fail when max_inferred = 1");
    assert!(
        err.to_string().contains("inferred") || err.to_string().contains("limit"),
        "unexpected error: {err:#}"
    );

    // Revival path (`revive_overdeleted`) must also enforce `max_inferred`: `ex:a a ex:Reach`
    // depends only on the surviving 2-hop mixed transitive edge `ex:a ex:tr ex:c`, so
    // Phase 1 over-deletes it and `revive_overdeleted` attempts to revive it.
    let (_d2, s2) = persistent();
    let cache2 = Cache::default();
    ttl(
        &s2,
        "ex:sub rdfs:subPropertyOf ex:tr .
         ex:tr a owl:TransitiveProperty .
         ex:Reach owl:someValuesFrom ex:Target ; owl:onProperty ex:tr .
         ex:c a ex:Target .
         ex:a ex:sub ex:b .
         ex:b ex:tr ex:c .
         ex:a a ex:Reach .",
    );
    let r_init = run(&s2, &Profile::OwlRl, None, Some(&cache2));
    change(&s2, &[], &[t("ex:a", "rdf:type", "ex:Reach")]);
    let zero_opts = ReasonOptions {
        max_inferred: 0,
        ..ReasonOptions::default()
    };
    let rev_err = materialize_incremental(
        &s2,
        &Profile::OwlRl,
        &Extras::default(),
        Incremental {
            since: Some(r_init.receipt.unwrap().commit.seq),
            cache: Some(&cache2),
        },
        &zero_opts,
    )
    .expect_err("revival producing a triple must fail when max_inferred = 0");
    assert!(
        rev_err.to_string().contains("inferred") || rev_err.to_string().contains("limit"),
        "unexpected error: {rev_err:#}"
    );
}

/// Cross-rule duplicates during list replay (`eval_list_rules_new`) are deduplicated before
/// enforcing `max_inferred`, so 1,200 subjects derived by two rules with identical heads
/// succeed at `max_inferred = 1200` and fail at `max_inferred = 1199`.
#[test]
fn list_replay_deduplicates_across_rules_at_exact_max_inferred() {
    let rdf_nil = format!("{RDF}nil");
    let nil = rdf_nil.as_str();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [r1: (?c ex:items1 ?l), listMember(?l, ex:x) -> (?c ex:hit ex:ok)]
         [r2: (?c ex:items2 ?l), listMember(?l, ex:x) -> (?c ex:hit ex:ok)]"
            .into(),
    );
    let setup = || {
        let (d, s) = persistent();
        let cache = Cache::default();
        let mut initial: Vec<Triple> = Vec::with_capacity(2_402);
        initial.push(t("ex:l0", "rdf:first", "ex:y"));
        initial.push(t("ex:l0", "rdf:rest", nil));
        for i in 0..1_200 {
            let c = format!("ex:c{i}");
            initial.push(t(&c, "ex:items1", "ex:l0"));
            initial.push(t(&c, "ex:items2", "ex:l0"));
        }
        change(&s, &initial, &[]);
        let r0 = run(&s, &rules, None, Some(&cache));
        let since = r0.receipt.unwrap().commit.seq;
        change(
            &s,
            &[t("ex:l0", "rdf:first", "ex:x")],
            &[t("ex:l0", "rdf:first", "ex:y")],
        );
        (d, s, cache, since)
    };

    // 1. At `max_inferred = 1199`, the 1,200 distinct heads must fail with the limit error.
    let (_d_low, s_low, cache_low, since_low) = setup();
    let too_low = ReasonOptions {
        max_inferred: 1_199,
        ..ReasonOptions::default()
    };
    let err = materialize_incremental(
        &s_low,
        &rules,
        &Extras::default(),
        Incremental {
            since: Some(since_low),
            cache: Some(&cache_low),
        },
        &too_low,
    )
    .expect_err("1,200 distinct heads must fail when max_inferred = 1199");
    assert!(
        err.to_string().contains("inferred") || err.to_string().contains("limit"),
        "unexpected error: {err:#}"
    );

    // 2. At `max_inferred = 1200`, the second rule's 1,200 duplicates must deduplicate
    //    against the first rule's staged output so the incremental run succeeds.
    let (_d, s, cache, since) = setup();
    let exact = ReasonOptions {
        max_inferred: 1_200,
        ..ReasonOptions::default()
    };
    let r1 = materialize_incremental(
        &s,
        &rules,
        &Extras::default(),
        Incremental {
            since: Some(since),
            cache: Some(&cache),
        },
        &exact,
    )
    .expect("exact max_inferred = 1200 with cross-rule duplicates must succeed");
    assert_eq!(r1.method, Method::Incremental, "{:?}", r1.fallback);
    assert_eq!(r1.inferred, 1_200);
    check(&s, &rules, &r1, "exact max_inferred list replay");
}
