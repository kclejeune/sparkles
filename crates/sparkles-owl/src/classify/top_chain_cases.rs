//! Named-domain semantic, dispatch and accounting fixtures.

use super::*;
use crate::model::{Axiom, Class, NamedIndividual};

mod activation;

pub(super) const ALL: &str =
    "SubObjectPropertyOf(ObjectPropertyChain(owl:topObjectProperty owl:topObjectProperty) :t)
     ObjectPropertyDomain(:t :D)";
pub(super) const TRAILING: &str =
    "SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t)
     ObjectPropertyDomain(:t :D)";
pub(super) const CONDITIONAL: &str = "SubClassOf(:A ObjectOneOf(:a))
     SubClassOf(:A ObjectSomeValuesFrom(:q :B))
     SubClassOf(:B ObjectOneOf(:b)) SubClassOf(:B :P)
     ObjectPropertyAssertion(:s :a :b)
     SubClassOf(ObjectSomeValuesFrom(:s :P) :Q)
     SubClassOf(:Q ObjectSomeValuesFrom(:r :Tail))";

pub(super) fn fixture(body: &str) -> Vec<Axiom> {
    crate::ofn_read::parse(&format!("Prefix(:=<urn:top-domain:>) Ontology({body})"))
        .unwrap()
        .axioms
}

pub(super) fn class(local: &str) -> Class {
    Class::new(format!("urn:top-domain:{local}"))
}

fn has_instance(sat: &Saturation, local: &str) -> bool {
    sat.describe(&class("D"))
        .unwrap()
        .instances
        .contains(&NamedIndividual::new(format!("urn:top-domain:{local}")))
}

// Record planning and delivery events from the ordinary classifier.
fn observe(sat: &mut Saturation) -> std::sync::Arc<top_chain_diagnostic::Probe> {
    let probe = std::sync::Arc::new(top_chain_diagnostic::Probe::default());
    sat.top_chain_probe = Some(probe.clone());
    probe
}

fn ordered(body: &str, mut check: impl FnMut(Saturation, &[&AxiomKind], &Limits)) {
    let axioms = fixture(body);
    let mut kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    for reversed in [false, true] {
        if reversed {
            kinds.reverse();
        }
        for threads in [1, 4] {
            check(
                Saturation::new(),
                &kinds,
                &Limits {
                    threads,
                    ..Limits::default()
                },
            );
        }
    }
}

#[test]
fn all_top_no_individuals_and_original_gap() {
    ordered(ALL, |mut sat, axioms, limits| {
        let out = sat.classify(axioms, None, limits).unwrap();
        assert!(
            out.taxonomy.is_subclass(&Class::thing(), &class("D")),
            "all Top missing Thing subclass of D"
        );
        assert!(out.complete_for_taken);
        assert!(!out.complete);
        assert_eq!(out.stats.tier, Tier::El);
        let chain = axioms
            .iter()
            .find(|&&k| matches!(k, AxiomKind::SubObjectPropertyOf(..)))
            .unwrap();
        assert!(
            matches!(sat.excluded(), [Excluded { axiom, gap: Gap::Partial(_) }] if axiom == *chain)
        );
        let strict = Limits {
            strict: true,
            ..limits.clone()
        };
        assert!(
            matches!(sat.classify(axioms, None, &strict), Err(Error::Incomplete(x))
                        if x.as_slice() == sat.excluded())
        );
    });
}

#[test]
fn all_top_empty_domain_clashes() {
    ordered(
        &format!("{ALL} SubClassOf(:D owl:Nothing)"),
        |mut sat, axioms, limits| {
            let out = sat.classify(axioms, None, limits).unwrap();
            assert_eq!(
                out.consistency,
                Consistency::Inconsistent,
                "all Top missed clash"
            );
        },
    );
}

#[test]
fn trailing_class_and_superroles() {
    let body = "SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t)
                SubObjectPropertyOf(:t :g) ObjectPropertyDomain(:g :D)
                SubObjectPropertyOf(:s :r) SubClassOf(:A ObjectSomeValuesFrom(:s :B))
                Declaration(Class(:C))";
    ordered(body, |mut sat, axioms, limits| {
        let out = sat.classify(axioms, None, limits).unwrap();
        assert!(
            out.taxonomy.is_subclass(&class("A"), &class("D")),
            "trailing superroles missing A subclass of D"
        );
        assert!(!out.taxonomy.is_unsatisfiable(&class("A")));
        assert!(!out.taxonomy.is_subclass(&Class::thing(), &class("D")));
        assert!(!out.taxonomy.is_subclass(&class("C"), &class("D")));
    });
}

#[test]
fn trailing_individual_and_isolated_tail() {
    let body = format!("{TRAILING} ObjectPropertyAssertion(:r :a :b) ClassAssertion(:C :c)");
    ordered(&body, |mut sat, axioms, limits| {
        sat.saturate(axioms, None, limits).unwrap();
        assert!(has_instance(&sat, "a"), "trailing missing D(a)");
        assert!(!has_instance(&sat, "b"));
        assert!(!has_instance(&sat, "c"));
    });
}

#[test]
fn trailing_domain_in_conditional_world() {
    ordered(
        &format!("{TRAILING} {CONDITIONAL} Declaration(Class(:C))"),
        |mut sat, axioms, limits| {
            let out = sat.classify(axioms, None, limits).unwrap();
            assert!(
                out.taxonomy.is_subclass(&class("A"), &class("D")),
                "conditional trailing missing A subclass of D"
            );
            assert!(!out.taxonomy.is_subclass(&Class::thing(), &class("D")));
            assert!(!out.taxonomy.is_subclass(&class("C"), &class("D")));
            assert!(!has_instance(&sat, "a"));
        },
    );
}

#[test]
fn ordinary_and_vacuous_controls() {
    let body = "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
                ObjectPropertyDomain(:t :D)
                SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)))
                SubObjectPropertyOf(ObjectPropertyChain(owl:bottomObjectProperty owl:topObjectProperty) :t)
                SubObjectPropertyOf(ObjectPropertyChain(owl:topObjectProperty owl:topObjectProperty) owl:topObjectProperty)
                Declaration(Class(:C))";
    ordered(body, |mut sat, axioms, limits| {
        let out = sat.classify(axioms, None, limits).unwrap();
        assert!(out.taxonomy.is_subclass(&class("A"), &class("D")));
        assert!(!out.taxonomy.is_subclass(&Class::thing(), &class("D")));
        assert!(!out.taxonomy.is_subclass(&class("C"), &class("D")));
        assert!(matches!(
            sat.excluded(),
            [Excluded {
                gap: Gap::Partial(_),
                ..
            }]
        ));
    });
}

fn compare_fresh(
    kept: &mut Classifier,
    current: &[&Axiom],
    limits: &Limits,
    source_subclass: bool,
    source_instance: bool,
) {
    let mut fresh = Classifier::of(current.iter().copied());
    let left = kept.classify(limits).unwrap();
    let right = fresh.classify(limits).unwrap();
    assert_eq!(left.consistency, right.consistency);
    assert_eq!(left.complete, right.complete);
    assert_eq!(left.complete_for_taken, right.complete_for_taken);
    assert_eq!(left.incomplete, right.incomplete);
    assert_eq!(left.taxonomy.axioms(), right.taxonomy.axioms());
    assert_eq!(kept.excluded(), fresh.excluded());
    assert_eq!(
        left.taxonomy.is_subclass(&class("A"), &class("D")),
        source_subclass,
    );
    kept.saturate(limits).unwrap();
    fresh.saturate(limits).unwrap();
    assert_eq!(kept.describe(&class("D")), fresh.describe(&class("D")));
    assert_eq!(has_instance(&kept.sat, "a"), source_instance);
}

#[test]
fn trailing_edits_and_switch_match_fresh() {
    let axioms = fixture(&format!(
        "{TRAILING} SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         ObjectPropertyAssertion(:r :a :b) ClassAssertion(:C :a) Declaration(Class(:D))"
    ));
    for (threads, reverse) in [(1, false), (1, true), (4, false), (4, true)] {
        let mut current: Vec<_> = axioms.iter().collect();
        let mut insertion = current.clone();
        if reverse {
            insertion.reverse();
        }
        let limits = Limits {
            threads,
            ..Limits::default()
        };
        let mut kept = Classifier::of(insertion);
        compare_fresh(&mut kept, &current, &limits, true, true);
        let probe = observe(&mut kept.sat);
        compare_fresh(&mut kept, &current, &limits, true, true);
        assert!(!kept.sat.reused);
        assert!(probe.global_messages.load(Ordering::Relaxed) > 0);

        for (removed, want_class, want_instance) in [
            (3, true, false),
            (2, false, true),
            (1, false, false),
            (0, false, false),
        ] {
            let axiom = current.remove(removed);
            kept.remove([axiom]);
            compare_fresh(&mut kept, &current, &limits, want_class, want_instance);
            kept.add([axiom]);
            current.insert(removed, axiom);
            compare_fresh(&mut kept, &current, &limits, true, true);
        }
        let el_only = Limits {
            el_only: true,
            ..limits.clone()
        };
        compare_fresh(&mut kept, &current, &el_only, false, false);
        compare_fresh(&mut kept, &current, &limits, true, true);
        kept.sat.top_chain_probe = None;
        compare_fresh(&mut kept, &current, &limits, true, true);
    }
}

#[test]
fn all_top_contexts_reset_across_switch_and_edits() {
    let axioms = fixture(&format!("{ALL} Declaration(Class(:D))"));
    let limits = Limits::default();
    let mut kept = Classifier::of(&axioms);
    let el_only = Limits {
        el_only: true,
        ..limits.clone()
    };
    let check = |kept: &mut Classifier, limits: &Limits, present: bool, want| {
        let mut fresh = Classifier::of(axioms.iter().skip(usize::from(!present)));
        let actual = kept.classify(limits).unwrap();
        let expected = fresh.classify(limits).unwrap();
        assert_eq!(actual.taxonomy.axioms(), expected.taxonomy.axioms());
        assert_eq!(kept.excluded(), fresh.excluded());
        assert_eq!(actual.consistency, expected.consistency);
        assert_eq!(
            actual.taxonomy.is_subclass(&Class::thing(), &class("D")),
            want
        );
        assert!(kept.sat.index.as_ref().unwrap().universal.is_empty());
    };
    check(&mut kept, &limits, true, true);
    check(&mut kept, &el_only, true, false);
    assert!(!kept.sat.reused);
    check(&mut kept, &limits, true, true);
    kept.remove([&axioms[0]]);
    check(&mut kept, &limits, false, false);
    assert!(!kept.sat.reused);
    kept.add([&axioms[0]]);
    check(&mut kept, &limits, true, true);
}

#[test]
fn inactive_and_provenance_boundaries() {
    let body = format!("{TRAILING} SubClassOf(:A ObjectSomeValuesFrom(:r :B))");
    let axioms = fixture(&body);
    let mut normal = Classifier::of(&axioms);
    let baseline = normal.classify(&Limits::default()).unwrap();
    assert!(baseline.taxonomy.is_subclass(&class("A"), &class("D")));
    assert!(normal.sat.index.as_ref().unwrap().top_chain.is_some());
    let probe = observe(&mut normal.sat);
    for limits in [
        Limits {
            el_only: true,
            ..Limits::default()
        },
        Limits {
            rooted: true,
            ..Limits::default()
        },
    ] {
        normal.classify(&limits).unwrap();
        assert!(normal.sat.index.as_ref().unwrap().top_chain.is_none());
    }
    assert_eq!(probe.build_visits.load(Ordering::Relaxed), 0);
    assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
    for body in [
        format!("{body} SubClassOf(:S ObjectAllValuesFrom(:q :B))"),
        format!("{body} ObjectPropertyRange(:t :X)"),
        format!("{body} SubObjectPropertyOf(owl:topObjectProperty :r)"),
        "SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t)
         SubClassOf(ObjectSomeValuesFrom(:t owl:Thing) :D)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))"
            .to_string(),
        "SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t)
         ObjectPropertyDomain(:t ObjectIntersectionOf(:D :X))
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))"
            .to_string(),
    ] {
        let mut k = Classifier::of(&fixture(&body));
        let probe = observe(&mut k.sat);
        k.classify(&Limits::default()).unwrap();
        assert!(k.sat.index.as_ref().unwrap().top_chain.is_none(), "{body}");
        assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0, "{body}");
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0, "{body}");
    }
}

fn links(sat: &Saturation) -> (usize, usize) {
    sat.ctx
        .iter()
        .filter_map(|slot| slot.get())
        .fold((0, 0), |(back, fwd), ctx| {
            let st = ctx.st.lock();
            (
                back + st
                    .back
                    .iter()
                    .map(|(_, edges)| edges.list.len())
                    .sum::<usize>(),
                fwd + st.fwd.list.len(),
            )
        })
}

#[test]
fn work_witness_counts_matches_and_zero_output_scans() {
    for threads in [1, 4] {
        for n in [32, 64] {
            for matching in [false, true] {
                let mut body = format!(
                    "{TRAILING} SubObjectPropertyOf(:t :g) ObjectPropertyDomain(:g :D)
                     ObjectPropertyDomain(:g :Second)"
                );
                let r = if matching { "r" } else { "q" };
                for i in 0..n {
                    body.push_str(&format!(
                        " SubClassOf(:A{i} ObjectSomeValuesFrom(:{r} :B{i}))"
                    ));
                }
                let axioms = fixture(&body);
                let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
                let limits = Limits {
                    threads,
                    ..Limits::default()
                };
                let mut baseline = Saturation::new();
                let el_only = Limits {
                    el_only: true,
                    ..limits.clone()
                };
                baseline.saturate(&kinds, None, &el_only).unwrap();
                let mut active = Saturation::new();
                let probe = observe(&mut active);
                active.saturate(&kinds, None, &limits).unwrap();
                let rows = active
                    .index
                    .as_ref()
                    .unwrap()
                    .top_chain
                    .as_ref()
                    .unwrap()
                    .len();
                assert_eq!(rows, 2, "the two D observers collapse to one rule");
                assert_eq!(
                    links(&active),
                    links(&baseline),
                    "the named messages add no links"
                );
                assert_eq!(probe.back_events.load(Ordering::Relaxed), n);
                assert_eq!(probe.back_tests.load(Ordering::Relaxed), rows * n);
                assert_eq!(probe.top_tests.load(Ordering::Relaxed), 0);
                assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
                assert_eq!(
                    probe.global_messages.load(Ordering::Relaxed),
                    if matching { rows * n } else { 0 }
                );
            }
        }
    }
    let axioms = fixture(&format!("{ALL} Declaration(Class(:C))"));
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let probe = observe(&mut sat);
    sat.saturate(&kinds, None, &Limits::default()).unwrap();
    let contexts = sat
        .ctx
        .iter()
        .filter(|c| c.get().is_some_and(|c| c.st.lock().init))
        .count();
    assert_eq!(probe.top_events.load(Ordering::Relaxed), contexts);
    assert_eq!(probe.top_tests.load(Ordering::Relaxed), contexts);
    assert_eq!(probe.global_messages.load(Ordering::Relaxed), contexts);
    assert_eq!(links(&sat), (0, 0));
}

#[test]
fn construction_refuses_scratch_and_retained_grants() {
    use super::admission_test::Point;
    use std::sync::{Arc, atomic::AtomicBool};

    let axioms = fixture(ALL);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    sat.saturate(&kinds, None, &Limits::default()).unwrap();
    let partial = BTreeMap::from([(kinds[0], index::TOP_CHAIN)]);
    let ix = sat.index.as_ref().unwrap();
    for point in [Point::TopChainScratch, Point::TopChainRetained] {
        let armed = Arc::new(AtomicBool::new(false));
        let refused = Arc::new(AtomicUsize::new(0));
        let limits = Limits {
            charge: Some(Arc::new({
                let (armed, refused) = (armed.clone(), refused.clone());
                move |bytes| {
                    if armed.load(Ordering::Relaxed) {
                        refused.fetch_add(bytes, Ordering::Relaxed);
                        false
                    } else {
                        true
                    }
                }
            })),
            ..Limits::default()
        };
        admission_test::set_hook(move |p| {
            if p == point {
                armed.store(true, Ordering::Relaxed);
            }
        });
        // Set an already paid baseline so the hook targets the allocation itself.
        let budget = numeric::Budget::new(&limits, 0, None);
        budget.baseline(1024).unwrap();
        let probe = Arc::new(top_chain_diagnostic::Probe::default());
        let result = top_chain::plan(
            &kinds,
            &partial,
            &sat.interner,
            ix,
            &budget,
            1024,
            Some(&probe),
        );
        admission_test::set_hook(|_| {});
        assert!(matches!(
            result,
            Err(Error::Memory {
                limit: usize::MAX,
                ..
            })
        ));
        assert!(refused.load(Ordering::Relaxed) > 0);
        assert_eq!(budget.retained_storage(), (1024, 0));
    }
}

#[test]
fn global_message_is_refused_before_enqueue() {
    use super::admission_test::Point;
    use std::sync::{Arc, atomic::AtomicBool};

    let axioms = fixture(ALL);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let probe = observe(&mut sat);
    sat.saturate(&kinds, None, &Limits::default()).unwrap();
    let before = probe.global_messages.load(Ordering::Relaxed);
    let armed = Arc::new(AtomicBool::new(false));
    let refused = AtomicUsize::new(0);
    let charge = |bytes| {
        if armed.load(Ordering::Relaxed) {
            refused.fetch_add(bytes, Ordering::Relaxed);
            false
        } else {
            true
        }
    };
    let ctx: Contexts = (0..sat.ctx.len()).map(|_| OnceLock::new()).collect();
    let bounds = Bounds {
        threads: 1,
        deadline: None,
        max_memory: None,
        cancelled: None,
        charge: Some(&charge),
        base_memory: 0,
        base_queued: 0,
        peak_memory: 0,
        paid_memory: 0,
        dynamic: None,
        dl: None,
        partial: false,
        steps: false,
    };
    let trigger = armed.clone();
    admission_test::set_hook(move |p| {
        if p == Point::TopChainGlobalMessage {
            trigger.store(true, Ordering::Relaxed);
        }
    });
    let (_, stop) =
        saturate::saturate_counted(sat.index.as_ref().unwrap(), &ctx, &[TOP], &[], &[], &bounds);
    admission_test::set_hook(|_| {});
    assert!(matches!(stop, Some(Stop::Memory { .. })));
    assert!(refused.load(Ordering::Relaxed) > 0);
    assert_eq!(probe.global_messages.load(Ordering::Relaxed), before);
    assert_eq!(admission_test::queued(&ctx, None), 0);
}

#[test]
fn one_shot_scan_global_and_world_stops_retry_fresh() {
    use super::admission_test::Point;
    use std::sync::{Arc, atomic::AtomicBool};

    for (point, extra) in [
        (
            Point::TopChainScan,
            "SubClassOf(:A ObjectSomeValuesFrom(:q :B))",
        ),
        (
            Point::TopChainGlobalMessage,
            "SubClassOf(:A ObjectSomeValuesFrom(:r :B))",
        ),
        (Point::TopChainWorldMessage, CONDITIONAL),
    ] {
        let axioms = fixture(&format!("{TRAILING} {extra}"));
        let mut kept = Classifier::of(&axioms);
        let probe = observe(&mut kept.sat);
        let armed = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let limits = Limits {
            threads: 1,
            cancelled: Some(Arc::new({
                let (armed, fired) = (armed.clone(), fired.clone());
                move || armed.load(Ordering::Relaxed) && !fired.swap(true, Ordering::Relaxed)
            })),
            ..Limits::default()
        };
        admission_test::set_hook(move |p| {
            if p == point {
                armed.store(true, Ordering::Relaxed);
            }
        });
        let result = kept.classify(&limits);
        admission_test::set_hook(|_| {});
        assert!(matches!(result, Err(Error::Cancelled)), "{point:?}");
        assert!(fired.load(Ordering::Relaxed), "{point:?}");
        assert!(kept.describe(&class("D")).is_none(), "{point:?}");
        assert!(kept.sat.index.is_none(), "{point:?}");
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
        if point != Point::TopChainWorldMessage {
            assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
        }
        let clean = Limits {
            cancelled: None,
            ..limits
        };
        let expected_class = point != Point::TopChainScan;
        let current: Vec<_> = axioms.iter().collect();
        compare_fresh(&mut kept, &current, &clean, expected_class, false);
        if point == Point::TopChainWorldMessage {
            assert!(probe.world_messages.load(Ordering::Relaxed) > 0);
        }
    }
}

pub(super) mod accounting {
    use super::*;
    use std::sync::Arc;

    /// A caller's cumulative grant. Every call attempts a positive increment.
    #[derive(Default)]
    pub(in crate::classify) struct Grant(parking_lot::Mutex<Vec<(usize, bool)>>);

    impl Grant {
        pub(in crate::classify) fn request(&self, bytes: usize, ceiling: usize) -> bool {
            assert!(bytes > 0);
            let mut calls = self.0.lock();
            let accepted: usize = calls.iter().filter(|(_, ok)| *ok).map(|(n, _)| n).sum();
            let ok = bytes <= ceiling.saturating_sub(accepted);
            calls.push((bytes, ok));
            ok
        }

        pub(in crate::classify) fn accepted(&self) -> usize {
            self.0
                .lock()
                .iter()
                .filter(|(_, ok)| *ok)
                .map(|(n, _)| n)
                .sum()
        }

        pub(in crate::classify) fn calls(&self) -> Vec<(usize, bool)> {
            self.0.lock().clone()
        }

        pub(in crate::classify) fn limits(self: &Arc<Self>, ceiling: usize) -> Limits {
            let grant = self.clone();
            Limits {
                threads: 1,
                max_memory: None,
                charge: Some(Arc::new(move |bytes| grant.request(bytes, ceiling))),
                ..Limits::default()
            }
        }
    }

    pub(in crate::classify) fn trailing_index(
        two_rows: bool,
    ) -> (Saturation, Arc<top_chain_diagnostic::Probe>) {
        let extra = if two_rows {
            "ObjectPropertyDomain(:t :E)"
        } else {
            ""
        };
        let axioms = fixture(&format!(
            "{TRAILING} {extra}
             Declaration(Class(:Source)) Declaration(Class(:Tail)) Declaration(Class(:Fence))
             Declaration(Class(:Producer)) Declaration(Class(:Parked))
             SubClassOf(ObjectSomeValuesFrom(:q :P) :Q)"
        ));
        let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        let probe = observe(&mut sat);
        let result = sat
            .saturate(
                &kinds,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
        assert_eq!(result.stats.tier, Tier::El);
        assert_eq!(
            sat.index
                .as_ref()
                .unwrap()
                .top_chain
                .as_ref()
                .unwrap()
                .len(),
            1 + usize::from(two_rows)
        );
        assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
        (sat, probe)
    }

    pub(in crate::classify) fn id(sat: &Saturation, local: &str) -> Ex {
        sat.interner.class_ex(&class(local)).unwrap()
    }

    pub(in crate::classify) fn prefix(sat: &Saturation) -> index::Role {
        sat.interner
            .object_role_id(&crate::model::ObjectProperty::new("urn:top-domain:r"))
            .unwrap()
    }
}

#[test]
fn paid_switch_on_reuses_the_clean_restart_credit() {
    check_paid_switch(true, true, false, 1);
}

#[test]
fn paid_trailing_switch_on_reuses_the_clean_restart_credit() {
    check_paid_switch(false, true, false, 1);
}

#[test]
fn paid_all_top_description_switch_on_reuses_the_clean_restart_credit() {
    check_paid_switch(true, false, false, 1);
}

#[test]
fn paid_trailing_description_switch_on_reuses_the_clean_restart_credit() {
    check_paid_switch(false, false, false, 1);
}

#[test]
fn paid_switch_on_preserves_upstream_credit() {
    check_paid_switch(false, true, true, 1);
}

#[test]
fn paid_parallel_domain_edit_reuses_admitted_credit() {
    check_paid_switch(false, true, true, 4);
}

fn check_paid_switch(all: bool, taxonomy: bool, layered: bool, threads: usize) {
    use accounting::Grant;
    use admission_test::{Point, with_hook};
    use std::sync::Arc;

    let mut body = if all {
        format!("{ALL} Declaration(Class(:D))")
    } else {
        format!(
            "{TRAILING} SubClassOf(:A ObjectSomeValuesFrom(:r :B)) Declaration(Class(:C)) Declaration(Class(:D))"
        )
    };
    if threads > 1 {
        body.push_str(&" Declaration(Class(:C))".repeat(MIN_AXIOMS));
    }
    let axioms = fixture(&body);
    if threads > 1 {
        assert!(axioms.len() > MIN_AXIOMS && axioms.len() >= PARALLEL_AXIOMS);
    }
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let domain = [kinds[1]];
    assert!(matches!(domain[0], AxiomKind::ObjectPropertyDomain(..)));
    let priming: Vec<_> = kinds
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(i, k)| (i != 1).then_some(k))
        .collect();
    let add_domain = Changes {
        added: &domain,
        removed: &[],
    };
    let run = |sat: &mut Saturation, input: &[&AxiomKind], changes, limits: &Limits| {
        if taxonomy {
            sat.classify(input, changes, limits).map(|out| {
                let stats = out.stats.clone();
                (Some(out), stats)
            })
        } else {
            sat.saturate(input, changes, limits)
                .map(|out| (None, out.stats))
        }
    };
    let plain = Limits {
        threads,
        ..Limits::default()
    };
    let prime = || {
        let mut sat = Saturation::new();
        let (_, stats) = run(&mut sat, &priming, None, &plain).unwrap();
        assert_eq!(stats.tier, Tier::El);
        assert!(!sat.dirty && !sat.entangled);
        assert!(sat.index.as_ref().unwrap().top_chain.is_none());
        let kept = sat.kept.as_ref().unwrap();
        assert!(kept.tier == KeptTier::El);
        assert!(!sat.must_restart(kept, add_domain));
        assert!(sat.context_bytes() > 0);
        sat
    };
    let grant_limits = |grant: &Arc<Grant>, cap| Limits {
        threads,
        ..grant.limits(cap)
    };
    let mut fresh = Saturation::new();
    let fresh_probe = observe(&mut fresh);
    let fresh_grant = Arc::new(Grant::default());
    let (expected, fresh_stats) = run(
        &mut fresh,
        &kinds,
        None,
        &grant_limits(&fresh_grant, usize::MAX),
    )
    .unwrap();
    let fresh_demand = fresh_grant.accepted();
    assert!(fresh_demand > 0);
    assert!(fresh_probe.global_messages.load(Ordering::Relaxed) > 0);

    // Sample the charge total at the real tentative-plan boundary, before
    // saturation or a clean rebuild can add anything to that lease.
    let at_kept_plan =
        |probe: Arc<top_chain_diagnostic::Probe>, grant: Arc<Grant>, sample: Arc<AtomicUsize>| {
            move |point| {
                if point == Point::TopChainKeptPlan {
                    assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
                    let restart = grant.accepted();
                    assert!(restart > 0);
                    assert_eq!(sample.swap(restart, Ordering::Relaxed), 0);
                }
            }
        };
    let mut sampled = prime();
    let sampled_probe = observe(&mut sampled);
    let sample_grant = Arc::new(Grant::default());
    let sample = Arc::new(AtomicUsize::new(0));
    with_hook(
        at_kept_plan(sampled_probe.clone(), sample_grant.clone(), sample.clone()),
        || {
            run(
                &mut sampled,
                &kinds,
                Some(add_domain),
                &grant_limits(&sample_grant, usize::MAX),
            )
            .unwrap()
        },
    );
    assert!(sampled_probe.global_messages.load(Ordering::Relaxed) > 0);
    let restart = sample.load(Ordering::Relaxed);
    let cap = restart.max(fresh_demand);

    let mut capped_fresh = Saturation::new();
    observe(&mut capped_fresh);
    let fresh_cap_grant = Arc::new(Grant::default());
    run(
        &mut capped_fresh,
        &kinds,
        None,
        &grant_limits(&fresh_cap_grant, cap),
    )
    .unwrap();
    assert_eq!(fresh_cap_grant.accepted(), fresh_demand);

    let prepaid = if layered {
        fresh_grant.calls()[0].0 / 2
    } else {
        0
    };
    let with_credit = |grant: &Arc<Grant>, cap| {
        let mut limits = grant_limits(grant, cap);
        if layered {
            assert!(prepaid > 0);
            assert!(grant.request(prepaid, cap));
            limits.charge = Some(drawing_on(prepaid, limits.charge.take().unwrap()));
        }
        limits
    };

    let mut under = Saturation::new();
    observe(&mut under);
    let small_grant = Arc::new(Grant::default());
    assert!(matches!(
        run(
            &mut under,
            &kinds,
            None,
            &with_credit(&small_grant, fresh_demand - 1)
        ),
        Err(Error::Memory { .. })
    ));
    assert!(small_grant.calls().iter().any(|&(_, ok)| !ok));

    let mut switched = prime();
    let switched_probe = observe(&mut switched);
    let grant = Arc::new(Grant::default());
    let measured = Arc::new(AtomicUsize::new(0));
    let result = with_hook(
        at_kept_plan(switched_probe.clone(), grant.clone(), measured.clone()),
        || {
            run(
                &mut switched,
                &kinds,
                Some(add_domain),
                &Limits {
                    max_memory: layered.then_some(cap),
                    ..with_credit(&grant, cap)
                },
            )
        },
    );
    assert_eq!(measured.load(Ordering::Relaxed), restart);
    eprintln!(
        "paid switch all={all} taxonomy={taxonomy} prepaid={prepaid} kept={restart} fresh={fresh_demand} cap={cap} accepted={} calls={:?}",
        grant.accepted(),
        grant.calls()
    );
    let (actual, stats) = result.unwrap_or_else(|err| {
        assert!(restart > 0);
        assert!(matches!(err, Error::Memory { .. }), "{err:?}");
        assert!(grant.calls().iter().any(|&(_, ok)| !ok));
        panic!("paid switch exceeded one grant: {err:?}");
    });
    assert_eq!(grant.accepted(), cap);
    if layered {
        let first = if restart > 0 {
            sample_grant.calls()[0].0
        } else {
            fresh_grant.calls()[0].0
        };
        assert_eq!(
            grant.calls()[..2],
            [(prepaid, true), (first - prepaid, true)]
        );
        assert_eq!(grant.accepted() - prepaid, cap - prepaid);
    }
    assert!(grant.calls().iter().all(|&(_, ok)| ok));
    assert_eq!(stats.memory, fresh_stats.memory);
    assert!(stats.peak_memory >= fresh_stats.peak_memory);
    assert!(stats.peak_memory >= restart);
    assert!(!switched.reused);
    assert_eq!(switched.consistent, fresh.consistent);
    assert_eq!(switched.excluded(), fresh.excluded());
    assert!(matches!(
        switched.excluded(),
        [Excluded {
            gap: Gap::Partial(index::TOP_CHAIN),
            ..
        }]
    ));
    assert_eq!(switched.describe(&class("D")), fresh.describe(&class("D")));
    if let (Some(actual), Some(expected)) = (actual, &expected) {
        assert_eq!(actual.consistency, expected.consistency);
        assert_eq!(actual.complete, expected.complete);
        assert_eq!(actual.complete_for_taken, expected.complete_for_taken);
        assert_eq!(actual.incomplete, expected.incomplete);
        assert_eq!(actual.taxonomy.axioms(), expected.taxonomy.axioms());
        let source = if all { Class::thing() } else { class("A") };
        assert!(actual.taxonomy.is_subclass(&source, &class("D")));
    }
    let index = switched.index.as_ref().unwrap();
    assert_eq!(index.top_chain.as_ref().unwrap().len(), 1);
    if all {
        assert!(index.universal.is_empty());
    }
    assert!(switched_probe.global_messages.load(Ordering::Relaxed) > 0);
}

#[test]
fn paid_ineligible_switch_keeps_the_ordinary_state() {
    use accounting::Grant;
    use std::sync::Arc;

    let axioms = fixture(
        "SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t)
         ObjectPropertyDomain(:q :D) SubClassOf(:A ObjectSomeValuesFrom(:r :B))",
    );
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let baseline = sat
        .classify(
            &kinds,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(sat.top_chain_probe.is_none());
    assert!(sat.index.as_ref().unwrap().top_chain.is_none());
    let probe = observe(&mut sat);
    let grant = Arc::new(Grant::default());
    let active = sat
        .classify(&kinds, Some(Changes::default()), &grant.limits(usize::MAX))
        .unwrap();
    assert!(sat.reused);
    assert!(grant.accepted() > 0);
    assert_eq!(probe.matches.load(Ordering::Relaxed), 0);
    assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
    assert!(sat.index.as_ref().unwrap().top_chain.is_none());
    assert_eq!(baseline.taxonomy.axioms(), active.taxonomy.axioms());
}

#[test]
fn named_domain_post_enqueue_cancellation_retries_fresh() {
    use accounting::Grant;
    use admission_test::{Point, with_hook};
    use std::sync::{Arc, atomic::AtomicBool};

    for in_world in [false, true] {
        let extra = if in_world {
            CONDITIONAL
        } else {
            "SubClassOf(:A ObjectSomeValuesFrom(:r :Tail))"
        };
        let axioms = fixture(&format!("{TRAILING} {extra} Declaration(Class(:C))"));
        let current: Vec<_> = axioms.iter().collect();
        let mut kept = Classifier::of(current.iter().copied());
        let probe = observe(&mut kept.sat);
        let counts = {
            let probe = probe.clone();
            move || {
                (
                    probe.global_messages.load(Ordering::Relaxed),
                    probe.world_messages.load(Ordering::Relaxed),
                )
            }
        };
        let fired = Arc::new(AtomicBool::new(false));
        let grant = Arc::new(Grant::default());
        let limits = Limits {
            cancelled: Some(Arc::new({
                let (fired, counts) = (fired.clone(), counts.clone());
                move || {
                    let (global, world) = counts();
                    (if in_world { world > 0 } else { global > 0 })
                        && !fired.swap(true, Ordering::Relaxed)
                }
            })),
            ..grant.limits(usize::MAX)
        };
        let boundary = Arc::new(AtomicUsize::new(0));
        let stopped = with_hook(
            {
                let (boundary, counts) = (boundary.clone(), counts.clone());
                move |point| {
                    let (global, world) = counts();
                    if (!in_world
                        && matches!(point, Point::TopChainGlobalQueued { .. })
                        && global > 0)
                        || (in_world && point == Point::TopChainWorldCheckpoint && world > 0)
                    {
                        boundary.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            || kept.classify(&limits),
        );
        assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
        assert!(boundary.load(Ordering::Relaxed) > 0);
        assert!(fired.load(Ordering::Relaxed));
        let (global, world) = counts();
        if in_world {
            assert!(world > 0);
        } else {
            assert!(global > 0);
            assert_eq!(world, 0);
        }
        assert!(grant.accepted() > 0);
        assert!(grant.calls().iter().all(|&(_, accepted)| accepted));
        assert!(kept.sat.index.is_none());
        assert!(kept.sat.worlds.is_empty());
        assert!(kept.describe(&class("D")).is_none());

        let clean = Limits {
            threads: 1,
            ..Limits::default()
        };
        compare_fresh(&mut kept, &current, &clean, true, false);
        let actual = kept.classify(&clean).unwrap();
        assert!(!actual.taxonomy.is_subclass(&Class::thing(), &class("D")));
        assert!(!actual.taxonomy.is_subclass(&class("C"), &class("D")));
        assert!(!has_instance(&kept.sat, "a"));
    }
}
