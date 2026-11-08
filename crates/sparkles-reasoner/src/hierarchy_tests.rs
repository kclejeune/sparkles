//! Behavioral preimages for item 675. Initially these exercise the independent
//! numeric budgets; the implementation attaches their actual owning parents.
use crate::budget::{Budget, BudgetFailure};
use crate::graph::Graph;
use crate::incremental::Closure;
use crate::terms::Terms;
use sparkles_core::sparql::{Ctx, pool::QueryPool};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;

const MIB: usize = 1 << 20;

#[test]
fn executor_and_reasoner_share_the_query_local_limit() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = (8 * MIB) as u64;
    let executor = ctx.charge((5 * MIB) as u64).unwrap();
    let reasoning = Budget::with_parent(Some(8 * MIB), Some(ctx.memory_parent().unwrap()));
    let result = reasoning.try_charge(5 * MIB);
    assert!(
        result.is_err(),
        "executor5MiB plus reasoning5MiB both admitted under local8MiB; reasoning used {}",
        reasoning.used()
    );
    assert!(matches!(
        result,
        Err(BudgetFailure::Parent {
            refusal: sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Local,
                ..
            },
            ..
        })
    ));
    assert_eq!(reasoning.used(), 0);
    drop(executor);
}

#[test]
fn reasoner_parent_refusal_is_server_memory_before_allocation() {
    let pool = QueryPool::new((8 * MIB) as u64);
    let executor = pool.empty_grant();
    executor.charge((5 * MIB) as u64).unwrap();
    // Local 16 MiB permits the second 5 MiB; only aggregate Q=8 MiB can refuse.
    let reasoning = Budget::with_parent(Some(16 * MIB), Some(pool.work_parent()));
    let before = (executor.used(), executor.granted(), pool.granted());
    let result = reasoning.try_charge(5 * MIB);
    assert!(
        result.is_err(),
        "independent reasoning admitted outside Q8MiB; original grant usage/reservation {:?}",
        before
    );
    assert!(matches!(
        result,
        Err(BudgetFailure::Parent {
            refusal: sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Work,
                ..
            },
            ..
        })
    ));
    assert_eq!(reasoning.used(), 0);
    assert_eq!(
        (executor.used(), executor.granted(), pool.granted()),
        before
    );
    drop(executor);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn failed_closure_budget_migration_preserves_all_old_owners() {
    let store = Store::in_memory(StoreOptions::default());
    let old = Budget::unlimited();
    let mut graph = Graph::with_capacity(4);
    graph.set_budget(&old).unwrap();
    let terms = Terms::new(store.snapshot());
    terms.set_budget(&old).unwrap();
    terms
        .try_id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
            "x".repeat(128 << 10),
        )))
        .unwrap();
    let mut closure = Closure::new(graph, terms).unwrap();
    let own = closure.heap_bytes() - closure.graph.heap_bytes() - closure.terms.heap_bytes();
    let destination = Budget::new(Some(own + closure.graph.heap_bytes()));
    let before = (old.used(), destination.used());
    assert!(closure.set_budget(&destination).is_err());
    assert!(
        Arc::ptr_eq(closure.graph.budget(), &old),
        "refused migration already changed graph owner"
    );
    assert!(Arc::ptr_eq(&closure.terms.budget(), &old));
    assert_eq!((old.used(), destination.used()), before);
}

#[test]
fn initial_term_parent_refusal_leaves_no_live_credit() {
    use sparkles_budget::{MemoryDomain, ParentRefused, Scope};
    let store = Store::in_memory(StoreOptions::default());
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        |requested| {
            Err(ParentRefused::Limit {
                limit: 0,
                requested,
                scope: Scope::Work,
            })
        },
        |_| panic!("refusal must not release unowned credit"),
    );
    let budget = Budget::with_parent(None, Some(parent));
    let error: anyhow::Error = Terms::new_in(store.snapshot(), &budget)
        .err()
        .unwrap()
        .into();
    assert!(matches!(
        error.downcast_ref::<sparkles_core::Error>(),
        Some(sparkles_core::Error::BudgetExceeded(
            sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            }
        ))
    ));
    assert_eq!((budget.used(), budget.peak()), (0, 0));
}

#[test]
fn term_payloads_drop_before_parent_credit() {
    use sparkles_budget::MemoryDomain;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let store = Store::in_memory(StoreOptions::default());
    let snap = Arc::new((*store.snapshot()).clone());
    let weak = Arc::downgrade(&snap);
    let retiring = Arc::new(AtomicBool::new(false));
    let check = retiring.clone();
    let owned = Arc::new(AtomicU64::new(0));
    let take = owned.clone();
    let give = owned.clone();
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            if check.load(Ordering::SeqCst) {
                assert!(
                    weak.upgrade().is_none(),
                    "term owner returned credit before payload fields dropped"
                );
            }
            give.fetch_sub(n, Ordering::SeqCst);
        },
    );
    let budget = Budget::with_parent(None, Some(parent));
    let terms = Terms::new_in(snap, &budget).unwrap();
    terms
        .try_id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
            "x".repeat(2 << 20),
        )))
        .unwrap();
    retiring.store(true, Ordering::SeqCst);
    drop(terms);
    assert_eq!(budget.used(), 0);
    drop(budget);
    assert_eq!(owned.load(Ordering::SeqCst), 0);
}
