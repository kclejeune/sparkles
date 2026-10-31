//! Exact permission preparation, with no uncontrolled guard fallback.
use sparkles_core::access::{
    Caller, Decider, GraphAccess, Graphs, GuardDecision, Limits, Preparation, Protection, Rule,
    TripleRules,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::task::Control;
use sparkles_core::{BudgetKind, Error};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct LegacyOnly(Arc<AtomicUsize>);
impl GuardDecision for LegacyOnly {
    fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec!["urn:guarded:a".into()]
    }
}

fn guarded(calls: &Arc<AtomicUsize>) -> GraphAccess {
    GraphAccess::graphs(
        Graphs::All.guarded_by(
            "urn:guarded:*",
            Decider(Arc::new(LegacyOnly(calls.clone()))),
        ),
        Graphs::none(),
    )
}

#[test]
fn unsupported_provider_is_refused_without_running_legacy_callback() {
    let store = Store::in_memory(StoreOptions::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
    let result = guarded(&calls).masked_controlled(&store.snapshot(), &preparation);
    assert!(
        result.is_err(),
        "an uncontrolled provider cannot prepare a bounded view"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_precedes_any_legacy_provider_work() {
    let store = Store::in_memory(StoreOptions::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let control = Control::none();
    let preparation = Preparation::new(control.clone(), 8 << 20).unwrap();
    control.cancel.cancel();
    let result = guarded(&calls).masked_controlled(&store.snapshot(), &preparation);
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn low_cap_refuses_a_real_mask_without_publishing_a_view() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> \"hidden\" ; <urn:visible> \"visible\" }",
        &Default::default(),
    )
    .unwrap();
    let protection = Protection {
        name: "secret".into(),
        predicates: Some(vec!["urn:secret".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: BTreeMap::new(),
        hide_inferences: false,
    };
    let access = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    );
    let snap = store.snapshot();
    let preparation = Preparation::new(Control::none(), 256).unwrap();
    let result = access.masked_controlled(&snap, &preparation);
    assert!(matches!(
        result,
        Err(Error::BudgetExceeded(ref budget))
            if budget.kind == BudgetKind::Memory && budget.limit == 256 && budget.requested == 257
    ));
    assert_eq!(store.snapshot().commit, snap.commit);
    assert_eq!(store.snapshot().len(), 2);
}

fn protection() -> Protection {
    Protection {
        name: "secret".into(),
        predicates: Some(vec!["urn:secret".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: BTreeMap::new(),
        hide_inferences: false,
    }
}

fn access(protections: Vec<(Protection, Graphs)>, read: Graphs) -> GraphAccess {
    GraphAccess::with_triples(
        read,
        Graphs::none(),
        TripleRules {
            rules: protections
                .into_iter()
                .map(|(protection, read)| Rule {
                    protection: Arc::new(protection),
                    read,
                    write: Graphs::none(),
                })
                .collect(),
            caller: Caller {
                user: Some("alice".into()),
                roles: vec!["reader".into()],
                groups: vec!["staff".into()],
            },
            limits: Limits::default(),
        },
    )
}

fn fixture() -> Store {
    use sparkles_core::io::{RdfFormat, Source};
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            br#"
        @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        <urn:a> a <urn:A> ; <urn:secret> "alpha" ; <urn:visible> "visible" .
        <urn:b> a <urn:B> ; <urn:secret> "beta" .
        <urn:B> rdfs:subClassOf <urn:A> . <urn:A> rdfs:subClassOf <urn:B> .
        <urn:g1> { <urn:a> <urn:secret> "graph-one" . <urn:c> <urn:visible> _:blank . }
        <urn:g2> { <urn:b> <urn:secret> "graph-two" . }
        <urn:x-sparkles:inferred> { <urn:a> <urn:secret> "inference" . }
        _:graph { _:blank <urn:secret> "blank-graph" . }
        <urn:says> <urn:term> <<( _:blank <urn:p> <<( <urn:s> <urn:p> "object"@en--ltr )>> )>> .
    "#
            .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    sparkles_core::sparql::update::update(&store,
        "DELETE DATA { <urn:b> <urn:secret> \"beta\" } ; INSERT DATA { <urn:d> a <urn:B> ; <urn:secret> \"delta\" . GRAPH <urn:g2> { <urn:d> <urn:secret> \"delta-graph\" } }",
        &Default::default()).unwrap();
    store
}

fn quads(snap: &Snapshot) -> std::collections::BTreeSet<[u64; 4]> {
    let mut rows = std::collections::BTreeSet::new();
    snap.for_each_quad(|quad| {
        rows.insert(quad.map(|id| id.0));
        Ok(())
    })
    .unwrap();
    rows
}

#[test]
fn sufficient_caps_match_legacy_graph_predicate_class_subclass_and_exemption_rules() {
    use sparkles_core::access::GraphRule;
    let store = fixture();
    let snapshot = store.snapshot();
    let mut cases = vec![GraphAccess::all()];
    for predicate in [
        None,
        Some(vec!["urn:secret".into()]),
        Some(vec!["urn:s*".into()]),
        Some(vec!["urn:missing".into()]),
    ] {
        for classes in [
            None,
            Some(vec!["urn:A".into()]),
            Some(vec!["urn:missing".into()]),
        ] {
            for subclasses in [false, true] {
                let mut p = protection();
                p.predicates = predicate.clone();
                p.classes = classes.clone();
                p.subclasses = subclasses;
                for grant in [
                    Graphs::none(),
                    Graphs::Only(GraphRule::new(["urn:g2"], &[])),
                ] {
                    cases.push(access(vec![(p.clone(), grant)], Graphs::All));
                }
            }
        }
    }
    let mut inferred = protection();
    inferred.hide_inferences = true;
    cases.push(access(
        vec![(inferred, Graphs::none()), (protection(), Graphs::none())],
        Graphs::All,
    ));
    let mut scoped = protection();
    scoped.graphs = Some(Graphs::Only(GraphRule::new(["urn:g1"], &[])));
    cases.push(access(
        vec![(scoped, Graphs::none())],
        Graphs::Only(GraphRule::new(["default", "urn:g*"], &[])),
    ));
    for view in cases {
        let legacy_access = Arc::new(view.clone()).bound(&snapshot);
        let legacy = legacy_access.masked(&snapshot).unwrap();
        // Fresh count/mask cache, same exact physical state. This requires real
        // controlled construction even after the differential legacy read.
        let cold = Arc::new(Snapshot {
            counts: Default::default(),
            ..(*snapshot).clone()
        });
        let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
        let actual = view.masked_controlled(&cold, &preparation).unwrap();
        assert_eq!(actual.access(), legacy_access.as_ref());
        assert_eq!(quads(actual.snapshot()), quads(&legacy));
        assert_eq!(actual.snapshot().len(), legacy.len());
        assert!(actual.snapshot().delta.predicates_consistent());
        assert_eq!(actual.snapshot().commit, snapshot.commit);
        let warm = view
            .masked_controlled(&cold, &Preparation::new(Control::none(), 32 << 20).unwrap())
            .unwrap();
        assert_eq!(quads(warm.snapshot()), quads(&legacy));
        if actual.snapshot().mask.is_some() {
            assert!(std::ptr::eq(actual.snapshot(), warm.snapshot()));
        }
    }
}

#[test]
fn static_public_rule_order_and_already_masked_scope_remain_unchanged() {
    use sparkles_core::access::{GraphGuard, GraphRule};
    let store = fixture();
    let mut rule = GraphRule::new(["urn:g1", "urn:g2"], &[]);
    rule.iris.reverse();
    rule.guards.push(GraphGuard {
        pattern: "urn:g*".into(),
        exempt: vec!["urn:g2".into(), "urn:g1".into()],
        decide: None,
    });
    let view = access(vec![(protection(), Graphs::none())], Graphs::Only(rule));
    let legacy = view.masked(&store.snapshot()).unwrap();
    let actual = view
        .masked_controlled(
            &store.snapshot(),
            &Preparation::new(Control::none(), 16 << 20).unwrap(),
        )
        .unwrap();
    assert_eq!(actual.access().read, view.read);
    assert_eq!(quads(actual.snapshot()), quads(&legacy));
    let again = GraphAccess::all()
        .masked_controlled(
            &legacy,
            &Preparation::new(Control::none(), 16 << 20).unwrap(),
        )
        .unwrap();
    assert!(std::ptr::eq(actual.snapshot(), again.snapshot()));
}

#[test]
fn controlled_streaming_and_term_decoding_match_all_legacy_terms_and_permutations() {
    use sparkles_core::index::Perm;
    let store = fixture();
    let snap = store.snapshot();
    let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
    for perm in Perm::ALL {
        let expected = snap.scan_keys(perm, &[]).unwrap();
        let mut actual = Vec::new();
        preparation
            .scan(&snap, perm, &[], |quad| {
                actual.push(perm.to_key(&quad));
                for id in quad {
                    preparation.with_term(&snap, id, |term| {
                        assert_eq!(term, snap.term(id).as_ref());
                        Ok(())
                    })?;
                }
                Ok(true)
            })
            .unwrap();
        assert_eq!(actual, expected);
    }
    let missing = preparation.lookup_iri(&snap, "urn:missing").unwrap();
    assert!(missing.is_none());
    assert_eq!(
        preparation.lookup_iri(&snap, "urn:secret").unwrap(),
        snap.lookup_iri("urn:secret")
    );
}

#[test]
fn patterns_explicitly_refuse_without_approximating_legacy_visibility() {
    let store = fixture();
    let mut p = protection();
    p.pattern = Some("FILTER(?user = \"alice\")".into());
    let view = access(vec![(p, Graphs::none())], Graphs::All);
    let legacy = view.masked(&store.snapshot()).unwrap();
    // Preserve the actual legacy scope of this nested FILTER pattern. The bounded
    // path must refuse its unsupported planning, never substitute point checks.
    assert!(!legacy.mask.as_ref().unwrap().hidden.is_empty());
    assert!(legacy.len() < store.snapshot().len());
    let result = view.masked_controlled(
        &store.snapshot(),
        &Preparation::new(Control::none(), 16 << 20).unwrap(),
    );
    assert!(
        matches!(result, Err(Error::Unsupported(ref message)) if message.contains("pattern planning"))
    );
}

#[test]
fn retained_cache_caps_cancellation_and_deadlines_apply_to_successful_fast_paths() {
    use std::time::Instant;
    let store = fixture();
    let view = access(vec![(protection(), Graphs::none())], Graphs::All);
    let snap = store.snapshot();
    let first = view
        .masked_controlled(&snap, &Preparation::new(Control::none(), 16 << 20).unwrap())
        .unwrap();
    let low = view.masked_controlled(&snap, &Preparation::new(Control::none(), 32 << 10).unwrap());
    assert!(matches!(low, Err(Error::BudgetExceeded(_))));
    let control = Control::none();
    let prep = Preparation::new(control.clone(), 16 << 20).unwrap();
    control.cancel.cancel();
    assert!(matches!(
        view.masked_controlled(&snap, &prep),
        Err(Error::Cancelled)
    ));
    let control = Control {
        deadline: Some(Instant::now()),
        ..Control::none()
    };
    assert!(matches!(
        Preparation::new(control, 16 << 20),
        Err(Error::Timeout)
    ));
    let next = view
        .masked_controlled(&snap, &Preparation::new(Control::none(), 16 << 20).unwrap())
        .unwrap();
    assert!(std::ptr::eq(first.snapshot(), next.snapshot()));
}

#[test]
fn supported_providers_use_the_same_preparation_and_keep_legacy_exemptions() {
    use sparkles_core::access::Exemptions;
    struct Supported;
    impl GuardDecision for Supported {
        fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
            vec!["urn:g2".into(), "urn:g1".into(), "urn:g1".into()]
        }
        fn exempt_controlled(
            &self,
            _: &Arc<Snapshot>,
            preparation: &Preparation,
        ) -> sparkles_core::Result<Exemptions> {
            let mut output = Exemptions::new(preparation)?;
            for iri in ["urn:g2", "urn:g1", "urn:g1"] {
                output.push(iri)?;
            }
            Ok(output)
        }
    }
    let store = fixture();
    let view = GraphAccess::graphs(
        Graphs::All.guarded_by("urn:g*", Decider(Arc::new(Supported))),
        Graphs::none(),
    );
    let old = Arc::new(view.clone()).bound(&store.snapshot());
    let actual = view
        .masked_controlled(
            &store.snapshot(),
            &Preparation::new(Control::none(), 16 << 20).unwrap(),
        )
        .unwrap();
    assert_eq!(actual.access(), old.as_ref());
    assert_eq!(quads(actual.snapshot()), quads(&store.snapshot()));
}

#[test]
fn cancellation_and_deadline_reach_large_physical_enumerations() {
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::task::Progress;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for row in 0..5000 {
        data.push_str(&format!(
            "<urn:s{row}> <urn:secret> <urn:o> <urn:g{row}> .\n"
        ));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    let snap = store.snapshot();
    let mut p = protection();
    p.predicates = Some(vec!["urn:s*".into()]);
    let view = access(vec![(p, Graphs::none())], Graphs::All);
    for deadline_mode in [false, true] {
        let observed = Arc::new(AtomicBool::new(false));
        let seen = observed.clone();
        let control = Control::none();
        let cancel = control.cancel.clone();
        let deadline = deadline_mode.then(|| Instant::now() + Duration::from_millis(300));
        let control = Control {
            deadline,
            progress: Progress::new(move |_, message| {
                if message == "scanning permission rows" && !seen.swap(true, Ordering::SeqCst) {
                    if let Some(deadline) = deadline {
                        while let Some(remaining) = deadline.checked_duration_since(Instant::now())
                        {
                            std::thread::park_timeout(remaining);
                        }
                    } else {
                        cancel.cancel();
                    }
                }
            }),
            ..control
        };
        let preparation = Preparation::new(control, 32 << 20).unwrap();
        let result = view.masked_controlled(&snap, &preparation);
        assert!(
            observed.load(Ordering::SeqCst),
            "no checkpoint after1024 actual physical rows"
        );
        if deadline_mode {
            assert!(matches!(result, Err(Error::Timeout)));
        } else {
            assert!(matches!(result, Err(Error::Cancelled)));
        }
        assert_eq!(store.snapshot().commit, snap.commit);
        assert_eq!(store.snapshot().len(), 5000);
    }
}

#[test]
fn controlled_term_decoder_preserves_existing_nesting_boundary() {
    use oxrdf::{NamedNode, Term};
    use sparkles_core::id::{self, Id};
    fn nested_key(depth: usize) -> Vec<u8> {
        let subject = id::iri_key("urn:subject");
        let predicate = id::iri_key("urn:predicate");
        let mut object = id::iri_key("urn:object");
        for _ in 0..depth {
            let mut key = vec![b'('];
            for part in [subject.as_slice(), predicate.as_slice(), object.as_slice()] {
                let mut length = part.len() as u64;
                while length >= 128 {
                    key.push(length as u8 | 128);
                    length >>= 7;
                }
                key.push(length as u8);
                key.extend_from_slice(part);
            }
            object = key;
        }
        object
    }
    let store = Store::in_memory(StoreOptions::default());
    let mut writer = store.write();
    let subject = writer
        .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:s")))
        .unwrap();
    let predicate = writer
        .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:p")))
        .unwrap();
    // The low-level encoded-key writer can represent damaged imported metadata.
    // The bounded decoder must enforce the existing C01 depth, not overflow its
    // cleanup stack or introduce a different allowed depth for valid RDF.
    let valid = writer
        .intern_key(&nested_key(sparkles_core::nesting::MAX_TRIPLE_TERMS))
        .unwrap();
    let invalid = writer
        .intern_key(&nested_key(sparkles_core::nesting::MAX_TRIPLE_TERMS + 1))
        .unwrap();
    writer
        .insert([subject, predicate, valid, Id::DEFAULT_GRAPH])
        .unwrap();
    writer
        .insert([subject, predicate, invalid, Id::DEFAULT_GRAPH])
        .unwrap();
    writer.commit().unwrap();
    let snap = store.snapshot();
    let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
    preparation
        .with_term(&snap, valid, |term| {
            assert_eq!(term, snap.term(valid).as_ref());
            Ok(())
        })
        .unwrap();
    assert!(
        matches!(preparation.with_term(&snap, invalid, |_| Ok(())), Err(Error::Corrupt(ref message)) if message.contains("nesting"))
    );
    assert_eq!(store.snapshot().commit, snap.commit);
    assert_eq!(store.snapshot().len(), 2);
}

#[test]
fn retained_policy_spare_capacity_is_admitted_before_returning_its_owner() {
    let store = Store::in_memory(StoreOptions::default());
    let caller = Caller {
        roles: Vec::with_capacity(1_000_000),
        ..Caller::default()
    };
    let view = GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection()),
                read: Graphs::All,
                write: Graphs::none(),
            }],
            caller,
            limits: Limits::default(),
        },
    );
    assert!(
        matches!(
            view.masked_controlled(
                &store.snapshot(),
                &Preparation::new(Control::none(), 64 << 10).unwrap()
            ),
            Err(Error::BudgetExceeded(_))
        ),
        "empty spare policy capacity was retained without admission"
    );
}

#[test]
fn mixed_static_and_undecided_rules_normalize_both_graph_sets_like_legacy_binding() {
    use sparkles_core::access::{Exemptions, GraphRule};
    struct Supported;
    impl GuardDecision for Supported {
        fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
            vec!["urn:g1".into()]
        }
        fn exempt_controlled(
            &self,
            _: &Arc<Snapshot>,
            p: &Preparation,
        ) -> sparkles_core::Result<Exemptions> {
            let mut values = Exemptions::new(p)?;
            values.push("urn:g1")?;
            Ok(values)
        }
    }
    let store = fixture();
    for undecided_read in [false, true] {
        let mut rule = GraphRule::new(["urn:g1", "urn:g2"], &[]);
        rule.iris.reverse();
        let fixed = Graphs::Only(rule);
        let dynamic = Graphs::All.guarded_by("urn:g*", Decider(Arc::new(Supported)));
        let access = if undecided_read {
            GraphAccess::graphs(dynamic, fixed)
        } else {
            GraphAccess::graphs(fixed, dynamic)
        };
        let expected = Arc::new(access.clone()).bound(&store.snapshot());
        let actual = access
            .masked_controlled(
                &store.snapshot(),
                &Preparation::new(Control::none(), 8 << 20).unwrap(),
            )
            .unwrap();
        assert_eq!(
            actual.access(),
            expected.as_ref(),
            "undecided read: {undecided_read}"
        );
    }
}

#[test]
fn retained_policy_capacities_include_all_owned_container_families() {
    use sparkles_core::access::{GraphGuard, GraphRule};
    let store = Store::in_memory(StoreOptions::default());
    for variant in 0..10 {
        let mut p = protection();
        let mut caller = Caller::default();
        let mut rules = Vec::new();
        let mut grants = Graphs::none();
        match variant {
            0 => caller.groups = Vec::with_capacity(100_000),
            1 => {
                let mut user = String::with_capacity(1 << 20);
                user.push_str("alice");
                caller.user = Some(user);
            }
            2 => rules = Vec::with_capacity(100_000),
            3 => p.predicates = Some(Vec::with_capacity(100_000)),
            4 => p.classes = Some(Vec::with_capacity(100_000)),
            5 => {
                p.name = String::with_capacity(1 << 20);
                p.name.push_str("secret");
            }
            6 => {
                let mut pattern = String::with_capacity(1 << 20);
                pattern.push_str("{}");
                p.pattern = Some(pattern);
            }
            7 => {
                let mut key = String::with_capacity(1 << 20);
                key.push_str("prefix");
                p.prefixes.insert(key, "urn:p".into());
            }
            8 => {
                let rule = GraphRule {
                    excluded: Vec::with_capacity(100_000),
                    ..GraphRule::default()
                };
                p.graphs = Some(Graphs::Only(rule));
            }
            _ => {
                let mut rule = GraphRule {
                    guards: Vec::with_capacity(100_000),
                    ..GraphRule::default()
                };
                rule.guards.push(GraphGuard {
                    pattern: "urn:*".into(),
                    exempt: Vec::new(),
                    decide: None,
                });
                grants = Graphs::Only(rule);
            }
        }
        rules.push(Rule {
            protection: Arc::new(p),
            read: Graphs::All,
            write: grants,
        });
        let view = GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules,
                caller,
                limits: Limits::default(),
            },
        );
        assert!(
            matches!(
                view.masked_controlled(
                    &store.snapshot(),
                    &Preparation::new(Control::none(), 64 << 10).unwrap()
                ),
                Err(Error::BudgetExceeded(_))
            ),
            "retained capacity family {variant} escaped admission"
        );
    }
}

#[test]
fn retained_nested_provider_state_refuses_without_callbacks_or_a_returned_owner() {
    use sparkles_core::access::Exemptions;
    use std::sync::atomic::AtomicBool;
    struct Opaque {
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
        controlled: bool,
        _payload: Vec<u8>,
    }
    impl Drop for Opaque {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
    impl GuardDecision for Opaque {
        fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        }
        fn exempt_controlled(
            &self,
            _: &Arc<Snapshot>,
            p: &Preparation,
        ) -> sparkles_core::Result<Exemptions> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.controlled {
                Exemptions::new(p)
            } else {
                Err(Error::Unsupported("legacy-only fixture provider".into()))
            }
        }
    }
    let store = Store::in_memory(StoreOptions::default());
    for location in 0..3 {
        for controlled in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicBool::new(false));
            let provider = Arc::new(Opaque {
                calls: calls.clone(),
                dropped: dropped.clone(),
                controlled,
                _payload: vec![1; 1 << 20],
            });
            let nested = Graphs::All.guarded_by("urn:guarded:*", Decider(provider.clone()));
            let mut p = protection();
            let (read, write) = match location {
                0 => (Graphs::All, nested),
                1 => (nested, Graphs::none()),
                _ => {
                    p.graphs = Some(nested);
                    (Graphs::All, Graphs::none())
                }
            };
            let access = GraphAccess::with_triples(
                Graphs::All,
                Graphs::All,
                TripleRules {
                    rules: vec![Rule {
                        protection: Arc::new(p),
                        read,
                        write,
                    }],
                    caller: Caller::default(),
                    limits: Limits::default(),
                },
            );
            // Existing binding deliberately leaves these nested guards unresolved.
            assert!(Arc::ptr_eq(
                Arc::new(access.clone())
                    .bound(&store.snapshot())
                    .triples
                    .as_ref()
                    .unwrap(),
                access.triples.as_ref().unwrap()
            ));
            let result = access.masked_controlled(
                &store.snapshot(),
                &Preparation::new(Control::none(), 64 << 10).unwrap(),
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(
                matches!(result, Err(Error::Unsupported(_))),
                "opaque nested provider state retained under small cap at location {location}"
            );
            drop(access);
            drop(provider);
            assert!(
                dropped.load(Ordering::SeqCst),
                "refusal kept an opaque provider owner"
            );
        }
    }
}

#[test]
fn static_nested_guards_preserve_legacy_visibility_without_opaque_providers() {
    use sparkles_core::access::{GraphGuard, GraphRule};
    let store = fixture();
    for location in 0..3 {
        let nested = Graphs::Only(GraphRule {
            default_graph: true,
            patterns: vec!["urn:*".into()],
            guards: vec![GraphGuard {
                pattern: "urn:g*".into(),
                exempt: vec!["urn:g1".into()],
                decide: None,
            }],
            ..GraphRule::default()
        });
        let mut p = protection();
        let (read, write) = match location {
            0 => (Graphs::All, nested),
            1 => (nested, Graphs::none()),
            _ => {
                p.graphs = Some(nested);
                (Graphs::All, Graphs::none())
            }
        };
        let access = GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(p),
                    read,
                    write,
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        );
        let legacy = access.masked(&store.snapshot()).unwrap();
        let snapshot = store.snapshot();
        let cold = Arc::new(Snapshot {
            counts: Default::default(),
            ..(*snapshot).clone()
        });
        let actual = access
            .masked_controlled(&cold, &Preparation::new(Control::none(), 16 << 20).unwrap())
            .unwrap();
        assert_eq!(actual.access(), &access);
        assert_eq!(quads(actual.snapshot()), quads(&legacy));
    }
}

#[test]
fn a_snapshot_masked_by_another_policy_is_refused() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> \"hidden\" ; <urn:visible> \"visible\" }",
        &Default::default(),
    )
    .unwrap();
    let a = access(vec![(protection(), Graphs::none())], Graphs::All);
    let b = access(
        vec![(
            Protection {
                name: "other".into(),
                predicates: Some(vec!["urn:visible".into()]),
                ..protection()
            },
            Graphs::none(),
        )],
        Graphs::All,
    );
    let masked = a.masked(&store.snapshot()).unwrap();
    assert_eq!(masked.len(), 1);
    let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
    // Policy B would otherwise be paired with the physical visibility A decided.
    assert!(matches!(
        b.masked_controlled(&masked, &preparation),
        Err(Error::Unsupported(_))
    ));
    // A policy that hides no triples keeps the stricter existing mask.
    let unhidden = GraphAccess::all()
        .masked_controlled(&masked, &preparation)
        .unwrap();
    assert_eq!(unhidden.snapshot().len(), 1);
    drop(unhidden);
    // A's own mask is the exact physical view of A.
    let same = a.masked_controlled(&masked, &preparation).unwrap();
    assert_eq!(same.snapshot().len(), 1);
    drop(same);
    let fresh = b
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    assert_eq!(fresh.snapshot().len(), 1);
}

/// A store whose 20,000 quads and their terms stay in the uncompacted delta.
fn uncompacted() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    for chunk in 0..4 {
        let rows = (chunk * 5000..(chunk + 1) * 5000)
            .map(|row| {
                let p = if row % 10 == 0 {
                    "urn:secret"
                } else {
                    "urn:visible"
                };
                format!("<urn:s{row}> <{p}> \"value {row}\" .\n")
            })
            .collect::<String>();
        sparkles_core::sparql::update::update(
            &store,
            &format!("INSERT DATA {{ {rows} }}"),
            &Default::default(),
        )
        .unwrap();
    }
    assert_eq!(store.snapshot().delta.inserts(), 20_000);
    store
}

#[test]
fn the_shared_input_delta_is_not_charged_to_the_view() {
    let store = uncompacted();
    let snap = store.snapshot();
    // A graph-only policy keeps the input snapshot and owns almost nothing.
    let graphs = GraphAccess::graphs(Graphs::All, Graphs::none());
    let small = Preparation::new(Control::none(), 64 << 10).unwrap();
    let view = graphs.masked_controlled(&snap, &small).unwrap();
    assert!(std::ptr::eq(view.snapshot(), &*snap));
    drop(view);
    // A mask charges the nodes it copies, not the 20,000 shared quads at 2 KiB each.
    let masked = access(vec![(protection(), Graphs::none())], Graphs::All);
    let cap = 16 << 20;
    let preparation = Preparation::new(Control::none(), cap).unwrap();
    let view = masked.masked_controlled(&snap, &preparation).unwrap();
    assert_eq!(view.snapshot().len(), 18_000);
    let legacy = masked.masked(&snap).unwrap();
    assert_eq!(quads(view.snapshot()), quads(&legacy));
    // the planner statistics of the hidden inserted quads leave as the legacy path's do
    assert!(view.snapshot().delta.predicates_consistent());
    let stats = |s: &Snapshot| {
        s.delta
            .predicate_stats()
            .map(|(p, d)| (p, *d))
            .collect::<Vec<_>>()
    };
    assert_eq!(stats(view.snapshot()), stats(&legacy));
    drop(view);
    // A cache hit is charged for the cached mask's own storage only.
    let view = masked.masked_controlled(&snap, &preparation).unwrap();
    assert_eq!(view.snapshot().len(), 18_000);
}
