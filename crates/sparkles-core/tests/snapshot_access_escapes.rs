//! Regression tests for roadmap item 1307 (snapshot, mask and graph-access escapes).
use sparkles_core::access::{
    Caller, Decider, Exemptions, GraphAccess, Graphs, GuardDecision, GuardSnapshot, Limits,
    Preparation, Protection, Rule, TripleRules,
};
use sparkles_core::history::At;
use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::{CursorOptions, QueryOptions, query_source, select_source_cursor};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::task::Control;
use sparkles_core::{Error, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

fn hiding_policy() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ))
}

struct CommitGuard;
impl GuardDecision for CommitGuard {
    fn exempt(&self, snap: &Arc<Snapshot>) -> Vec<String> {
        if snap.commit <= 1 {
            vec!["urn:guarded:a".into()]
        } else {
            Vec::new()
        }
    }

    fn exempt_controlled(
        &self,
        snap: &GuardSnapshot<'_>,
        preparation: &Preparation,
    ) -> Result<Exemptions> {
        let mut out = Exemptions::new(preparation)?;
        if snap.commit() <= 1 {
            out.push("urn:guarded:a")?;
        }
        Ok(out)
    }
}

fn guarded_policy() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by("urn:guarded:*", Decider(Arc::new(CommitGuard))),
        Graphs::none(),
    ))
}

#[test]
fn prepared_view_and_read_source_return_pool_credit_to_exact_zero_on_drop() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:visible> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let pool = QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let policy = Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ));
    {
        let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
        let baseline = grant.used();
        let view = store.prepare_read(policy.clone(), &prep).unwrap();
        assert!(view.has_mask());
        assert_eq!(view.len(), 1);
        assert!(grant.used() > baseline);

        // Retaining BoundReadSource keeps credit alive after PreparedView drops.
        let source = view.read_source();
        drop(view);
        assert!(grant.used() > baseline);

        let mut cursor = select_source_cursor(
            source,
            &store,
            "SELECT ?p WHERE { <urn:s> ?p ?o }",
            &QueryOptions {
                graphs: Some(policy.clone()),
                grant: Some(grant.clone()),
                ..Default::default()
            },
            &CursorOptions::default(),
        )
        .unwrap();
        let batch = cursor.next_batch().unwrap().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(
            batch.term(0, 0).unwrap().unwrap().as_term().to_string(),
            "<urn:visible>"
        );
        drop(batch);
        drop(cursor);
        assert_eq!(grant.used(), baseline);
    }
    assert_eq!(grant.used(), 0);
}

#[test]
fn bound_policy_from_commit_n_is_refused_on_commit_n_plus_1_and_other_stores() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { GRAPH <urn:guarded:a> { <urn:s> <urn:p> 1 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    let commit_1 = store.head_commit().seq;
    let snap_1 = store.snapshot();
    let unbound = guarded_policy();
    let bound_at_1 = unbound.bound(&snap_1);
    assert!(bound_at_1.read.allows_iri("urn:guarded:a"));

    // Same commit is accepted on checked paths.
    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    let view_1 = store.prepare_read(bound_at_1.clone(), &prep).unwrap();
    assert!(view_1.allows_read_iri("urn:guarded:a"));
    drop(view_1);

    // Advance store to commit N + 1 where CommitGuard no longer exempts urn:guarded:a.
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:other> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(store.head_commit().seq > commit_1);
    let snap_2 = store.snapshot();

    // Every checked path refuses applying the commit-1 bound policy to commit 2.
    assert!(matches!(
        store.prepare_read(bound_at_1.clone(), &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
    assert!(matches!(
        store.prepare_read_at(&At::Head, bound_at_1.clone(), &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
    assert!(matches!(
        bound_at_1.masked_controlled(&snap_2, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));

    // Rebinding the original unbound policy against commit 2 works and denies urn:guarded:a.
    let view_2 = store.prepare_read(unbound.clone(), &prep).unwrap();
    assert!(!view_2.allows_read_iri("urn:guarded:a"));
    drop(view_2);

    // A policy bound to another store at the same commit number is also refused.
    let foreign = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &foreign,
        "INSERT DATA { GRAPH <urn:guarded:a> { <urn:s> <urn:p> 1 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(foreign.head_commit().seq, commit_1);
    assert!(matches!(
        foreign.prepare_read(bound_at_1.clone(), &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));

    // A policy bound to an uncommitted transaction view is accepted on that exact view,
    // and refused on a different uncommitted view or committed snapshot.
    let txn = store.write();
    let uncommitted = Arc::new(txn.view());
    let uncommitted_other = Arc::new(txn.view());
    let bound_uncommitted = unbound.bound(&uncommitted);
    assert!(
        bound_uncommitted
            .masked_controlled(&uncommitted, &prep)
            .is_ok(),
        "same uncommitted view must be accepted"
    );
    assert!(matches!(
        bound_uncommitted.masked_controlled(&uncommitted_other, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
    assert!(matches!(
        bound_uncommitted.masked_controlled(&snap_2, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
    drop(txn);

    // A masked committed derivative and the original unmasked committed snapshot are
    // distinct views and must not be treated as interchangeable by BoundSnapshot.
    let masked_1 = hiding_policy().masked(&snap_1).unwrap();
    let bound_masked_1 = unbound.bound(&masked_1);
    assert!(matches!(
        bound_masked_1.masked_controlled(&snap_1, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
    assert!(matches!(
        bound_at_1.masked_controlled(&masked_1, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
}

#[test]
fn nested_bound_guards_in_triple_rules_are_refused_on_other_commits() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { GRAPH <urn:guarded:a> { <urn:s> <urn:secret> 1 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    let snap_1 = store.snapshot();
    let bound_graphs_1 = guarded_policy().bound(&snap_1).read.clone();

    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:other> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let snap_2 = store.snapshot();
    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();

    // Stale bound guard copied into Rule.read must be refused on commit 2.
    let nested_rule_read = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: bound_graphs_1.clone(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    );
    assert!(matches!(
        nested_rule_read.masked_controlled(&snap_2, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));

    // Stale bound guard copied into Protection.graphs must be refused on commit 2.
    let nested_protection_graphs = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(bound_graphs_1),
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    );
    assert!(matches!(
        nested_protection_graphs.masked_controlled(&snap_2, &prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
}

#[test]
fn controlled_mask_is_not_shared_with_legacy_masked_caller() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:visible> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let snap = store.snapshot();
    let policy = hiding_policy();
    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    let controlled = policy.masked_controlled(&snap, &prep).unwrap();
    let legacy = policy.masked(&snap).unwrap();
    assert_ne!(
        controlled.mask_id(),
        legacy.mask_id(),
        "legacy GraphAccess::masked must not upgrade a mask paid for by masked_controlled"
    );
}

#[test]
fn write_txn_prepare_read_view_enforces_protections_and_returns_credit() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:visible> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();

    let policy = Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ));
    let mut txn = store.write_with(
        sparkles_core::commit::CommitKind::Update,
        sparkles_core::guard::WriteOptions {
            graphs: Some(policy),
            ..Default::default()
        },
    );
    let s = txn
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:new",
        )))
        .unwrap();
    let p = txn
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:visible",
        )))
        .unwrap();
    let o = txn
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:val",
        )))
        .unwrap();
    txn.insert([s, p, o, sparkles_core::id::Id::DEFAULT_GRAPH])
        .unwrap();

    let pool = QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    {
        let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
        let baseline = grant.used();
        // A tiny cap must refuse before constructing the transaction snapshot/delta.
        let tiny = Preparation::new(Control::none(), 4096).unwrap();
        assert!(matches!(
            txn.prepare_read_view(&tiny),
            Err(Error::BudgetExceeded(_))
        ));
        let prepared = txn.prepare_read_view(&prep).unwrap();
        assert!(prepared.has_mask());
        // Sees the committed visible triple + the uncommitted visible triple, not urn:secret.
        assert_eq!(prepared.len(), 2);
        assert!(grant.used() > baseline);

        let res = prepared
            .query(
                "SELECT ?p WHERE { ?s ?p ?o }",
                &QueryOptions {
                    grant: Some(grant.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let rows = res.collect_rows().unwrap();
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row[0].as_ref().unwrap().to_string(), "<urn:visible>");
        }
        drop(res);
        drop(prepared);
        assert_eq!(grant.used(), baseline);
    }
    assert_eq!(grant.used(), 0);
    // Explicit discard_terms rolls back uncommitted vocabulary cleanly after scoped view drops.
    txn.discard_terms().unwrap();
    drop(txn);

    // Dry-run WriteTxn rolls back vocabulary cleanly after scoped PreparedTxnView drops.
    let mut dry = store.write_with(
        sparkles_core::commit::CommitKind::Update,
        sparkles_core::guard::WriteOptions {
            dry_run: Some(sparkles_core::preview::DryRun::default()),
            ..Default::default()
        },
    );
    let ds = dry
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:dry:s",
        )))
        .unwrap();
    let dp = dry
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:dry:p",
        )))
        .unwrap();
    let do_ = dry
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:dry:o",
        )))
        .unwrap();
    dry.insert([ds, dp, do_, sparkles_core::id::Id::DEFAULT_GRAPH])
        .unwrap();
    {
        let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
        let baseline = grant.used();
        let prepared = dry.prepare_read_view(&prep).unwrap();
        let res = prepared
            .query(
                "ASK { <urn:dry:s> <urn:dry:p> <urn:dry:o> }",
                &QueryOptions {
                    grant: Some(grant.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(res.boolean());
        drop(res);
        drop(prepared);
        assert_eq!(grant.used(), baseline);
    }
    drop(dry);
    assert_eq!(grant.used(), 0);
    assert!(store.snapshot().lookup_iri("urn:dry:s").is_none());

    // Pre-bound WriteOptions.graphs is accepted on a clean txn and refused once dirty.
    let base_snap = store.snapshot();
    let writable_guarded = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by("urn:guarded:*", Decider(Arc::new(CommitGuard))),
        Graphs::All,
    ));
    let bound_base = writable_guarded.bound(&base_snap);
    let mut txn_bound = store.write_with(
        sparkles_core::commit::CommitKind::Update,
        sparkles_core::guard::WriteOptions {
            graphs: Some(bound_base),
            ..Default::default()
        },
    );
    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    {
        let clean_view = txn_bound.prepare_read_view(&prep).unwrap();
        assert!(clean_view.allows_read_iri("urn:guarded:a"));
    }
    let s2 = txn_bound
        .intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "urn:dirty",
        )))
        .unwrap();
    txn_bound
        .insert([s2, s2, s2, sparkles_core::id::Id::DEFAULT_GRAPH])
        .unwrap();
    assert!(matches!(
        txn_bound.prepare_read_view(&prep),
        Err(Error::Invalid(ref m)) if m.contains("different snapshot")
    ));
    txn_bound.discard_terms().unwrap();
}

#[test]
fn bound_evidence_view_prepare_view_retains_credit_and_refuses_stale_bound_policy() {
    let store = Store::in_memory(StoreOptions::default());
    let init_prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    store
        .activate_memory_configuration_history(&init_prep)
        .unwrap();
    drop(init_prep);
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:visible> 2 . GRAPH <urn:guarded:a> { <urn:x> <urn:p> 1 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    let snap_1 = store.snapshot();
    let bound_at_1 = guarded_policy().bound(&snap_1);
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s2> <urn:visible> 3 }",
        &QueryOptions::default(),
    )
    .unwrap();

    let pool = QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    {
        let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
        let baseline = grant.used();
        let evidence = store.evidence_view(&At::Head, &prep).unwrap();
        let after_evidence = grant.used();
        assert!(after_evidence > baseline);

        // Stale bound policy from commit 1 is refused against commit 2's evidence view.
        assert!(matches!(
            evidence.prepare_view(bound_at_1.clone(), &prep),
            Err(Error::Invalid(ref m)) if m.contains("different snapshot")
        ));

        // Preparing a hiding policy retains BoundEvidenceView's reservation even after
        // the original BoundEvidenceView handle is dropped.
        let policy = hiding_policy();
        let prepared = evidence.prepare_view(policy.clone(), &prep).unwrap();
        assert!(prepared.has_mask());
        let after_prepared = grant.used();
        assert!(after_prepared > after_evidence);
        drop(evidence);
        assert_eq!(
            grant.used(),
            after_prepared,
            "PreparedView must retain BoundEvidenceView's reservation until dropped"
        );

        // Wrapped evidence view must still refuse a foreign Store in query_source
        // and OutputBudget::receiving_source.
        let foreign = Store::in_memory(StoreOptions::default());
        assert!(
            query_source(
                prepared.read_source(),
                &foreign,
                "SELECT ?p WHERE { <urn:s> ?p ?o }",
                &QueryOptions {
                    grant: Some(grant.clone()),
                    ..Default::default()
                },
            )
            .is_err(),
            "wrapped evidence view must refuse a foreign Store in query_source"
        );
        assert!(
            sparkles_core::sparql::output::OutputBudget::receiving_source(
                prepared.read_source(),
                &foreign,
                &QueryOptions {
                    grant: Some(grant.clone()),
                    ..Default::default()
                },
            )
            .is_err(),
            "wrapped evidence view must refuse a foreign Store in OutputBudget::receiving_source"
        );

        // Strict cursor execution must accept the original policy Arc on the same Store
        // and refuse a replacement policy Arc.
        let mut cursor = select_source_cursor(
            prepared.read_source(),
            &store,
            "SELECT ?p WHERE { <urn:s> ?p ?o }",
            &QueryOptions {
                graphs: Some(policy.clone()),
                grant: Some(grant.clone()),
                ..Default::default()
            },
            &CursorOptions::default(),
        )
        .unwrap();
        let batch = cursor.next_batch().unwrap().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(
            batch.term(0, 0).unwrap().unwrap().as_term().to_string(),
            "<urn:visible>"
        );
        drop(batch);
        drop(cursor);
        assert!(matches!(
            select_source_cursor(
                prepared.read_source(),
                &store,
                "SELECT ?p WHERE { <urn:s> ?p ?o }",
                &QueryOptions {
                    graphs: Some(Arc::new(GraphAccess::all())),
                    grant: Some(grant.clone()),
                    ..Default::default()
                },
                &CursorOptions::default(),
            ),
            Err(Error::NotPermitted(ref m)) if m.contains("replace")
        ));

        let res = query_source(
            prepared.read_source(),
            &store,
            "SELECT ?p WHERE { <urn:s> ?p ?o }",
            &QueryOptions {
                grant: Some(grant.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let rows = res.rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows.iter().next().unwrap().values()[0]
                .as_ref()
                .unwrap()
                .to_string(),
            "<urn:visible>"
        );
        drop(rows);
        drop(res);
        drop(prepared);
        assert_eq!(grant.used(), baseline);
    }
    assert_eq!(grant.used(), 0);
}

#[test]
fn controlled_guard_callback_uses_scoped_guard_snapshot_on_selected_and_evidence_paths() {
    struct LookupGuard;
    impl GuardDecision for LookupGuard {
        fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
            panic!("controlled paths must not invoke legacy exempt")
        }
        fn exempt_controlled(
            &self,
            snap: &GuardSnapshot<'_>,
            preparation: &Preparation,
        ) -> Result<Exemptions> {
            let mut out = Exemptions::new(preparation)?;
            assert!(snap.content_key().is_some());
            assert!(!snap.has_mask());
            if let Some(p) = snap.lookup_iri(preparation, "urn:allow")? {
                let mut found = false;
                snap.scan(preparation, sparkles_core::index::Perm::Pso, &[p.0], |_| {
                    found = true;
                    Ok(false)
                })?;
                if found {
                    out.push("urn:guarded:ok")?;
                }
            }
            Ok(out)
        }
    }

    let store = Store::in_memory(StoreOptions::default());
    let init_prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    store
        .activate_memory_configuration_history(&init_prep)
        .unwrap();
    drop(init_prep);
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:allow> 1 . GRAPH <urn:guarded:ok> { <urn:a> <urn:p> 1 } }",
        &QueryOptions::default(),
    )
    .unwrap();

    let policy = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by("urn:guarded:*", Decider(Arc::new(LookupGuard))),
        Graphs::none(),
    ));
    let pool = QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    {
        let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
        let baseline = grant.used();

        let selected = store.prepare_read(policy.clone(), &prep).unwrap();
        assert!(selected.allows_read_iri("urn:guarded:ok"));
        drop(selected);
        assert_eq!(grant.used(), baseline);

        let evidence = store.evidence_view(&At::Head, &prep).unwrap();
        let prepared = evidence.prepare_view(policy.clone(), &prep).unwrap();
        assert!(prepared.allows_read_iri("urn:guarded:ok"));
        drop(evidence);
        drop(prepared);
        assert_eq!(grant.used(), baseline);
    }
    assert_eq!(grant.used(), 0);
}
