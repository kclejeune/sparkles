//! Tests of the OWL 2 EL classifier (spec F24): worked examples, a comparison with a
//! naive completion procedure on random ontologies, incremental runs against full ones,
//! and the limits of a run.

use proptest::prelude::*;
use sparkles_owl::classify::{Classifier, Error, Gap, Limits};
use sparkles_owl::model::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn c(s: &str) -> Class {
    Class::new(format!("http://ex.org/{s}"))
}
fn ce(s: &str) -> ClassExpression {
    c(s).into()
}
fn p(s: &str) -> ObjectProperty {
    ObjectProperty::new(format!("http://ex.org/{s}"))
}
fn sub(a: impl Into<ClassExpression>, b: impl Into<ClassExpression>) -> Axiom {
    Axiom::sub_class_of(a, b)
}
fn some(r: &str, f: impl Into<ClassExpression>) -> ClassExpression {
    ClassExpression::some(p(r), f)
}
fn and(cs: impl IntoIterator<Item = ClassExpression>) -> ClassExpression {
    ClassExpression::and(cs)
}
fn ax(k: AxiomKind) -> Axiom {
    Axiom::new(k)
}
fn ope(s: &str) -> ObjectPropertyExpression {
    p(s).into()
}

fn run(axioms: &[Axiom]) -> sparkles_owl::classify::Classification {
    Classifier::of(axioms).classify(&Limits::default()).unwrap()
}

#[test]
fn told_hierarchy_and_equivalence() {
    let out = run(&[
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        Axiom::equivalent_classes([ce("C"), ce("D")]),
    ]);
    let t = &out.taxonomy;
    assert!(out.consistent);
    assert!(t.is_subclass(&c("A"), &c("C")));
    assert!(t.is_subclass(&c("A"), &c("D")));
    assert_eq!(t.equivalents(&c("C")), &[c("C"), c("D")]);
    let parents: Vec<&Class> = t
        .parents(&c("A"))
        .iter()
        .map(|&n| &t.node(n).classes[0])
        .collect();
    assert_eq!(parents, vec![&c("B")]);
    assert_eq!(t.parents(&c("C")), &[0]);
    assert_eq!(t.unsatisfiable().count(), 0);
}

#[test]
fn definitions_by_existentials_and_intersections() {
    // Nucleus ⊑ ∃partOf.Cell, CellPart ≡ ∃partOf.Cell, Organelle ≡ CellPart ⊓ Structure
    let out = run(&[
        sub(c("Nucleus"), some("partOf", c("Cell"))),
        sub(c("Nucleus"), c("Structure")),
        Axiom::equivalent_classes([ce("CellPart"), some("partOf", c("Cell"))]),
        Axiom::equivalent_classes([ce("Organelle"), and([ce("CellPart"), ce("Structure")])]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("Nucleus"), &c("CellPart")));
    assert!(t.is_subclass(&c("Nucleus"), &c("Organelle")));
    let parents: Vec<&Class> = t
        .parents(&c("Nucleus"))
        .iter()
        .map(|&n| &t.node(n).classes[0])
        .collect();
    assert_eq!(parents, vec![&c("Organelle")]);
}

#[test]
fn role_hierarchy_chains_and_transitivity() {
    let out = run(&[
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope("hasPart")),
            ope("hasComponent"),
        )),
        ax(AxiomKind::TransitiveObjectProperty(ope("partOf"))),
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("locatedIn"), ope("partOf")]),
            ope("locatedIn"),
        )),
        sub(c("A"), some("hasPart", c("B"))),
        Axiom::equivalent_classes([ce("HasComp"), some("hasComponent", c("B"))]),
        sub(c("X"), some("partOf", c("Y"))),
        sub(c("Y"), some("partOf", c("Z"))),
        Axiom::equivalent_classes([ce("PartOfZ"), some("partOf", c("Z"))]),
        sub(c("L"), some("locatedIn", c("X"))),
        Axiom::equivalent_classes([ce("InZ"), some("locatedIn", c("Z"))]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("A"), &c("HasComp")));
    assert!(t.is_subclass(&c("X"), &c("PartOfZ")));
    assert!(t.is_subclass(&c("L"), &c("InZ")));
    assert!(!t.is_subclass(&c("Y"), &c("InZ")));
}

#[test]
fn domains_ranges_and_reflexivity() {
    let out = run(&[
        ax(AxiomKind::ObjectPropertyDomain(
            ope("teaches"),
            ce("Teacher"),
        )),
        ax(AxiomKind::ObjectPropertyRange(ope("teaches"), ce("Course"))),
        sub(
            c("Prof"),
            some("teaches", ClassExpression::Class(Class::thing())),
        ),
        Axiom::equivalent_classes([ce("TeachesCourse"), some("teaches", c("Course"))]),
        ax(AxiomKind::ReflexiveObjectProperty(ope("knows"))),
        Axiom::equivalent_classes([ce("KnowsPerson"), some("knows", c("Person"))]),
        sub(c("Student"), c("Person")),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("Prof"), &c("Teacher")));
    assert!(t.is_subclass(&c("Prof"), &c("TeachesCourse")));
    assert!(t.is_subclass(&c("Student"), &c("KnowsPerson")));
    assert!(t.is_subclass(&c("Person"), &c("KnowsPerson")));
}

#[test]
fn disjointness_and_unsatisfiable_classes() {
    let out = run(&[
        Axiom::disjoint_classes([ce("A"), ce("B"), ce("C")]),
        sub(c("AB"), c("A")),
        sub(c("AB"), c("B")),
        sub(c("D"), some("r", c("AB"))),
        sub(c("E"), c("A")),
    ]);
    let t = &out.taxonomy;
    let unsat: Vec<&Class> = t.unsatisfiable().collect();
    assert_eq!(unsat, vec![&c("AB"), &c("D")]);
    assert!(t.is_subclass(&c("D"), &c("E")));
    assert!(!t.is_unsatisfiable(&c("E")));
    assert!(out.consistent);
    let axioms = t.axioms();
    assert!(axioms.contains(&Axiom::equivalent_classes([
        ClassExpression::Class(Class::nothing()),
        ce("AB"),
        ce("D")
    ])));
}

#[test]
fn inconsistency_from_assertions() {
    let a = NamedIndividual::new("http://ex.org/a");
    let out = run(&[
        Axiom::disjoint_classes([ce("A"), ce("B")]),
        Axiom::class_assertion(ce("A"), Individual::Named(a.clone())),
        Axiom::class_assertion(ce("B"), Individual::Named(a)),
        sub(c("C"), c("A")),
    ]);
    assert!(!out.consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("C")));
}

#[test]
fn equivalent_to_thing() {
    let thing = ClassExpression::Class(Class::thing());
    let out = run(&[sub(thing, c("Any")), sub(c("A"), c("B"))]);
    let t = &out.taxonomy;
    assert_eq!(t.top().classes, vec![Class::thing(), c("Any")]);
    assert!(t.is_subclass(&c("B"), &c("Any")));
}

#[test]
fn has_self_loops() {
    let self_r = ClassExpression::ObjectHasSelf(ope("r"));
    let out = run(&[
        sub(c("A"), self_r.clone()),
        sub(c("A"), c("B")),
        Axiom::equivalent_classes([ce("RB"), some("r", c("B"))]),
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope("r")),
            ope("s"),
        )),
        Axiom::equivalent_classes([ce("SelfS"), ClassExpression::ObjectHasSelf(ope("s"))]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("A"), &c("RB")));
    assert!(t.is_subclass(&c("A"), &c("SelfS")));
}

#[test]
fn axioms_outside_el_are_reported() {
    let mut k = Classifier::of(&[
        sub(c("A"), ClassExpression::or([ce("B"), ce("C")])),
        sub(c("A"), c("D")),
        sub(
            c("E"),
            ClassExpression::ObjectHasValue(ope("r"), Individual::named("http://ex.org/i")),
        ),
        ax(AxiomKind::InverseObjectProperties(ope("r"), ope("s"))),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    // classes of axioms left out are still in the taxonomy
    assert!(out.taxonomy.node_of(&c("B")).is_some());
    let ex = k.excluded();
    assert_eq!(ex.len(), 3, "{ex:#?}");
    let outside: Vec<&Excl> = ex
        .iter()
        .filter(|e| matches!(e.gap, Gap::Outside { .. }))
        .collect();
    assert_eq!(outside.len(), 2);
    assert!(
        ex.iter()
            .any(|e| matches!(e.gap, Gap::Partial("individuals")))
    );
}

type Excl = sparkles_owl::classify::Excluded;

#[test]
fn chains_against_the_range_restriction_are_left_out() {
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("s"), ce("R"))),
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("a"), ope("b")]),
            ope("s"),
        )),
    ]);
    k.classify(&Limits::default()).unwrap();
    let ex = k.excluded();
    assert_eq!(ex.len(), 1);
    assert!(matches!(&ex[0].gap, Gap::Outside { rule, .. } if rule.contains("2.2.6")));
}

#[test]
fn limits_stop_a_run_and_the_next_run_starts_again() {
    let mut axioms = Vec::new();
    for i in 0..300 {
        axioms.push(sub(
            c(&format!("A{i}")),
            some("r", c(&format!("A{}", i + 1))),
        ));
        axioms.push(sub(c(&format!("A{i}")), c(&format!("B{}", i % 7))));
    }
    axioms.push(ax(AxiomKind::TransitiveObjectProperty(ope("r"))));
    for i in 0..50 {
        axioms.push(Axiom::equivalent_classes([
            ce(&format!("E{i}")),
            some("r", c(&format!("A{}", i * 5))),
        ]));
    }
    let mut k = Classifier::of(&axioms);
    let past = Limits {
        deadline: Some(std::time::Instant::now()),
        ..Limits::default()
    };
    assert_eq!(k.classify(&past).unwrap_err(), Error::Timeout);
    let tiny = Limits {
        max_memory: Some(10_000),
        ..Limits::default()
    };
    assert!(matches!(
        k.classify(&tiny).unwrap_err(),
        Error::Memory { .. }
    ));
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    let cancel = Limits {
        cancelled: Some(Arc::new(move || f.load(Ordering::Relaxed))),
        ..Limits::default()
    };
    assert_eq!(k.classify(&cancel).unwrap_err(), Error::Cancelled);
    flag.store(false, Ordering::Relaxed);
    let out = k.classify(&cancel).unwrap();
    assert!(!out.stats.incremental);
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    assert_eq!(out.taxonomy, full.taxonomy);
    assert!(out.taxonomy.is_subclass(&c("A0"), &c("E30")));
}

#[test]
fn incremental_runs_reuse_contexts() {
    let mut axioms = Vec::new();
    for i in 0..200 {
        axioms.push(sub(c(&format!("A{i}")), c(&format!("A{}", i + 1))));
        axioms.push(sub(c(&format!("X{i}")), c(&format!("Y{i}"))));
    }
    let mut k = Classifier::of(&axioms);
    k.classify(&Limits::default()).unwrap();
    let extra = sub(c("X5"), c("A199"));
    k.add([&extra]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.stats.incremental);
    assert!(out.stats.cleared < 10, "{:?}", out.stats);
    assert!(out.taxonomy.is_subclass(&c("X5"), &c("A200")));
    k.remove([&extra]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("X5"), &c("A200")));
    assert_eq!(
        out.taxonomy,
        Classifier::of(&axioms)
            .classify(&Limits::default())
            .unwrap()
            .taxonomy
    );
}

fn nested(depth: usize, filler: &str) -> ClassExpression {
    let mut e = ce(filler);
    for _ in 0..depth {
        e = some("r", e);
    }
    e
}

#[test]
fn an_expression_nested_too_deeply_leaves_its_axiom_out() {
    // a stand-in for the deep fillers would make the two left sides one expression
    let mut k = Classifier::of(&[sub(c("A"), nested(300, "C")), sub(nested(300, "D"), c("B"))]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("B")));
    let partial = k
        .excluded()
        .iter()
        .filter(|e| matches!(e.gap, Gap::Partial("class expressions nested too deeply")))
        .count();
    assert_eq!(partial, 2);
}

fn dp(s: &str) -> DataProperty {
    DataProperty::new(format!("http://ex.org/{s}"))
}

#[test]
fn a_change_to_a_data_axiom_runs_everything_again() {
    let has = ClassExpression::DataHasValue(dp("p"), Literal::simple("x"));
    let mut k = Classifier::of(&[sub(c("A"), has)]);
    k.classify(&Limits::default()).unwrap();
    let domain = ax(AxiomKind::DataPropertyDomain(dp("p"), ce("D")));
    k.add([&domain]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
}

#[test]
fn assertions_on_a_property_with_a_range_are_partial() {
    let i = |s: &str| Individual::named(format!("http://ex.org/{s}"));
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("r"), ce("C"))),
        ax(AxiomKind::ObjectPropertyRange(ope("s"), ce("D"))),
        ax(AxiomKind::DisjointClasses(
            [ce("C"), ce("D")].into_iter().collect(),
        )),
        ax(AxiomKind::ObjectPropertyAssertion(ope("r"), i("a"), i("b"))),
        ax(AxiomKind::ObjectPropertyAssertion(ope("s"), i("d"), i("b"))),
    ]);
    k.classify(&Limits::default()).unwrap();
    let partial = k
        .excluded()
        .iter()
        .filter(|e| matches!(e.gap, Gap::Partial(_)))
        .count();
    assert_eq!(partial, 2, "{:#?}", k.excluded());
}

#[test]
fn axioms_on_the_top_and_bottom_properties_are_partial() {
    let top = ObjectPropertyExpression::from(ObjectProperty::top());
    let bottom = ObjectPropertyExpression::from(ObjectProperty::bottom());
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyDomain(top, ce("C"))),
        ax(AxiomKind::ReflexiveObjectProperty(bottom)),
    ]);
    k.classify(&Limits::default()).unwrap();
    let ex = k.excluded();
    assert_eq!(ex.len(), 2, "{ex:#?}");
    assert!(ex.iter().all(|e| matches!(e.gap, Gap::Partial(_))));
}

#[test]
fn the_property_closure_is_bounded_by_the_memory_limit() {
    let mut axioms = Vec::new();
    for i in 0..1000 {
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope(&format!("r{i}"))),
            ope(&format!("r{}", i + 1)),
        )));
    }
    let limit = Limits {
        max_memory: Some(128_000),
        ..Limits::default()
    };
    assert!(matches!(
        Classifier::of(&axioms).classify(&limit).unwrap_err(),
        Error::Memory { .. }
    ));
}

#[test]
fn waiting_messages_count_against_the_memory_limit() {
    // every link from A to B through a subrole of s meets every rule at B, so A's
    // to-do list takes the square of n messages and stores only n of them
    let n = 300;
    let mut axioms = Vec::new();
    for i in 0..n {
        let r = format!("r{i}");
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope(&r)),
            ope("s"),
        )));
        axioms.push(sub(c("A"), some(&r, c("B"))));
        axioms.push(sub(c("B"), c(&format!("F{i}"))));
        axioms.push(sub(some("s", c(&format!("F{i}"))), c(&format!("G{i}"))));
    }
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    assert!(full.taxonomy.is_subclass(&c("A"), &c("G7")));
    let limit = Limits {
        threads: 1,
        max_memory: Some(full.stats.memory + 200_000),
        ..Limits::default()
    };
    // the error reports what passed the limit, the waiting messages included
    match Classifier::of(&axioms).classify(&limit).unwrap_err() {
        Error::Memory { limit, used } => assert!(used > limit, "{used} <= {limit}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn chains_that_share_operands_are_charged_once_per_entry() {
    let mut axioms = Vec::new();
    for i in 0..100 {
        for (base, sub) in [("r", "rs"), ("s", "ss")] {
            axioms.push(ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ope(&format!("{sub}{i}"))),
                ope(base),
            )));
        }
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("r"), ope("s")]),
            ope(&format!("t{i}")),
        )));
    }
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    let limit = Limits {
        max_memory: Some(full.stats.memory * 2),
        ..Limits::default()
    };
    Classifier::of(&axioms).classify(&limit).unwrap();
}

#[test]
fn the_property_closure_polls_the_cancellation() {
    let mut axioms = Vec::new();
    for i in 0..3000 {
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope(&format!("r{i}"))),
            ope(&format!("r{}", i + 1)),
        )));
    }
    // normalization polls once, then the closure is the next to poll
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let m = n.clone();
    let limits = Limits {
        cancelled: Some(Arc::new(move || m.fetch_add(1, Ordering::Relaxed) >= 1)),
        ..Limits::default()
    };
    let start = std::time::Instant::now();
    assert_eq!(
        Classifier::of(&axioms).classify(&limits).unwrap_err(),
        Error::Cancelled
    );
    // the closure stopped at its first poll, not after its 4.5 million entries
    assert!(n.load(Ordering::Relaxed) >= 2);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn a_stop_after_the_saturation_keeps_the_saturated_state() {
    let mut axioms = Vec::new();
    for i in 0..1000 {
        axioms.push(sub(c(&format!("A{i}")), c(&format!("A{}", i + 1))));
    }
    let counting = |stop_at: usize| {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let m = n.clone();
        let limits = Limits {
            threads: 1,
            cancelled: Some(Arc::new(move || {
                m.fetch_add(1, Ordering::Relaxed) + 1 == stop_at
            })),
            ..Limits::default()
        };
        (n, limits)
    };
    // the number of polls of a whole run, the last of them after the taxonomy
    let (n, limits) = counting(0);
    Classifier::of(&axioms).classify(&limits).unwrap();
    let last = n.load(Ordering::Relaxed);
    let (_, limits) = counting(last);
    let mut k = Classifier::of(&axioms);
    assert_eq!(k.classify(&limits).unwrap_err(), Error::Cancelled);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.stats.incremental);
    assert_eq!(out.stats.cleared, 0);
    assert!(out.taxonomy.is_subclass(&c("A0"), &c("A1000")));
}

#[test]
fn removing_an_axiom_not_held_does_not_cancel_a_later_addition() {
    let a = sub(c("A"), c("B"));
    let mut k = Classifier::new();
    k.remove([&a]);
    k.add([&a]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
}

// ---------------------------------------------------------------------------------
// A naive completion procedure for EL++ without nominals or concrete domains (Baader,
// Brandt and Lutz, "Pushing the EL Envelope", IJCAI 2005, with the range rule of their
// 2008 follow-up), written independently of the classifier as an oracle.

#[derive(Default)]
struct Naive {
    n: usize,
    names: BTreeMap<String, usize>,
    /// A1 ⊓ … ⊓ An ⊑ B
    nf1: Vec<(Vec<usize>, usize)>,
    /// A ⊑ ∃r.B
    nf2: Vec<(usize, usize, usize)>,
    /// ∃r.A ⊑ B
    nf3: Vec<(usize, usize, usize)>,
    roles: BTreeMap<String, usize>,
    role_sub: Vec<(usize, usize)>,
    chains: Vec<(usize, usize, usize)>,
    reflexive: BTreeSet<usize>,
    ranges: Vec<(usize, usize)>,
}

const NTOP: usize = 0;
const NBOT: usize = 1;

impl Naive {
    fn new() -> Naive {
        Naive {
            n: 2,
            ..Naive::default()
        }
    }
    fn fresh(&mut self) -> usize {
        self.n += 1;
        self.n - 1
    }
    fn role(&mut self, p: &ObjectPropertyExpression) -> usize {
        let k = p.named().as_str().to_string();
        let next = self.roles.len();
        *self.roles.entry(k).or_insert(next)
    }
    fn name(&mut self, e: &ClassExpression) -> usize {
        match e {
            ClassExpression::Class(c) if c == &Class::thing() => NTOP,
            ClassExpression::Class(c) if c == &Class::nothing() => NBOT,
            ClassExpression::Class(c) => {
                if let Some(&i) = self.names.get(c.as_str()) {
                    return i;
                }
                let i = self.fresh();
                self.names.insert(c.as_str().to_string(), i);
                i
            }
            ClassExpression::ObjectIntersectionOf(cs) => {
                let parts: Vec<usize> = cs.iter().map(|c| self.name(c)).collect();
                let x = self.fresh();
                for &a in &parts {
                    self.nf1.push((vec![x], a));
                }
                self.nf1.push((parts, x));
                x
            }
            ClassExpression::ObjectSomeValuesFrom(r, f) => {
                let f = self.name(f);
                let r = self.role(r);
                let x = self.fresh();
                self.nf2.push((x, r, f));
                self.nf3.push((r, f, x));
                x
            }
            other => panic!("not in the generator: {other:?}"),
        }
    }
    fn axiom(&mut self, a: &Axiom) {
        match &a.kind {
            AxiomKind::SubClassOf(x, y) => {
                let (x, y) = (self.name(x), self.name(y));
                self.nf1.push((vec![x], y));
            }
            AxiomKind::EquivalentClasses(cs) => {
                let ns: Vec<usize> = cs.iter().map(|c| self.name(c)).collect();
                for &a in &ns {
                    for &b in &ns {
                        self.nf1.push((vec![a], b));
                    }
                }
            }
            AxiomKind::DisjointClasses(cs) => {
                let ns: Vec<usize> = cs.iter().map(|c| self.name(c)).collect();
                for i in 0..ns.len() {
                    for j in i + 1..ns.len() {
                        self.nf1.push((vec![ns[i], ns[j]], NBOT));
                    }
                }
            }
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(r), s) => {
                let (r, s) = (self.role(r), self.role(s));
                self.role_sub.push((r, s));
            }
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(rs), s) => {
                assert_eq!(rs.len(), 2);
                let (a, b, s) = (self.role(&rs[0]), self.role(&rs[1]), self.role(s));
                self.chains.push((a, b, s));
            }
            AxiomKind::TransitiveObjectProperty(r) => {
                let r = self.role(r);
                self.chains.push((r, r, r));
            }
            AxiomKind::ReflexiveObjectProperty(r) => {
                let r = self.role(r);
                self.reflexive.insert(r);
            }
            AxiomKind::ObjectPropertyDomain(r, c) => {
                let (r, c) = (self.role(r), self.name(c));
                self.nf3.push((r, NTOP, c));
            }
            AxiomKind::ObjectPropertyRange(r, c) => {
                let (r, c) = (self.role(r), self.name(c));
                self.ranges.push((r, c));
            }
            AxiomKind::Declaration(Entity::Class(c)) => {
                self.name(&c.clone().into());
            }
            other => panic!("not in the generator: {other:?}"),
        }
    }

    /// The subsumers of every basic concept.
    #[allow(clippy::needless_range_loop)]
    fn complete(mut self) -> (BTreeMap<String, usize>, Vec<BTreeSet<usize>>) {
        let nr = self.roles.len();
        // the reflexive-transitive closure of the role hierarchy
        let mut sup = vec![BTreeSet::new(); nr];
        for (r, s) in sup.iter_mut().enumerate() {
            s.insert(r);
        }
        loop {
            let mut changed = false;
            for &(r, s) in &self.role_sub {
                let add: Vec<usize> = sup[s].iter().copied().collect();
                for t in add {
                    changed |= sup[r].insert(t);
                }
            }
            if !changed {
                break;
            }
        }
        // ranges push into the fillers of existentials on the right
        let mut rng = vec![BTreeSet::new(); nr];
        for &(r, c) in &self.ranges {
            for (q, s) in sup.iter().enumerate() {
                if s.contains(&r) {
                    rng[q].insert(c);
                }
            }
        }
        let old = std::mem::take(&mut self.nf2);
        for (x, r, f) in old {
            if rng[r].is_empty() {
                self.nf2.push((x, r, f));
            } else {
                let y = self.fresh();
                self.nf1.push((vec![y], f));
                for &c in &rng[r] {
                    self.nf1.push((vec![y], c));
                }
                self.nf2.push((x, r, y));
            }
        }
        for &r in &self.reflexive.clone() {
            for &c in &rng[r] {
                self.nf1.push((vec![NTOP], c));
            }
        }
        let n = self.n;
        let mut s: Vec<BTreeSet<usize>> = (0..n).map(|x| BTreeSet::from([x, NTOP])).collect();
        let mut rel: Vec<BTreeSet<(usize, usize)>> = vec![BTreeSet::new(); nr];
        let reflexive: BTreeSet<usize> = self
            .reflexive
            .iter()
            .flat_map(|&r| sup[r].iter().copied().collect::<Vec<_>>())
            .collect();
        for &r in &reflexive {
            for x in 0..n {
                rel[r].insert((x, x));
            }
        }
        loop {
            let mut changed = false;
            for x in 0..n {
                for (lhs, b) in &self.nf1 {
                    if lhs.iter().all(|a| s[x].contains(a)) && !s[x].contains(b) {
                        s[x].insert(*b);
                        changed = true;
                    }
                }
                for &(a, r, b) in &self.nf2 {
                    if s[x].contains(&a) {
                        for &t in &sup[r] {
                            changed |= rel[t].insert((x, b));
                        }
                    }
                }
            }
            for r in 0..nr {
                let pairs: Vec<(usize, usize)> = rel[r].iter().copied().collect();
                for (x, y) in pairs {
                    for &(r2, a, b) in &self.nf3 {
                        if r2 == r && s[y].contains(&a) && !s[x].contains(&b) {
                            s[x].insert(b);
                            changed = true;
                        }
                    }
                    if s[y].contains(&NBOT) && !s[x].contains(&NBOT) {
                        s[x].insert(NBOT);
                        changed = true;
                    }
                    for &t in &sup[r] {
                        changed |= rel[t].insert((x, y));
                    }
                }
            }
            for &(r1, r2, t) in &self.chains {
                let left: Vec<(usize, usize)> = rel[r1].iter().copied().collect();
                let right: Vec<(usize, usize)> = rel[r2].iter().copied().collect();
                for &(x, y) in &left {
                    for &(y2, z) in &right {
                        if y == y2 {
                            for &u in &sup[t] {
                                changed |= rel[u].insert((x, z));
                            }
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        (self.names, s)
    }
}

fn naive_subsumptions(axioms: &[Axiom]) -> BTreeSet<(String, String)> {
    let mut nv = Naive::new();
    for a in axioms {
        nv.axiom(a);
    }
    let (names, s) = nv.complete();
    let mut all: Vec<(String, usize)> = names.into_iter().collect();
    all.push((Class::thing().as_str().to_string(), NTOP));
    all.push((Class::nothing().as_str().to_string(), NBOT));
    let inconsistent = s[NTOP].contains(&NBOT);
    let mut out = BTreeSet::new();
    for (a, x) in &all {
        for (b, y) in &all {
            if inconsistent
                || s[*x].contains(y)
                || s[*x].contains(&NBOT)
                || *y == NTOP
                || *x == NBOT
            {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    out
}

fn classifier_subsumptions(t: &sparkles_owl::classify::Taxonomy) -> BTreeSet<(String, String)> {
    let mut all: Vec<Class> = t.classes().cloned().collect();
    all.push(Class::thing());
    all.push(Class::nothing());
    let mut out = BTreeSet::new();
    for a in &all {
        for b in &all {
            if t.is_subclass(a, b) {
                out.insert((a.as_str().to_string(), b.as_str().to_string()));
            }
        }
    }
    out
}

const CLASSES: usize = 6;
const ROLES: usize = 4;

fn gen_class() -> impl Strategy<Value = ClassExpression> {
    prop_oneof![
        8 => (0..CLASSES).prop_map(|i| ce(&format!("C{i}"))),
        1 => Just(ClassExpression::Class(Class::thing())),
    ]
}

fn gen_expr() -> impl Strategy<Value = ClassExpression> {
    gen_class().prop_recursive(2, 8, 2, |inner| {
        prop_oneof![
            (0..ROLES, inner.clone()).prop_map(|(r, f)| some(&format!("r{r}"), f)),
            (inner.clone(), inner).prop_map(|(a, b)| and([a, b])),
        ]
    })
}

fn r(i: usize) -> ObjectPropertyExpression {
    ope(&format!("r{i}"))
}

/// Ranges go on r0 and r1 only, and chains and transitivity on r2 and r3, which are
/// never subproperties of r0 or r1, so that every ontology meets the range restriction
/// of OWL 2 EL.
fn gen_axiom() -> impl Strategy<Value = Axiom> {
    let role_edges = [
        (0, 1),
        (1, 0),
        (0, 2),
        (0, 3),
        (1, 2),
        (1, 3),
        (2, 3),
        (3, 2),
    ];
    prop_oneof![
        10 => (gen_expr(), gen_expr()).prop_map(|(a, b)| sub(a, b)),
        6 => ((0..CLASSES), gen_expr()).prop_map(|(a, b)| sub(ce(&format!("C{a}")), b)),
        3 => ((0..CLASSES), gen_expr())
            .prop_map(|(a, b)| Axiom::equivalent_classes([ce(&format!("C{a}")), b])),
        1 => ((0..CLASSES), (0..CLASSES)).prop_filter("distinct", |(a, b)| a != b).prop_map(|(a, b)| {
            Axiom::disjoint_classes([ce(&format!("C{a}")), ce(&format!("C{b}"))])
        }),
        2 => (0..role_edges.len()).prop_map(move |i| {
            let (a, b) = role_edges[i];
            ax(AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(r(a)), r(b)))
        }),
        1 => ((0..ROLES), (0..ROLES), (2..ROLES)).prop_map(|(a, b, s)| {
            ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Chain(vec![r(a), r(b)]),
                r(s),
            ))
        }),
        1 => (2..ROLES).prop_map(|s| ax(AxiomKind::TransitiveObjectProperty(r(s)))),
        1 => (0..ROLES).prop_map(|s| ax(AxiomKind::ReflexiveObjectProperty(r(s)))),
        1 => ((0..ROLES), gen_class()).prop_map(|(s, c)| ax(AxiomKind::ObjectPropertyDomain(r(s), c))),
        1 => ((0..2usize), gen_class()).prop_map(|(s, c)| ax(AxiomKind::ObjectPropertyRange(r(s), c))),
    ]
}

fn with_declarations(mut axioms: Vec<Axiom>) -> Vec<Axiom> {
    for i in 0..CLASSES {
        axioms.push(Axiom::declaration(Entity::Class(c(&format!("C{i}")))));
    }
    axioms
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn agrees_with_naive_completion(axioms in prop::collection::vec(gen_axiom(), 0..20), threads in 1..4usize) {
        let axioms = with_declarations(axioms);
        let want = naive_subsumptions(&axioms);
        let out = Classifier::of(&axioms)
            .classify(&Limits { threads, ..Limits::default() })
            .unwrap();
        let got = classifier_subsumptions(&out.taxonomy);
        prop_assert_eq!(got, want);
    }

    #[test]
    fn incremental_equals_full(
        base in prop::collection::vec(gen_axiom(), 0..12),
        steps in prop::collection::vec((prop::collection::vec(gen_axiom(), 0..4), prop::collection::vec(any::<prop::sample::Index>(), 0..4)), 1..5),
        threads in 1..4usize,
    ) {
        let limits = Limits { threads, ..Limits::default() };
        let mut held = with_declarations(base);
        let mut k = Classifier::of(&held);
        k.classify(&limits).unwrap();
        for (adds, removes) in steps {
            let mut removed = Vec::new();
            for i in removes {
                if !held.is_empty() {
                    removed.push(held.remove(i.index(held.len())));
                }
            }
            k.remove(&removed);
            k.add(&adds);
            held.extend(adds);
            let inc = k.classify(&limits).unwrap();
            let full = Classifier::of(&held).classify(&limits).unwrap();
            prop_assert_eq!(&inc.taxonomy, &full.taxonomy);
            prop_assert_eq!(inc.consistent, full.consistent);
        }
    }
}
