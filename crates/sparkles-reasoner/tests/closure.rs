//! Differential tests of the closure operators and the cost-based planner (spec F23,
//! roadmap items 485 and 487): every run with both derives exactly the facts,
//! generalized ones included, that the same run derives with transitive and equivalence
//! rules evaluated as ordinary rules and with the greedy planner.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{Profile, ReasonOptions, bench_derive, infer};
use std::collections::BTreeSet;

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

fn options(closures: bool) -> ReasonOptions {
    ReasonOptions {
        closure_operators: closures,
        // the reference run is the engine before items 485 and 487
        cost_planning: closures,
        ..ReasonOptions::default()
    }
}

/// (derived facts, iterations, order-free digest) of a run.
fn derive(s: &Store, p: &Profile, closures: bool) -> (usize, usize, u64) {
    let (n, it, digest, _) = bench_derive(s.snapshot(), p, &options(closures), &|| {})
        .unwrap_or_else(|e| panic!("{e:#}"));
    (n, it, digest)
}

fn facts(s: &Store, p: &Profile, closures: bool) -> BTreeSet<String> {
    let (ts, _) = infer(s.snapshot(), p, &options(closures)).unwrap();
    ts.iter().map(|t| t.to_string()).collect()
}

/// Both runs derive the same facts; returns the iterations of (operators, rules).
fn same(s: &Store, p: &Profile, what: &str) -> (usize, usize) {
    let (n1, i1, d1) = derive(s, p, true);
    let (n0, i0, d0) = derive(s, p, false);
    if (n1, d1) != (n0, d0) {
        let (a, b) = (facts(s, p, true), facts(s, p, false));
        let extra: Vec<_> = a.difference(&b).take(10).collect();
        let missing: Vec<_> = b.difference(&a).take(10).collect();
        panic!(
            "{what}, {}: the operators derive {n1} facts and the rules {n0}\nonly with operators: {extra:#?}\nonly with rules: {missing:#?}",
            p.name()
        );
    }
    (i1, i0)
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

/// A random ontology with its data: class and property hierarchies with cycles,
/// transitive, symmetric and inverse properties, equivalences, sameAs among individuals
/// and with literals (some with equal values), and property chains.
fn random_ontology(seed: u64) -> String {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let classes = 3 + r.below(8);
    let props = 2 + r.below(5);
    let inds = 4 + r.below(20);
    let mut t = String::new();
    let c = |i: u64| format!("ex:C{i}");
    let p = |i: u64| format!("ex:p{i}");
    let x = |i: u64| format!("ex:i{i}");
    for _ in 0..r.below(2 * classes + 1) {
        t += &format!(
            "{} rdfs:subClassOf {} .\n",
            c(r.below(classes)),
            c(r.below(classes))
        );
    }
    if r.chance(30) {
        t += &format!(
            "{} owl:equivalentClass {} .\n",
            c(r.below(classes)),
            c(r.below(classes))
        );
    }
    for _ in 0..r.below(props + 1) {
        t += &format!(
            "{} rdfs:subPropertyOf {} .\n",
            p(r.below(props)),
            p(r.below(props))
        );
    }
    for i in 0..props {
        if r.chance(50) {
            t += &format!("{} a owl:TransitiveProperty .\n", p(i));
        }
        if r.chance(30) {
            t += &format!("{} a owl:SymmetricProperty .\n", p(i));
        }
        if r.chance(15) {
            t += &format!("{} owl:inverseOf {} .\n", p(i), p(r.below(props)));
        }
        if r.chance(15) {
            t += &format!("{} rdfs:domain {} .\n", p(i), c(r.below(classes)));
        }
        if r.chance(10) {
            t += &format!("{} a owl:FunctionalProperty .\n", p(i));
        }
    }
    if r.chance(25) {
        t += &format!(
            "{} owl:propertyChainAxiom ( {} {} ) .\n",
            p(r.below(props)),
            p(r.below(props)),
            p(r.below(props))
        );
    }
    if r.chance(20) {
        t += &format!(
            "{} owl:equivalentProperty {} .\n",
            p(r.below(props)),
            p(r.below(props))
        );
    }
    if r.chance(20) {
        let k = r.below(props);
        t += &format!(
            "{} owl:someValuesFrom {} ; owl:onProperty {} .\n",
            c(r.below(classes)),
            c(r.below(classes)),
            p(k)
        );
    }
    for _ in 0..r.below(3 * inds + 1) {
        t += &format!(
            "{} {} {} .\n",
            x(r.below(inds)),
            p(r.below(props)),
            x(r.below(inds))
        );
    }
    for _ in 0..r.below(inds) {
        t += &format!("{} a {} .\n", x(r.below(inds)), c(r.below(classes)));
    }
    for _ in 0..r.below(inds / 2 + 1) {
        t += &format!("{} owl:sameAs {} .\n", x(r.below(inds)), x(r.below(inds)));
    }
    if r.chance(15) {
        // literals in a sameAs class, sometimes two with equal values and distinct ids
        let lit = [
            "\"1\"^^xsd:integer",
            "\"01\"^^xsd:integer",
            "\"a\"",
            "\"2\"^^xsd:integer",
        ];
        for _ in 0..1 + r.below(3) {
            t += &format!(
                "{} owl:sameAs {} .\n",
                x(r.below(inds)),
                lit[r.below(lit.len() as u64) as usize]
            );
        }
    }
    if r.chance(15) {
        t += &format!(
            "{} {} \"{}\"^^xsd:integer .\n",
            x(r.below(inds)),
            p(r.below(props)),
            r.below(3)
        );
    }
    t
}

#[test]
fn random_ontologies_close_as_the_rules_do() {
    let mut fewer = 0;
    let cases = 300;
    for seed in 0..cases {
        let ttl = random_ontology(seed);
        let s = store(&ttl);
        for p in [Profile::OwlRl, Profile::Rdfs, Profile::RdfsSimple] {
            let (with, without) = same(&s, &p, &format!("seed {seed}\n{ttl}"));
            assert!(
                with <= without + 1,
                "seed {seed}, {}: {with} iterations with operators, {without} without",
                p.name()
            );
            fewer += (with < without) as usize;
        }
    }
    // the operators reach the fixpoint in fewer iterations somewhere
    assert!(fewer > 0);
}

/// A rule program that derives a few new edges of a large relation late in the run, so
/// that the operator updates its closure from the new edges only.
#[test]
fn late_edges_update_the_closure() {
    let rules = "
[t: (?a <http://ex.org/r> ?b), (?b <http://ex.org/r> ?c) -> (?a <http://ex.org/r> ?c)]
[l: (?a <http://ex.org/link> ?b), (?a <http://ex.org/flag> ?f) -> (?a <http://ex.org/r> ?b)]
[f: (?a <http://ex.org/r> ?b), (?b <http://ex.org/mark> ?m) -> (?b <http://ex.org/flag> ?m)]
";
    for seed in 0..60u64 {
        let mut r = Rng(seed * 7919 + 13);
        let n = 30 + r.below(40);
        let mut t = String::new();
        for i in 0..n {
            // a long chain with a few back edges and side branches
            t += &format!("ex:n{i} ex:r ex:n{} .\n", i + 1);
            if r.chance(8) {
                t += &format!("ex:n{i} ex:r ex:n{} .\n", r.below(n));
            }
            if r.chance(20) {
                t += &format!("ex:n{} ex:link ex:m{} .\n", r.below(n), r.below(n));
            }
            if r.chance(10) {
                t += &format!("ex:m{} ex:r ex:n{} .\n", r.below(n), r.below(n));
            }
            if r.chance(10) {
                t += &format!("ex:n{i} ex:mark ex:x .\n");
            }
        }
        let s = store(&t);
        same(
            &s,
            &Profile::Rules(rules.into()),
            &format!("seed {seed}\n{t}"),
        );
    }
}

/// Literal subjects in a relation whose transitive rule filters `notEqual`, which compares
/// literal values: the operator gives the rule back to the evaluator and the result is the
/// rules' fixpoint.
#[test]
fn literal_values_in_filtered_relations() {
    let rules = "
[s: (?x <http://ex.org/s> ?y) -> (?y <http://ex.org/r> ?x)]
[k: (?x <http://ex.org/k> ?y) -> (?x <http://ex.org/r> ?y)]
[t: (?a <http://ex.org/r> ?b), (?b <http://ex.org/r> ?c), notEqual(?a, ?c) -> (?a <http://ex.org/r> ?c)]
[eq: (?x <http://ex.org/e> ?y), notEqual(?x, ?y) -> (?y <http://ex.org/e> ?x)]
[eqt: (?a <http://ex.org/e> ?b), (?b <http://ex.org/e> ?c), notEqual(?a, ?c) -> (?a <http://ex.org/e> ?c)]
";
    let cases = [
        // "1" -> "2" -> "01" -> "02" through literal subjects: ("1", "02") is not derived
        "ex:a ex:s \"1\"^^xsd:integer . ex:a ex:k \"2\"^^xsd:integer .
         ex:b ex:s \"2\"^^xsd:integer . ex:b ex:k \"01\"^^xsd:integer .
         ex:c ex:s \"01\"^^xsd:integer . ex:c ex:k \"02\"^^xsd:integer .",
        // an equivalence class with two literals of equal value
        "ex:a ex:e \"1\"^^xsd:integer . ex:a ex:e \"01\"^^xsd:integer . ex:b ex:e ex:a .",
        // literals of distinct values stay with the operator
        "ex:a ex:e \"1\"^^xsd:integer . ex:a ex:e \"2\"^^xsd:integer . ex:b ex:e ex:a .
         ex:a ex:s \"x\" . ex:b ex:k ex:a .",
    ];
    for t in cases {
        let s = store(t);
        same(&s, &Profile::Rules(rules.into()), t);
    }
    // and the OWL 2 RL sameAs rules with literals of equal value
    let s = store(
        "ex:a owl:sameAs \"1\"^^xsd:integer . ex:a owl:sameAs \"01\"^^xsd:integer .
         ex:b owl:sameAs ex:a . ex:b ex:p ex:c .",
    );
    same(&s, &Profile::OwlRl, "sameAs with equal literal values");
}

/// Cliques of a symmetric and transitive property, as OWL2Bench's hasSameHomeTownWith
/// builds them, and a long subClassOf chain.
#[test]
fn cliques_and_chains() {
    let mut t = String::from("ex:home a owl:SymmetricProperty , owl:TransitiveProperty .\n");
    for g in 0..5 {
        for i in 0..20 {
            t += &format!("ex:g{g}p{i} ex:home ex:g{g}p{} .\n", (i * 7 + 3) % 20);
        }
    }
    for i in 0..200 {
        t += &format!("ex:C{i} rdfs:subClassOf ex:C{} .\n", i + 1);
    }
    t += "ex:C200 rdfs:subClassOf ex:C100 .\n ex:x a ex:C0 .\n";
    let s = store(&t);
    for p in [Profile::OwlRl, Profile::Rdfs, Profile::RdfsSimple] {
        let (with, without) = same(&s, &p, "cliques and chains");
        assert!(
            with < without,
            "{}: {with} vs {without} iterations",
            p.name()
        );
    }
}

/// A rule program with `noValue` keeps the ordinary evaluation, whose result depends on
/// the order of derivation.
#[test]
fn non_monotonic_programs_keep_the_rules() {
    let rules = "
[t: (?a <http://ex.org/r> ?b), (?b <http://ex.org/r> ?c) -> (?a <http://ex.org/r> ?c)]
[n: (?a <http://ex.org/r> ?b), noValue(?a, <http://ex.org/r>, <http://ex.org/z>) -> (?a <http://ex.org/q> ?b)]
";
    let s = store("ex:a ex:r ex:b . ex:b ex:r ex:c . ex:c ex:r ex:z .");
    let (with, without) = same(&s, &Profile::Rules(rules.into()), "noValue");
    assert_eq!(with, without);
}
