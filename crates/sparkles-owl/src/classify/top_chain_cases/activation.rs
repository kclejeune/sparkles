//! Independent activation-boundary fixtures on the named-domain accounting overlay.

use super::*;
use admission_test::Attempt;
use std::fmt::Write as _;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, ThreadId};

const REFLEXIVE: &str = "Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:t))
     Declaration(Class(:D)) Declaration(Class(:C))
     ReflexiveObjectProperty(:r)
     SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t)
     ObjectPropertyDomain(:t :D)";
const BOTTOM_DOMAIN: &str = "Declaration(ObjectProperty(:t))
     SubObjectPropertyOf(ObjectPropertyChain(owl:topObjectProperty owl:topObjectProperty) :t)
     ObjectPropertyDomain(:t owl:Nothing)";
const PRIVACY: &str = "SubClassOf(:Other ObjectOneOf(:a)) DisjointClasses(:Other :D)";

fn original_chains(kinds: &[&AxiomKind]) -> Vec<Excluded> {
    let mut excluded: Vec<_> = kinds
        .iter()
        .filter(|&&k| {
            matches!(
                k,
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _)
            )
        })
        .map(|&k| Excluded {
            axiom: k.clone(),
            gap: Gap::Partial(index::TOP_CHAIN),
        })
        .collect();
    excluded.sort();
    excluded
}

fn incomplete(sat: &Saturation, out: &Classification, kinds: &[&AxiomKind]) {
    assert_eq!(out.stats.tier, Tier::El);
    assert!(out.complete_for_taken);
    assert!(!out.complete);
    assert_eq!(sat.excluded(), original_chains(kinds));
}

#[test]
fn reflexive_prefix_uses_back_without_asserted_edges() {
    ordered(REFLEXIVE, |mut sat, kinds, limits| {
        let probe = observe(&mut sat);
        let out = sat.classify(kinds, None, limits).unwrap();
        assert_ne!(out.consistency, Consistency::Inconsistent);
        assert!(out.taxonomy.is_subclass(&Class::thing(), &class("D")));
        assert!(out.taxonomy.is_subclass(&class("C"), &class("D")));
        incomplete(&sat, &out, kinds);
        let ix = sat.index.as_ref().unwrap();
        assert_eq!(ix.top_chain.as_ref().unwrap().len(), 1);
        let r = sat
            .interner
            .object_role_id(&ObjectProperty::new("urn:top-domain:r"))
            .unwrap();
        assert!(!ix.is_universal(r));
        let top = sat.ctx[TOP as usize].get().unwrap().st.lock();
        assert!(top.loops.contains(&r));
        assert!(
            top.back
                .iter()
                .any(|(role, src)| *role == r && src.list.contains(&TOP))
        );
        let back_tests = probe.back_tests.load(Ordering::Relaxed);
        let messages = probe.global_messages.load(Ordering::Relaxed);
        assert!(messages > 0 && back_tests >= messages);
        assert_eq!(probe.top_tests.load(Ordering::Relaxed), 0);
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
        eprintln!("activation reflexive back_tests={back_tests} global_messages={messages}");

        for el_only in [false, true] {
            let mut control = Saturation::new();
            let probe = el_only.then(|| observe(&mut control));
            let control_limits = Limits {
                el_only,
                ..limits.clone()
            };
            let control_out = control.classify(kinds, None, &control_limits).unwrap();
            // The ordinary calculus can compose the witnessed r and Top self-loops.
            assert_eq!(control_out.taxonomy, out.taxonomy);
            incomplete(&control, &control_out, kinds);
            assert_eq!(
                control.index.as_ref().unwrap().top_chain.is_some(),
                !el_only
            );
            if let Some(probe) = probe {
                assert_eq!(probe.build_visits.load(Ordering::Relaxed), 0);
                assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
            }
        }
    });
}

#[test]
fn direct_bottom_domain_clash_retains_original_gap() {
    ordered(BOTTOM_DOMAIN, |mut sat, kinds, limits| {
        let probe = observe(&mut sat);
        let out = sat.classify(kinds, None, limits).unwrap();
        assert_eq!(out.consistency, Consistency::Inconsistent);
        incomplete(&sat, &out, kinds);
        assert_eq!(
            sat.index
                .as_ref()
                .unwrap()
                .top_chain
                .as_ref()
                .unwrap()
                .len(),
            1
        );
        assert!(sat.ctx[TOP as usize].get().unwrap().st.lock().bottom);
        let messages = probe.global_messages.load(Ordering::Relaxed);
        assert!(messages > 0);
        assert_eq!(probe.back_tests.load(Ordering::Relaxed), 0);
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
        eprintln!("activation bottom_domain global_messages={messages}");

        let strict = Limits {
            strict: true,
            ..limits.clone()
        };
        assert!(matches!(sat.classify(kinds, None, &strict),
            Err(Error::Incomplete(excluded)) if excluded == original_chains(kinds)));

        for el_only in [false, true] {
            let mut control = Saturation::new();
            let probe = el_only.then(|| observe(&mut control));
            let control_limits = Limits {
                el_only,
                ..limits.clone()
            };
            let control_out = control.classify(kinds, None, &control_limits).unwrap();
            assert_eq!(
                control_out.consistency == Consistency::Inconsistent,
                !el_only
            );
            incomplete(&control, &control_out, kinds);
            assert_eq!(
                control.index.as_ref().unwrap().top_chain.is_some(),
                !el_only
            );
            if let Some(probe) = probe {
                assert_eq!(probe.build_visits.load(Ordering::Relaxed), 0);
                assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
            }
        }
    });
}

const PLANNER_SOURCE: &str = "SubClassOf(:A ObjectSomeValuesFrom(:r :B)) Declaration(Class(:C))";
const ROW_LIMIT: usize = 4096;

#[test]
fn original_domain_ceiling() {
    for overflow in [false, true] {
        let mut body = format!("{TRAILING} {PLANNER_SOURCE}");
        for i in 1..ROW_LIMIT + usize::from(overflow) {
            write!(body, " ObjectPropertyDomain(:noise :N{i})").unwrap();
        }
        check_ceiling(&body, ROW_LIMIT + usize::from(overflow), 1, overflow);
    }
}

#[test]
fn original_domain_occurrence_ceiling() {
    let axioms = fixture(&format!("{TRAILING} {PLANNER_SOURCE}"));
    let noise = fixture("ObjectPropertyDomain(:noise :Noise)");
    for overflow in [false, true] {
        let domains = ROW_LIMIT + usize::from(overflow);
        let mut kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        // Reuse the same original axiom through the borrowed entry point.
        kinds.extend(std::iter::repeat_n(&noise[0].kind, domains - 1));
        admission_test::with_hook(
            move |point| {
                assert!(
                    !overflow || point != admission_test::Point::TopChainScratch,
                    "original-domain overflow reached planner scratch"
                );
            },
            || check_borrowed_ceiling(&kinds, domains, 1, overflow),
        );
    }
}

#[test]
fn prededup_match_ceiling() {
    for overflow in [false, true] {
        let mut body = PLANNER_SOURCE.to_owned();
        for i in 0..64 {
            write!(
                body,
                " SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty) :t{i})
                           SubObjectPropertyOf(:t{i} :hub)
                           SubObjectPropertyOf(:hub :g{i})
                           ObjectPropertyDomain(:g{i} :D)"
            )
            .unwrap();
        }
        if overflow {
            body.push_str(" ObjectPropertyDomain(:t0 :D)");
        }
        check_ceiling(
            &body,
            64 + usize::from(overflow),
            ROW_LIMIT + usize::from(overflow),
            overflow,
        );
    }
}

fn check_ceiling(body: &str, domains: usize, matches: usize, decline: bool) {
    let axioms = fixture(body);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    check_borrowed_ceiling(&kinds, domains, matches, decline);
}

fn check_borrowed_ceiling(kinds: &[&AxiomKind], domains: usize, matches: usize, decline: bool) {
    assert_eq!(
        kinds
            .iter()
            .filter(|&&k| matches!(k, AxiomKind::ObjectPropertyDomain(..)))
            .count(),
        domains
    );
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut baseline = Saturation::new();
    let el_only = Limits {
        el_only: true,
        ..limits.clone()
    };
    baseline.saturate(kinds, None, &el_only).unwrap();
    let excluded = original_chains(kinds);
    assert!(!excluded.is_empty());
    assert_eq!(baseline.excluded(), excluded);
    assert!(
        !baseline
            .describe(&class("A"))
            .unwrap()
            .superclasses
            .contains(&class("D"))
    );
    let partial = excluded
        .iter()
        .map(|e| (&e.axiom, index::TOP_CHAIN))
        .collect();
    let ix = baseline.index.as_ref().unwrap();
    let base = 1024;
    let scratch = domains * size_of::<(index::Role, Ex)>();
    let row = size_of::<(Option<index::Role>, Ex)>();
    let matched = matches * row;
    let peak = base + scratch + matched + row;
    let plan = |limits: &Limits| {
        let budget = numeric::Budget::new(limits, 0, None);
        budget.baseline(base).unwrap();
        let probe = Arc::new(top_chain_diagnostic::Probe::default());
        let result = top_chain::plan(
            kinds,
            &partial,
            &baseline.interner,
            ix,
            &budget,
            base,
            Some(&probe),
        );
        (result, budget, probe.matches.load(Ordering::Relaxed))
    };

    if decline {
        // Only the match-cap arm needs domain scratch. Neither overflow may allocate
        // matched-row scratch or install a retained table.
        let expected = if domains > ROW_LIMIT {
            (base, 0)
        } else {
            (base + scratch, matches)
        };
        let capped = Limits {
            max_memory: Some(expected.0),
            ..limits.clone()
        };
        let (result, budget, visits) = plan(&capped);
        assert!(result.unwrap().is_none());
        assert_eq!((budget.peak(), visits), expected);
        assert_eq!(budget.retained_storage(), (base, 0));
        eprintln!(
            "activation planner decline domains={domains} matches={visits} peak={}",
            expected.0
        );
    } else {
        let exact = Limits {
            max_memory: Some(peak),
            ..limits.clone()
        };
        let (result, budget, visits) = plan(&exact);
        assert_eq!(result.unwrap().unwrap().len(), 1);
        assert_eq!(visits, matches * 2);
        assert_eq!(budget.peak(), peak);
        assert_eq!(budget.retained_storage(), (base + row, 0));

        // A literal byte cap one below the retained transfer and an independent
        // caller grant refusal at each growth point must both produce typed stops.
        let capped = Limits {
            max_memory: Some(peak - 1),
            ..limits.clone()
        };
        let (result, budget, visits) = plan(&capped);
        assert!(
            matches!(result, Err(Error::Memory { limit, used }) if limit == peak - 1 && used == peak)
        );
        assert_eq!(visits, matches * 2);
        assert_eq!(budget.retained_storage(), (base, 0));

        let sequence = [base, scratch, matched, row];
        for refuse_at in 1..sequence.len() {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let record = requests.clone();
            let paid = Limits {
                charge: Some(Arc::new(move |bytes| {
                    let mut record = record.lock().unwrap();
                    record.push(bytes);
                    record.len() != refuse_at + 1
                })),
                ..limits.clone()
            };
            let (result, budget, visits) = plan(&paid);
            let used_expected = sequence[..=refuse_at].iter().sum::<usize>();
            assert!(
                matches!(result, Err(Error::Memory { limit: usize::MAX, used }) if used == used_expected)
            );
            assert_eq!(*requests.lock().unwrap(), sequence[..=refuse_at]);
            assert_eq!(visits, [0, 0, matches, matches * 2][refuse_at]);
            assert_eq!(budget.peak(), sequence[..refuse_at].iter().sum::<usize>());
            assert_eq!(budget.retained_storage(), (base, 0));
        }
        eprintln!(
            "activation planner admit domains={domains} matches={visits} peak={peak} deltas={sequence:?}"
        );
    }

    // Check that the normal classifier dispatch installs the same one-row table,
    // or returns the ordinary answer after a deterministic decline.
    let mut active = Saturation::new();
    let probe = observe(&mut active);
    active.saturate(kinds, None, &limits).unwrap();
    let expected_matches = match (decline, domains > ROW_LIMIT) {
        (false, _) => 2 * matches,
        (true, true) => 0,
        (true, false) => matches,
    };
    assert_eq!(probe.matches.load(Ordering::Relaxed), expected_matches);
    assert_eq!(active.excluded(), excluded);
    assert_eq!(
        active
            .index
            .as_ref()
            .unwrap()
            .top_chain
            .as_ref()
            .map(top_chain::Rules::len),
        (!decline).then_some(1)
    );
    assert_eq!(
        probe.global_messages.load(Ordering::Relaxed),
        usize::from(!decline)
    );
    let a = active.describe(&class("A")).unwrap();
    assert!(a.satisfiable);
    assert_eq!(a.superclasses.contains(&class("D")), !decline);
    assert!(
        !active
            .describe(&class("C"))
            .unwrap()
            .superclasses
            .contains(&class("D"))
    );
}

#[test]
fn selected_worlds_keep_their_nominal_labels_private() {
    let body = format!("{TRAILING} {CONDITIONAL} {PRIVACY}");
    let axioms = fixture(&body);
    let mut kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    for reversed in [false, true] {
        if reversed {
            kinds.reverse();
        }
        for (observed, el_only) in [(true, false), (false, false), (true, true)] {
            let limits = Limits {
                threads: 1,
                el_only,
                ..Limits::default()
            };
            let mut sat = Saturation::new();
            let probe = observed.then(|| observe(&mut sat));
            let out = sat.classify(&kinds, None, &limits).unwrap();
            check_private_worlds(&sat, &out, &kinds, !el_only);
            if let Some(probe) = probe {
                let messages = probe.world_messages.load(Ordering::Relaxed);
                assert_eq!(messages > 0, !el_only);
                if el_only {
                    assert_eq!(probe.build_visits.load(Ordering::Relaxed), 0);
                    assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
                }
                eprintln!(
                    "activation privacy reversed={reversed} el_only={el_only} world_messages={messages}"
                );
            }
        }
    }

    // A genuine global premise is the sensitivity control for the negative world.
    let promoted = fixture(&format!("{body} ClassAssertion(:A :a)"));
    let kinds: Vec<_> = promoted.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    observe(&mut sat);
    let out = sat
        .classify(
            &kinds,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert_ne!(out.consistency, Consistency::Inconsistent);
    incomplete(&sat, &out, &kinds);
    assert!(has_instance(&sat, "a"));
    assert!(out.taxonomy.is_unsatisfiable(&class("Other")));
}

fn check_private_worlds(
    sat: &Saturation,
    out: &Classification,
    kinds: &[&AxiomKind],
    active: bool,
) {
    incomplete(sat, out, kinds);
    assert_ne!(out.consistency, Consistency::Inconsistent);
    assert_eq!(sat.index.as_ref().unwrap().top_chain.is_some(), active);
    let ex = |local| sat.interner.class_ex(&class(local)).unwrap();
    let d = ex("D");
    for (local, expect_d) in [("A", active), ("Other", false)] {
        let (bottom, subs) = sat
            .worlds
            .get(&ex(local))
            .expect("fixture must select both worlds");
        assert!(!bottom, "{local} must remain satisfiable in its own world");
        assert_eq!(subs.contains(&d), expect_d, "world answer for {local}");
        assert!(!out.taxonomy.is_unsatisfiable(&class(local)));
        assert_eq!(
            out.taxonomy.is_subclass(&class(local), &class("D")),
            expect_d
        );
    }
    assert!(!has_instance(sat, "a"));
    assert!(!has_instance(sat, "b"));
    assert!(!out.taxonomy.is_subclass(&Class::thing(), &class("D")));
}

#[derive(Default)]
struct WorldEntries {
    classes: FxHashSet<Ex>,
    threads: FxHashSet<ThreadId>,
}

#[test]
fn parallel_selected_worlds_overlap_and_retire_private_state() {
    let mut body = format!("{TRAILING} {CONDITIONAL}");
    // Padding separates A and Other in expression order, so the sorted
    // selected classes put the two privacy hypotheses in different chunks.
    for i in 0..MIN_CLASSES - 2 {
        write!(body, " SubClassOf(:Pad{i} ObjectOneOf(:pad))").unwrap();
    }
    write!(body, " {PRIVACY}").unwrap();
    let axioms = fixture(&body);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut serial = Saturation::new();
    observe(&mut serial);
    let serial_out = serial
        .classify(
            &kinds,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    check_private_worlds(&serial, &serial_out, &kinds, true);
    assert_eq!(serial.worlds.len(), MIN_CLASSES + 1);
    let a = serial.interner.class_ex(&class("A")).unwrap();
    let other = serial.interner.class_ex(&class("Other")).unwrap();

    let entries = Arc::new((Mutex::new(WorldEntries::default()), Condvar::new()));
    let world_ready = {
        let entries = entries.clone();
        move |class, reported| {
            let (lock, ready) = entries.as_ref();
            let mut state = lock.lock().unwrap();
            assert!(state.classes.insert(class), "world completed twice");
            if class == a || class == other {
                assert!(
                    reported > 0,
                    "world has no live private debit at rendezvous"
                );
                assert!(state.threads.insert(thread::current().id()));
                ready.notify_all();
                let (state, _) = ready
                    .wait_timeout_while(state, admission_test::WAIT, |s| s.threads.len() < 2)
                    .unwrap();
                assert_eq!(state.threads.len(), 2, "world workers failed to rendezvous");
            }
        }
    };
    let probe = Arc::new(top_chain_diagnostic::Probe {
        world_ready: Some(Box::new(world_ready)),
        ..top_chain_diagnostic::Probe::default()
    });
    let mut parallel = Saturation::new();
    parallel.top_chain_probe = Some(probe.clone());
    let limits = Limits {
        threads: 4,
        charge: Some(Arc::new(|_| true)),
        ..Limits::default()
    };
    let joined = Arc::new(Mutex::new(None));
    let out = admission_test::with_attempt(
        None,
        false,
        {
            let joined = joined.clone();
            move |event| {
                if let Attempt::Joined {
                    base,
                    live,
                    rows,
                    paid,
                } = event
                {
                    assert!(
                        joined
                            .lock()
                            .unwrap()
                            .replace((base, live, rows, paid))
                            .is_none()
                    );
                }
            }
        },
        || parallel.classify(&kinds, None, &limits).unwrap(),
    );
    check_private_worlds(&parallel, &out, &kinds, true);
    assert_eq!(parallel.interner.class_ex(&class("A")), Some(a));
    assert_eq!(parallel.interner.class_ex(&class("Other")), Some(other));
    assert_eq!(out.taxonomy, serial_out.taxonomy);
    assert_eq!(out.consistency, serial_out.consistency);
    assert_eq!(out.incomplete, serial_out.incomplete);
    assert_eq!(parallel.describe(&class("D")), serial.describe(&class("D")));

    let entries = entries.0.lock().unwrap();
    assert_eq!(entries.threads.len(), 2);
    assert_eq!(entries.classes.len(), MIN_CLASSES + 1);
    assert!(parallel.worlds.keys().all(|d| entries.classes.contains(d)));
    assert_eq!(entries.classes.len(), parallel.worlds.len());
    let (base, live, rows, paid) = joined.lock().unwrap().unwrap();
    assert!(rows > 0);
    assert_eq!(
        live, rows as isize,
        "only successful result rows should remain at join"
    );
    assert!(paid >= base + rows);
    let messages = probe.world_messages.load(Ordering::Relaxed);
    assert!(messages > 0);
    eprintln!(
        "activation parallel worlds={} workers={} world_messages={messages} base={base} live={live} rows={rows} paid={paid}",
        entries.classes.len(),
        entries.threads.len(),
    );
}

#[test]
fn checker_component_keeps_projection_inactive() {
    for (range, checker) in [("xsd:string", false), ("rdf:langString", true)] {
        let axioms = fixture(&format!(
            "{TRAILING} {PLANNER_SOURCE}
             DataPropertyRange(:label {range})
             SubClassOf(:Labelled DataSomeValuesFrom(:label xsd:string))"
        ));
        let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        assert!(kinds.iter().all(|k| el_break(k).is_none()));
        let mut sat = Saturation::new();
        let probe = observe(&mut sat);
        let out = sat
            .classify(
                &kinds,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
        incomplete(&sat, &out, &kinds);
        assert_eq!(out.stats.outside_el, OutsideEl::default());
        let ix = sat.index.as_ref().unwrap();
        assert_eq!(ix.dt.is_some(), checker);
        if let Some(dt) = &ix.dt {
            assert!(dt.comps.checker(&DataProperty::new("urn:top-domain:label")));
        }
        assert_eq!(ix.top_chain.is_some(), !checker);
        assert_eq!(out.taxonomy.is_subclass(&class("A"), &class("D")), !checker);
        assert_eq!(out.taxonomy.is_unsatisfiable(&class("Labelled")), checker);
        assert_eq!(probe.build_visits.load(Ordering::Relaxed) > 0, !checker);
        assert_eq!(probe.global_messages.load(Ordering::Relaxed) > 0, !checker);
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
        eprintln!(
            "activation checker range={range} checker={checker} projection={}",
            ix.top_chain.is_some()
        );
    }
}

#[test]
fn rooted_tier_d_dispatch_keeps_projection_inactive() {
    let axioms = fixture(&format!(
        "{TRAILING} {PLANNER_SOURCE}
         SubClassOf(:Root ObjectUnionOf(:Left :Right))
         SubClassOf(:Left :Hit) SubClassOf(:Right :Hit)"
    ));
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let probe = observe(&mut sat);
    let limits = Limits {
        threads: 1,
        rooted: true,
        rooted_decline: false,
        ..Limits::default()
    };
    let out = sat.classify(&kinds, None, &limits).unwrap();
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert!(sat.reduced && sat.dl_rooted && sat.dl_tried);
    assert!(!out.stats.dl_over_budget);
    assert!(out.taxonomy.is_subclass(&class("Root"), &class("Hit")));
    assert!(!out.taxonomy.is_unsatisfiable(&class("Root")));
    assert!(
        out.taxonomy
            .classes()
            .all(|k| !k.as_str().starts_with(dl_reduce::PREFIX))
    );
    assert!(
        !sat.describe(&Class::thing())
            .unwrap()
            .instances
            .iter()
            .any(|i| i.as_str().starts_with(dl_reduce::PREFIX))
    );
    assert!(sat.excluded().iter().all(|e| kinds.contains(&&e.axiom)));
    assert!(sat.index.as_ref().unwrap().top_chain.is_none());
    assert_eq!(probe.build_visits.load(Ordering::Relaxed), 0);
    assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
    assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
    eprintln!(
        "activation rooted tier={:?} rooted={} reduced={}",
        out.stats.tier, sat.dl_rooted, sat.reduced
    );
}

#[test]
fn paid_horn_fallback_keeps_legacy_top_chain_answer() {
    // This is the Horn growth pattern exercised by tests/horn.rs, with separate
    // names for the chain and the positive EL premise.
    let axioms = fixture(&format!(
        "{TRAILING} {PLANNER_SOURCE}
         TransitiveObjectProperty(:part_of)
         InverseObjectProperties(:part_of :has_part)
         SubClassOf(:FilopodiumMembrane :Membrane)
         SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))
         SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))
         SubClassOf(:Cytoskeleton :Organelle)
         EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))
         EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))"
    ));
    let input: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let el_grant = Arc::new(accounting::Grant::default());
    let mut el = Saturation::new();
    let old = el
        .classify(
            &input,
            None,
            &Limits {
                el_only: true,
                ..el_grant.limits(usize::MAX)
            },
        )
        .unwrap();
    assert!(!old.taxonomy.is_subclass(&class("A"), &class("D")));

    let horn_grant = Arc::new(accounting::Grant::default());
    let mut full = Saturation::new();
    let horn = full
        .classify(&input, None, &horn_grant.limits(usize::MAX))
        .unwrap();
    assert_eq!(horn.stats.tier, Tier::Horn);
    assert!(!horn.stats.horn_over_budget);
    assert!(horn_grant.accepted() > el_grant.accepted());

    let cap = horn_grant.accepted() - 1;
    let grant = Arc::new(accounting::Grant::default());
    let mut fallback = Saturation::new();
    let probe = observe(&mut fallback);
    let out = fallback
        .classify(&input, None, &grant.limits(cap))
        .expect("the admitted EL attempt must return after the Horn refusal");
    assert!(out.stats.horn_over_budget);
    // The tier records the input's requirements even when EL supplies the answer.
    assert_eq!(out.stats.tier, old.stats.tier);
    assert_eq!(out.stats.outside_el.taken, 0);
    assert!(fallback.index.as_ref().unwrap().horn.is_none());
    assert!(out.stats.peak_memory > cap);
    assert!(grant.accepted() <= cap);
    assert!(grant.calls().iter().any(|&(_, admitted)| !admitted));
    assert_eq!(out.taxonomy.axioms(), old.taxonomy.axioms());
    assert_eq!(fallback.excluded(), el.excluded());
    assert!(
        fallback
            .excluded()
            .iter()
            .any(|e| e.gap == Gap::Partial(index::TOP_CHAIN))
    );
    assert!(fallback.index.as_ref().unwrap().top_chain.is_none());
    assert_eq!(probe.build_visits.load(Ordering::Relaxed), 0);
    assert_eq!(probe.global_messages.load(Ordering::Relaxed), 0);
    assert_eq!(probe.world_messages.load(Ordering::Relaxed), 0);
    eprintln!(
        "activation paid Horn fallback cap={cap} paid={}",
        grant.accepted()
    );
}
