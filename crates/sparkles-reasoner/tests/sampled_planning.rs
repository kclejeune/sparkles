//! Differential tests of rule-body planning from sampled bindings (spec F23, roadmap item
//! 487): a run that orders rule bodies from the sample derives exactly the facts that the
//! same run derives with the greedy planner's orders.

use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Extras, INFERRED_GRAPH, Incremental, Profile, ReasonOptions, bench_derive, bench_sampled_plans,
    bench_sampled_search_all, infer, materialize, materialize_incremental,
};
use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap_or_else(|e| panic!("{e:#}\n{ttl}"));
    s
}

/// The count of plans that took a sampled order is process-wide, so the tests that read
/// it run one at a time.
fn count_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The options of a run. The inputs of these tests are small, and a small body keeps
/// the greedy order without a sample, so the tests sample and search every body.
fn options(sampled: bool, closures: bool) -> ReasonOptions {
    bench_sampled_search_all(true);
    ReasonOptions {
        sampled_planning: sampled,
        closure_operators: closures,
        ..ReasonOptions::default()
    }
}

fn facts(s: &Store, p: &Profile, o: &ReasonOptions) -> BTreeSet<String> {
    let (ts, _) = infer(s.snapshot(), p, o).unwrap_or_else(|e| panic!("{e:#}"));
    ts.iter().map(|t| t.to_string()).collect()
}

/// Both planners derive the same facts, generalized ones included. Returns their number.
fn same(s: &Store, p: &Profile, closures: bool, what: &str) -> usize {
    let run = |sampled: bool| {
        let (n, _, digest, _) = bench_derive(s.snapshot(), p, &options(sampled, closures), &|| {})
            .unwrap_or_else(|e| panic!("{what}: {e:#}"));
        (n, digest)
    };
    let (sampled, greedy) = (run(true), run(false));
    if sampled != greedy {
        let (a, b) = (
            facts(s, p, &options(true, closures)),
            facts(s, p, &options(false, closures)),
        );
        let extra: Vec<_> = a.difference(&b).take(10).collect();
        let missing: Vec<_> = b.difference(&a).take(10).collect();
        panic!(
            "{what}, {}: the sampled orders derive {} facts and the greedy orders {}\nonly sampled: {extra:#?}\nonly greedy: {missing:#?}",
            p.name(),
            sampled.0,
            greedy.0
        );
    }
    sampled.0
}

/// xorshift, so that the cases do not depend on a crate
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// A random ontology with its data, made of the axioms whose OWL 2 RL rules have three
/// or more body atoms: keys, restrictions, cardinalities, functional and inverse
/// functional properties and property chains, over classes of uneven sizes.
fn random_ontology(seed: u64) -> String {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let classes = 3 + r.below(6);
    let props = 3 + r.below(5);
    let inds = 10 + r.below(50);
    let mut t = String::new();
    let c = |i: u64| format!("ex:C{i}");
    let p = |i: u64| format!("ex:p{i}");
    let x = |i: u64| format!("ex:i{i}");
    for _ in 0..r.below(classes + 1) {
        t += &format!(
            "{} rdfs:subClassOf {} .\n",
            c(r.below(classes)),
            c(r.below(classes))
        );
    }
    for _ in 0..r.below(props) {
        t += &format!(
            "{} rdfs:subPropertyOf {} .\n",
            p(r.below(props)),
            p(r.below(props))
        );
    }
    for i in 0..props {
        if r.chance(25) {
            t += &format!("{} a owl:FunctionalProperty .\n", p(i));
        }
        if r.chance(25) {
            t += &format!("{} a owl:InverseFunctionalProperty .\n", p(i));
        }
        if r.chance(15) {
            t += &format!("{} a owl:TransitiveProperty .\n", p(i));
        }
        if r.chance(15) {
            t += &format!("{} a owl:SymmetricProperty .\n", p(i));
        }
    }
    for _ in 0..1 + r.below(2) {
        if r.chance(70) {
            t += &format!(
                "{} owl:hasKey ( {} ) .\n",
                c(r.below(classes)),
                p(r.below(props))
            );
        } else {
            t += &format!(
                "{} owl:hasKey ( {} {} ) .\n",
                c(r.below(classes)),
                p(r.below(props)),
                p(r.below(props))
            );
        }
    }
    for k in 0..r.below(4) {
        let (on, filler) = (p(r.below(props)), c(r.below(classes)));
        let kind = match r.below(6) {
            0 => format!("owl:someValuesFrom {filler}"),
            1 => format!("owl:allValuesFrom {filler}"),
            2 => format!("owl:hasValue {}", x(r.below(inds))),
            3 => "owl:maxCardinality \"1\"^^xsd:nonNegativeInteger".to_string(),
            4 => format!(
                "owl:maxQualifiedCardinality \"1\"^^xsd:nonNegativeInteger ; owl:onClass {filler}"
            ),
            _ => "owl:someValuesFrom owl:Thing".to_string(),
        };
        t += &format!("ex:R{k} a owl:Restriction ; owl:onProperty {on} ; {kind} .\n");
        if r.chance(60) {
            t += &format!("{} rdfs:subClassOf ex:R{k} .\n", c(r.below(classes)));
        }
        if r.chance(40) {
            t += &format!("ex:R{k} rdfs:subClassOf {} .\n", c(r.below(classes)));
        }
    }
    if r.chance(50) {
        t += &format!(
            "{} owl:propertyChainAxiom ( {} {} ) .\n",
            p(r.below(props)),
            p(r.below(props)),
            p(r.below(props))
        );
    }
    if r.chance(30) {
        t += &format!(
            "{} owl:propertyChainAxiom ( {} {} {} ) .\n",
            p(r.below(props)),
            p(r.below(props)),
            p(r.below(props)),
            p(r.below(props))
        );
    }
    if r.chance(40) {
        t += &format!(
            "{} owl:intersectionOf ( {} {} ) .\n",
            c(r.below(classes)),
            c(r.below(classes)),
            c(r.below(classes))
        );
    }
    // one large class and several small ones, as a keyed class is among many others
    for i in 0..inds {
        if r.chance(60) {
            t += &format!("{} a {} .\n", x(i), c(0));
        }
        if r.chance(20) {
            t += &format!("{} a {} .\n", x(i), c(1 + r.below(classes - 1)));
        }
    }
    // values few enough that keys and functional properties identify individuals
    let values = 2 + inds / 2;
    for _ in 0..r.below(4 * inds) {
        if r.chance(40) {
            t += &format!(
                "{} {} \"{}\" .\n",
                x(r.below(inds)),
                p(r.below(props)),
                r.below(values)
            );
        } else {
            t += &format!(
                "{} {} {} .\n",
                x(r.below(inds)),
                p(r.below(props)),
                x(r.below(inds))
            );
        }
    }
    t
}

#[test]
fn random_ontologies_derive_the_same_facts() {
    let _one = count_lock();
    let mut derived = 0usize;
    let before = bench_sampled_plans();
    for seed in 0..80 {
        let ttl = random_ontology(seed);
        let s = store(&ttl);
        let what = format!("seed {seed}\n{ttl}");
        derived += same(&s, &Profile::OwlRl, true, &what);
        if seed % 5 == 0 {
            same(&s, &Profile::OwlRl, false, &what);
            same(&s, &Profile::Rdfs, true, &what);
        }
    }
    assert!(derived > 0);
    // the runs took sampled orders, so the comparison is of two different evaluations
    let changed = bench_sampled_plans() - before;
    assert!(changed > 100, "{changed} plans took the sampled order");
}

/// A keyed class of many members beside many small classes, the case where the greedy
/// order joins the class with itself: both planners find the two individuals with one
/// key value.
#[test]
fn a_key_identifies_the_same_individuals() {
    let _one = count_lock();
    let mut t = String::from("ex:Person owl:hasKey ( ex:id ) .\n");
    for i in 0..300 {
        t += &format!("ex:x{i} a ex:Person ; ex:id \"{}\" .\n", i.min(298));
        t += &format!("ex:y{i} a ex:K{} ; ex:other{} ex:x{i} .\n", i % 60, i % 7);
    }
    let s = store(&t);
    let before = bench_sampled_plans();
    same(&s, &Profile::OwlRl, true, "key");
    assert!(bench_sampled_plans() > before);
    let f = facts(&s, &Profile::OwlRl, &options(true, true));
    assert!(f.contains(
        "<http://ex.org/x298> <http://www.w3.org/2002/07/owl#sameAs> <http://ex.org/x299>"
    ));
    assert!(
        !f.contains("<http://ex.org/x1> <http://www.w3.org/2002/07/owl#sameAs> <http://ex.org/x2>")
    );
}

/// User rules with long bodies, a constant, builtins that bind and test variables, a
/// negation and a recursive rule.
#[test]
fn user_rules_derive_the_same_facts() {
    let _one = count_lock();
    let rules = "@prefix ex: <http://ex.org/> .
[tri: (?x ex:memberOf ?d), (?d ex:partOf ?u), (?x ex:degreeFrom ?u), (?x rdf:type ex:Student)
      -> (?x ex:stayedAt ?u)]
[adv: (?x ex:advisor ?y), (?y ex:teaches ?c), (?x ex:takes ?c), (?c rdf:type ex:Course)
      -> (?x ex:taughtByAdvisor ?c)]
[one: (?x ex:advisor ?y), (?y ex:teaches ?c), (?x ex:takes ?c), (?x ex:memberOf ex:d0)
      -> (?x ex:closeTo ?y)]
[sum: (?x ex:age ?a), (?y ex:age ?b), (?x ex:advisor ?y), sum(?a, ?b, ?n), (?z ex:age ?n)
      -> (?x ex:agesAddUpTo ?z)]
[lone: (?x rdf:type ex:Student), (?x ex:memberOf ?d), (?d ex:partOf ?u), noValue(?x, ex:advisor)
      -> (?x ex:unadvisedAt ?u)]
[reach: (?x ex:closeTo ?y), (?y ex:advisor ?z), (?x ex:memberOf ?d), (?z ex:memberOf ?d)
      -> (?x ex:closeTo ?z)]
";
    let profile = Profile::Rules(rules.to_string());
    let before = bench_sampled_plans();
    for seed in 0..40u64 {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let (people, depts, courses) = (10 + r.below(60), 1 + r.below(4), 2 + r.below(10));
        let mut t = String::new();
        for d in 0..depts {
            t += &format!("ex:d{d} ex:partOf ex:u{} .\n", d % 2);
        }
        for c in 0..courses {
            if r.chance(80) {
                t += &format!("ex:c{c} a ex:Course .\n");
            }
        }
        for i in 0..people {
            t += &format!(
                "ex:p{i} ex:memberOf ex:d{} ; ex:age {} .\n",
                r.below(depts),
                20 + r.below(30)
            );
            if r.chance(70) {
                t += &format!(
                    "ex:p{i} a ex:Student ; ex:degreeFrom ex:u{} .\n",
                    r.below(2)
                );
            }
            if r.chance(60) {
                t += &format!("ex:p{i} ex:advisor ex:p{} .\n", r.below(people));
            }
            for _ in 0..r.below(4) {
                t += &format!("ex:p{i} ex:takes ex:c{} .\n", r.below(courses));
            }
            if r.chance(30) {
                t += &format!("ex:p{i} ex:teaches ex:c{} .\n", r.below(courses));
            }
        }
        let s = store(&t);
        same(&s, &profile, true, &format!("seed {seed}\n{t}"));
    }
    assert!(bench_sampled_plans() > before);
}

/// A materialization and its incremental update keep the same closure with either
/// planner.
#[test]
fn incremental_runs_keep_the_same_closure() {
    let _one = count_lock();
    let closure = |sampled: bool| -> BTreeSet<String> {
        let ttl = random_ontology(7);
        let s = store(&ttl);
        let o = options(sampled, true);
        let r = materialize(&s, &Profile::OwlRl, &o).unwrap();
        let since = r.receipt.unwrap().commit.seq;
        s.load(&[Source::from_bytes(
            format!("{PREFIXES}ex:i1 a ex:C0 ; ex:p0 \"1\" . ex:i2 a ex:C0 ; ex:p0 \"1\" . ex:i3 ex:p1 ex:i1 .\n")
                .into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        materialize_incremental(
            &s,
            &Profile::OwlRl,
            &Extras::default(),
            Incremental {
                since: Some(since),
                cache: None,
            },
            &o,
        )
        .unwrap();
        // the inferred graph matches a full run, and is what the caller compares
        let (full, _) = infer(s.snapshot(), &Profile::OwlRl, &options(false, true)).unwrap();
        let snap = s.snapshot();
        let g = snap.lookup_iri(INFERRED_GRAPH).unwrap();
        let written: BTreeSet<String> = snap
            .scan_keys(Perm::Gspo, &[g.0])
            .unwrap()
            .iter()
            .map(|k| {
                let q = snap.quad_to_terms(&Perm::Gspo.to_quad(k)).unwrap();
                oxrdf::Triple::new(q.subject, q.predicate, q.object).to_string()
            })
            .collect();
        assert_eq!(
            written,
            full.iter().map(|t| t.to_string()).collect::<BTreeSet<_>>(),
            "sampled {sampled}"
        );
        written
    };
    let before = bench_sampled_plans();
    let sampled = closure(true);
    assert!(bench_sampled_plans() > before);
    let greedy = closure(false);
    assert!(!sampled.is_empty());
    assert_eq!(sampled, greedy);
}
