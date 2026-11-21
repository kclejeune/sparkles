//! A differential sweep of incremental OWL 2 EL classification (roadmap item 1258):
//! edit sequences over the constructors whose rules read state that a context derives
//! without holding the changed expression, each run against a classifier that starts
//! from nothing. Role axioms, individuals and data stay in the fixed part of each
//! ontology, since a change to them starts the run again, so the edits reach the
//! kept path. `OWL_SWEEP_SEEDS` sets the number of ontologies of each family.

use sparkles_owl::classify::{Changes, Classifier, Limits, Saturation};
use sparkles_owl::model::*;
use std::collections::{BTreeMap, BTreeSet};

fn c(s: &str) -> Class {
    Class::new(format!("http://ex.org/{s}"))
}
fn ce(s: &str) -> ClassExpression {
    c(s).into()
}
fn p(s: &str) -> ObjectPropertyExpression {
    ObjectProperty::new(format!("http://ex.org/{s}")).into()
}
fn ind(s: &str) -> Individual {
    Individual::named(format!("http://ex.org/{s}"))
}
fn ax(k: AxiomKind) -> Axiom {
    Axiom::new(k)
}

/// A xorshift sequence, so that a failing case is found again by its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const CLASSES: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    /// self restrictions in the fixed part and now and then in an edit
    Selfs,
    /// the top and bottom object properties in edited class axioms
    TopBottom,
    /// individuals, nominals and assertions in the fixed part
    Nominals,
    /// ranges on the roles of edited existential restrictions
    Ranges,
    /// `owl:Nothing` and disjointness, so that kept targets are unsatisfiable
    Unsatisfiable,
    /// reflexive roles and chains in the fixed part
    Loops,
}

const FAMILIES: [Family; 6] = [
    Family::Selfs,
    Family::TopBottom,
    Family::Nominals,
    Family::Ranges,
    Family::Unsatisfiable,
    Family::Loops,
];

struct Gen {
    rng: Rng,
    family: Family,
}

impl Gen {
    fn class(&mut self) -> ClassExpression {
        match self.rng.below(12) {
            0 => ClassExpression::Class(Class::thing()),
            1 if self.family == Family::Unsatisfiable => ClassExpression::Class(Class::nothing()),
            _ => ce(&format!("C{}", self.rng.below(CLASSES))),
        }
    }

    fn role(&mut self) -> ObjectPropertyExpression {
        if self.family == Family::TopBottom && self.rng.chance(30) {
            if self.rng.chance(50) {
                ObjectProperty::top().into()
            } else {
                ObjectProperty::bottom().into()
            }
        } else {
            p(&format!("r{}", self.rng.below(4)))
        }
    }

    /// A class expression of OWL 2 EL, with self restrictions when `selfs` holds.
    fn expr(&mut self, depth: usize, selfs: bool) -> ClassExpression {
        if depth == 0 {
            return self.class();
        }
        match self.rng.below(10) {
            0..=2 => {
                let r = self.role();
                ClassExpression::some(r, self.expr(depth - 1, selfs))
            }
            3 | 4 => {
                ClassExpression::and([self.expr(depth - 1, selfs), self.expr(depth - 1, selfs)])
            }
            5 if selfs => ClassExpression::ObjectHasSelf(p(&format!("r{}", self.rng.below(4)))),
            _ => self.class(),
        }
    }

    /// A class axiom without individuals, which an edit may add or remove.
    fn class_axiom(&mut self, selfs: bool) -> Axiom {
        if selfs && self.rng.chance(50) {
            // a negative self restriction, alone, under an existential restriction or in
            // an intersection, whose subsumer a loop by a subrole gives
            let s = ClassExpression::ObjectHasSelf(p(&format!("r{}", self.rng.below(4))));
            let left = match self.rng.below(3) {
                0 => s,
                1 => {
                    let r = self.role();
                    ClassExpression::some(r, s)
                }
                _ => ClassExpression::and([self.class(), s]),
            };
            return Axiom::sub_class_of(left, self.expr(1, false));
        }
        match self.rng.below(10) {
            0..=4 => Axiom::sub_class_of(self.expr(2, selfs), self.expr(2, selfs)),
            5..=7 => Axiom::sub_class_of(
                ce(&format!("C{}", self.rng.below(CLASSES))),
                self.expr(2, selfs),
            ),
            8 => Axiom::equivalent_classes([
                ce(&format!("C{}", self.rng.below(CLASSES))),
                self.expr(2, selfs),
            ]),
            _ => {
                let a = self.rng.below(CLASSES);
                let b = (a + 1 + self.rng.below(CLASSES - 1)) % CLASSES;
                Axiom::disjoint_classes([ce(&format!("C{a}")), ce(&format!("C{b}"))])
            }
        }
    }

    /// The fixed part: role axioms that meet the range restriction of OWL 2 EL, with
    /// ranges on r0 and r1 and chains and transitivity on r2 and r3, which are never
    /// subproperties of r0 or r1, and the family's individuals.
    fn fixed(&mut self) -> Vec<Axiom> {
        let mut out = Vec::new();
        let edges = [
            (0, 1),
            (1, 0),
            (0, 2),
            (0, 3),
            (1, 2),
            (1, 3),
            (2, 3),
            (3, 2),
        ];
        for _ in 0..1 + self.rng.below(3) {
            let (a, b) = edges[self.rng.below(edges.len())];
            out.push(ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(p(&format!("r{a}"))),
                p(&format!("r{b}")),
            )));
        }
        let chains = self.family == Family::Loops || self.rng.chance(20);
        if chains {
            for _ in 0..1 + self.rng.below(2) {
                let (a, b, s) = (self.rng.below(4), self.rng.below(4), 2 + self.rng.below(2));
                out.push(ax(AxiomKind::SubObjectPropertyOf(
                    SubObjectPropertyExpression::Chain(vec![
                        p(&format!("r{a}")),
                        p(&format!("r{b}")),
                    ]),
                    p(&format!("r{s}")),
                )));
            }
            if self.rng.chance(50) {
                out.push(ax(AxiomKind::TransitiveObjectProperty(p(&format!(
                    "r{}",
                    2 + self.rng.below(2)
                )))));
            }
        }
        if self.family == Family::Loops || self.rng.chance(15) {
            out.push(ax(AxiomKind::ReflexiveObjectProperty(p(&format!(
                "r{}",
                self.rng.below(4)
            )))));
        }
        if self.family == Family::Ranges || self.rng.chance(15) {
            for r in 0..2 {
                if self.rng.chance(70) {
                    let range = self.class();
                    out.push(ax(AxiomKind::ObjectPropertyRange(
                        p(&format!("r{r}")),
                        range,
                    )));
                }
            }
        }
        if self.family == Family::Selfs {
            for _ in 0..2 {
                let a = self.class();
                let s = ClassExpression::ObjectHasSelf(p(&format!("r{}", self.rng.below(4))));
                let c = self.expr(1, false);
                out.push(if self.rng.chance(50) {
                    Axiom::sub_class_of(a, s)
                } else {
                    Axiom::sub_class_of(s, c)
                });
            }
        }
        if self.family == Family::Nominals {
            for _ in 0..1 + self.rng.below(3) {
                let i = ind(&format!("i{}", self.rng.below(3)));
                let x = self.expr(1, false);
                out.push(ax(AxiomKind::ClassAssertion(x, i)));
            }
            for _ in 0..self.rng.below(3) {
                out.push(ax(AxiomKind::ObjectPropertyAssertion(
                    p(&format!("r{}", self.rng.below(4))),
                    ind(&format!("i{}", self.rng.below(3))),
                    ind(&format!("i{}", self.rng.below(3))),
                )));
            }
            let x = ce(&format!("C{}", self.rng.below(CLASSES)));
            let one = ClassExpression::ObjectOneOf(
                [ind(&format!("i{}", self.rng.below(3)))]
                    .into_iter()
                    .collect(),
            );
            out.push(if self.rng.chance(50) {
                Axiom::sub_class_of(x, one)
            } else {
                Axiom::sub_class_of(one, x)
            });
        }
        for i in 0..CLASSES {
            out.push(Axiom::declaration(Entity::Class(c(&format!("C{i}")))));
        }
        out
    }
}

#[derive(Default, Debug)]
struct Count {
    runs: usize,
    kept: usize,
    /// the runs whose axioms changed, and those of them that kept contexts
    changed: usize,
    kept_changed: usize,
    /// the kept runs whose changed axioms name the top or the bottom object property
    kept_top: usize,
    kept_bottom: usize,
}

impl Count {
    fn add(&mut self, kept: bool, changed: bool, (top, bottom): (bool, bool)) {
        self.runs += 1;
        self.kept += kept as usize;
        self.changed += changed as usize;
        self.kept_changed += (kept && changed) as usize;
        self.kept_top += (kept && top) as usize;
        self.kept_bottom += (kept && bottom) as usize;
    }
}

const STEPS: usize = 6;

/// Runs one sequence of edits through a [`Classifier`] and, with the same axioms, a
/// [`Saturation`] given explicit changes, against fresh classifiers.
fn sweep_one(
    seed: u64,
    family: Family,
    threads: usize,
    counts: &mut BTreeMap<(Family, &str), Count>,
) {
    let mut g = Gen {
        rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1),
        family,
    };
    let fixed = g.fixed();
    let selfs = matches!(family, Family::Selfs | Family::Loops);
    let mut edited: Vec<Axiom> = (0..3 + g.rng.below(6))
        .map(|_| g.class_axiom(false))
        .collect();
    let limits = Limits {
        threads,
        ..Limits::default()
    };
    let all = |e: &[Axiom]| -> Vec<Axiom> { fixed.iter().chain(e).cloned().collect() };
    let kinds =
        |xs: &[Axiom]| -> BTreeSet<AxiomKind> { xs.iter().map(|a| a.kind.clone()).collect() };
    let mut k = Classifier::of(&all(&edited));
    k.classify(&limits).unwrap();
    let mut sat = Saturation::new();
    let mut before = kinds(&all(&edited));
    sat.classify(&before.iter().collect::<Vec<_>>(), None, &limits)
        .unwrap();
    for step in 0..STEPS {
        let mut removed = Vec::new();
        for _ in 0..g.rng.below(3) {
            if !edited.is_empty() {
                let i = g.rng.below(edited.len());
                removed.push(edited.remove(i));
            }
        }
        let added: Vec<Axiom> = (0..g.rng.below(3))
            .map(|_| {
                let s = selfs && g.rng.chance(25);
                g.class_axiom(s)
            })
            .collect();
        edited.extend(added.iter().cloned());
        k.remove(&removed);
        k.add(&added);
        let now_axioms = all(&edited);
        let fresh = Classifier::of(&now_axioms).classify(&limits).unwrap();
        let inc = k.classify(&limits).unwrap();
        let what = || {
            format!(
                "seed {seed} family {family:?} threads {threads} step {step}, added {added:?}, removed {removed:?}, fixed {fixed:?}, held {edited:?}"
            )
        };
        assert_eq!(inc.taxonomy, fresh.taxonomy, "classifier: {}", what());
        assert_eq!(inc.consistent, fresh.consistent, "classifier: {}", what());
        let now = kinds(&now_axioms);
        let add: Vec<&AxiomKind> = now.difference(&before).collect();
        let rem: Vec<&AxiomKind> = before.difference(&now).collect();
        let changed = !add.is_empty() || !rem.is_empty();
        let names = |p: ObjectProperty| {
            add.iter().chain(&rem).any(|k| {
                ax((*k).clone())
                    .signature_and_shape()
                    .0
                    .contains(&Entity::ObjectProperty(p.clone()))
            })
        };
        let builtins = (
            names(ObjectProperty::top()),
            names(ObjectProperty::bottom()),
        );
        counts.entry((family, "classifier")).or_default().add(
            inc.stats.incremental,
            changed,
            builtins,
        );
        let held: Vec<&AxiomKind> = now.iter().collect();
        let out = sat
            .classify(
                &held,
                Some(Changes {
                    added: &add,
                    removed: &rem,
                }),
                &limits,
            )
            .unwrap();
        assert_eq!(out.taxonomy, fresh.taxonomy, "saturation: {}", what());
        assert_eq!(out.consistent, fresh.consistent, "saturation: {}", what());
        counts.entry((family, "saturation")).or_default().add(
            out.stats.incremental,
            changed,
            builtins,
        );
        before = now;
    }
}

#[test]
fn incremental_runs_agree_with_fresh_ones_across_constructors() {
    let seeds: u64 = std::env::var("OWL_SWEEP_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let mut counts = BTreeMap::new();
    for family in FAMILIES {
        for seed in 0..seeds {
            for threads in [1, 3] {
                sweep_one(seed, family, threads, &mut counts);
            }
        }
    }
    for ((family, path), n) in &counts {
        eprintln!(
            "sweep {family:?} {path}: {} runs, {} kept, {} changed, {} kept of those, \
             {} kept with the top property, {} with the bottom property",
            n.runs, n.kept, n.changed, n.kept_changed, n.kept_top, n.kept_bottom
        );
    }
    // the sweep tests the kept path only when changed axioms reach it, through both
    // entry points; a merge with an individual ties the contexts of the next run
    // together, so the nominal family keeps fewer
    for ((family, path), n) in &counts {
        let share = if *family == Family::Nominals { 10 } else { 4 };
        assert!(
            n.kept_changed * share >= n.changed,
            "{family:?} {path} kept {} of {} changed runs",
            n.kept_changed,
            n.changed
        );
        if *family == Family::TopBottom {
            assert!(
                n.kept_top > 0 && n.kept_bottom > 0,
                "{path} kept no edit of the top or the bottom property: {n:?}"
            );
        }
    }
}

/// A merge with an individual ties the contexts together, so the next edit starts again,
/// and its answer is that of a fresh run.
#[test]
fn an_edit_after_a_merge_with_an_individual_starts_again() {
    let one = ClassExpression::ObjectOneOf([ind("i")].into_iter().collect());
    let held = vec![
        Axiom::sub_class_of(ce("A"), one.clone()),
        Axiom::sub_class_of(ce("B"), one),
        Axiom::sub_class_of(ce("A"), ce("X")),
    ];
    follows_with_restart(held, Axiom::sub_class_of(ce("X"), ce("Y")));
}

/// A fact of the universal role reaches every context, so the next edit starts again,
/// and its answer is that of a fresh run.
#[test]
fn an_edit_after_a_fact_of_the_universal_role_starts_again() {
    let held = vec![
        ax(AxiomKind::ClassAssertion(ce("C"), ind("i"))),
        Axiom::sub_class_of(
            ClassExpression::some(ObjectProperty::top(), ce("C")),
            ce("D"),
        ),
        Axiom::declaration(Entity::Class(c("A"))),
    ];
    follows_with_restart(held, Axiom::sub_class_of(ce("D"), ce("E")));
}

fn follows_with_restart(held: Vec<Axiom>, edit: Axiom) {
    let limits = Limits::default();
    let mut k = Classifier::of(&held);
    k.classify(&limits).unwrap();
    k.add([&edit]);
    let out = k.classify(&limits).unwrap();
    assert!(!out.stats.incremental, "{:?}", out.stats);
    let mut all = held;
    all.push(edit);
    let fresh = Classifier::of(&all).classify(&limits).unwrap();
    assert_eq!(out.taxonomy, fresh.taxonomy);
    assert_eq!(out.consistent, fresh.consistent);
}
