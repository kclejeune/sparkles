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

#[test]
fn actual_infallible_interning_keeps_typed_parent_refusal_sticky() {
    use sparkles_budget::{MemoryDomain, ParentRefused};
    let store = Store::in_memory(StoreOptions::default());
    let parent = MemoryDomain::unpooled().parent(
        |bytes| {
            if bytes == 0 {
                Ok(())
            } else {
                Err(ParentRefused::Overflow)
            }
        },
        |_| {},
    );
    let mut ctx = Ctx::new(store.snapshot());
    ctx.set_memory_parent(parent).unwrap();
    let ctx = Arc::new(ctx);
    assert!(ctx.intern_key(b"<urn:sticky-refusal>").is_undef());
    let error = ctx
        .check()
        .expect_err("resource refusal must survive actual infallible interning");
    fn origin(error: Error) {
        let Error::Component(component) = error else {
            panic!("lost typed parent: {error:?}")
        };
        assert_eq!(
            (component.component, component.code.as_str()),
            ("query-memory", "ownership")
        );
        assert_eq!(
            component
                .source
                .as_ref()
                .unwrap()
                .downcast_ref::<ParentRefused>(),
            Some(&ParentRefused::Overflow)
        );
    }
    origin(error);
    ctx.fail_memory(&Error::BudgetExceeded(crate::Budget {
        kind: BudgetKind::Memory,
        limit: 1,
        requested: 2,
    }));
    ctx.fail_memory(&Error::Cancelled);
    origin(ctx.check().unwrap_err());
    let output = OutputBudget::new(ctx.clone());
    origin(output.decode(Id::UNDEF).err().unwrap());
    assert_eq!(ctx.live_bytes(), 0);
}

#[test]
fn sticky_resource_outcomes_preserve_first_type_and_ignore_nonresource_errors() {
    for timeout in [false, true] {
        let budget = budget(4096);
        budget
            .ctx
            .fail_memory(&Error::Service("ordinary scalar failure".into()));
        budget
            .ctx
            .fail_memory(&Error::Invalid("ordinary invalid value".into()));
        budget.ctx.check().unwrap();
        assert!(budget.decode(Id::UNDEF).unwrap().is_none());
        let first = if timeout {
            Error::Timeout
        } else {
            Error::Cancelled
        };
        budget.ctx.fail_memory(&first);
        budget
            .ctx
            .fail_memory(&Error::BudgetExceeded(crate::Budget {
                kind: BudgetKind::Memory,
                limit: 1,
                requested: 2,
            }));
        for _ in 0..2 {
            match budget.ctx.check().unwrap_err() {
                Error::Timeout if timeout => (),
                Error::Cancelled if !timeout => (),
                error => panic!("lost original control: {error:?}"),
            }
        }
    }
    let budget = budget(4096);
    let expected = crate::Budget {
        kind: BudgetKind::Memory,
        limit: 7,
        requested: 9,
    };
    budget.ctx.fail_memory(&Error::BudgetExceeded(expected));
    budget.ctx.fail_memory(&Error::Timeout);
    assert!(matches!(budget.ctx.check(), Err(Error::BudgetExceeded(found)) if found == expected));
}

#[test]
fn cancelled_receiving_copy_refuses_even_an_unbound_cell() {
    let budget = budget(4096);
    budget
        .ctx
        .cancel
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        matches!(budget.decode(Id::UNDEF), Err(Error::Cancelled)),
        "a cancelled receiving budget must refuse before allocating even UNDEF output"
    );
    assert_eq!(budget.ctx.live_bytes(), 0);
}

#[test]
fn receiving_container_admission_precedes_allocation_and_refusal_retires_partial_state() {
    use crate::algo::memory::probe;
    use std::{cell::Cell, rc::Rc};
    let bytes = (4 * std::mem::size_of::<Option<Term>>()
        + std::mem::size_of::<RowsBacking>()
        + 2 * std::mem::size_of::<usize>()) as u64;
    for reason in ["budget", "cancel", "deadline", "allocation", "ok"] {
        let mut receiver = budget(bytes - u64::from(reason == "budget"));
        if reason == "cancel" {
            receiver
                .ctx
                .cancel
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if reason == "deadline" {
            Arc::get_mut(&mut receiver.ctx).unwrap().deadline = Some(std::time::Instant::now());
        }
        let allocations = Rc::new(Cell::new(0));
        let seen = allocations.clone();
        let owner = receiver.ctx.clone();
        let hook = probe::install(
            move |point| {
                if let probe::Point::Allocation("owned-output-container", 4) = point {
                    seen.set(seen.get() + 1);
                    assert_eq!(
                        owner.live_bytes(),
                        bytes,
                        "entire fixed container and Arc header are prepaid"
                    );
                }
            },
            (reason == "allocation").then_some("owned-output-container"),
        );
        let result = receiver.decode_rows(2, 2, |_, _| Id::UNDEF);
        match reason {
            "budget" => assert!(matches!(result, Err(Error::BudgetExceeded(_)))),
            "cancel" => assert!(matches!(result, Err(Error::Cancelled))),
            "deadline" => assert!(matches!(result, Err(Error::Timeout))),
            "allocation" => assert!(matches!(result, Err(Error::Invalid(_)))),
            _ => {
                let rows = result.unwrap();
                assert_eq!((rows.len(), rows.width()), (2, 2));
                assert_eq!(receiver.ctx.live_bytes(), bytes);
                drop(rows);
            }
        }
        assert_eq!(
            allocations.get(),
            usize::from(matches!(reason, "allocation" | "ok"))
        );
        assert_eq!(receiver.ctx.live_bytes(), 0);
        drop(hook);
    }
}

#[test]
fn receiving_copy_checks_controls_during_unbound_work_and_after_last_cell() {
    use std::cell::Cell;
    let receiver = budget(1 << 20);
    let calls = Cell::new(0usize);
    let result = receiver.build(2048, 1, |_, _, _| {
        calls.set(calls.get() + 1);
        receiver
            .ctx
            .cancel
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(None)
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(calls.get(), 1024);
    assert_eq!(receiver.ctx.live_bytes(), 0);
    receiver
        .ctx
        .cancel
        .store(false, std::sync::atomic::Ordering::Release);
    let result = receiver.build(1, 1, |_, _, _| {
        receiver
            .ctx
            .cancel
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(None)
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(receiver.ctx.live_bytes(), 0);
}

#[test]
fn late_receiving_refusal_drops_constructed_terms_and_zero_width_needs_no_loop() {
    let receiver = budget(4096);
    let result = receiver.build(2, 1, |row, _, credit| {
        if row == 1 {
            return Err(Error::Cancelled);
        }
        credit.add(64)?;
        Ok(Some(Term::Literal(oxrdf::Literal::new_simple_literal(
            "partial",
        ))))
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(receiver.ctx.live_bytes(), 0);
    let rows = receiver
        .decode_rows(usize::MAX, 0, |_, _| unreachable!())
        .unwrap();
    assert_eq!((rows.len(), rows.width()), (usize::MAX, 0));
    drop(rows);
    assert_eq!(receiver.ctx.live_bytes(), 0);
}
