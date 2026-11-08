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

#[test]
fn relabelled_graph_scratch_retains_nested_payload_and_actual_credit() {
    let source = oxrdf::Triple::new(
        oxrdf::BlankNode::new_unchecked("root"),
        oxrdf::NamedNode::new_unchecked("urn:p"),
        Term::Triple(Box::new(oxrdf::Triple::new(
            oxrdf::BlankNode::new_unchecked("inner"),
            oxrdf::NamedNode::new_unchecked("urn:q"),
            oxrdf::Literal::new_simple_literal("x".repeat(64 << 10)),
        ))),
    );
    let budget = budget(1 << 20);
    let context = Arc::downgrade(&budget.ctx);
    let value = budget.relabel_graph_triple(&source, 2, 3, 4).unwrap();
    let held = budget.ctx.live_bytes();
    assert!(held >= 64 << 10);
    let clone = value.clone();
    assert_eq!(budget.ctx.live_bytes(), held);
    let Term::Triple(outer) = clone.as_term() else {
        panic!("triple payload");
    };
    assert_eq!(outer.subject.to_string(), "_:t2b3q4xroot");
    assert_eq!(outer.predicate.as_str(), "urn:p");
    let Term::Triple(inner) = &outer.object else {
        panic!("nested triple payload");
    };
    assert_eq!(inner.subject.to_string(), "_:t2b3q4xinner");
    assert_eq!(inner.predicate.as_str(), "urn:q");
    assert!(matches!(&inner.object, Term::Literal(text) if text.value().len() == 64 << 10));
    assert_eq!(source.subject.to_string(), "_:root");
    drop(value);
    drop(budget);
    assert_eq!(context.upgrade().unwrap().live_bytes(), held);
    drop(clone);
    assert!(context.upgrade().is_none());
}

#[test]
fn relabelled_graph_scratch_refuses_before_allocation_and_cancels_without_output() {
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let source = oxrdf::Triple::new(
        oxrdf::BlankNode::new_unchecked("root"),
        oxrdf::NamedNode::new_unchecked("urn:p"),
        oxrdf::Literal::new_simple_literal("x".repeat(64 << 10)),
    );
    let receiver = budget(8192);
    let allocations = Arc::new(AtomicUsize::new(0));
    let observed = allocations.clone();
    let hook = probe::install(
        move |point| {
            if let probe::Point::Allocation("relabelled-graph-scratch", 1) = point {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        },
        None,
    );
    assert!(matches!(receiver.relabel_graph_triple(&source, 2, 3, 4),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory));
    drop(hook);
    assert_eq!(allocations.load(Ordering::Relaxed), 0);
    assert_eq!(receiver.ctx.live_bytes(), 0);
    assert_eq!(source.subject.to_string(), "_:root");
    for cancelled in [false, true] {
        let receiver = budget(1 << 20);
        let flag = receiver.ctx.cancel.clone();
        let hook = probe::install(
            move |point| {
                if cancelled && let probe::Point::Allocation("relabelled-graph-scratch", 1) = point
                {
                    flag.store(true, Ordering::Relaxed);
                }
            },
            (!cancelled).then_some("relabelled-graph-scratch"),
        );
        let result = receiver.relabel_graph_triple(&source, 2, 3, 4);
        drop(hook);
        if cancelled {
            assert!(matches!(result, Err(Error::Cancelled)));
        } else {
            assert!(matches!(result, Err(Error::Invalid(_))));
        }
        assert_eq!(receiver.ctx.live_bytes(), 0);
    }
}

#[test]
fn owned_text_prepays_utf8_and_keeps_original_fast_owner_until_last_clone() {
    let source = budget(1 << 20);
    let weak = Arc::downgrade(&source.ctx);
    let text = "é".repeat(32 << 10);
    LEDGER_OPS.with(|n| n.set(0));
    let value = source.text(&text).unwrap();
    assert_eq!(value.as_str(), text);
    assert!(source.ctx.live_bytes() >= text.len() as u64);
    assert_eq!(LEDGER_OPS.with(|n| n.get()), 0);
    let last = value.clone();
    drop(value);
    drop(source);
    assert_eq!(last.as_str(), text);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
}

#[test]
fn owned_text_refuses_before_copy_and_preserves_source_on_destination_failure() {
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let source = budget(1 << 20);
    let original = source.text(&"x".repeat(64 << 10)).unwrap();
    let live = source.ctx.live_bytes();
    let destination = budget(8192);
    let allocations = Arc::new(AtomicUsize::new(0));
    let observed = allocations.clone();
    let hook = probe::install(
        move |point| {
            if let probe::Point::Allocation("owned-output-text", 1) = point {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        },
        None,
    );
    assert!(
        matches!(original.try_copy_in(&destination),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory)
    );
    drop(hook);
    assert_eq!(allocations.load(Ordering::Relaxed), 0);
    assert_eq!(destination.ctx.live_bytes(), 0);
    assert_eq!(source.ctx.live_bytes(), live);
    for cancel in [false, true] {
        let destination = budget(1 << 20);
        let flag = destination.ctx.cancel.clone();
        let hook = probe::install(
            move |point| {
                if cancel && let probe::Point::Allocation("owned-output-text", 1) = point {
                    flag.store(true, Ordering::Relaxed);
                }
            },
            (!cancel).then_some("owned-output-text"),
        );
        let result = original.try_copy_in(&destination);
        drop(hook);
        if cancel {
            assert!(matches!(result, Err(Error::Cancelled)));
        } else {
            assert!(matches!(result, Err(Error::Invalid(_))));
        }
        assert_eq!(destination.ctx.live_bytes(), 0);
        assert_eq!(source.ctx.live_bytes(), live);
    }
    assert_eq!(original.len(), 64 << 10);
}

#[test]
fn borrowed_term_copy_admits_before_clone_and_retains_its_receiving_owner() {
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let source = Term::Literal(oxrdf::Literal::new_simple_literal("x".repeat(64 << 10)));
    let destination = budget(8192);
    let copies = Arc::new(AtomicUsize::new(0));
    let observed = copies.clone();
    let hook = probe::install(
        move |point| {
            if let probe::Point::Allocation("borrowed-output-term", 1) = point {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        },
        None,
    );
    assert!(
        matches!(destination.copy_term(source.as_ref()),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory)
    );
    drop(hook);
    assert_eq!(copies.load(Ordering::Relaxed), 0);
    assert_eq!(destination.ctx.live_bytes(), 0);
    let destination = budget(1 << 20);
    let weak = Arc::downgrade(&destination.ctx);
    let term = destination.copy_term(source.as_ref()).unwrap();
    assert_eq!(term.as_term(), &source);
    assert!(destination.ctx.live_bytes() >= 64 << 10);
    drop(source);
    drop(destination);
    assert!(matches!(term.as_term(),Term::Literal(l) if l.value().len()==64 << 10));
    drop(term);
    assert!(weak.upgrade().is_none());
}

#[test]
fn receiving_root_headers_refuse_before_allocation_and_faults_refund_real_pool() {
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(16 << 20);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hook = probe::install(
        move |point| {
            if let probe::Point::Allocation("output-root-preparation", 1) = point {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        },
        None,
    );
    assert!(
        matches!(SnapshotOutputRoot::new(store.snapshot(),&super::super::QueryOptions { pool:Some(pool.clone()),max_memory_bytes:Some(512),..Default::default() }),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory && b.limit==512 && b.requested==1024)
    );
    drop(hook);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(pool.granted(), 0);
    for existing in [false, true] {
        let grant = Arc::new(pool.empty_grant());
        let hook = probe::install(|_| {}, Some("output-root-preparation"));
        assert!(matches!(
            SnapshotOutputRoot::new(
                store.snapshot(),
                &super::super::QueryOptions {
                    grant: existing.then(|| grant.clone()),
                    pool: Some(pool.clone()),
                    max_memory_bytes: Some(1 << 20),
                    ..Default::default()
                }
            ),
            Err(Error::Invalid(_))
        ));
        drop(hook);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn receiving_root_queries_and_retained_output_share_actual_aggregate_preparation() {
    let store = Store::in_memory(StoreOptions::default());
    let mut write = store.write();
    let s = write
        .intern(&oxrdf::NamedNode::new_unchecked("urn:s").into())
        .unwrap();
    let p = write
        .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
        .unwrap();
    let o = write
        .intern(&oxrdf::Literal::new_simple_literal("x".repeat(32 << 10)).into())
        .unwrap();
    write.insert([s, p, o, Id::DEFAULT_GRAPH]).unwrap();
    write.commit().unwrap();
    let pool = QueryPool::new(16 << 20);
    let opts = super::super::QueryOptions {
        pool: Some(pool.clone()),
        max_memory_bytes: Some(1 << 20),
        no_cache: true,
        ..Default::default()
    };
    let root = SnapshotOutputRoot::new(store.snapshot(), &opts).unwrap();
    let physical = Arc::downgrade(root.source.query_input().0);
    let parsed =
        super::super::parse_query("SELECT ?o WHERE { <urn:s> <urn:p> ?o }", None, &[]).unwrap();
    let first = root
        .execute(
            &parsed,
            &super::super::QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
    let last = first
        .term(first.table().get(0, 0).unwrap())
        .unwrap()
        .unwrap();
    let used = root
        .budget
        .ctx
        .source_preparation
        .as_ref()
        .unwrap()
        .inner_used();
    let room = root.budget.ctx.source_preparation.as_ref().unwrap().room();
    let pressure = root.budget.reserve(room).unwrap();
    assert!(
        matches!(root.execute(&parsed,&super::super::QueryOptions { no_cache:true,..Default::default() }),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory)
    );
    assert!(matches!(last.as_term(),Term::Literal(l) if l.value().len()==32 << 10));
    drop(pressure);
    assert_eq!(
        root.budget
            .ctx
            .source_preparation
            .as_ref()
            .unwrap()
            .inner_used(),
        used
    );
    let second = root
        .execute(
            &parsed,
            &super::super::QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(second.len(), 1);
    drop(second);
    drop(first);
    drop(store);
    drop(root);
    assert!(physical.upgrade().is_some());
    assert!(pool.granted() > 0);
    assert!(matches!(last.as_term(),Term::Literal(l) if l.value().len()==32 << 10));
    drop(last);
    assert!(physical.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn receiving_root_original_controls_deadline_and_policy_survive_child_options() {
    use crate::access::{GraphAccess, GraphRule, Graphs};
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[crate::io::Source::from_bytes(
            b"<urn:a> { <urn:s> <urn:p> 1 } <urn:hidden> { <urn:h> <urn:p> 2 }".to_vec(),
            crate::io::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let parsed =
        super::super::parse_query("SELECT ?g WHERE { GRAPH ?g { ?s ?p ?o } }", None, &[]).unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let root = SnapshotOutputRoot::new(
        store.snapshot(),
        &super::super::QueryOptions {
            deadline: Some(end),
            graphs: Some(Arc::new(GraphAccess::graphs(
                Graphs::Only(GraphRule::new(["urn:a"], &[])),
                Graphs::none(),
            ))),
            ..Default::default()
        },
    )
    .unwrap();
    let child = root
        .execute(
            &parsed,
            &super::super::QueryOptions {
                timeout: Some(std::time::Duration::from_secs(300)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(child.len(), 1);
    assert_eq!(
        child
            .inner
            .ctx
            .source_preparation
            .as_ref()
            .unwrap()
            .control()
            .deadline,
        Some(end)
    );
    assert!(
        matches!(child.term(child.table().get(0,0).unwrap()).unwrap().unwrap().as_term(),Term::NamedNode(n) if n.as_str()=="urn:a")
    );
    for composed in [false, true] {
        let flag = Arc::new(AtomicBool::new(false));
        let opts = super::super::QueryOptions {
            cancel: (!composed).then(|| flag.clone()),
            cancellation: composed.then(|| crate::task::Cancellation::from_flags([flag.clone()])),
            ..Default::default()
        };
        let root = SnapshotOutputRoot::new(store.snapshot(), &opts).unwrap();
        let mut held = root.budget.reserve(4096).unwrap();
        flag.store(true, Ordering::Relaxed);
        assert!(matches!(
            root.execute(&parsed, &Default::default()),
            Err(Error::Cancelled)
        ));
        assert!(matches!(held.grow_to(8192), Err(Error::Cancelled)));
        assert_eq!(held.bytes(), 4096);
        held.release_to(0).unwrap();
        assert_eq!(held.bytes(), 0);
    }
}

#[test]
fn receiving_reservations_keep_nonempty_overlap_and_refusal_preserves_all_values() {
    struct Buffer {
        data: Vec<u8>,
        held: OutputReservation,
    }
    fn filled(b: &OutputBudget, n: usize, value: u8) -> Buffer {
        let held = b.reserve(n as u64).unwrap();
        let mut data = Vec::new();
        data.try_reserve_exact(n).unwrap();
        assert_eq!(data.capacity(), n);
        data.resize(n, value);
        Buffer { data, held }
    }
    let b = budget(8192);
    LEDGER_OPS.with(|n| n.set(0));
    let source = filled(&b, 1024, 1);
    let destination = filled(&b, 2048, 2);
    let mut replacement = filled(&b, 4096, 3);
    let live = b.ctx.live_bytes();
    assert_eq!(live, 7168);
    assert!(
        matches!(replacement.held.grow_to(8192),Err(Error::BudgetExceeded(e)) if e.kind==BudgetKind::Memory)
    );
    assert_eq!(replacement.held.bytes(), 4096);
    assert_eq!(b.ctx.live_bytes(), live);
    assert!(source.data.iter().all(|v| *v == 1));
    assert!(destination.data.iter().all(|v| *v == 2));
    assert!(replacement.data.iter().all(|v| *v == 3));
    assert_eq!(LEDGER_OPS.with(|n| n.get()), 0);
    drop(destination);
    drop(source);
    drop(replacement);
    assert_eq!(b.ctx.live_bytes(), 0);
}

#[test]
fn receiving_exact_contribution_does_not_consume_or_strand_a_live_siblings_credit() {
    let store = Store::in_memory(StoreOptions::default());
    let root = SnapshotOutputRoot::new(
        store.snapshot(),
        &super::super::QueryOptions {
            max_memory_bytes: Some(256 << 10),
            ..Default::default()
        },
    )
    .unwrap();
    let original = root.budget.ctx.charge(4096).unwrap();
    let sibling = vec![7u8; 4096];
    let preparation = root.budget.ctx.source_preparation.as_ref().unwrap();
    let base = preparation.inner_used();
    let live = root.budget.ctx.live_bytes();
    let mut reservation = root.budget.reserve(8192).unwrap();
    assert_eq!(preparation.inner_used(), base + 8192);
    assert_eq!(root.budget.ctx.live_bytes(), live + 8192);
    reservation.grow_to(12288).unwrap();
    assert_eq!(preparation.inner_used(), base + 12288);
    assert!(matches!(
        reservation.grow_to(256 << 10),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(reservation.bytes(), 12288);
    assert_eq!(preparation.inner_used(), base + 12288);
    reservation.release_to(2048).unwrap();
    assert_eq!(preparation.inner_used(), base + 2048);
    drop(reservation);
    assert_eq!(preparation.inner_used(), base);
    assert_eq!(root.budget.ctx.live_bytes(), live);
    assert!(sibling.iter().all(|b| *b == 7));
    drop(sibling);
    drop(original);
}

#[test]
fn receiving_root_intersects_both_original_control_sets_without_child_renewal() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = Store::in_memory(StoreOptions::default());
    let parsed = super::super::parse_query("ASK {}", None, &[]).unwrap();
    for selected in 0..4 {
        let flags: [Arc<AtomicBool>; 4] = std::array::from_fn(|_| Arc::new(AtomicBool::new(false)));
        let shorter = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let root = SnapshotOutputRoot::new_with_controls(
            store.snapshot(),
            &super::super::QueryOptions {
                cancel: Some(flags[0].clone()),
                cancellation: Some(crate::task::Cancellation::from_flags([flags[1].clone()])),
                timeout: Some(std::time::Duration::from_secs(300)),
                ..Default::default()
            },
            Some(shorter),
            Some(flags[2].clone()),
            Some(crate::task::Cancellation::from_flags([flags[3].clone()])),
        )
        .unwrap();
        assert_eq!(
            root.source.query_input().2.control().deadline,
            Some(shorter)
        );
        assert_eq!(
            root.execute(&parsed, &Default::default()).unwrap().boolean,
            true
        );
        flags[selected].store(true, Ordering::Relaxed);
        assert!(matches!(
            root.term(Id::DEFAULT_GRAPH),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            root.execute(&parsed, &Default::default()),
            Err(Error::Cancelled)
        ));
        assert!(matches!(root.budget().reserve(1), Err(Error::Cancelled)));
    }
}

fn stored_literal_answer(shared: bool) -> super::super::QueryResult {
    use crate::io::Source;
    use oxrdfio::RdfFormat;
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            format!("<urn:s> <urn:p> \"{}\" .", "x".repeat(4096)).into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let result = super::super::query(
        store.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &super::super::QueryOptions {
            max_memory_bytes: Some(8192),
            pool: shared.then(|| QueryPool::new(1 << 20)),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        result.inner.table.cols[0][0].tag(),
        crate::id::Tag::Vocab,
        "the witness must reconstruct a real stored vocabulary key"
    );
    result
}

#[test]
fn stored_simple_literal_moves_one_admitted_buffer_within_original_eight_kib() {
    for shared in [false, true] {
        let result = stored_literal_answer(shared);
        let observed = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let events = observed.clone();
        *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, value| {
            if stage.starts_with("owned-key-") {
                events.lock().push((stage, value));
            }
        }));
        let before = result.inner.ctx.live_bytes();
        LEDGER_OPS.with(|n| n.set(0));
        let rows = result.rows().unwrap();
        if !shared {
            assert_eq!(
                LEDGER_OPS.with(|n| n.get()),
                0,
                "ordinary transfer stays fast"
            );
        }
        let Term::Literal(value) = rows.row(0).unwrap().values()[0].as_ref().unwrap() else {
            panic!("original bound-literal oracle");
        };
        assert_eq!(value.value(), "x".repeat(4096));
        let events = observed.lock();
        let pointer = events
            .iter()
            .find(|(name, _)| *name == "owned-key-pointer")
            .unwrap()
            .1;
        let capacity = events
            .iter()
            .find(|(name, _)| *name == "owned-key-capacity")
            .unwrap()
            .1;
        assert_eq!(
            value.value().as_ptr() as usize,
            pointer,
            "same allocation, no lexical copy"
        );
        assert!(capacity >= 4098);
        assert!(result.inner.ctx.live_bytes() >= before + capacity as u64);
        drop(events);
        let held = result.inner.ctx.live_bytes();
        let last = rows.clone();
        drop(rows);
        assert_eq!(result.inner.ctx.live_bytes(), held);
        let weak = Arc::downgrade(&result.inner.ctx);
        drop(result);
        assert!(
            matches!(&last.row(0).unwrap().values()[0], Some(Term::Literal(v)) if v.value()=="x".repeat(4096))
        );
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn retained_stored_literal_prevents_second_copy_and_returns_exact_capacity() {
    for shared in [false, true] {
        let result = stored_literal_answer(shared);
        let before = result.inner.ctx.live_bytes();
        let first = result.rows().unwrap();
        let held = result.inner.ctx.live_bytes();
        assert!(held > before + 4096);
        assert!(matches!(result.rows(), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::Memory && b.limit == 8192 && b.requested > 8192));
        assert_eq!(
            result.inner.ctx.live_bytes(),
            held,
            "failed competing decode refunds partial storage"
        );
        assert!(
            matches!(&first.row(0).unwrap().values()[0],Some(Term::Literal(v)) if v.value().len()==4096)
        );
        drop(first);
        assert_eq!(result.inner.ctx.live_bytes(), before);
        let next = result.rows().unwrap();
        drop(next);
        assert_eq!(result.inner.ctx.live_bytes(), before);
    }
}

#[test]
fn stored_literal_cancel_and_injected_failure_return_real_reconstruction_credit() {
    use crate::algo::memory::probe;
    use std::sync::atomic::Ordering;
    for cancel in [false, true] {
        let result = stored_literal_answer(false);
        let before = result.inner.ctx.live_bytes();
        let flag = result.inner.ctx.cancel.clone();
        let hook = probe::install(
            move |point| {
                if cancel && matches!(point, probe::Point::Allocation("chunk-term", _)) {
                    flag.store(true, Ordering::Relaxed);
                }
            },
            (!cancel).then_some("chunk-term"),
        );
        let failure = result.rows();
        drop(hook);
        if cancel {
            assert!(matches!(failure, Err(Error::Cancelled)));
        } else {
            assert!(matches!(failure, Err(Error::Invalid(_))));
        }
        assert_eq!(result.inner.ctx.live_bytes(), before);
    }
}
