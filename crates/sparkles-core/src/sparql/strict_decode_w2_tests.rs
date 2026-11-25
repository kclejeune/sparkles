//! Regression tests for roadmap item 1308 Wave 2: strict decoding in eager mode,
//! expression combinators, executor operators, external sort, streaming/eager group
//! accumulators, SERVICE, text highlighting, DESCRIBE, `restrict_ctx`, `eq_key`,
//! incremental pass evaluation, and GeoSPARQL execution operators.

use super::*;
use crate::error::{BudgetKind, Error};
use crate::id::Tag;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};
use oxrdf::{Literal, Term};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;

fn assert_memory_budget_error<T: std::fmt::Debug>(res: Result<T>, what: &str) {
    match res {
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory => {}
        Err(other) => panic!("{what}: expected Memory BudgetExceeded, got Err({other:?})"),
        Ok(val) => panic!("{what}: expected Memory BudgetExceeded, got Ok({val:?})"),
    }
}

fn hold_all_but<'a>(ctx: &'a Ctx, headroom: u64) -> super::ctx::Charge<'a> {
    let live = ctx.live_bytes();
    let reserve = ctx.mem_limit.saturating_sub(live).saturating_sub(headroom);
    ctx.charge(reserve).expect("reserve headroom")
}

fn store_with_turtle(ttl: &str, compact: bool) -> Store {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    if compact {
        store.compact().unwrap();
    }
    store
}

// =========================================================================
// T1: Eager decode bypass closure & Ctx helpers
// =========================================================================

#[test]
fn eager_decode_bypass_closed_across_vocab_delta_local() {
    let big_v1 = format!("a{}", "v".repeat(64 * 1024));
    let big_v2 = format!("b{}", "w".repeat(64 * 1024));
    let big_d = format!("c{}", "d".repeat(64 * 1024));
    let short_a = "short_a".repeat(8);
    let short_b = "short_b".repeat(8);

    let ttl_base = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:big1 ex:v \"{big_v1}\" ; ex:v2 \"{big_v2}\" .\n\
         ex:small ex:v \"{short_a}\" ; ex:v2 \"{short_b}\" .\n"
    );
    let store = store_with_turtle(&ttl_base, true);
    store
        .load(&[Source::from_bytes(
            format!(
                "@prefix ex: <http://example.org/> .\n\
                 ex:delta ex:d \"{big_d}\" .\n"
            )
            .into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();

    let snap = store.snapshot();
    let v1_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&big_v1)))
        .unwrap();
    let v2_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&big_v2)))
        .unwrap();
    let d_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&big_d)))
        .unwrap();
    let short_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&short_a)))
        .unwrap();
    assert_eq!(v1_id.tag(), Tag::Vocab);
    assert_eq!(v2_id.tag(), Tag::Vocab);
    assert_eq!(d_id.tag(), Tag::Delta);

    // 1. Positive control in eager mode: 64-byte literals succeed under 16 KiB budget.
    let mut ctx_ok = Ctx::new(store.snapshot());
    ctx_ok.mem_limit = 16 * 1024;
    assert!(!ctx_ok.streams());
    assert!(ctx_ok.term(short_id).is_some());
    assert!(ctx_ok.value(short_id).is_some());
    ctx_ok.check().expect("control decode fits in 16 KiB");

    // 2. Direct eager Ctx::term / Ctx::value / simple_literal_cmp / string_floor refuse
    //    without reconstructing a 64 KiB key before admission.
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = 16 * 1024;
    assert!(!ctx.streams());
    let mut term_out = None;
    let largest = crate::test_alloc::record_largest(|| {
        term_out = ctx.term(v1_id);
    });
    assert!(
        term_out.is_none(),
        "eager Ctx::term must refuse 64 KiB vocab literal"
    );
    assert_memory_budget_error(ctx.check(), "eager Ctx::term latches Memory BudgetExceeded");
    assert!(
        largest < 32 * 1024,
        "vocab key must not be reconstructed before admission (largest_alloc={largest})"
    );

    // Delta literal in eager mode
    let mut ctx_d = Ctx::new(store.snapshot());
    ctx_d.mem_limit = 16 * 1024;
    assert!(ctx_d.value(d_id).is_none());
    assert_memory_budget_error(ctx_d.check(), "eager Ctx::value on delta literal");

    // Local literal in eager mode
    let mut ctx_l = Ctx::new(store.snapshot());
    ctx_l.mem_limit = MIB;
    let local_id = ctx_l.intern_term(&Term::Literal(Literal::new_simple_literal(
        "l".repeat(64 * 1024),
    )));
    assert_eq!(local_id.tag(), Tag::Local);
    let _held = hold_all_but(&ctx_l, 16 * 1024);
    assert!(ctx_l.value(local_id).is_none());
    assert_memory_budget_error(ctx_l.check(), "eager Ctx::value on local literal");
    drop(_held);

    // simple_literal_cmp in eager mode
    let mut ctx_cmp = Ctx::new(store.snapshot());
    ctx_cmp.mem_limit = 16 * 1024;
    let mut cmp_out = None;
    let largest_cmp = crate::test_alloc::record_largest(|| {
        cmp_out = ctx_cmp.simple_literal_cmp(v1_id, v2_id);
    });
    assert!(
        cmp_out.is_none(),
        "eager simple_literal_cmp must refuse 64 KiB vocab keys"
    );
    assert_memory_budget_error(ctx_cmp.check(), "eager simple_literal_cmp latches refusal");
    assert!(
        largest_cmp < 32 * 1024,
        "simple_literal_cmp must not allocate 64 KiB key before admission (largest={largest_cmp})"
    );

    // string_floor / with_vocab_key_prev in eager mode
    let mut ctx_floor = Ctx::new(store.snapshot());
    ctx_floor.mem_limit = 16 * 1024;
    let mut floor_out = None;
    let largest_floor = crate::test_alloc::record_largest(|| {
        floor_out = ctx_floor.string_floor(v1_id);
    });
    assert!(
        floor_out.is_none(),
        "eager string_floor must refuse 64 KiB vocab key"
    );
    assert_memory_budget_error(ctx_floor.check(), "eager string_floor latches refusal");
    assert!(
        largest_floor < 32 * 1024,
        "string_floor must not allocate 64 KiB key before admission (largest={largest_floor})"
    );

    // 3. query_regex::replace in eager mode must refuse output growth past budget.
    let mut ctx_re = Ctx::new(store.snapshot());
    ctx_re.mem_limit = 256 * 1024;
    let re = ctx_re.regex("a", "").unwrap().unwrap();
    let _held_re = hold_all_but(&ctx_re, 16 * 1024);
    let src = "a".repeat(2048);
    let res = re.replace(&ctx_re, &src, "bbbbbbbb");
    assert_memory_budget_error(
        res,
        "eager query_regex::replace output reserve_decode_checked",
    );
    drop(_held_re);
}

#[test]
fn with_controlled_key_and_cursor_value_checked_honor_controls_and_budget() {
    let big = "k".repeat(16 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{big}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let vocab_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&big)))
        .unwrap();
    assert_eq!(vocab_id.tag(), Tag::Vocab);

    for cursor in [false, true] {
        // Control passes with adequate budget
        let mut ctx_ok = Ctx::new(store.snapshot());
        ctx_ok.mem_limit = MIB;
        if cursor {
            ctx_ok.enable_cursor_accounting().unwrap();
        }
        assert!(ctx_ok.cursor_value_checked(vocab_id).unwrap().is_some());
        assert_eq!(
            ctx_ok
                .with_controlled_key(vocab_id, |k| Ok(k.len()))
                .unwrap(),
            Some(big.len() + 2)
        );
        assert!(ctx_ok.decoded_bytes(vocab_id).unwrap() > big.len() as u64);

        // Tight budget refuses without latching sticky failure in cursor_value_checked
        let mut ctx_tight = Ctx::new(store.snapshot());
        ctx_tight.mem_limit = 8 * 1024;
        if cursor {
            ctx_tight.enable_cursor_accounting().unwrap();
        }
        assert_memory_budget_error(
            ctx_tight.cursor_value_checked(vocab_id),
            &format!("cursor_value_checked tight budget cursor={cursor}"),
        );
        assert!(
            !ctx_tight.has_memory_failure(),
            "cursor_value_checked must remain recoverable and not latch sticky failure"
        );
        assert_memory_budget_error(
            ctx_tight.with_controlled_key(vocab_id, |k| Ok(k.len())),
            &format!("with_controlled_key tight budget cursor={cursor}"),
        );
        assert_memory_budget_error(
            ctx_tight.decoded_bytes(vocab_id),
            &format!("decoded_bytes tight budget cursor={cursor}"),
        );

        // Live cancel returns Error::Cancelled
        let mut ctx_cancel = Ctx::new(store.snapshot());
        ctx_cancel.mem_limit = MIB;
        if cursor {
            ctx_cancel.enable_cursor_accounting().unwrap();
        }
        ctx_cancel.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            ctx_cancel.cursor_value_checked(vocab_id),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            ctx_cancel.with_controlled_key(vocab_id, |k| Ok(k.len())),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            ctx_cancel.decoded_bytes(vocab_id),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            super::eqkey::eq_key(&ctx_cancel, vocab_id),
            Err(Error::Cancelled)
        ));

        // Expired deadline returns Error::Timeout
        let mut ctx_to = Ctx::new(store.snapshot());
        ctx_to.mem_limit = MIB;
        if cursor {
            ctx_to.enable_cursor_accounting().unwrap();
        }
        ctx_to.deadline = Some(Instant::now() - Duration::from_secs(1));
        assert!(matches!(
            ctx_to.cursor_value_checked(vocab_id),
            Err(Error::Timeout)
        ));
        assert!(matches!(
            ctx_to.with_controlled_key(vocab_id, |k| Ok(k.len())),
            Err(Error::Timeout)
        ));
        assert!(matches!(
            ctx_to.decoded_bytes(vocab_id),
            Err(Error::Timeout)
        ));
        assert!(matches!(
            super::eqkey::eq_key(&ctx_to, vocab_id),
            Err(Error::Timeout)
        ));
    }
}
