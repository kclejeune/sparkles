//! Explanations from F17's demand engine, why-not answers and the tree details of spec
//! F18 Phase 1.

use oxrdf::{BlankNode, NamedNode, Term};
use sparkles_core::id::Id;
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::explain::{
    DemandInput, Derivation, DerivationKind, ExplainInput, ExplainOptions, Explained, explain,
    explain_demand,
};
use sparkles_reasoner::extras::Extras;
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::whynot::{WhyNot, WhyNotOptions, why_not};
use sparkles_reasoner::{INFERRED_GRAPH, Inputs, Profile, ReasonOptions, materialize};
use std::collections::BTreeSet;

const EX: &str = "http://ex.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const GEO: &str = "http://www.opengis.net/ont/geosparql#";

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// The triples of graph `g` (`None` for the default graph).
fn graph_triples(s: &Store, g: Option<&str>) -> Vec<[Term; 3]> {
    let snap = s.snapshot();
    let gid = match g {
        None => Id::DEFAULT_GRAPH,
        Some(iri) => match snap.lookup_iri(iri) {
            Some(i) => i,
            None => return Vec::new(),
        },
    };
    let mut out: Vec<[Id; 3]> = Vec::new();
    snap.scan(Perm::Gspo, &[gid.0], |c| {
        match c {
            sparkles_core::store::Chunk::Block(b, st, e) => {
                out.extend((st..e).map(|i| [Id(b.cols[1][i]), Id(b.cols[2][i]), Id(b.cols[3][i])]))
            }
            sparkles_core::store::Chunk::Row(k) => out.push([Id(k[1]), Id(k[2]), Id(k[3])]),
        }
        Ok(true)
    })
    .unwrap();
    out.into_iter()
        .map(|t| t.map(|i| snap.term(i).unwrap()))
        .collect()
}

/// Whether graph `g` holds `t`.
fn holds(s: &Store, g: Option<&str>, t: &[Term; 3]) -> bool {
    graph_triples(s, g).contains(t)
}

fn n(iri: &str) -> Term {
    Term::NamedNode(NamedNode::new(iri).unwrap())
}

fn ex(l: &str) -> Term {
    n(&format!("{EX}{l}"))
}

fn a() -> Term {
    n(&format!("{RDF}type"))
}

fn reasoner(p: &Profile) -> QueryReasoner {
    QueryReasoner::new(p.clone(), &Extras::default(), Inputs::default()).unwrap()
}

fn demand(s: &Store, p: &Profile, t: [Term; 3], opts: &ExplainOptions) -> Explained {
    let qr = reasoner(p);
    explain_demand(
        &DemandInput {
            reasoner: &qr,
            snap: s.snapshot(),
            readable: None,
        },
        &t,
        opts,
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

fn trees(e: Explained) -> (Vec<Derivation>, bool) {
    match e {
        Explained::Found { trees, more } => (trees, more),
        e => panic!("no derivation: {e:?}"),
    }
}

/// Checks that no fact is among its own premises, that every leaf is asserted in the
/// store or an axiom, that a derived node has premises and a rule, and that its depth is
/// more than its premises'.
fn sound(s: &Store, d: &Derivation, path: &mut Vec<[Term; 3]>) {
    assert!(
        !path.contains(&d.triple),
        "{:?} is its own premise",
        d.triple
    );
    if d.shared {
        return;
    }
    match d.kind {
        DerivationKind::Asserted => {
            assert!(holds(s, None, &d.triple), "{:?} is not asserted", d.triple);
        }
        DerivationKind::Axiom => assert!(d.premises.is_empty()),
        DerivationKind::Derived => {
            assert!(d.rule.is_some());
            assert!(!d.premises.is_empty());
            path.push(d.triple.clone());
            for p in &d.premises {
                assert!(p.shared || p.depth < d.depth, "{p:?} under {d:?}");
                sound(s, p, path);
            }
            path.pop();
        }
        k => panic!("unexpected {k:?}"),
    }
}

/// Replay the named rule's head and body triples using the values exported in the tree.
fn replay(d: &Derivation, rules: &[sparkles_reasoner::parser::Rule]) {
    use sparkles_reasoner::parser::{Clause, Node, TriplePattern};
    if d.shared {
        return;
    }
    if matches!(d.kind, DerivationKind::Derived | DerivationKind::Axiom) {
        let name = &d.rule.as_ref().unwrap().name;
        let rule = rules
            .iter()
            .find(|r| &r.label() == name)
            .unwrap_or_else(|| panic!("unknown rule {name}"));
        let fits = |p: &TriplePattern, t: &[Term; 3]| {
            [&p.subject, &p.predicate, &p.object]
                .iter()
                .zip(t)
                .all(|(slot, t)| match slot {
                    Node::Const(c) => c == t,
                    Node::Var(v) => d.bindings.iter().any(|(n, value)| n == v && value == t),
                    Node::Any => true,
                    Node::Functor(..) => panic!("the generator does not use functor terms"),
                })
        };
        assert!(
            rule.head
                .iter()
                .any(|c| matches!(c,Clause::Triple(p) if fits(p,&d.triple))),
            "wrong head or bindings: {d:?}"
        );
        for c in &rule.body {
            if let Clause::Triple(p) = c {
                assert!(
                    d.premises.iter().any(|premise| fits(p, &premise.triple)),
                    "rule premise missing or bound incorrectly: {d:?}"
                );
            }
        }
    }
    for p in &d.premises {
        replay(p, rules);
    }
}

const A1: &str = "ex:Cat rdfs:subClassOf ex:Mammal . ex:Mammal rdfs:subClassOf ex:Animal .
ex:tom a ex:Cat .";

/// F18 A1 and A2 over the demand engine.
#[test]
fn demand_trees_for_a1_and_a2() {
    let s = store(A1);
    let t = [ex("tom"), a(), ex("Animal")];
    let (ts, _) = trees(demand(
        &s,
        &Profile::Rdfs,
        t.clone(),
        &ExplainOptions::default(),
    ));
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0].rule.as_ref().unwrap().name, "rdfs9");
    sound(&s, &ts[0], &mut Vec::new());
    // the bindings and the builtins of the instance are named
    let names: BTreeSet<&str> = ts[0].bindings.iter().map(|(n, _)| n.as_str()).collect();
    assert!(!names.is_empty(), "{ts:?}");
    let two = ExplainOptions {
        max_trees: 2,
        ..Default::default()
    };
    let (ts, _) = trees(demand(&s, &Profile::Rdfs, t, &two));
    assert_eq!(ts.len(), 2, "{ts:#?}");
    let rules: BTreeSet<String> = ts
        .iter()
        .flat_map(|t| t.premises.iter())
        .filter_map(|p| p.rule.as_ref().map(|r| r.name.clone()))
        .collect();
    assert!(
        rules.contains("rdfs9") || rules.contains("rdfs11"),
        "{rules:?}"
    );
    for t in &ts {
        sound(&s, t, &mut Vec::new());
    }
    assert!(matches!(
        demand(
            &s,
            &Profile::Rdfs,
            [ex("tom"), a(), ex("Plant")],
            &ExplainOptions::default()
        ),
        Explained::NotFound
    ));
}

/// The builtins a Jena rule passed and its bindings are in the tree (ROADMAP item 137).
#[test]
fn builtins_and_bindings_are_named() {
    let s = store("ex:a ex:p0 ex:b . ex:b ex:p0 ex:c .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [chain: (?x ex:p0 ?y), (?y ex:p0 ?z), notEqual(?x, ?z) -> (?x ex:p1 ?z)]"
            .into(),
    );
    let (ts, _) = trees(demand(
        &s,
        &p,
        [ex("a"), ex("p1"), ex("c")],
        &ExplainOptions::default(),
    ));
    let t = &ts[0];
    assert_eq!(t.rule.as_ref().unwrap().name, "chain");
    let b: Vec<(String, String)> = t
        .bindings
        .iter()
        .map(|(n, v)| (n.clone(), v.to_string()))
        .collect();
    assert_eq!(
        b,
        vec![
            ("x".to_string(), format!("<{EX}a>")),
            ("y".to_string(), format!("<{EX}b>")),
            ("z".to_string(), format!("<{EX}c>")),
        ]
    );
    assert_eq!(t.builtins, vec![format!("notEqual(<{EX}a>, <{EX}c>)")]);
}

/// A derivation through a generalized triple is found on demand, and its node says the
/// triple is not RDF; the materialized search does not hold it (ROADMAP item 136).
#[test]
fn generalized_premises_are_found_on_demand() {
    let s = store("ex:a ex:val \"1\" . ex:b ex:val \"1\" .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [g1: (?x ex:val ?v) -> (?v ex:of ?x)]
         [g2: (?v ex:of ?x), (?v ex:of ?y), notEqual(?x, ?y) -> (?x ex:same ?y)]"
            .into(),
    );
    let t = [ex("a"), ex("same"), ex("b")];
    let (ts, _) = trees(demand(&s, &p, t.clone(), &ExplainOptions::default()));
    let general: Vec<&Derivation> = ts[0].premises.iter().filter(|d| d.generalized).collect();
    assert_eq!(general.len(), 2, "{ts:#?}");
    assert!(matches!(general[0].triple[0], Term::Literal(_)));
    // the materialized closure lacks the generalized premises
    materialize(&s, &p, &ReasonOptions::default()).unwrap();
    let snap = s.snapshot();
    let inferred = snap.lookup_iri(INFERRED_GRAPH);
    let e = explain(
        &ExplainInput {
            snap,
            profile: &p,
            extras: &Extras::default(),
            inputs: &[Id::DEFAULT_GRAPH],
            inferred,
        },
        &t,
        &ExplainOptions::default(),
    )
    .unwrap();
    assert!(matches!(e, Explained::NotFound), "{e:?}");
}

/// A GeoSPARQL default geometry is derived from the feature's one geometry, over the
/// materialized closure and on demand (ROADMAP item 136).
#[test]
fn default_geometries_are_explained() {
    let s = store("ex:f geo:hasGeometry ex:g .");
    let extras = Extras {
        geo_default_geometry: true,
        ..Default::default()
    };
    let p = Profile::Rdfs;
    let opts = ReasonOptions::default();
    sparkles_reasoner::materialize_incremental(
        &s,
        &p,
        &extras,
        sparkles_reasoner::Incremental {
            since: None,
            cache: None,
        },
        &opts,
    )
    .unwrap();
    let t = [ex("f"), n(&format!("{GEO}hasDefaultGeometry")), ex("g")];
    let snap = s.snapshot();
    let inferred = snap.lookup_iri(INFERRED_GRAPH);
    let e = explain(
        &ExplainInput {
            snap: snap.clone(),
            profile: &p,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred,
        },
        &t,
        &ExplainOptions::default(),
    )
    .unwrap();
    let (ts, _) = trees(e);
    assert_eq!(ts[0].rule.as_ref().unwrap().name, "geo-default-geometry");
    assert_eq!(ts[0].premises[0].kind, DerivationKind::Asserted);
    let qr = QueryReasoner::new(p.clone(), &extras, Inputs::default()).unwrap();
    let e = explain_demand(
        &DemandInput {
            reasoner: &qr,
            snap,
            readable: None,
        },
        &t,
        &ExplainOptions::default(),
    )
    .unwrap();
    let (ts, _) = trees(e);
    assert_eq!(ts[0].rule.as_ref().unwrap().name, "geo-default-geometry");
}

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

fn random_triple(r: &mut Rng, owl: bool) -> String {
    let c = |r: &mut Rng| format!("ex:C{}", r.below(6));
    let p = |r: &mut Rng| format!("ex:p{}", r.below(5));
    let i = |r: &mut Rng| format!("ex:i{}", r.below(10));
    match r.below(if owl { 14 } else { 10 }) {
        0 => format!("{} rdfs:subClassOf {}", c(r), c(r)),
        1 => format!("{} rdfs:subPropertyOf {}", p(r), p(r)),
        2 => format!("{} rdfs:domain {}", p(r), c(r)),
        3 => format!("{} rdfs:range {}", p(r), c(r)),
        4 | 5 => format!("{} a {}", i(r), c(r)),
        6..=8 => format!("{} {} {}", i(r), p(r), i(r)),
        9 => format!("{} {} \"v{}\"", i(r), p(r), r.below(3)),
        10 => format!("{} owl:sameAs {}", i(r), i(r)),
        11 => format!("{} owl:inverseOf {}", p(r), p(r)),
        12 => format!(
            "{} a owl:{}",
            p(r),
            ["TransitiveProperty", "SymmetricProperty"][r.below(2)]
        ),
        _ => format!("{} owl:equivalentClass {}", c(r), c(r)),
    }
}

/// F18 §11: every triple of the inferred graph has a demand tree, and every tree is well
/// founded, with leaves the store asserts, for every profile on random data.
#[test]
fn every_inferred_triple_has_a_sound_demand_tree() {
    for (p, owl) in [
        (Profile::Rdfs, false),
        (Profile::OwlRl, true),
        (
            Profile::Rules(
                "@prefix ex: <http://ex.org/>.
                 [a: (?x ex:p0 ?y), (?y ex:p0 ?z), notEqual(?x, ?z) -> (?x ex:p1 ?z)]
                 [b: (?x ex:p1 ?y) -> (?y ex:p1 ?x)]
                 [anc1: (?x ex:p0 ?y) -> (?x ex:anc ?y)]
                 [anc2: (?x ex:p0 ?y), (?y ex:anc ?z) -> (?x ex:anc ?z)]
                 [h: (?x rdf:type ex:C1), noValue(?x, ex:p0) -> (?x rdf:type ex:C2)]"
                    .into(),
            ),
            false,
        ),
    ] {
        for seed in 1..=3u64 {
            let mut r = Rng(seed * 104_729 + 7);
            let mut ttl = String::new();
            for _ in 0..40 {
                ttl.push_str(&format!("{} .\n", random_triple(&mut r, owl)));
            }
            let s = store(&ttl);
            materialize(&s, &p, &ReasonOptions::default()).unwrap();
            let all = graph_triples(&s, Some(INFERRED_GRAPH));
            assert!(!all.is_empty());
            // a sample, so that the test stays quick
            let step = (all.len() / 40).max(1);
            let qr = reasoner(&p);
            for t in all.iter().step_by(step) {
                let ask = format!("ASK {{ {} {} {} }}", t[0], t[1], t[2]);
                let o = sparkles_core::sparql::QueryOptions {
                    reasoning: Some(qr.provider()),
                    ..Default::default()
                };
                let r = sparkles_core::sparql::query(s.snapshot(), &ask, &o).unwrap();
                assert!(
                    r.boolean,
                    "{p} seed {seed}: demand misses materialized {t:?}\n{ttl}"
                );
                let e = demand(&s, &p, t.clone(), &ExplainOptions::default());
                let Explained::Found { trees: ts, .. } = e else {
                    panic!("{p} seed {seed}: {t:?} has no demand tree: {e:?}\n{ttl}");
                };
                for d in &ts {
                    assert_eq!(&d.triple, t);
                    sound(&s, d, &mut Vec::new());
                    replay(d, &p.rules().unwrap());
                }
            }
        }
    }
}

fn whynot(s: &Store, p: &Profile, t: [Term; 3], opts: &WhyNotOptions) -> WhyNot {
    let qr = reasoner(p);
    why_not(
        &DemandInput {
            reasoner: &qr,
            snap: s.snapshot(),
            readable: None,
        },
        &t,
        opts,
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

/// Each hypothesis holds: a full materialization over the data and its facts, with
/// placeholders as blank nodes, derives the triple (spec F18 §11).
fn rematerialize(ttl: &str, p: &Profile, missing: &[[Term; 3]], t: &[Term; 3]) {
    let s = store(ttl);
    let mut add = String::new();
    for f in missing {
        let term = |t: &Term| match t {
            Term::BlankNode(b) => format!("_:{}", b.as_str()),
            t => t.to_string(),
        };
        add.push_str(&format!(
            "{} {} {} .\n",
            term(&f[0]),
            term(&f[1]),
            term(&f[2])
        ));
    }
    s.load(&[Source::from_bytes(
        add.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    materialize(&s, p, &ReasonOptions::default()).unwrap();
    assert!(
        holds(&s, Some(INFERRED_GRAPH), t),
        "{missing:?} does not derive {t:?}"
    );
}

/// F18 A3: the candidates and the hypotheses of a missing range axiom, with a verified
/// tree, and a placeholder hypothesis through a subclass.
#[test]
fn why_not_a3() {
    let ttl = "ex:ann ex:owns ex:rex .";
    let s = store(ttl);
    let t = [ex("rex"), a(), ex("Pet")];
    let WhyNot::NotEntailed {
        rules, hypotheses, ..
    } = whynot(&s, &Profile::OwlRl, t.clone(), &WhyNotOptions::default())
    else {
        panic!("entailed");
    };
    let names: BTreeSet<&str> = rules.iter().map(|c| c.rule.name.as_str()).collect();
    assert!(
        names.contains("prp-rng") && names.contains("cax-sco"),
        "{names:?}"
    );
    let rng = rules.iter().find(|c| c.rule.name == "prp-rng").unwrap();
    assert!(
        rng.unifier.iter().any(|(v, t)| v == "c" && *t == ex("Pet")),
        "{rng:?}"
    );
    let first: Vec<&Vec<[Term; 3]>> = hypotheses.iter().take(3).map(|h| &h.missing).collect();
    let range = vec![[ex("owns"), n(&format!("{RDFS}range")), ex("Pet")]];
    assert!(first.contains(&&range), "{first:#?}");
    let placeholder = hypotheses.iter().take(3).find(|h| {
        h.missing.len() == 2
            && h.missing
                .iter()
                .any(|f| f[1] == n(&format!("{RDFS}subClassOf")) && f[2] == ex("Pet"))
            && h.missing.iter().any(|f| f[0] == ex("rex") && f[1] == a())
    });
    let ph = placeholder.unwrap_or_else(|| panic!("{first:#?}"));
    assert_eq!(ph.placeholders, vec!["y1".to_string()]);
    let y = Term::BlankNode(BlankNode::new_unchecked("y1"));
    assert!(
        ph.missing.iter().all(|f| f.contains(&y)),
        "{:?}",
        ph.missing
    );
    for h in &hypotheses {
        assert_eq!(h.tree.triple, t);
        fn has_missing(d: &Derivation) -> bool {
            d.missing || d.premises.iter().any(has_missing)
        }
        assert!(has_missing(&h.tree), "{:?}", h.tree);
        rematerialize(ttl, &Profile::OwlRl, &h.missing, &t);
    }
}

/// F18 A4: a rule blocked by `noValue` names the blocking fact.
#[test]
fn why_not_a4_blocked_by_negation() {
    let s = store("ex:bob a ex:Person . ex:bob ex:died \"1990\" .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [alive: (?x rdf:type ex:Person), noValue(?x, ex:died) -> (?x rdf:type ex:Living)]"
            .into(),
    );
    let WhyNot::NotEntailed { rules, .. } = whynot(
        &s,
        &p,
        [ex("bob"), a(), ex("Living")],
        &WhyNotOptions::default(),
    ) else {
        panic!("entailed");
    };
    let alive = rules.iter().find(|c| c.rule.name == "alive").unwrap();
    assert_eq!(alive.blocked_by.len(), 1, "{alive:?}");
    let b = &alive.blocked_by[0];
    assert!(b.call.starts_with("noValue("), "{}", b.call);
    assert_eq!(
        b.fact,
        Some([
            ex("bob"),
            ex("died"),
            Term::Literal(oxrdf::Literal::new_simple_literal("1990"))
        ])
    );
}

/// An entailed triple answers so, and a hypothesis is never bigger than `max_missing`.
#[test]
fn why_not_of_an_entailed_triple_and_the_size_limit() {
    let s = store(A1);
    assert!(matches!(
        whynot(
            &s,
            &Profile::Rdfs,
            [ex("tom"), a(), ex("Animal")],
            &WhyNotOptions::default()
        ),
        WhyNot::Entailed
    ));
    let one = WhyNotOptions {
        max_missing: 1,
        ..Default::default()
    };
    let WhyNot::NotEntailed { hypotheses, .. } =
        whynot(&s, &Profile::Rdfs, [ex("tom"), a(), ex("Plant")], &one)
    else {
        panic!("entailed");
    };
    assert!(!hypotheses.is_empty());
    for h in &hypotheses {
        assert_eq!(h.missing.len(), 1, "{h:?}");
        rematerialize(
            A1,
            &Profile::Rdfs,
            &h.missing,
            &[ex("tom"), a(), ex("Plant")],
        );
    }
    // the subclass of a class tom already has is one of them
    assert!(hypotheses.iter().any(|h| h.missing
        == vec![[ex("Animal"), n(&format!("{RDFS}subClassOf")), ex("Plant")]]
        || h.missing == vec![[ex("Cat"), n(&format!("{RDFS}subClassOf")), ex("Plant")]]));
}

#[test]
fn why_not_missing_facts_are_sets_and_truncation_is_reported() {
    let s = store("");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
      [duplicate: (?x ex:p ?y), (?x ex:p ?y) -> (?x ex:target ?y)]
      [other: (?x ex:q ?y) -> (?x ex:target ?y)]"
            .into(),
    );
    let t = [ex("a"), ex("target"), ex("b")];
    let WhyNot::NotEntailed {
        hypotheses,
        complete,
        ..
    } = whynot(
        &s,
        &p,
        t.clone(),
        &WhyNotOptions {
            max_missing: 1,
            max_hypotheses: 1,
            ..Default::default()
        },
    )
    else {
        panic!("entailed");
    };
    assert_eq!(hypotheses.len(), 1);
    assert_eq!(hypotheses[0].missing.len(), 1);
    assert!(!complete, "the second verified hypothesis was not explored");
    rematerialize("", &p, &hypotheses[0].missing, &t);
}

#[test]
fn why_not_reports_builtin_value_failures_and_preserves_cancellation() {
    let s = store("ex:bob ex:age 5 .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
      [adult: (?x ex:age ?age), greaterThan(?age, 17) -> (?x ex:adult ex:yes)]"
            .into(),
    );
    let t = [ex("bob"), ex("adult"), ex("yes")];
    let WhyNot::NotEntailed { rules, .. } = whynot(&s, &p, t.clone(), &WhyNotOptions::default())
    else {
        panic!("entailed");
    };
    assert!(rules.iter().any(|r| {
        r.rule.name == "adult"
            && r.blocked_by
                .iter()
                .any(|b| b.call.starts_with("greaterThan("))
    }));
    let qr = reasoner(&p);
    let e = why_not(
        &DemandInput {
            reasoner: &qr,
            snap: s.snapshot(),
            readable: None,
        },
        &t,
        &WhyNotOptions {
            explain: ExplainOptions {
                cancel: Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    true,
                ))),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        e.chain().any(|c| matches!(
            c.downcast_ref::<sparkles_core::Error>(),
            Some(sparkles_core::Error::Cancelled)
        )),
        "{e:#}"
    );
}

#[test]
fn why_not_does_not_claim_exhaustive_literal_hypotheses() {
    let s = store("");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
      [literal: (?x ex:p ?value), isLiteral(?value) -> (?x ex:target ex:yes)]"
            .into(),
    );
    let WhyNot::NotEntailed {
        hypotheses,
        complete,
        ..
    } = whynot(
        &s,
        &p,
        [ex("alice"), ex("target"), ex("yes")],
        &WhyNotOptions::default(),
    )
    else {
        panic!("entailed");
    };
    assert!(
        hypotheses.is_empty(),
        "blank-node placeholders cannot satisfy isLiteral"
    );
    assert!(
        !complete,
        "literal-valued substitutions were not enumerated"
    );
}

#[test]
fn why_not_verifies_smaller_sets_before_pruning_their_supersets() {
    let s = store("");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
      [block: (ex:a ex:grant ex:b) -> (ex:a ex:blocked ex:b)]
      [conditional: (ex:a ex:grant ex:b), noValue(ex:a, ex:blocked, ex:b) -> (ex:a ex:allowed ex:b)]
      [override: (ex:a ex:grant ex:b), (ex:a ex:waiver ex:b) -> (ex:a ex:allowed ex:b)]"
            .into(),
    );
    let target = [ex("a"), ex("allowed"), ex("b")];
    let WhyNot::NotEntailed { hypotheses, .. } =
        whynot(&s, &p, target.clone(), &WhyNotOptions::default())
    else {
        panic!("entailed");
    };
    let wanted = [
        [ex("a"), ex("grant"), ex("b")],
        [ex("a"), ex("waiver"), ex("b")],
    ];
    let hypothesis = hypotheses
        .iter()
        .find(|h| wanted.iter().all(|t| h.missing.contains(t)))
        .expect("the one-fact candidate blocks itself; the two-fact override is valid");
    rematerialize("", &p, &hypothesis.missing, &target);
}

#[test]
fn review_missing_intermediates_can_share_one_added_fact() {
    for count in [2, 3] {
        let s = store("");
        let arms: Vec<_> = (0..count).map(|i| format!("(?x ex:arm{i} ?y)")).collect();
        let rules: String = (0..count)
            .map(|i| format!("[arm{i}: (?x ex:base ?y) -> {}]\n", arms[i]))
            .collect();
        let p = Profile::Rules(format!(
            "@prefix ex: <http://ex.org/> .\n{rules}[join: {} -> (?x ex:target ?y)]",
            arms.join(", ")
        ));
        let target = [ex("a"), ex("target"), ex("b")];
        let WhyNot::NotEntailed { hypotheses, .. } = whynot(
            &s,
            &p,
            target.clone(),
            &WhyNotOptions {
                max_missing: 1,
                ..Default::default()
            },
        ) else {
            panic!("entailed");
        };
        let wanted = vec![[ex("a"), ex("base"), ex("b")]];
        let h = hypotheses
            .iter()
            .find(|h| h.missing == wanted)
            .expect("all intermediate facts follow from the same added fact");
        rematerialize("", &p, &h.missing, &target);
    }
}

#[test]
fn review_blocker_strings_are_charged_to_the_request_memory_limit() {
    let value = "z".repeat(100_000);
    let ttl = (0..10)
        .map(|i| format!("ex:a ex:value \"{value}{i}\" .\n"))
        .collect::<String>();
    let s = store(&ttl);
    let p = Profile::Rules("@prefix ex: <http://ex.org/> . [nonliteral: (?x ex:value ?value), notLiteral(?value) -> (?x ex:target ex:yes)]".into());
    let qr = reasoner(&p);
    let answer = why_not(
        &DemandInput {
            reasoner: &qr,
            snap: s.snapshot(),
            readable: None,
        },
        &[ex("a"), ex("target"), ex("yes")],
        &WhyNotOptions {
            explain: ExplainOptions {
                max_memory: Some(200_000),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(
        answer.is_err(),
        "retained blocker text must not exceed the request's memory budget"
    );
    assert!(
        answer
            .unwrap_err()
            .downcast_ref::<sparkles_core::explain::ExplainBudgetExceeded>()
            .is_some()
    );
    let answer = why_not(
        &DemandInput {
            reasoner: &qr,
            snap: s.snapshot(),
            readable: None,
        },
        &[ex("a"), ex("target"), ex("yes")],
        &WhyNotOptions {
            explain: ExplainOptions {
                max_memory: Some(20_000_000),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let WhyNot::NotEntailed { rules, .. } = answer else {
        panic!("blocked")
    };
    assert_eq!(rules[0].blocked_by.len(), 10);
    assert!(
        rules[0]
            .blocked_by
            .iter()
            .all(|blocker| blocker.call.len() > 100_000)
    );
}

#[test]
fn review_geometry_hypotheses_are_sound_after_full_rematerialization() {
    let ttl = "ex:feature geo:hasGeometry ex:first .";
    let s = store(ttl);
    let p = Profile::Rules("@prefix ex: <http://ex.org/> . @prefix geo: <http://www.opengis.net/ont/geosparql#> .
      [both: (?f geo:hasDefaultGeometry ex:first), (?f geo:hasGeometry ex:second) -> (?f ex:target ex:yes)]".into());
    let extras = Extras {
        geo_default_geometry: true,
        ..Default::default()
    };
    let qr = QueryReasoner::new(p.clone(), &extras, Inputs::default()).unwrap();
    let target = [ex("feature"), ex("target"), ex("yes")];
    let WhyNot::NotEntailed { hypotheses, .. } = why_not(
        &DemandInput {
            reasoner: &qr,
            snap: s.snapshot(),
            readable: None,
        },
        &target,
        &WhyNotOptions::default(),
    )
    .unwrap() else {
        panic!("entailed");
    };
    assert!(!hypotheses.is_empty());
    for h in hypotheses {
        let extra = h
            .missing
            .iter()
            .map(|t| format!("{} {} {} .\n", t[0], t[1], t[2]))
            .collect::<String>();
        let fresh = store(&format!("{ttl}\n{extra}"));
        sparkles_reasoner::materialize_with(&fresh, &p, &extras, &ReasonOptions::default())
            .unwrap();
        assert!(
            holds(&fresh, Some(INFERRED_GRAPH), &target),
            "unverified geometry hypothesis: {:?}",
            h.missing
        );
    }
}

#[test]
fn review_default_geometry_facts_use_the_session_term_dictionary() {
    let s = store("ex:feature geo:hasGeometry ex:shape .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> . @prefix geo: <http://www.opengis.net/ont/geosparql#> .
      [unrelated: (?f ex:unrelated ?g) -> (?f ex:target ?g)]
      [geometry: (?f geo:hasDefaultGeometry ?g) -> (?f ex:geometry ?g)]"
            .into(),
    );
    let extras = Extras {
        geo_default_geometry: true,
        ..Default::default()
    };
    let qr = QueryReasoner::new(p, &extras, Inputs::default()).unwrap();
    let input = DemandInput {
        reasoner: &qr,
        snap: s.snapshot(),
        readable: None,
    };
    assert!(
        matches!(
            why_not(
                &input,
                &[ex("feature"), ex("target"), ex("shape")],
                &WhyNotOptions::default()
            )
            .unwrap(),
            WhyNot::NotEntailed { .. }
        ),
        "a synthetic default must not alias an unrelated rule predicate"
    );
    assert!(matches!(
        explain_demand(
            &input,
            &[ex("feature"), ex("geometry"), ex("shape")],
            &ExplainOptions::default()
        )
        .unwrap(),
        Explained::Found { .. }
    ));
}

#[test]
fn geometry_why_not_unfolds_the_implicit_default_rule() {
    let s = store("");
    let extras = Extras {
        geo_default_geometry: true,
        ..Default::default()
    };
    let p = Profile::Rules("@prefix ex: <http://ex.org/> . @prefix geo: <http://www.opengis.net/ont/geosparql#> . [use: (?f geo:hasDefaultGeometry ?g) -> (?f ex:target ?g)]".into());
    let qr = QueryReasoner::new(p, &extras, Inputs::default()).unwrap();
    let input = DemandInput {
        reasoner: &qr,
        snap: s.snapshot(),
        readable: None,
    };
    let missing = vec![[ex("feature"), n(&format!("{GEO}hasGeometry")), ex("shape")]];
    for predicate in [n(&format!("{GEO}hasDefaultGeometry")), ex("target")] {
        let WhyNot::NotEntailed {
            rules, hypotheses, ..
        } = why_not(
            &input,
            &[ex("feature"), predicate.clone(), ex("shape")],
            &WhyNotOptions {
                max_missing: 1,
                ..Default::default()
            },
        )
        .unwrap()
        else {
            panic!("entailed");
        };
        if predicate == n(&format!("{GEO}hasDefaultGeometry")) {
            assert!(rules.iter().any(|r| r.rule.name == "geo-default-geometry"));
        }
        assert!(
            hypotheses.iter().any(|h| h.missing == missing),
            "{hypotheses:?}"
        );
    }
}

#[test]
fn review_r2_nested_blockers_do_not_accumulate_discarded_strings() {
    let value = "z".repeat(1_000_000);
    let s = store(&format!("ex:a ex:value \"{value}\" ."));
    let alternatives: String = (0..20)
        .map(|i| format!("[blocked{i}: (?x ex:value ?v), notLiteral(?v) -> (?x ex:mid ex:yes)]\n"))
        .collect();
    let p = Profile::Rules(format!(
        "@prefix ex: <http://ex.org/> . [target: (?x ex:mid ex:yes) -> (?x ex:target ex:yes)] {alternatives}"
    ));
    let answer = whynot(
        &s,
        &p,
        [ex("a"), ex("target"), ex("yes")],
        &WhyNotOptions {
            explain: ExplainOptions {
                max_memory: Some(6_000_000),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let WhyNot::NotEntailed {
        rules, hypotheses, ..
    } = answer
    else {
        panic!("not entailed");
    };
    assert_eq!(rules.len(), 1);
    assert!(rules[0].blocked_by.is_empty());
    assert!(
        hypotheses
            .iter()
            .any(|h| h.missing == [[ex("a"), ex("mid"), ex("yes")]])
    );
}

#[test]
fn rule_local_blank_constants_are_not_insertable_why_not_hypotheses() {
    let s = store("");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> . [fixed: (_:anchor ex:p ex:b) -> (ex:a ex:target ex:b)]"
            .into(),
    );
    let target = [ex("a"), ex("target"), ex("b")];
    let WhyNot::NotEntailed {
        hypotheses,
        complete,
        ..
    } = whynot(&s, &p, target.clone(), &WhyNotOptions::default())
    else {
        panic!("not entailed");
    };
    for hypothesis in &hypotheses {
        // Inserting the serialized blank allocates a store node, rather than recreating
        // the compiler's rule-local constant. An overlay must not claim otherwise.
        rematerialize("", &p, &hypothesis.missing, &target);
    }
    assert!(hypotheses.is_empty());
    assert!(!complete);
}

#[test]
fn blank_head_rules_do_not_claim_complete_abduction() {
    let s = store("");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> . [blank: (?x ex:p ?y) -> (_:anchor ex:q ?y)]".into(),
    );
    let target = [
        Term::BlankNode(BlankNode::new("anchor").unwrap()),
        ex("q"),
        ex("b"),
    ];
    let WhyNot::NotEntailed {
        hypotheses,
        complete,
        rules,
    } = whynot(&s, &p, target, &WhyNotOptions::default())
    else {
        panic!("not entailed");
    };
    assert_eq!(rules.len(), 1);
    assert!(hypotheses.is_empty());
    assert!(!complete);
}

#[test]
fn compact_dense_type_full_materialization_control() {
    let started = std::time::Instant::now();
    let s = store(
        "ex:ann ex:owns ex:rex . rdf:type rdfs:domain owl:SymmetricProperty . ex:Pet rdf:type ex:rex .",
    );
    materialize(&s, &Profile::OwlRl, &ReasonOptions::default()).unwrap();
    let target = [ex("rex"), a(), ex("Pet")];
    assert!(holds(&s, Some(INFERRED_GRAPH), &target));
    eprintln!(
        "A3_SYMMETRIC full_materialization done {:?} inferred={}",
        started.elapsed(),
        graph_triples(&s, Some(INFERRED_GRAPH)).len()
    );
}

#[test]
fn compact_dense_type_demand_preserves_sound_replay() {
    let started = std::time::Instant::now();
    let s = store(
        "ex:ann ex:owns ex:rex . rdf:type rdfs:domain owl:SymmetricProperty . ex:Pet rdf:type ex:rex .",
    );
    let target = [ex("rex"), a(), ex("Pet")];
    let explained = demand(
        &s,
        &Profile::OwlRl,
        target.clone(),
        &ExplainOptions::default(),
    );
    let Explained::Found { trees, .. } = explained else {
        panic!("{explained:?}")
    };
    assert!(!trees.is_empty());
    for tree in &trees {
        assert_eq!(tree.triple, target);
        sound(&s, tree, &mut Vec::new());
        replay(tree, &Profile::OwlRl.rules().unwrap());
    }
    eprintln!("A3_SYMMETRIC demand done {:?}", started.elapsed());
}

#[test]
fn compact_positive_proofs_keep_minimum_depth_and_multiple_top_rules() {
    let profile = Profile::Rules(format!(
        "{}
@prefix ex: <http://ex.org/> .
[r-long1: (?s ex:p ?o) -> (?s ex:x ?o)]
[r-long2: (?s ex:x ?o) -> (?s ex:q ?o)]
[r-short: (?s ex:p ?o) -> (?s ex:q ?o)]",
        include_str!("../rules/owl-rl.rules")
    ));
    let s = store("ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .");
    let target = [ex("s"), ex("q"), ex("o")];
    let (one, one_more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions::default(),
    ));
    assert_eq!(one.len(), 1);
    assert!(one_more);
    assert_eq!(one[0].depth, 1);
    assert_eq!(one[0].rule.as_ref().unwrap().name, "r-short");
    sound(&s, &one[0], &mut Vec::new());
    replay(&one[0], &profile.rules().unwrap());
    let (many, _) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions {
            max_trees: 2,
            ..ExplainOptions::default()
        },
    ));
    let names: BTreeSet<_> = many
        .iter()
        .filter_map(|tree| tree.rule.as_ref().map(|rule| rule.name.as_str()))
        .collect();
    assert_eq!(names, BTreeSet::from(["r-long2", "r-short"]));
    for tree in &many {
        assert_eq!(tree.triple, target);
        sound(&s, tree, &mut Vec::new());
        replay(tree, &profile.rules().unwrap());
    }
    materialize(&s, &profile, &ReasonOptions::default()).unwrap();
    assert!(holds(&s, Some(INFERRED_GRAPH), &target));
}

#[test]
fn compact_root_exclusion_keeps_late_independent_alternative() {
    let profile = Profile::Rules(format!(
        "{}\n@prefix ex: <http://ex.org/> .\n[r-short: (?s ex:p ?o) -> (?s ex:q ?o)]\n[early: (?s ex:q ?o) -> (?s ex:x ?o)]\n[late1: (?s ex:p ?o) -> (?s ex:u ?o)]\n[late2: (?s ex:u ?o) -> (?s ex:v ?o)]\n[late3: (?s ex:v ?o) -> (?s ex:x ?o)]\n[alt: (?s ex:x ?o) -> (?s ex:q ?o)]",
        include_str!("../rules/owl-rl.rules")
    ));
    let s = store("ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .");
    let target = [ex("s"), ex("q"), ex("o")];
    let (one, one_more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions::default(),
    ));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].depth, 1);
    assert_eq!(one[0].rule.as_ref().unwrap().name, "r-short");
    assert!(one_more);
    let (many, more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions {
            max_trees: 2,
            ..ExplainOptions::default()
        },
    ));
    let names: BTreeSet<_> = many
        .iter()
        .filter_map(|tree| tree.rule.as_ref().map(|rule| rule.name.as_str()))
        .collect();
    assert_eq!(names, BTreeSet::from(["alt", "r-short"]));
    assert!(!more);
    for tree in one.iter().chain(&many) {
        assert_eq!(tree.triple, target);
        sound(&s, tree, &mut Vec::new());
        replay(tree, &profile.rules().unwrap());
    }
}

#[test]
fn compact_root_cycles_do_not_advertise_an_independent_alternative() {
    let profile = Profile::Rules(format!(
        "{}\n@prefix ex: <http://ex.org/> .\n[r-short: (?s ex:p ?o) -> (?s ex:q ?o)]\n[cycle1: (?s ex:q ?o) -> (?s ex:x ?o)]\n[cycle2: (?s ex:x ?o) -> (?s ex:q ?o)]",
        include_str!("../rules/owl-rl.rules")
    ));
    let s = store("ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .");
    let target = [ex("s"), ex("q"), ex("o")];
    let (one, more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions::default(),
    ));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].depth, 1);
    assert_eq!(one[0].rule.as_ref().unwrap().name, "r-short");
    assert!(!more);
    sound(&s, &one[0], &mut Vec::new());
    replay(&one[0], &profile.rules().unwrap());
}

#[test]
fn compact_first_round_alternatives_keep_more_metadata() {
    let profile = Profile::Rules(format!(
        "{}\n@prefix ex: <http://ex.org/> .\n[first: (?s ex:p ?o) -> (?s ex:q ?o)]\n[second: (?s ex:p ?o) -> (?s ex:q ?o)]",
        include_str!("../rules/owl-rl.rules")
    ));
    let s = store("ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .");
    let target = [ex("s"), ex("q"), ex("o")];
    let (one, more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions::default(),
    ));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].depth, 1);
    assert!(more);
    let (many, more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions {
            max_trees: 2,
            ..ExplainOptions::default()
        },
    ));
    let names: BTreeSet<_> = many
        .iter()
        .filter_map(|tree| tree.rule.as_ref().map(|rule| rule.name.as_str()))
        .collect();
    assert_eq!(names, BTreeSet::from(["first", "second"]));
    assert!(!more);
    for tree in one.iter().chain(&many) {
        assert_eq!(tree.triple, target);
        sound(&s, tree, &mut Vec::new());
        replay(tree, &profile.rules().unwrap());
    }
}

#[test]
fn compact_same_rule_distinct_supports_count_as_distinct_instances() {
    let profile = Profile::Rules(format!(
        "{}\n@prefix ex: <http://ex.org/> .\n[choose: (?s ex:edge ?m), (?m ex:edge ?o) -> (?s ex:q ?o)]",
        include_str!("../rules/owl-rl.rules")
    ));
    let s = store(
        "ex:s ex:edge ex:a . ex:a ex:edge ex:o . ex:s ex:edge ex:b . ex:b ex:edge ex:o . rdf:type rdfs:subPropertyOf rdf:type .",
    );
    let target = [ex("s"), ex("q"), ex("o")];
    let (one, more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions::default(),
    ));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].depth, 1);
    assert!(more);
    let (many, more) = trees(demand(
        &s,
        &profile,
        target.clone(),
        &ExplainOptions {
            max_trees: 2,
            ..ExplainOptions::default()
        },
    ));
    assert_eq!(many.len(), 2);
    assert!(!more);
    let middles: Vec<_> = many
        .iter()
        .map(|tree| {
            assert_eq!(tree.rule.as_ref().unwrap().name, "choose");
            tree.bindings
                .iter()
                .find(|(name, _)| name == "m")
                .unwrap()
                .1
                .clone()
        })
        .collect();
    assert!(middles.contains(&ex("a")) && middles.contains(&ex("b")));
    for tree in one.iter().chain(&many) {
        assert_eq!(tree.triple, target);
        sound(&s, tree, &mut Vec::new());
        replay(tree, &profile.rules().unwrap());
    }
}

/// A second explanation on the same view reads the equality stages' facts from the
/// table cache, and its trees are as sound and replay as the first's.
#[test]
fn explanations_from_cached_equality_facts_are_sound() {
    let s = store(
        "ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c . ex:c a ex:C . ex:C rdfs:subClassOf ex:D .",
    );
    let p = Profile::OwlRl;
    let t = [ex("a"), a(), ex("D")];
    let first = trees(demand(&s, &p, t.clone(), &ExplainOptions::default()));
    for _ in 0..2 {
        let (ts, more) = trees(demand(&s, &p, t.clone(), &ExplainOptions::default()));
        assert_eq!(ts.len(), first.0.len());
        assert_eq!(more, first.1);
        for d in &ts {
            sound(&s, d, &mut Vec::new());
            replay(d, &p.rules().unwrap());
        }
    }
}
