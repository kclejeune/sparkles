//! A required warning admission must never fail after publishing the transaction.
use super::*;
use sparkles_budget::{MemoryDomain, ParentRefused, Scope};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

fn fixture() -> (Store, Cache) {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            b"<urn:a> <urn:p> <urn:b> .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let cache = Cache::default();
    materialize_incremental(
        &store,
        &Profile::Rules("[seed: (?s <urn:p> ?o) -> (?s <urn:q> ?o)]".into()),
        &Extras::default(),
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &ReasonOptions {
            threads: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        cache.kept().is_some(),
        "the control starts with a real prior publication"
    );
    (store, cache)
}

fn options(counter: &Arc<AtomicUsize>) -> ReasonOptions {
    let counter = counter.clone();
    ReasonOptions {
        threads: Some(1),
        progress: Some(Arc::new(move |_, message| {
            if message == "done" {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        })),
        ..Default::default()
    }
}

fn invoke_finish(
    store: &Store,
    cache: Option<&Cache>,
    budget: &Arc<Budget>,
    warnings: WorkingWarnings,
    resolved_warnings: Vec<String>,
    opts: &ReasonOptions,
) -> anyhow::Result<ReasonReport> {
    let mut txn = store.write();
    let snap = txn.base().clone();
    let mut resolved = inputs::resolve(&snap, &Inputs::default());
    resolved.warnings = resolved_warnings;
    let mut ids = [Id::UNDEF; 4];
    for (slot, iri) in ids
        .iter_mut()
        .zip(["urn:new", "urn:q", "urn:b", INFERRED_GRAPH])
    {
        *slot = txn
            .intern(&Term::NamedNode(NamedNode::new_unchecked(iri)))
            .unwrap();
    }
    assert!(
        txn.insert(ids).unwrap(),
        "an actual new inference is pending"
    );
    let prior = snap
        .lookup_iri(INFERRED_GRAPH)
        .map_or(0, |g| snap.scan_keys(Perm::Gspo, &[g.0]).unwrap().len());
    let done = Done {
        closure: None,
        witnesses: None,
        budget: budget.clone(),
        base_scans: 0,
        threads: 1,
        generalized: FxHashSet::default(),
        generalized_saved: None,
        written: Written {
            added: 1,
            inferred: prior as u64 + 1,
            ..Default::default()
        },
        rules: 1,
        iterations: 1,
        warnings,
        method: Method::Full,
        fallback: None,
        changes: None,
    };
    finish(
        store,
        txn,
        &snap,
        &Profile::RdfsSimple,
        0,
        &resolved,
        Incremental { since: None, cache },
        opts,
        Instant::now(),
        done,
    )
}

fn assert_unpublished(
    store: &Store,
    before: &Arc<Snapshot>,
    prior_cache: Option<(usize, u64)>,
    cache: &Cache,
    done: &AtomicUsize,
) {
    let after = store.snapshot();
    assert_eq!(
        after.commit, before.commit,
        "warning refusal published transaction"
    );
    assert_eq!(
        after.version, before.version,
        "warning refusal published a catalog version"
    );
    assert!(Arc::ptr_eq(&after.generation, &before.generation));
    assert_eq!(
        after.dvocab_len, before.dvocab_len,
        "uncommitted vocabulary escaped"
    );
    assert_eq!(
        after.scan_keys(Perm::Gspo, &[]).unwrap(),
        before.scan_keys(Perm::Gspo, &[]).unwrap(),
        "prior graph changed"
    );
    assert_eq!(cache.kept(), prior_cache, "prior kept publication changed");
    assert_eq!(
        done.load(Ordering::SeqCst),
        0,
        "success progress preceded warning admission"
    );
}

#[test]
fn local_final_warning_refusal_does_not_publish() {
    let (store, cache) = fixture();
    let before = store.snapshot();
    let prior = cache.kept();
    let budget = Budget::new(Some(512));
    let mut warnings = WorkingWarnings::new(&budget);
    warnings
        .push_fmt(format_args!("{}", "w".repeat(300)))
        .unwrap();
    let first = budget.used();
    assert!(
        first < 512 && first * 2 > 512,
        "first warning owner fits, live second copy cannot"
    );
    let progress = Arc::new(AtomicUsize::new(0));
    let error = invoke_finish(
        &store,
        Some(&cache),
        &budget,
        warnings,
        Vec::new(),
        &options(&progress),
    )
    .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<MemoryLimitExceeded>(), Some(e) if e.limit == 512),
        "original local error: {error:#}"
    );
    assert_unpublished(&store, &before, prior, &cache, &progress);
    assert_eq!(budget.used(), 0);
}

#[test]
fn parent_final_warning_refusal_does_not_publish_or_release_another_owner() {
    let (store, cache) = fixture();
    let before = store.snapshot();
    let prior = cache.kept();
    let reserved = Arc::new(AtomicU64::new(0));
    let refused = Arc::new(AtomicU64::new(0));
    let take = reserved.clone();
    let give = reserved.clone();
    let last = refused.clone();
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            let old = take.load(Ordering::SeqCst);
            let next = old.checked_add(n).ok_or(ParentRefused::Overflow)?;
            if next > 600 {
                last.store(next, Ordering::SeqCst);
                return Err(ParentRefused::Limit {
                    limit: 600,
                    requested: next,
                    scope: Scope::Work,
                });
            }
            assert_eq!(
                take.compare_exchange(old, next, Ordering::SeqCst, Ordering::SeqCst),
                Ok(old)
            );
            Ok(())
        },
        move |n| {
            let prior = give.fetch_sub(n, Ordering::SeqCst);
            assert!(prior >= n);
        },
    );
    let unrelated = parent.clone().reserve(37).unwrap();
    let budget = Budget::with_parent(None, Some(parent));
    let mut warnings = WorkingWarnings::new(&budget);
    warnings
        .push_fmt(format_args!("{}", "w".repeat(300)))
        .unwrap();
    let first = budget.used();
    assert!(
        first + 37 < 600 && first * 2 + 37 > 600,
        "only the second live warning copy is refused"
    );
    let progress = Arc::new(AtomicUsize::new(0));
    let error = invoke_finish(
        &store,
        Some(&cache),
        &budget,
        warnings,
        Vec::new(),
        &options(&progress),
    )
    .unwrap_err();
    assert!(error.downcast_ref::<MemoryLimitExceeded>().is_none());
    assert!(
        matches!(error.downcast_ref::<sparkles_core::Error>(), Some(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::ServerMemory && b.limit == 600 && b.requested == refused.load(Ordering::SeqCst)),
        "original parent error: {error:#}"
    );
    assert_unpublished(&store, &before, prior, &cache, &progress);
    assert_eq!(budget.used(), 0);
    drop(budget);
    assert_eq!(reserved.load(Ordering::SeqCst), 37);
    drop(unrelated);
    assert_eq!(reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn public_inspect_warning_refusal_does_not_publish() {
    let (store, cache) = fixture();
    let before = store.snapshot();
    let prior = cache.kept();
    let progress = Arc::new(AtomicUsize::new(0));
    let mut opts = options(&progress);
    opts.max_memory = Some(8 << 20);
    opts.inspect = Some(Arc::new(|_| Ok(vec!["w".repeat(5 << 20)])));
    let error = materialize_incremental(
        &store,
        &Profile::Rules("[changed: (?s <urn:p> ?o) -> (?s <urn:newq> ?o)]".into()),
        &Extras::default(),
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &opts,
    )
    .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<MemoryLimitExceeded>(), Some(e) if e.limit == 8 << 20),
        "original public local error: {error:#}"
    );
    assert_unpublished(&store, &before, prior, &cache, &progress);
}

#[test]
fn successful_final_warnings_preserve_order_and_commit_once() {
    let (store, _cache) = fixture();
    let before = store.snapshot();
    let budget = Budget::unlimited();
    let mut warnings = WorkingWarnings::new(&budget);
    warnings.push_fmt(format_args!("done-first")).unwrap();
    warnings.push_fmt(format_args!("done-second")).unwrap();
    let progress = Arc::new(AtomicUsize::new(0));
    let report = invoke_finish(
        &store,
        None,
        &budget,
        warnings,
        vec!["resolved-first".into(), "resolved-second".into()],
        &options(&progress),
    )
    .unwrap();
    assert_eq!(
        report.warnings,
        [
            "resolved-first",
            "resolved-second",
            "done-first",
            "done-second"
        ]
    );
    let receipt = report.receipt.unwrap();
    assert!(receipt.committed);
    assert_eq!(receipt.commit.seq, before.commit + 1);
    assert_eq!(
        store.snapshot().scan_keys(Perm::Gspo, &[]).unwrap().len(),
        before.scan_keys(Perm::Gspo, &[]).unwrap().len() + 1
    );
    assert_eq!(progress.load(Ordering::SeqCst), 1);
    assert_eq!(budget.used(), 0);
}

#[test]
fn absent_empty_and_multiple_final_warnings_preserve_success() {
    let cases = [
        (Vec::<&str>::new(), Vec::<&str>::new(), Vec::<&str>::new()),
        (vec![""], vec![""], vec!["", ""]),
        (
            vec!["resolved"],
            vec!["", "body", ""],
            vec!["resolved", "", "body", ""],
        ),
    ];
    for (resolved, body, expected) in cases {
        let (store, _cache) = fixture();
        let before = store.snapshot();
        let budget = Budget::unlimited();
        let mut warnings = WorkingWarnings::new(&budget);
        for warning in body {
            warnings.push_fmt(format_args!("{warning}")).unwrap();
        }
        let progress = Arc::new(AtomicUsize::new(0));
        let report = invoke_finish(
            &store,
            None,
            &budget,
            warnings,
            resolved.into_iter().map(str::to_owned).collect(),
            &options(&progress),
        )
        .unwrap();
        assert_eq!(report.warnings, expected);
        let receipt = report.receipt.unwrap();
        assert!(receipt.committed);
        assert_eq!(receipt.commit.seq, before.commit + 1);
        assert_eq!(progress.load(Ordering::SeqCst), 1);
        assert_eq!(budget.used(), 0);
    }
}
