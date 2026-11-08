use super::*;
use crate::error::BudgetKind;
use crate::sparql::pool::QueryPool;
use crate::sparql::query_memory::LEDGER_OPS;
use crate::store::{Store, StoreOptions};

fn budget(bytes: u64) -> OutputBudget {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = bytes;
    OutputBudget::new(Arc::new(ctx))
}

fn literal(budget: &OutputBudget, len: usize) -> Id {
    let id = budget
        .ctx
        .intern_term(&Term::Literal(oxrdf::Literal::new_simple_literal(
            "x".repeat(len),
        )));
    assert!(!id.is_undef(), "fixture term fits before output admission");
    budget.ctx.check_memory_failure().unwrap();
    id
}

#[test]
fn last_cell_keeps_payload_context_and_real_credit_after_result_drop() {
    let budget = budget(1 << 20);
    let id = literal(&budget, 64 << 10);
    let base = budget.ctx.live_bytes();
    let ctx = Arc::downgrade(&budget.ctx);
    let term = budget.decode(id).unwrap().unwrap();
    let owned = budget.ctx.live_bytes();
    assert!(owned > base + (64 << 10));
    let last = term.clone();
    drop(term);
    drop(budget);
    assert_eq!(ctx.upgrade().unwrap().live_bytes(), owned);
    assert!(matches!(last.as_term(), Term::Literal(value) if value.value().len() == 64 << 10));
    drop(last);
    assert!(
        ctx.upgrade().is_none(),
        "last physical owner releases the context"
    );
}

#[test]
fn copies_pay_destination_overlap_and_refusal_changes_neither_owner() {
    let source = budget(1 << 20);
    let term = source.decode(literal(&source, 16 << 10)).unwrap().unwrap();
    let receiver = budget(40 << 10);
    let first = term.try_copy_in(&receiver).unwrap();
    let second = term.try_copy_in(&receiver).unwrap();
    let before = receiver.ctx.live_bytes();
    let source_before = source.ctx.live_bytes();
    assert!(
        matches!(term.try_copy_in(&receiver), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
    );
    assert_eq!(receiver.ctx.live_bytes(), before);
    assert_eq!(source.ctx.live_bytes(), source_before);
    assert_eq!(first.as_term(), second.as_term());
    drop(first);
    let third = term.try_copy_in(&receiver).unwrap();
    drop(second);
    drop(third);
    assert_eq!(receiver.ctx.live_bytes(), 0);
}

#[test]
fn receiving_pool_refusal_is_server_memory_and_returns_partial_container() {
    let source = budget(1 << 20);
    let term = source.decode(literal(&source, 32 << 10)).unwrap().unwrap();
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(16 << 10);
    let grant = Arc::new(pool.empty_grant());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = 1 << 20;
    ctx.set_grant(Some(grant));
    let receiver = OutputBudget::new(Arc::new(ctx));
    assert!(
        matches!(term.try_copy_in(&receiver), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory)
    );
    assert_eq!(receiver.ctx.live_bytes(), 0);
    drop(receiver);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn eager_output_stays_fast_and_promotion_retains_the_original_account() {
    let budget = budget(1 << 20);
    let id = literal(&budget, 4096);
    LEDGER_OPS.with(|n| n.set(0));
    let before = budget.ctx.live_bytes();
    let term = budget.decode(id).unwrap().unwrap();
    assert_eq!(LEDGER_OPS.with(|n| n.get()), 0);
    assert!(budget.ctx.live_bytes() > before);
    let _child = budget.ctx.memory_parent().unwrap();
    drop(term);
    assert_eq!(
        budget.ctx.live_bytes(),
        before,
        "paid fast output follows promotion"
    );
}

#[test]
fn zero_width_rows_and_unbound_cells_remain_distinct() {
    let budget = budget(1 << 20);
    let zero = budget.decode_rows(3, 0, |_, _| unreachable!()).unwrap();
    assert_eq!((zero.len(), zero.width()), (3, 0));
    assert!(zero.row(2).unwrap().is_empty());
    assert!(zero.row(3).is_err());
    let unbound = budget.decode_rows(3, 2, |_, _| Id::UNDEF).unwrap();
    assert_eq!((unbound.len(), unbound.width()), (3, 2));
    assert!(unbound.iter().all(|row| row.values() == [None, None]));
}

#[test]
fn borrowed_coordinates_distinguish_unbound_from_invalid() {
    let budget = budget(1 << 20);
    let id = literal(&budget, 32);
    let rows = budget
        .decode_rows(2, 2, |r, c| if r == c { id } else { Id::UNDEF })
        .unwrap();
    let row = rows.row(0).unwrap();
    assert!(row.get(0).unwrap().is_some());
    assert!(row.get(1).unwrap().is_none());
    assert!(row.get(2).is_err());
    assert_eq!(row.iter().count(), 2);
    let handle = rows.row_handle(1).unwrap();
    assert!(handle.term(0).unwrap().is_none());
    assert_eq!(
        handle.term(1).unwrap().unwrap().as_term(),
        rows.row_handle(0)
            .unwrap()
            .term(0)
            .unwrap()
            .unwrap()
            .as_term()
    );
    assert!(handle.term(2).is_err());
    assert!(rows.row_handle(2).is_err());
}

#[test]
fn later_cell_refusal_returns_partial_terms_before_credit() {
    let source = budget(1 << 20);
    let id = literal(&source, 16 << 10);
    let rows = source.decode_rows(1, 3, |_, _| id).unwrap();
    // One cell and its container fit. The second cell reaches a real admission
    // refusal after the first owned term has been constructed.
    let receiver = budget(24 << 10);
    let single = rows.row_handle(0).unwrap().term(0).unwrap().unwrap();
    let positive = single.try_copy_in(&receiver).unwrap();
    drop(positive);
    assert_eq!(receiver.ctx.live_bytes(), 0);
    let before = source.ctx.live_bytes();
    assert!(
        matches!(rows.try_copy_in(&receiver), Err(Error::BudgetExceeded(b))
        if b.kind == BudgetKind::Memory && b.limit == 24 << 10)
    );
    assert_eq!(receiver.ctx.live_bytes(), 0);
    assert_eq!(source.ctx.live_bytes(), before);
    assert_eq!(
        single.as_term(),
        rows.row_handle(0)
            .unwrap()
            .term(2)
            .unwrap()
            .unwrap()
            .as_term()
    );
}

#[test]
fn retained_row_and_cell_share_one_paid_backing_until_last_thread_owner() {
    let budget = budget(1 << 20);
    let id = literal(&budget, 4096);
    let rows = budget.decode_rows(8, 2, |_, _| id).unwrap();
    let base = budget.ctx.live_bytes();
    let cell = rows.row_handle(3).unwrap().term(1).unwrap().unwrap();
    assert_eq!(
        budget.ctx.live_bytes(),
        base,
        "views allocate no decoded payload"
    );
    let weak = Arc::downgrade(&rows.inner);
    let ctx = Arc::downgrade(&budget.ctx);
    let ready = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let worker = {
        let ready = ready.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            ready.wait();
            assert!(matches!(cell.as_term(), Term::Literal(t) if t.value().len() == 4096));
            release.wait();
            drop(cell);
        })
    };
    ready.wait();
    drop(rows);
    drop(budget);
    assert!(weak.upgrade().is_some());
    assert_eq!(ctx.upgrade().unwrap().live_bytes(), base);
    release.wait();
    worker.join().unwrap();
    assert!(weak.upgrade().is_none());
    assert!(ctx.upgrade().is_none());
}

#[test]
fn impossible_container_refuses_without_decoding() {
    let budget = budget(1 << 20);
    let decoded = std::cell::Cell::new(false);
    let result = budget.decode_rows(usize::MAX, 2, |_, _| {
        decoded.set(true);
        Id::UNDEF
    });
    assert!(matches!(result, Err(Error::BudgetExceeded(b))
        if b.kind == BudgetKind::Memory && b.requested == u64::MAX));
    assert!(!decoded.get());
    assert_eq!(budget.ctx.live_bytes(), 0);
}
