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

fn assert_memory_budget_error<T>(res: Result<T>, what: &str) {
    match res {
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory => {}
        Err(other) => panic!("{what}: expected Memory BudgetExceeded, got Err({other:?})"),
        Ok(_) => panic!("{what}: expected Memory BudgetExceeded, got Ok(_)"),
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
}

#[test]
fn regex_replace_refuses_final_output_reservation() {
    // Choose headroom (48 KiB) that admits the
    //    1 KiB base + 2*16 KiB growth loop (~33 KiB) so refusal happens specifically at
    //    the final output-lifetime `reserve_decode_checked(output.capacity())` (~129 KiB),
    //    paired with a control replacement whose final decode reservation fits.
    let store = Store::in_memory(StoreOptions::default());
    let src = "a".repeat(2048);
    let regex_ctx = || {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 256 * 1024;
        ctx
    };
    let control = regex_ctx();
    let re = control.regex("a", "").unwrap().unwrap();
    let _held = hold_all_but(&control, 48 * 1024);
    assert_eq!(
        re.replace(&control, &src, "b")
            .expect("regex control fits growth loop and final reserve_decode_checked")
            .len(),
        2048
    );
    let target = regex_ctx();
    let re = target.regex("a", "").unwrap().unwrap();
    let _held = hold_all_but(&target, 48 * 1024);
    let Err(Error::BudgetExceeded(b)) = re.replace(&target, &src, "bbbbbbbb") else {
        panic!("regex output must refuse at its final decode reservation");
    };
    assert_eq!(b.kind, BudgetKind::Memory);
    // 1 KiB scratch, twice the 16 KiB output for growth, then the final decode
    // envelope of 8 * 16 KiB + 1 KiB. The growth stage fits by itself.
    assert_eq!(b.requested, b.limit - 48 * 1024 + 162 * 1024);
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

// =========================================================================
// T2: Expression combinators & batch post-checks (eager & cursor)
// =========================================================================

fn run_select_both_modes(store: &Store, sparql: &str, mem_limit: u64, what: &str, expect_ok: bool) {
    let opts = QueryOptions {
        max_memory_bytes: Some(mem_limit),
        no_cache: true,
        ..QueryOptions::default()
    };
    let eager_res = super::query(store.snapshot(), sparql, &opts);
    if expect_ok {
        eager_res.unwrap_or_else(|e| panic!("{what} (eager control) failed: {e:?}"));
    } else {
        assert_memory_budget_error(eager_res, &format!("{what} (eager)"));
    }

    let cursor_opts = super::cursor::CursorOptions {
        batch_rows: 16,
        ..super::cursor::CursorOptions::default()
    };
    let cursor_res: Result<usize> = (|| {
        let mut cursor =
            super::cursor::select_cursor(store.snapshot(), sparql, &opts, &cursor_opts)?;
        let mut total = 0usize;
        while let Some(batch) = cursor.next_batch()? {
            total += batch.len();
        }
        Ok(total)
    })();
    if expect_ok {
        cursor_res.unwrap_or_else(|e| panic!("{what} (cursor control) failed: {e:?}"));
    } else {
        assert_memory_budget_error(cursor_res, &format!("{what} (cursor)"));
    }
}

#[test]
fn expr_combinators_do_not_swallow_decode_refusal() {
    let big1 = format!("a{}", "x".repeat(32 * 1024));
    let big2 = format!("b{}", "y".repeat(32 * 1024));
    let short1 = "short_a".repeat(4);
    let short2 = "short_b".repeat(4);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:big ex:a \"{big1}\" ; ex:b \"{big2}\" .\n\
         ex:small ex:a \"{short1}\" ; ex:b \"{short2}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let budget = 64 * 1024;

    let cases = [
        (
            "FILTER STRLEN",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:small ex:a ?x . FILTER(STRLEN(?x) > 0) }",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:big ex:a ?x . FILTER(STRLEN(?x) > 0) }",
        ),
        (
            "FILTER simple_literal_cmp",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:small ex:a ?x ; ex:b ?y . FILTER(?x < ?y) }",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:big ex:a ?x ; ex:b ?y . FILTER(?x < ?y) }",
        ),
        (
            "BIND COALESCE fallback",
            "PREFIX ex: <http://example.org/> SELECT ?r WHERE { ex:small ex:a ?x . BIND(COALESCE(STRLEN(?x), 1) AS ?r) }",
            "PREFIX ex: <http://example.org/> SELECT ?r WHERE { ex:big ex:a ?x . BIND(COALESCE(STRLEN(?x), 1) AS ?r) }",
        ),
        (
            "FILTER OR true",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:small ex:a ?x . FILTER(STRLEN(?x) > 0 || true) }",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:big ex:a ?x . FILTER(STRLEN(?x) > 0 || true) }",
        ),
        (
            "FILTER AND false",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:small ex:a ?x . FILTER(STRLEN(?x) > 0 && false) }",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:big ex:a ?x . FILTER(STRLEN(?x) > 0 && false) }",
        ),
        (
            "FILTER IN list",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:small ex:a ?x . FILTER(\"ok\" IN (STR(?x), \"ok\")) }",
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:big ex:a ?x . FILTER(\"ok\" IN (STR(?x), \"ok\")) }",
        ),
        (
            "BIND STR",
            "PREFIX ex: <http://example.org/> SELECT ?s WHERE { ex:small ex:a ?x . BIND(STR(?x) AS ?s) }",
            "PREFIX ex: <http://example.org/> SELECT ?s WHERE { ex:big ex:a ?x . BIND(STR(?x) AS ?s) }",
        ),
        (
            "BIND DATATYPE",
            "PREFIX ex: <http://example.org/> SELECT ?d WHERE { ex:small ex:a ?x . BIND(DATATYPE(?x) AS ?d) }",
            "PREFIX ex: <http://example.org/> SELECT ?d WHERE { ex:big ex:a ?x . BIND(DATATYPE(?x) AS ?d) }",
        ),
        (
            "BIND cdt:List constructor",
            "PREFIX ex: <http://example.org/> PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> \
             SELECT ?l WHERE { ex:small ex:a ?x . BIND(cdt:List(?x, 1) AS ?l) }",
            "PREFIX ex: <http://example.org/> PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> \
             SELECT ?l WHERE { ex:big ex:a ?x . BIND(cdt:List(?x, 1) AS ?l) }",
        ),
        (
            "BIND cdt:Map constructor",
            "PREFIX ex: <http://example.org/> PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> \
             SELECT ?m WHERE { ex:small ex:a ?x . BIND(cdt:Map(\"k\", ?x) AS ?m) }",
            "PREFIX ex: <http://example.org/> PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> \
             SELECT ?m WHERE { ex:big ex:a ?x . BIND(cdt:Map(\"k\", ?x) AS ?m) }",
        ),
    ];

    for (label, control_q, target_q) in cases {
        run_select_both_modes(&store, control_q, budget, label, true);
        run_select_both_modes(&store, target_q, budget, label, false);
    }

    // SameTerm when intern_value refuses on computed CONCAT strings must fail, not return true!
    let mid = "m".repeat(2048);
    let ttl_st = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{mid}\" .\n\
         ex:small ex:p \"ok\" .\n"
    );
    let store_st = store_with_turtle(&ttl_st, true);
    let st_budget = 64 * 1024;
    let st_control = "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:small ex:p ?x . \
                      FILTER(sameTerm(CONCAT(?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x), \
                                      CONCAT(?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x))) }";
    let st_target = "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ex:s ex:p ?x . \
                     FILTER(sameTerm(CONCAT(?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x), \
                                     CONCAT(?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x, ?x))) }";
    run_select_both_modes(&store_st, st_control, st_budget, "FILTER sameTerm", true);
    run_select_both_modes(&store_st, st_target, st_budget, "FILTER sameTerm", false);
}

// =========================================================================
// T3: Registered scalars & RDF 1.2 triple terms hold combined credit
// =========================================================================

#[test]
fn expr_registered_and_triple_terms_hold_combined_credit() {
    use super::extensions::{ExtensionRegistryBuilder, ScalarContext, ScalarDescriptor};
    use oxrdf::NamedNode;

    // Three 2 KiB distinct compacted IRIs/literals:
    // - Each single term checked decode is ~16.5 KiB (fits inside 36 KiB).
    // - Three coexisting terms inside TRIPLE(?s, ?p, ?o) or registered 3-arg callback
    //   need ~50 KiB > 36 KiB and must refuse!
    let s_iri = format!("http://example.org/s/{}", "s".repeat(2048));
    let p_iri = format!("http://example.org/p/{}", "p".repeat(2048));
    let o_lit = "o".repeat(2048);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:row ex:s <{s_iri}> ; ex:p <{p_iri}> ; ex:o \"{o_lit}\" .\n\
         ex:small ex:s <http://example.org/s1> ; ex:p <http://example.org/p1> ; ex:o \"ok\" .\n\
         ex:reified ex:t <<( <{s_iri}> <{p_iri}> \"{o_lit}\" )>> .\n\
         ex:reified_small ex:t <<( <http://example.org/s1> <http://example.org/p1> \"ok\" )>> .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let budget = 40 * 1024;

    // 1. TRIPLE(?s, ?p, ?o) holds combined credit across all 3 operands.
    let triple_control = "PREFIX ex: <http://example.org/> \
        SELECT ?t WHERE { ex:small ex:s ?s ; ex:p ?p ; ex:o ?o . BIND(TRIPLE(?s, ?p, ?o) AS ?t) }";
    let triple_target = "PREFIX ex: <http://example.org/> \
        SELECT ?t WHERE { ex:row ex:s ?s ; ex:p ?p ; ex:o ?o . BIND(TRIPLE(?s, ?p, ?o) AS ?t) }";
    run_select_both_modes(&store, triple_control, budget, "BIND TRIPLE", true);
    run_select_both_modes(&store, triple_target, budget, "BIND TRIPLE", false);

    // 2. SUBJECT / PREDICATE / OBJECT on oversized triple term must fail cleanly.
    for (name, fn_name) in [
        ("SUBJECT", "SUBJECT"),
        ("PREDICATE", "PREDICATE"),
        ("OBJECT", "OBJECT"),
    ] {
        let ctrl = format!(
            "PREFIX ex: <http://example.org/> SELECT ?r WHERE {{ ex:reified_small ex:t ?t . BIND({fn_name}(?t) AS ?r) }}"
        );
        let tgt = format!(
            "PREFIX ex: <http://example.org/> SELECT ?r WHERE {{ ex:reified ex:t ?t . BIND({fn_name}(?t) AS ?r) }}"
        );
        run_select_both_modes(&store, &ctrl, budget, name, true);
        run_select_both_modes(&store, &tgt, budget, name, false);
    }

    // 3. Registered 3-arg scalar callback holds combined credit across coexisting terms:
    //    each 6 KiB term's checked decode peaks ~49.5 KiB and retains ~6.2 KiB after resize;
    //    2 coexisting 6 KiB terms fit inside 58 KiB (~56 KiB peak), while 3 exceed it (~62 KiB).
    let s6 = format!("http://example.org/s6/{}", "s".repeat(6 * 1024));
    let p6 = format!("http://example.org/p6/{}", "p".repeat(6 * 1024));
    let o6 = "o".repeat(6 * 1024);
    let ttl6 = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:row6 ex:s <{s6}> ; ex:p <{p6}> ; ex:o \"{o6}\" .\n"
    );
    let store6 = store_with_turtle(&ttl6, true);
    let mut reg = ExtensionRegistryBuilder::default();
    let fn2_iri = "http://example.org/ext/combine2";
    let fn3_iri = "http://example.org/ext/combine3";
    reg.register_scalar(
        ScalarDescriptor::new(
            fn2_iri,
            2..=2,
            |_ctx: &ScalarContext<'_>, _args: &[Term]| {
                Ok(Term::NamedNode(NamedNode::new_unchecked(
                    "http://example.org/ok",
                )))
            },
        )
        .unwrap(),
    )
    .unwrap();
    reg.register_scalar(
        ScalarDescriptor::new(
            fn3_iri,
            3..=3,
            |_ctx: &ScalarContext<'_>, _args: &[Term]| {
                Ok(Term::NamedNode(NamedNode::new_unchecked(
                    "http://example.org/ok",
                )))
            },
        )
        .unwrap(),
    )
    .unwrap();
    let ext = reg.build();

    let reg_ctrl = format!(
        "PREFIX ex: <http://example.org/> SELECT ?r WHERE {{ ex:row6 ex:s ?s ; ex:p ?p . BIND(<{fn2_iri}>(?s, ?p) AS ?r) }}"
    );
    let reg_tgt = format!(
        "PREFIX ex: <http://example.org/> SELECT ?r WHERE {{ ex:row6 ex:s ?s ; ex:p ?p ; ex:o ?o . BIND(<{fn3_iri}>(?s, ?p, ?o) AS ?r) }}"
    );
    let eager_opts = QueryOptions {
        max_memory_bytes: Some(71 * 1024),
        extensions: Some(ext.clone()),
        no_cache: true,
        ..QueryOptions::default()
    };
    super::query(store6.snapshot(), &reg_ctrl, &eager_opts)
        .expect("registered 2-arg 6 KiB control (eager)");
    assert_memory_budget_error(
        super::query(store6.snapshot(), &reg_tgt, &eager_opts),
        "registered 3-arg 6 KiB scalar (eager)",
    );
    let cursor_opts = super::cursor::CursorOptions {
        batch_rows: 16,
        ..super::cursor::CursorOptions::default()
    };
    let probe_opts = QueryOptions {
        max_memory_bytes: Some(MIB),
        extensions: Some(ext.clone()),
        no_cache: true,
        ..QueryOptions::default()
    };
    let fixed_setup = {
        let probe =
            super::cursor::select_cursor(store6.snapshot(), &reg_ctrl, &probe_opts, &cursor_opts)
                .expect("probe cursor");
        probe.memory_meter().live_bytes()
    };
    let tight_cursor_opts = QueryOptions {
        max_memory_bytes: Some(fixed_setup + 71 * 1024),
        extensions: Some(ext),
        no_cache: true,
        ..QueryOptions::default()
    };
    let mut c_ok = super::cursor::select_cursor(
        store6.snapshot(),
        &reg_ctrl,
        &tight_cursor_opts,
        &cursor_opts,
    )
    .unwrap();
    while c_ok.next_batch().unwrap().is_some() {}
    let c_err: Result<()> = (|| {
        let mut c = super::cursor::select_cursor(
            store6.snapshot(),
            &reg_tgt,
            &tight_cursor_opts,
            &cursor_opts,
        )?;
        while c.next_batch()?.is_some() {}
        Ok(())
    })();
    assert_memory_budget_error(c_err, "registered 3-arg 6 KiB scalar (cursor)");
}

// =========================================================================
// T4: exec::unpack, exec::assign (LET), exec::unfold (UNFOLD)
// =========================================================================

#[test]
fn exec_unpack_assign_unfold_propagate_decode_refusal() {
    let big = "u".repeat(16 * 1024);
    let big2 = "v".repeat(16 * 1024);
    let cdt_big = format!("[\"{}\", 1]", "z".repeat(16 * 1024));
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> .\n\
         ex:r_big rdf:reifies <<( ex:s ex:p \"{big}\" )>> .\n\
         ex:r_small rdf:reifies <<( ex:s ex:p \"ok\" )>> .\n\
         ex:a_big ex:x \"{big}\" ; ex:y \"{big2}\" .\n\
         ex:a_small ex:x \"a\" ; ex:y \"b\" .\n\
         ex:u_big ex:list \"{}\"^^cdt:List .\n\
         ex:u_small ex:list \"[1, 2]\"^^cdt:List .\n",
        cdt_big.replace('"', "\\\"")
    );
    let store = store_with_turtle(&ttl, true);
    let budget = 48 * 1024;

    // 1. unpack (reified triple decomposition)
    let unpack_ctrl = "PREFIX ex: <http://example.org/> \
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
        SELECT ?o WHERE { ex:r_small rdf:reifies <<( ?s ?p ?o )>> }";
    let unpack_tgt = "PREFIX ex: <http://example.org/> \
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
        SELECT ?o WHERE { ex:r_big rdf:reifies <<( ?s ?p ?o )>> }";
    run_select_both_modes(&store, unpack_ctrl, budget, "unpack", true);
    run_select_both_modes(&store, unpack_tgt, budget, "unpack", false);

    // 2. assign (LET on already-bound variable with two differing literals)
    let assign_ctrl = "PREFIX ex: <http://example.org/> \
        SELECT ?x WHERE { ex:a_small ex:x ?x ; ex:y ?y . LET (?x := ?y) }";
    let assign_tgt = "PREFIX ex: <http://example.org/> \
        SELECT ?x WHERE { ex:a_big ex:x ?x ; ex:y ?y . LET (?x := ?y) }";
    run_select_both_modes(&store, assign_ctrl, budget, "assign LET", true);
    run_select_both_modes(&store, assign_tgt, budget, "assign LET", false);

    // 3. unfold (UNFOLD over oversized cdt:List literal)
    let unfold_ctrl = "PREFIX ex: <http://example.org/> \
        SELECT ?e ?pos WHERE { ex:u_small ex:list ?l . UNFOLD(?l AS ?e, ?pos) }";
    let unfold_tgt = "PREFIX ex: <http://example.org/> \
        SELECT ?e ?pos WHERE { ex:u_big ex:list ?l . UNFOLD(?l AS ?e, ?pos) }";
    run_select_both_modes(&store, unfold_ctrl, budget, "unfold", true);
    run_select_both_modes(&store, unfold_tgt, budget, "unfold", false);
}

// =========================================================================
// T5: TopK ordered, prefilter, offer_rows, key_rows_charged
// =========================================================================

#[test]
fn exec_topk_ordered_and_heap_propagate_decode_refusal() {
    let mut ttl = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..20 {
        let big = format!("val_{i:02}_{}", "k".repeat(16 * 1024));
        ttl.push_str(&format!(
            "ex:big_{i} ex:k {i} ; ex:v \"{big}\" .\n\
             ex:small_{i} ex:k {i} ; ex:v \"short_{i:02}\" .\n"
        ));
    }
    let store = store_with_turtle(&ttl, true);
    let budget = 64 * 1024;

    // 1. Single-key ORDER BY ?v LIMIT 2 (hits OrderedTopK / offer_rows / key_rows_charged)
    let q_ctrl = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?v WHERE { ?s ex:v ?v . FILTER(STRSTARTS(STR(?s), \"http://example.org/small_\")) } \
        ORDER BY ?v LIMIT 2";
    let q_tgt = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?v WHERE { ?s ex:v ?v . FILTER(STRSTARTS(STR(?s), \"http://example.org/big_\")) } \
        ORDER BY ?v LIMIT 2";
    run_select_both_modes(&store, q_ctrl, budget, "topk single-key", true);
    run_select_both_modes(&store, q_tgt, budget, "topk single-key", false);

    // 2. Multi-key ORDER BY ?k ?v LIMIT 2 (hits HeapKey::Var secondary key decode in offer_rows)
    let q2_ctrl = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?k ?v WHERE { ?s ex:k ?k ; ex:v ?v . FILTER(STRSTARTS(STR(?s), \"http://example.org/small_\")) } \
        ORDER BY ?k ?v LIMIT 2";
    let q2_tgt = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?k ?v WHERE { ?s ex:k ?k ; ex:v ?v . FILTER(STRSTARTS(STR(?s), \"http://example.org/big_\")) } \
        ORDER BY ?k ?v LIMIT 2";
    run_select_both_modes(&store, q2_ctrl, budget, "topk multi-key HeapKey::Var", true);
    run_select_both_modes(&store, q2_tgt, budget, "topk multi-key HeapKey::Var", false);

    // 3. Multi-key ORDER BY ?k LCASE(?v) LIMIT 2 (hits HeapKey::Expr secondary key decode)
    let q3_ctrl = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?k WHERE { ?s ex:k ?k ; ex:v ?v . FILTER(STRSTARTS(STR(?s), \"http://example.org/small_\")) } \
        ORDER BY ?k LCASE(?v) LIMIT 2";
    let q3_tgt = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?k WHERE { ?s ex:k ?k ; ex:v ?v . FILTER(STRSTARTS(STR(?s), \"http://example.org/big_\")) } \
        ORDER BY ?k LCASE(?v) LIMIT 2";
    run_select_both_modes(
        &store,
        q3_ctrl,
        budget,
        "topk multi-key HeapKey::Expr",
        true,
    );
    run_select_both_modes(
        &store,
        q3_tgt,
        budget,
        "topk multi-key HeapKey::Expr",
        false,
    );
}

// =========================================================================
// T8: SERVICE, DESCRIBE, restrict_ctx, entity_vector / vector_search
// =========================================================================

#[test]
fn service_describe_restrict_and_vector_strict_decode() {
    use crate::access::{GraphAccess, GraphRule, Graphs};
    use oxrdf::NamedNode;

    // 1. SERVICE endpoint decode refusal must return BudgetKind::Memory, NOT Error::Service
    let long_url = format!("http://example.org/endpoint/{}", "e".repeat(16 * 1024));
    let store_svc = store_with_turtle(
        &format!("<{long_url}> <http://example.org/p> \"v\" .\n"),
        true,
    );
    let svc_q = format!("SELECT ?x WHERE {{ SERVICE <{long_url}> {{ ?x ?y ?z }} }}");
    let svc_opts = QueryOptions {
        allow_service: true,
        no_cache: true,
        max_memory_bytes: Some(32 * 1024),
        ..QueryOptions::default()
    };
    assert_memory_budget_error(
        super::query(store_svc.snapshot(), &svc_q, &svc_opts),
        "SERVICE endpoint decode refusal must not become Error::Service",
    );

    // 2. SERVICE cache key allocation must refuse BEFORE allocating key String
    let cache_store = Store::in_memory(StoreOptions {
        service_cache_bytes: 1 << 20,
        ..StoreOptions::default()
    });
    let cache_q = "SELECT ?x WHERE { SERVICE <cache:http://example.org/remote> { ?x ?y ?z } }";
    let scope: Arc<str> = Arc::from("s".repeat(32 * 1024));
    let cache_opts = QueryOptions {
        allow_service: true,
        no_cache: false,
        service_scope: Some(scope),
        max_memory_bytes: Some(16 * 1024),
        ..QueryOptions::default()
    };
    let mut cache_res = None;
    let largest = crate::test_alloc::record_largest(|| {
        cache_res = Some(super::query(cache_store.snapshot(), cache_q, &cache_opts));
    });
    assert_memory_budget_error(
        cache_res.unwrap(),
        "SERVICE cache key try_key refuses past budget",
    );
    assert!(
        largest < 24 * 1024,
        "SERVICE cache key must refuse before allocating oversized key String (largest={largest})"
    );

    // 3. DESCRIBE decodes via term_checked_into and propagates memory budget refusal
    let big_lit = "d".repeat(16 * 1024);
    let desc_ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:big ex:p \"{big_lit}\" .\n\
         ex:small ex:p \"ok\" .\n"
    );
    let store_desc = store_with_turtle(&desc_ttl, true);
    let desc_opts = QueryOptions {
        max_memory_bytes: Some(40 * 1024),
        no_cache: true,
        ..QueryOptions::default()
    };
    super::query(
        store_desc.snapshot(),
        "PREFIX ex: <http://example.org/> DESCRIBE ex:small",
        &desc_opts,
    )
    .expect("DESCRIBE control passes");
    assert_memory_budget_error(
        super::query(
            store_desc.snapshot(),
            "PREFIX ex: <http://example.org/> DESCRIBE ex:big",
            &desc_opts,
        ),
        "DESCRIBE oversized literal refuses with Memory BudgetExceeded",
    );

    // 4. restrict_ctx propagates memory refusal instead of hiding graph silently
    let big_g = format!("http://example.org/graph/{}", "g".repeat(16 * 1024));
    let small_g = "http://example.org/graph/small";
    let store_g = Store::in_memory(StoreOptions::default());
    store_g
        .load(&[Source::from_bytes(
            b"<http://example.org/s> <http://example.org/p> \"seed\" .\n".to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    store_g.compact().unwrap();
    let access = Arc::new(GraphAccess {
        read: Graphs::Only(GraphRule {
            default_graph: true,
            ..Default::default()
        }),
        write: Graphs::All,
        triples: None,
        views: Default::default(),
    });
    // Control: short graph IRI fits under 16 KiB headroom
    let mut ctx_ok = Ctx::new(store_g.snapshot());
    ctx_ok.mem_limit = MIB;
    let small_gid = ctx_ok.intern_term(&Term::NamedNode(NamedNode::new_unchecked(small_g)));
    ctx_ok.dataset.named = Some(vec![small_gid]);
    ctx_ok.mem_limit = ctx_ok.live_bytes() + 16 * 1024;
    super::restrict_ctx(&mut ctx_ok, Some(&access)).expect("restrict_ctx control passes");

    // Target: 16 KiB graph IRI refuses under 16 KiB headroom
    let mut ctx_bad = Ctx::new(store_g.snapshot());
    ctx_bad.mem_limit = MIB;
    let big_gid = ctx_bad.intern_term(&Term::NamedNode(NamedNode::new_unchecked(&big_g)));
    ctx_bad.dataset.named = Some(vec![big_gid]);
    ctx_bad.mem_limit = ctx_bad.live_bytes() + 16 * 1024;
    assert_memory_budget_error(
        super::restrict_ctx(&mut ctx_bad, Some(&access)),
        "restrict_ctx propagates memory error on oversized graph IRI",
    );

    // 5. vector_search variable query literal refuses instead of falling back to entity_vector
    let vec_lit = "x".repeat(16 * 1024);
    let vec_ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix spk: <urn:x-sparkles:> .\n\
         ex:q_big ex:q \"{vec_lit}\" .\n\
         ex:q_small ex:q \"[1.0, 0.0]\"^^spk:vector .\n\
         ex:doc ex:emb \"[1.0, 0.0]\"^^spk:vector .\n"
    );
    let store_vec = store_with_turtle(&vec_ttl, true);
    let vec_ctrl = "PREFIX ex: <http://example.org/> PREFIX spk: <urn:x-sparkles:> \
        SELECT ?s ?score WHERE { ex:q_small ex:q ?q . (?s ?score) spk:vectorSearch (ex:emb ?q 5) }";
    let vec_tgt = "PREFIX ex: <http://example.org/> PREFIX spk: <urn:x-sparkles:> \
        SELECT ?s ?score WHERE { ex:q_big ex:q ?q . (?s ?score) spk:vectorSearch (ex:emb ?q 5) }";
    let ctrl_res = super::query(
        store_vec.snapshot(),
        vec_ctrl,
        &QueryOptions {
            max_memory_bytes: Some(48 * 1024),
            no_cache: true,
            ..QueryOptions::default()
        },
    )
    .expect("vector_search control passes");
    assert_eq!(ctrl_res.len(), 1, "vector_search control must match ex:doc");
    run_select_both_modes(
        &store_vec,
        vec_ctrl,
        48 * 1024,
        "vector_search var query",
        true,
    );
    run_select_both_modes(
        &store_vec,
        vec_tgt,
        48 * 1024,
        "vector_search var query",
        false,
    );
}

// =========================================================================
// T6: External sort Head::load admits before decode & preserves drop order
// =========================================================================

#[test]
fn extsort_head_load_admits_before_decode_and_preserves_drop_order() {
    use crate::spill::{Spill, SpillOptions};

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 256 << 20,
        query_bytes: 256 << 20,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();

    // Positive control: 3,000 short literals spill and merge cleanly under 384 KiB.
    let mut ttl_ok = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..3_000 {
        let k = (i * 7919) % 3_000;
        ttl_ok.push_str(&format!("ex:s{i} ex:v \"v{k:05}\" .\n"));
    }
    let store_ok = store_with_turtle(&ttl_ok, true);

    let cursor_opts = CursorOptions {
        batch_rows: 256,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    for (label, q) in [
        (
            "extsort Key::Column control",
            "PREFIX ex: <http://example.org/> SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v",
        ),
        (
            "extsort Key::Expr control",
            "PREFIX ex: <http://example.org/> SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY LCASE(?v)",
        ),
    ] {
        let mut cursor = super::select_cursor(
            store_ok.snapshot(),
            q,
            &QueryOptions {
                max_memory_bytes: Some(512 * 1024),
                spill: Some(spill.clone()),
                no_cache: true,
                ..QueryOptions::default()
            },
            &cursor_opts,
        )
        .unwrap_or_else(|e| panic!("{label}: open failed: {e:?}"));
        let mut count = 0usize;
        let mut prev: Option<String> = None;
        while let Some(batch) = cursor
            .next_batch()
            .unwrap_or_else(|e| panic!("{label}: next_batch failed: {e:?}"))
        {
            for r in 0..batch.len() {
                let Some(t) = batch.term(r, 1).unwrap() else {
                    panic!("{label}: expected literal sort key");
                };
                let Term::Literal(l) = t.as_term() else {
                    panic!("{label}: expected literal sort key");
                };
                let cur = l.value().to_owned();
                if let Some(p) = &prev {
                    assert!(
                        p <= &cur,
                        "{label}: external sort output out of order ({p} > {cur})"
                    );
                }
                prev = Some(cur);
            }
            count += batch.len();
        }
        assert_eq!(count, 3_000, "{label}: all sorted rows returned");
        let plan_json = cursor.plan_json().unwrap();
        assert!(
            plan_json.contains("spill") || plan_json.contains("runs"),
            "{label}: control must exercise spilled external sort (plan={plan_json})"
        );
    }

    // Target: 600 rows with 4 KiB distinct literals (total raw strings ~2.4 MiB)
    // under a tight 192 KiB budget with spill enabled. The external sort spills runs
    // and then Head::load / run-build must refuse with BudgetKind::Memory (never
    // producing SortKey::NULL corrupted output!).
    let mut ttl_big = String::from("@prefix ex: <http://example.org/> .\n");
    let pad = "x".repeat(4096);
    for i in 0..600 {
        let k = (i * 353) % 600;
        ttl_big.push_str(&format!("ex:s{i} ex:v \"{k:04}-{pad}\" .\n"));
    }
    let store_big = store_with_turtle(&ttl_big, true);

    for (label, q) in [
        (
            "extsort Key::Column tight",
            "PREFIX ex: <http://example.org/> SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v",
        ),
        (
            "extsort Key::Expr tight",
            "PREFIX ex: <http://example.org/> SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY LCASE(?v)",
        ),
    ] {
        let res: Result<usize> = (|| {
            let mut cursor = super::select_cursor(
                store_big.snapshot(),
                q,
                &QueryOptions {
                    max_memory_bytes: Some(192 * 1024),
                    spill: Some(spill.clone()),
                    no_cache: true,
                    ..QueryOptions::default()
                },
                &cursor_opts,
            )?;
            let mut count = 0usize;
            while let Some(batch) = cursor.next_batch()? {
                count += batch.len();
            }
            Ok(count)
        })();
        assert_memory_budget_error(res, label);
    }
}

// =========================================================================
// T7: Group accumulators & per-group Rows aggregates strict decode
// =========================================================================

#[test]
fn group_accumulators_and_rows_aggregates_strict_decode() {
    let big_str = "g".repeat(32 * 1024);
    let big_num_pad = "0".repeat(32 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         ex:small ex:g \"1\" ; ex:s \"alpha\" ; ex:n \"1.5\"^^xsd:float .\n\
         ex:small2 ex:g \"1\" ; ex:s \"beta\" ; ex:n \"2.5\"^^xsd:float .\n\
         ex:big ex:g \"2\" ; ex:s \"{big_str}\" .\n\
         ex:big2 ex:g \"2\" ; ex:s \"zzz\" ; ex:n \"2.5\"^^xsd:float .\n"
    );
    let store = store_with_turtle(&ttl, true);
    // Load the oversized xsd:float into Tag::Delta (uncompacted) so it bypasses
    // the compacted generation's typed numeric column and exercises strict decoding.
    store
        .load(&[Source::from_bytes(
            format!(
                "@prefix ex: <http://example.org/> .\n\
                 @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
                 ex:big ex:n \"{big_num_pad}1.5\"^^xsd:float .\n"
            )
            .into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let big_float_id = store
        .snapshot()
        .lookup_term(&Term::Literal(Literal::new_typed_literal(
            format!("{big_num_pad}1.5"),
            oxrdf::vocab::xsd::FLOAT,
        )))
        .expect("big xsd:float interned");
    assert_eq!(
        big_float_id.tag(),
        Tag::Delta,
        "oversized xsd:float must live in delta vocab"
    );
    let budget = 64 * 1024;

    let cases: &[(&str, &str, &str)] = &[
        (
            "MIN(?v)",
            "PREFIX ex: <http://example.org/> SELECT (MIN(?v) AS ?r) WHERE { ?x ex:g \"1\" ; ex:s ?v }",
            "PREFIX ex: <http://example.org/> SELECT (MIN(?v) AS ?r) WHERE { ?x ex:g \"2\" ; ex:s ?v }",
        ),
        (
            "MAX(?v)",
            "PREFIX ex: <http://example.org/> SELECT (MAX(?v) AS ?r) WHERE { ?x ex:g \"1\" ; ex:s ?v }",
            "PREFIX ex: <http://example.org/> SELECT (MAX(?v) AS ?r) WHERE { ?x ex:g \"2\" ; ex:s ?v }",
        ),
        (
            "SUM(?n)",
            "PREFIX ex: <http://example.org/> SELECT (SUM(?n) AS ?r) WHERE { ?x ex:g \"1\" ; ex:n ?n }",
            "PREFIX ex: <http://example.org/> SELECT (SUM(?n) AS ?r) WHERE { ?x ex:g \"2\" ; ex:n ?n }",
        ),
        (
            "AVG(?n)",
            "PREFIX ex: <http://example.org/> SELECT (AVG(?n) AS ?r) WHERE { ?x ex:g \"1\" ; ex:n ?n }",
            "PREFIX ex: <http://example.org/> SELECT (AVG(?n) AS ?r) WHERE { ?x ex:g \"2\" ; ex:n ?n }",
        ),
        (
            "GROUP_CONCAT(?v)",
            "PREFIX ex: <http://example.org/> SELECT (GROUP_CONCAT(?v; SEPARATOR=\",\") AS ?r) WHERE { ?x ex:g \"1\" ; ex:s ?v }",
            "PREFIX ex: <http://example.org/> SELECT (GROUP_CONCAT(?v; SEPARATOR=\",\") AS ?r) WHERE { ?x ex:g \"2\" ; ex:s ?v }",
        ),
        (
            "STDEV(?n)",
            "PREFIX ex: <http://example.org/> SELECT (STDEV(?n) AS ?r) WHERE { ?x ex:g \"1\" ; ex:n ?n }",
            "PREFIX ex: <http://example.org/> SELECT (STDEV(?n) AS ?r) WHERE { ?x ex:g \"2\" ; ex:n ?n }",
        ),
        (
            "MEDIAN(?n)",
            "PREFIX ex: <http://example.org/> SELECT (MEDIAN(?n) AS ?r) WHERE { ?x ex:g \"1\" ; ex:n ?n }",
            "PREFIX ex: <http://example.org/> SELECT (MEDIAN(?n) AS ?r) WHERE { ?x ex:g \"2\" ; ex:n ?n }",
        ),
        (
            "FOLD list",
            "PREFIX ex: <http://example.org/> SELECT (FOLD(?v) AS ?r) WHERE { ?x ex:g \"1\" ; ex:s ?v }",
            "PREFIX ex: <http://example.org/> SELECT (FOLD(?v) AS ?r) WHERE { ?x ex:g \"2\" ; ex:s ?v }",
        ),
        (
            "FOLD map",
            "PREFIX ex: <http://example.org/> SELECT (FOLD(?v, ?v) AS ?r) WHERE { ?x ex:g \"1\" ; ex:s ?v }",
            "PREFIX ex: <http://example.org/> SELECT (FOLD(?v, ?v) AS ?r) WHERE { ?x ex:g \"2\" ; ex:s ?v }",
        ),
    ];

    for (label, ctrl_q, tgt_q) in cases {
        run_select_both_modes(&store, ctrl_q, budget, label, true);
        run_select_both_modes(&store, tgt_q, budget, label, false);
    }

    // Also test the per-group Rows kernel (incremental_group = false) for MIN, SUM, GROUP_CONCAT.
    for (label, ctrl_q, tgt_q) in &cases[..5] {
        let opt = super::Optimizations {
            incremental_group: false,
            ..Default::default()
        };
        let ctrl_res = super::query(
            store.snapshot(),
            ctrl_q,
            &QueryOptions {
                max_memory_bytes: Some(budget),
                no_cache: true,
                optimizations: Some(opt),
                ..QueryOptions::default()
            },
        );
        if let Err(err) = ctrl_res {
            panic!("{label} (incremental_group=false control): expected Ok, got Err({err:?})");
        }
        let tgt_res = super::query(
            store.snapshot(),
            tgt_q,
            &QueryOptions {
                max_memory_bytes: Some(budget),
                no_cache: true,
                optimizations: Some(opt),
                ..QueryOptions::default()
            },
        );
        assert_memory_budget_error(
            tgt_res,
            &format!("{label} (incremental_group=false target)"),
        );
    }

    // Mid-chunk GROUP_CONCAT growth test (< CHUNK_ROWS = 65,536):
    // 400 rows x 512-byte strings = 200 KiB output, while each string fits 128 KiB easily.
    // Control: 10 rows x 512 bytes (~5 KiB) passes under the same 128 KiB budget.
    let mut ttl_mc = String::from("@prefix ex: <http://example.org/> .\n");
    let chunk_val = "c".repeat(512);
    for i in 0..400 {
        ttl_mc.push_str(&format!("ex:r{i} ex:big \"{i:04}-{chunk_val}\" .\n"));
    }
    for i in 0..10 {
        ttl_mc.push_str(&format!("ex:c{i} ex:small \"{i:04}-{chunk_val}\" .\n"));
    }
    let store_mc = store_with_turtle(&ttl_mc, true);
    let mc_budget = 128 * 1024;
    let mc_ctrl = "PREFIX ex: <http://example.org/> SELECT (GROUP_CONCAT(?v; SEPARATOR=\",\") AS ?r) WHERE { ?s ex:small ?v }";
    let mc_tgt = "PREFIX ex: <http://example.org/> SELECT (GROUP_CONCAT(?v; SEPARATOR=\",\") AS ?r) WHERE { ?s ex:big ?v }";
    run_select_both_modes(
        &store_mc,
        mc_ctrl,
        mc_budget,
        "GROUP_CONCAT mid-chunk",
        true,
    );
    run_select_both_modes(
        &store_mc,
        mc_tgt,
        mc_budget,
        "GROUP_CONCAT mid-chunk",
        false,
    );
}

// =========================================================================
// T9: GeoSPARQL execution operators, GeoMemo, spatialF, join, rewrite, search
// =========================================================================

#[cfg(feature = "geo")]
#[test]
fn geo_execution_operators_and_spatialf_strict_decode() {
    let pad = " ".repeat(32 * 1024);
    let big_wkt = format!("{pad}POINT(1 2)");
    // Keep geo:asWKT geometries short in `store` so spatial:nearbyGeom's control
    // query scans only short indexable geometries while ex:q_big tests bound-arg
    // decoding (and `big_rewrite_store` below tests geo:asWKT scan/rewrite paths).
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n\
         ex:f_small geo:hasGeometry ex:g_small .\n\
         ex:g_small geo:asWKT \"POINT(1 2)\"^^geo:wktLiteral .\n\
         ex:f_small2 geo:hasGeometry ex:g_small2 .\n\
         ex:g_small2 geo:asWKT \"POINT(1 2)\"^^geo:wktLiteral .\n\
         ex:q_small ex:wkt \"POINT(1 2)\"^^geo:wktLiteral .\n\
         ex:q_big ex:wkt \"{big_wkt}\"^^geo:wktLiteral .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let big_id = store
        .snapshot()
        .lookup_term(&Term::Literal(Literal::new_typed_literal(
            &big_wkt,
            oxrdf::NamedNode::new_unchecked(crate::geo::vocab::WKT_LITERAL),
        )))
        .expect("big WKT literal interned");
    assert_eq!(big_id.tag(), Tag::Vocab);

    // 1. GeoMemo::by_id_checked & by_id refuse under tight budget and NEVER cache None!
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 16 * 1024;
        assert_memory_budget_error(
            crate::geo::memo::by_id_checked(&ctx, big_id),
            "GeoMemo::by_id_checked refuses oversized WKT",
        );
        // Raise budget on the SAME ctx and retry: must parse and return Some(geom),
        // proving refusal was NOT negatively cached in GeoMemo!
        ctx.mem_limit = MIB;
        let geom = crate::geo::memo::by_id_checked(&ctx, big_id)
            .expect("retry after raising budget succeeds")
            .expect("valid WKT parses on retry");
        assert!(!geom.empty);
    }
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 16 * 1024;
        assert!(crate::geo::memo::by_id(&ctx, big_id, None).is_err());
        assert_memory_budget_error(
            ctx.check(),
            "GeoMemo::by_id latches memory refusal onto ctx",
        );
    }

    let budget = 64 * 1024;

    // 2. spatialF:transformSRS, geof:envelope, spatial:nearbyGeom, geo join Side::of_table
    let sf_cases: &[(&str, &str, &str)] = &[
        (
            "spatialF:transformSRS",
            "PREFIX ex: <http://example.org/> \
             PREFIX spatialF: <http://jena.apache.org/function/spatial#> \
             SELECT ?out WHERE { ex:q_small ex:wkt ?g . BIND(spatialF:transformSRS(?g, <http://www.opengis.net/def/crs/EPSG/0/4326>) AS ?out) }",
            "PREFIX ex: <http://example.org/> \
             PREFIX spatialF: <http://jena.apache.org/function/spatial#> \
             SELECT ?out WHERE { ex:q_big ex:wkt ?g . BIND(spatialF:transformSRS(?g, <http://www.opengis.net/def/crs/EPSG/0/4326>) AS ?out) }",
        ),
        (
            "geof:envelope",
            "PREFIX ex: <http://example.org/> \
             PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
             SELECT ?out WHERE { ex:q_small ex:wkt ?g . BIND(geof:envelope(?g) AS ?out) }",
            "PREFIX ex: <http://example.org/> \
             PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
             SELECT ?out WHERE { ex:q_big ex:wkt ?g . BIND(geof:envelope(?g) AS ?out) }",
        ),
        (
            "spatial:nearbyGeom bound arg",
            "PREFIX ex: <http://example.org/> \
             PREFIX spatial: <http://jena.apache.org/spatial#> \
             SELECT ?f WHERE { ex:q_small ex:wkt ?g . ?f spatial:nearbyGeom (?g 1000 <http://www.opengis.net/def/uom/OGC/1.0/metre>) }",
            "PREFIX ex: <http://example.org/> \
             PREFIX spatial: <http://jena.apache.org/spatial#> \
             SELECT ?f WHERE { ex:q_big ex:wkt ?g . ?f spatial:nearbyGeom (?g 1000 <http://www.opengis.net/def/uom/OGC/1.0/metre>) }",
        ),
        (
            "geo join Side::of_table",
            "PREFIX ex: <http://example.org/> \
             PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
             SELECT ?g1 ?g2 WHERE { ex:q_small ex:wkt ?g1 . ex:q_small ex:wkt ?g2 . FILTER(geof:sfIntersects(?g1, ?g2)) }",
            "PREFIX ex: <http://example.org/> \
             PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
             SELECT ?g1 ?g2 WHERE { ex:q_small ex:wkt ?g1 . ex:q_big ex:wkt ?g2 . FILTER(geof:sfIntersects(?g1, ?g2)) }",
        ),
    ];

    for (label, ctrl_q, tgt_q) in sf_cases {
        run_select_both_modes(&store, ctrl_q, budget, label, true);
        run_select_both_modes(&store, tgt_q, budget, label, false);
    }

    // 3. geo/rewrite (geo:sfIntersects topological property rewrite) & geo/search fallback:
    let small_ttl = "@prefix ex: <http://example.org/> .\n\
        @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n\
        ex:f1 geo:hasDefaultGeometry ex:g1 .\n\
        ex:g1 geo:asWKT \"POINT(1 2)\"^^geo:wktLiteral .\n\
        ex:f2 geo:hasDefaultGeometry ex:g2 .\n\
        ex:g2 geo:asWKT \"POINT(1 2)\"^^geo:wktLiteral .\n";
    let small_store = store_with_turtle(small_ttl, true);
    small_store.pause_geo_build(true);
    small_store
        .enable_geo(crate::geo::GeoConfig {
            query_rewrite: true,
            ..crate::geo::GeoConfig::default()
        })
        .unwrap();
    let big_rewrite_ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n\
         ex:f1 geo:hasDefaultGeometry ex:g1 .\n\
         ex:g1 geo:asWKT \"POINT(1 2)\"^^geo:wktLiteral .\n\
         ex:f2 geo:hasDefaultGeometry ex:g2 .\n\
         ex:g2 geo:asWKT \"{big_wkt}\"^^geo:wktLiteral .\n"
    );
    let big_rewrite_store = store_with_turtle(&big_rewrite_ttl, true);
    big_rewrite_store.pause_geo_build(true);
    big_rewrite_store
        .enable_geo(crate::geo::GeoConfig {
            query_rewrite: true,
            ..crate::geo::GeoConfig::default()
        })
        .unwrap();
    let rewrite_q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        SELECT ?a ?b WHERE { ?a geo:sfIntersects ?b }";
    run_select_both_modes(
        &small_store,
        rewrite_q,
        budget,
        "geo:sfIntersects rewrite",
        true,
    );
    run_select_both_modes(
        &big_rewrite_store,
        rewrite_q,
        budget,
        "geo:sfIntersects rewrite",
        false,
    );

    // 4. geo/search fallback (Literals::entry) on store containing oversized geo:asWKT:
    let search_q = "PREFIX spatial: <http://jena.apache.org/spatial#> \
        PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        SELECT ?f WHERE { ?f spatial:nearbyGeom (\"POINT(1 2)\"^^geo:wktLiteral 1000 <http://www.opengis.net/def/uom/OGC/1.0/metre>) }";
    run_select_both_modes(
        &small_store,
        search_q,
        budget,
        "geo/search fallback Literals::entry",
        true,
    );
    run_select_both_modes(
        &big_rewrite_store,
        search_q,
        budget,
        "geo/search fallback Literals::entry",
        false,
    );
}

// =========================================================================
// T10: Large ORDER BY / top-k payload pre-admission (Sol #2)
// =========================================================================

#[test]
fn large_order_by_and_topk_admit_payloads_before_retaining() {
    // > PAR_THRESHOLD (16,384) rows so full sort and top-k hit the large-input parallel
    // `key_column_charged` / `key_per_value` / `offer_rows` branches.
    let n = 17_000usize;
    let pad_a = "a".repeat(128);
    let pad_b = "b".repeat(128);
    let mut ttl = String::with_capacity(n * 320);
    ttl.push_str("@prefix ex: <http://example.org/> .\n");
    for i in 0..n {
        let bucket = i % 32;
        ttl.push_str(&format!(
            "ex:r{i} ex:k {bucket} ; ex:a \"{i:05}_{pad_a}\" ; ex:b \"{i:05}_{pad_b}\" .\n"
        ));
    }
    let store = store_with_turtle(&ttl, true);

    // 1. Full sort on two-variable CONCAT(?a, ?b): per-distinct reuse declines (2 vars),
    //    so `key_column_charged` (> PAR_THRESHOLD) must admit chunks as they are produced.
    let full_q = "PREFIX ex: <http://example.org/> \
        SELECT ?s WHERE { ?s ex:a ?a ; ex:b ?b } ORDER BY CONCAT(?a, ?b)";
    let tight_full = QueryOptions {
        max_memory_bytes: Some(2 * MIB),
        no_cache: true,
        ..QueryOptions::default()
    };
    assert_memory_budget_error(
        super::query(store.snapshot(), full_q, &tight_full),
        "large ORDER BY CONCAT(?a, ?b) refuses under tight budget",
    );

    // 2. Multi-key top-k heap (ORDER BY ?k CONCAT(?a, ?b, ...) LIMIT 3000):
    //    secondary keys are kept inside EagerHeapRows / cursor Offer and must be charged
    //    live until eviction, refusing when the 3000 kept ~1 KiB strings (~3.2 MiB)
    //    exceed the 2.5 MiB budget, while an identical query without string secondary keys passes.
    let heap_ctrl_q = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?k WHERE { ?s ex:k ?k ; ex:a ?a ; ex:b ?b } \
        ORDER BY ?k LIMIT 3000";
    let heap_q = "PREFIX ex: <http://example.org/> \
        SELECT ?s ?k WHERE { ?s ex:k ?k ; ex:a ?a ; ex:b ?b } \
        ORDER BY ?k CONCAT(?a, ?b, ?a, ?b, ?a, ?b, ?a, ?b) LIMIT 3000";
    let ok_heap = QueryOptions {
        max_memory_bytes: Some(16 * MIB),
        no_cache: true,
        ..QueryOptions::default()
    };
    let ok_res = super::query(store.snapshot(), heap_q, &ok_heap).expect("large top-k control");
    assert_eq!(ok_res.len(), 3000);
    // 2.5 MiB fits the input table + 3000-entry integer heap (~1.8 MiB eager, less in cursor),
    // but refuses in both modes when the 3000 kept ~1 KiB secondary SortKey strings (~3.2 MiB)
    // are admitted into the heap.
    run_select_both_modes(
        &store,
        heap_ctrl_q,
        2560 * 1024,
        "large top-k heap control",
        true,
    );
    run_select_both_modes(
        &store,
        heap_q,
        2560 * 1024,
        "large top-k heap secondary key",
        false,
    );
}

// =========================================================================
// T11: Declined FILTER sampling isolation & control propagation (Sol #4, Neon #7)
// =========================================================================

#[test]
fn declined_filter_sample_releases_decode_state_and_propagates_controls() {
    let med = "m".repeat(2048);
    let big = "b".repeat(16 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s1 ex:v \"{med}\" .\n\
         ex:s2 ex:v \"{big}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let med_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&med)))
        .unwrap();
    let big_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&big)))
        .unwrap();

    // Budget admits medium literal (~17 KiB decode envelope) and refuses big (~129 KiB).
    let mut ctx = Ctx::new(snap.clone());
    ctx.mem_limit = 48 * 1024;
    let baseline = ctx.live_bytes();

    // Trial decodes medium (growing trial's envelope), then refuses big; trial drops and
    // leaves `ctx` at exact baseline live_bytes with no sticky refusal latch.
    let res: Result<Option<usize>> = ctx.with_sampling_trial(|trial| {
        assert!(trial.value(med_id).is_some(), "medium literal fits trial");
        assert!(trial.live_bytes() > 0, "trial charged medium literal");
        let _ = trial.value(big_id);
        trial.check()?;
        Ok(Some(1))
    });
    assert!(matches!(res, Ok(None)), "declined sample returns Ok(None)");
    assert_eq!(
        ctx.live_bytes(),
        baseline,
        "declined sample must release all decode-envelope / cache state"
    );
    ctx.check().expect("declined sample must not poison ctx");
    // Subsequent exact evaluation under the same tight budget succeeds.
    assert!(ctx.value(med_id).is_some());
    ctx.check()
        .expect("exact evaluation succeeds after declined sample");

    let mut cursor = Ctx::new(snap.clone());
    cursor.mem_limit = 48 * 1024;
    cursor.enable_cursor_accounting().unwrap();
    let hold = cursor.charge(4096).unwrap();
    let baseline = cursor.live_bytes();
    let result: Result<Option<()>> = cursor.with_sampling_trial(|trial| {
        let decode = trial.charge(0)?;
        assert!(trial.value_checked_into(med_id, &decode)?.is_some());
        assert!(cursor.live_bytes() >= baseline + med.len() as u64);
        trial.value_checked_into(big_id, &decode)?;
        Ok(Some(()))
    });
    assert!(matches!(result, Ok(None)));
    assert_eq!(cursor.live_bytes(), baseline);
    cursor.check().unwrap();
    drop(hold);

    // Pre-existing refusal on ctx is preserved and propagated (not cleared).
    let mut ctx_latched = Ctx::new(snap.clone());
    ctx_latched.mem_limit = 48 * 1024;
    assert!(ctx_latched.value(big_id).is_none());
    assert!(ctx_latched.has_memory_failure());
    assert_memory_budget_error(
        ctx_latched.with_sampling_trial(|_| Ok(Some(1usize))),
        "pre-existing refusal preserved through with_sampling_trial",
    );

    // Cancellation propagates through with_sampling_trial.
    let ctx_cancel = Ctx::new(snap.clone());
    ctx_cancel.cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        ctx_cancel.with_sampling_trial(|_| Ok(Some(1usize))),
        Err(Error::Cancelled)
    ));

    // Timeout propagates through with_sampling_trial.
    let mut ctx_to = Ctx::new(snap);
    ctx_to.deadline = Some(Instant::now() - Duration::from_secs(1));
    assert!(matches!(
        ctx_to.with_sampling_trial(|_| Ok(Some(1usize))),
        Err(Error::Timeout)
    ));
}

#[test]
fn filter_trial_admits_setup_before_copying() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    const SETUP: usize = 128 * 1024;
    ctx.dataset.default = Some(vec![Id::DEFAULT_GRAPH; SETUP / 8]);
    ctx.dataset.named = Some(vec![Id::vocab(1); SETUP / 8]);
    ctx.base_iri = Some(oxiri::Iri::parse(format!("urn:base:{}", "x".repeat(SETUP))).unwrap());
    let v = ctx.var(&"v".repeat(SETUP));
    let local = ctx.intern_term(&Term::Literal(Literal::new_simple_literal("local")));
    ctx.mem_limit = ctx.live_bytes() + MIB;
    ctx.enable_cursor_accounting().unwrap();
    let hold = hold_all_but(&ctx, 16 * 1024);
    let baseline = ctx.live_bytes();
    let mut result = None;
    let peak = crate::test_alloc::record_peak_live(|| {
        result = Some(ctx.with_sampling_trial(|_| Ok(Some(()))));
    });
    assert!(peak < SETUP, "trial allocated {peak} bytes before refusing");
    assert!(matches!(result.unwrap(), Ok(None)));
    assert_eq!(ctx.live_bytes(), baseline);
    ctx.check().unwrap();
    drop(hold);

    let baseline = ctx.live_bytes();
    let result = ctx
        .with_sampling_trial(|trial| {
            assert!(ctx.live_bytes() >= baseline + 4 * SETUP as u64);
            assert_eq!(trial.dataset.default, ctx.dataset.default);
            assert_eq!(trial.dataset.named, ctx.dataset.named);
            assert_eq!(trial.base_iri, ctx.base_iri);
            assert_eq!(trial.var_name(v), ctx.var_name(v));
            assert!(trial.term_checked(local)?.is_some());
            Ok(Some(()))
        })
        .unwrap();
    assert_eq!(result, Some(()));
    assert_eq!(ctx.live_bytes(), baseline);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn filter_trial_admits_vocabulary_after_map_growth() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    // Just past 7,168 members, the index grows to 16,384 buckets.
    const ENTRIES: u64 = 7169;
    const COPY_BYTES: u64 = 16384 * 25 + ENTRIES * 16;
    let mut last = Id::UNDEF;
    for i in 0..ENTRIES {
        last = ctx.intern_term(&Term::Literal(Literal::new_simple_literal(i.to_string())));
    }
    assert_eq!(last, Id::local(ENTRIES - 1));
    ctx.mem_limit = ctx.live_bytes() + MIB;
    ctx.enable_cursor_accounting().unwrap();

    // The old 64-byte estimate per term fits, but the backing storage does not.
    let hold = hold_all_but(&ctx, 480 * 1024);
    let baseline = ctx.live_bytes();
    let mut result = None;
    let peak = crate::test_alloc::record_peak_live(|| {
        result = Some(ctx.with_sampling_trial(|_| Ok(Some(()))));
    });
    assert!(
        peak < 8 * 1024,
        "trial allocated {peak} bytes before refusal"
    );
    assert!(matches!(result.unwrap(), Ok(None)));
    assert_eq!(ctx.live_bytes(), baseline);
    ctx.check().unwrap();
    drop(hold);

    let baseline = ctx.live_bytes();
    let result = ctx
        .with_sampling_trial(|trial| {
            assert!(ctx.live_bytes() >= baseline + COPY_BYTES);
            assert_eq!(
                trial.term_checked(last)?,
                Some(Term::Literal(Literal::new_simple_literal(
                    (ENTRIES - 1).to_string()
                )))
            );
            Ok(Some(()))
        })
        .unwrap();
    assert_eq!(result, Some(()));
    assert_eq!(ctx.live_bytes(), baseline);
}

// =========================================================================
// T12: Decode-envelope mutex released before pool wake (Sol #5)
// =========================================================================

#[test]
fn grow_decode_leaves_envelope_mutex_before_pool_wake() {
    use std::sync::atomic::AtomicUsize;

    let store = Store::in_memory(StoreOptions::default());
    for limit in [4096, MIB] {
        let pool = super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());

        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = limit;
        ctx.set_grant(Some(grant.clone()));
        ctx.enable_cursor_accounting().unwrap();
        let ctx = ctx.into_shared();
        grant.cover_transient(64 * MIB).unwrap();

        // Trimming the unused grant admits the waiter. Its synchronous callback
        // probes the mutex without blocking, so the old lock order fails promptly.
        let seen = Arc::new(AtomicUsize::new(0));
        let callback = {
            let seen = seen.clone();
            let ctx = ctx.clone();
            Arc::new(move || {
                let unlocked = ctx.decode_state_unlocked();
                seen.store(if unlocked { 1 } else { 2 }, Ordering::SeqCst);
                if unlocked {
                    ctx.reserve_decode_checked(32).unwrap();
                }
            })
        };
        let _ticket = pool.enter(80 * MIB, Some(callback));

        let result = ctx.reserve_decode_checked(4096);
        assert_eq!(result.is_ok(), limit == MIB);
        assert_eq!(seen.load(Ordering::SeqCst), 1, "limit {limit}");
        ctx.release_work();
        assert_eq!(ctx.live_bytes(), 0);
    }
}

// =========================================================================
// T13: Triple-term framing & depth validation + non-allocating entry_bytes (Sol #7)
// =========================================================================

#[test]
fn triple_key_framing_and_depth_validated_before_recursive_decode() {
    use crate::id::term_key;
    use oxrdf::{NamedNode, Triple};

    fn write_varint(mut n: u64, out: &mut Vec<u8>) {
        loop {
            let mut b = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                b |= 0x80;
            }
            out.push(b);
            if n == 0 {
                break;
            }
        }
    }

    fn encode_triple_key(s: &[u8], p: &[u8], o: &[u8]) -> Vec<u8> {
        let mut out = vec![b'('];
        for part in [s, p, o] {
            write_varint(part.len() as u64, &mut out);
            out.extend_from_slice(part);
        }
        out
    }

    // Build a valid nested triple key of depth `depth` (depth=1 is a single triple term).
    fn nested_triple_key(depth: usize) -> Vec<u8> {
        let s = term_key(&Term::NamedNode(NamedNode::new_unchecked("http://x/s")));
        let p = term_key(&Term::NamedNode(NamedNode::new_unchecked("http://x/p")));
        let mut obj = term_key(&Term::Literal(Literal::new_simple_literal("leaf")));
        for _ in 0..depth {
            obj = encode_triple_key(&s, &p, &obj);
        }
        obj
    }

    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = 8 * MIB;

    // 1. Valid maximum-depth triple decodes cleanly across checked and infallible decoders.
    let valid_key = nested_triple_key(crate::nesting::MAX_TRIPLE_TERMS);
    let valid_id = ctx.intern_local_raw_for_test(&valid_key);
    assert!(matches!(
        ctx.term_checked(valid_id).unwrap(),
        Some(Term::Triple(_))
    ));
    assert!(matches!(
        ctx.value_checked(valid_id).unwrap(),
        Some(super::value::Value::Triple(_))
    ));
    assert!(matches!(
        ctx.cursor_value_checked(valid_id).unwrap(),
        Some(super::value::Value::Triple(_))
    ));

    // 2. Malformed / truncated triple key returns Error::Corrupt (never panics).
    let bad_key = b"(\x05abc".to_vec();
    let bad_id = ctx.intern_local_raw_for_test(&bad_key);
    assert!(matches!(ctx.term_checked(bad_id), Err(Error::Corrupt(_))));
    assert!(matches!(ctx.value_checked(bad_id), Err(Error::Corrupt(_))));
    assert!(matches!(
        ctx.cursor_value_checked(bad_id),
        Err(Error::Corrupt(_))
    ));

    // 3. Over-depth triple key (MAX_TRIPLE_TERMS + 1) returns Error::Corrupt across all decoders.
    let over_key = nested_triple_key(crate::nesting::MAX_TRIPLE_TERMS + 1);
    let over_id = ctx.intern_local_raw_for_test(&over_key);
    assert!(matches!(ctx.term_checked(over_id), Err(Error::Corrupt(_))));
    assert!(matches!(ctx.value_checked(over_id), Err(Error::Corrupt(_))));
    assert!(matches!(
        ctx.cursor_value_checked(over_id),
        Err(Error::Corrupt(_))
    ));

    // Infallible Ctx::term latches Error::Corrupt rather than panicking.
    let mut ctx_inf = Ctx::new(store.snapshot());
    ctx_inf.mem_limit = 8 * MIB;
    let inf_bad = ctx_inf.intern_local_raw_for_test(&bad_key);
    assert!(ctx_inf.term(inf_bad).is_none());
    assert!(matches!(ctx_inf.check(), Err(Error::Corrupt(_))));

    // 4. ValueCache::entry_bytes on Value::Triple does not allocate `t.to_string()`.
    let big_tr = Arc::new(Triple::new(
        NamedNode::new_unchecked("http://example.org/s"),
        NamedNode::new_unchecked("http://example.org/p"),
        Term::Literal(Literal::new_simple_literal("t".repeat(32 * 1024))),
    ));
    let val = super::value::Value::Triple(big_tr);
    let mut est = 0u64;
    let largest = crate::test_alloc::record_largest(|| {
        est = super::ctx::ValueCache::entry_bytes(&val);
    });
    assert!(est >= 32 * 1024);
    assert_eq!(
        largest, 0,
        "ValueCache::entry_bytes must not allocate when sizing Value::Triple (largest={largest})"
    );
}

// =========================================================================
// T14: Atomic DatasetSpec restriction (Sol #9)
// =========================================================================

#[test]
fn dataset_spec_restrict_checked_is_atomic_on_named_refusal() {
    use crate::access::{GraphAccess, GraphRule, Graphs};
    use oxrdf::NamedNode;

    let store = store_with_turtle(
        "<http://example.org/s> <http://example.org/p> \"v\" .\n",
        true,
    );
    let access = Arc::new(GraphAccess {
        read: Graphs::Only(GraphRule {
            default_graph: true,
            ..Default::default()
        }),
        write: Graphs::All,
        triples: None,
        views: Default::default(),
    });
    let big_g = format!("http://example.org/graph/{}", "g".repeat(16 * 1024));
    let small_g = "http://example.org/graph/small";

    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let small_gid = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(small_g)));
    let big_gid = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(&big_g)));
    ctx.dataset.default = Some(vec![small_gid]);
    ctx.dataset.named = Some(vec![big_gid]);
    ctx.dataset.union_default = false;
    let orig_default = ctx.dataset.default.clone();
    let orig_named = ctx.dataset.named.clone();
    let orig_union = ctx.dataset.union_default;

    // Headroom admits `small_gid` in default pass and refuses `big_gid` in named pass.
    ctx.mem_limit = ctx.live_bytes() + 16 * 1024;
    assert_memory_budget_error(
        super::restrict_ctx(&mut ctx, Some(&access)),
        "restrict_ctx refuses in named pass",
    );
    assert_eq!(
        ctx.dataset.default, orig_default,
        "default must not mutate on error"
    );
    assert_eq!(
        ctx.dataset.named, orig_named,
        "named must not become None on error"
    );
    assert_eq!(ctx.dataset.union_default, orig_union);

    // Direct DatasetSpec::restrict_checked atomicity check.
    let mut ds = super::ctx::DatasetSpec {
        default: Some(vec![small_gid]),
        named: Some(vec![big_gid]),
        union_default: false,
    };
    let snap = store.snapshot();
    let bound = access.bound(&snap);
    assert_memory_budget_error(
        ds.restrict_checked(&snap, &bound, &|id| ctx.term_checked(id)),
        "DatasetSpec::restrict_checked refuses in named pass",
    );
    assert_eq!(ds.default, Some(vec![small_gid]));
    assert_eq!(ds.named, Some(vec![big_gid]));
    assert!(!ds.union_default);
}

// =========================================================================
// T15: Streamed DESCRIBE handoff & RDF 1.2 reifiers + cursor SERVICE + text highlight (Sol #8, #10, Neon #8, #9)
// =========================================================================

#[test]
fn describe_cursor_handoff_reifiers_cursor_service_and_cdt_strict_decode() {
    // 1. Streamed DESCRIBE via graph_cursor: success + refusal unwinding + RDF 1.2 reifiers.
    let big_lit = "r".repeat(16 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         ex:small ex:p \"ok\" .\n\
         ex:big ex:p \"{big_lit}\" .\n\
         ex:reif_small rdf:reifies <<( ex:rs ex:p \"ok\" )>> ; ex:note \"n1\" .\n\
         ex:rs ex:p \"ok\" .\n\
         ex:reif_big rdf:reifies <<( ex:rb ex:p \"{big_lit}\" )>> ; ex:note \"n2\" .\n\
         ex:rb ex:p \"{big_lit}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let cursor_opts = CursorOptions {
        batch_rows: 16,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };

    // Streamed DESCRIBE positive control
    let mut cur_ok = super::graph_cursor(
        store.snapshot(),
        "PREFIX ex: <http://example.org/> DESCRIBE ex:small",
        &QueryOptions {
            max_memory_bytes: Some(256 * 1024),
            no_cache: true,
            ..QueryOptions::default()
        },
        &cursor_opts,
    )
    .expect("streamed DESCRIBE control open");
    let mut count = 0usize;
    while let Some(b) = cur_ok
        .next_batch()
        .expect("streamed DESCRIBE control batch")
    {
        count += b.len();
    }
    assert_eq!(count, 1);

    // Streamed DESCRIBE refusal on oversized literal
    let streamed_err: Result<usize> = (|| {
        let mut cur = super::graph_cursor(
            store.snapshot(),
            "PREFIX ex: <http://example.org/> DESCRIBE ex:big",
            &QueryOptions {
                max_memory_bytes: Some(40 * 1024),
                no_cache: true,
                ..QueryOptions::default()
            },
            &cursor_opts,
        )?;
        let mut n = 0;
        while let Some(b) = cur.next_batch()? {
            n += b.len();
        }
        Ok(n)
    })();
    assert_memory_budget_error(streamed_err, "streamed DESCRIBE refuses oversized literal");

    // DESCRIBE with RDF 1.2 reifiers (`reifiers_of`) control & refusal
    let reif_ok = super::query(
        store.snapshot(),
        "PREFIX ex: <http://example.org/> DESCRIBE ex:rs",
        &QueryOptions {
            max_memory_bytes: Some(256 * 1024),
            no_cache: true,
            ..QueryOptions::default()
        },
    )
    .expect("DESCRIBE reifiers control");
    assert!(reif_ok.len() >= 2, "reifier description included");
    assert_memory_budget_error(
        super::query(
            store.snapshot(),
            "PREFIX ex: <http://example.org/> DESCRIBE ex:rb",
            &QueryOptions {
                max_memory_bytes: Some(40 * 1024),
                no_cache: true,
                ..QueryOptions::default()
            },
        ),
        "DESCRIBE reifiers_of refuses oversized reified literal",
    );

    // 2. Cursor-mode SERVICE endpoint and cache-key refusal (exercises cursor fallback on
    //    this branch before origin/main's ServiceReader integration).
    let long_url = format!("http://example.org/endpoint/{}", "e".repeat(16 * 1024));
    let store_svc = store_with_turtle(
        &format!("<{long_url}> <http://example.org/p> \"v\" .\n"),
        true,
    );
    let svc_q = format!("SELECT ?x WHERE {{ SERVICE <{long_url}> {{ ?x ?y ?z }} }}");
    let svc_cur_res: Result<()> = (|| {
        let mut cur = super::select_cursor(
            store_svc.snapshot(),
            &svc_q,
            &QueryOptions {
                allow_service: true,
                no_cache: true,
                max_memory_bytes: Some(32 * 1024),
                ..QueryOptions::default()
            },
            &CursorOptions {
                fallback: FallbackPolicy::AllowMaterialization,
                ..Default::default()
            },
        )?;
        while cur.next_batch()?.is_some() {}
        Ok(())
    })();
    assert_memory_budget_error(
        svc_cur_res,
        "cursor SERVICE endpoint decode refusal must return BudgetKind::Memory",
    );

    // 3. CDT binary ops (`containsKey`, `get`, `remove`) & constructor output admission.
    let cdt_big = format!("[\"{}\"]", "c".repeat(16 * 1024));
    let cdt_ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> .\n\
         ex:big ex:l \"{}\"^^cdt:List ; ex:k \"{}\" .\n\
         ex:small ex:l \"[1, 2]\"^^cdt:List ; ex:k \"1\" .\n",
        cdt_big.replace('"', "\\\""),
        "c".repeat(16 * 1024)
    );
    let cdt_store = store_with_turtle(&cdt_ttl, true);
    let cdt_ctrl = "PREFIX ex: <http://example.org/> \
        PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> \
        SELECT ?ok WHERE { ex:small ex:l ?l ; ex:k ?k . BIND(cdt:contains(?l, ?k) AS ?ok) }";
    let cdt_tgt = "PREFIX ex: <http://example.org/> \
        PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> \
        SELECT ?ok WHERE { ex:big ex:l ?l ; ex:k ?k . BIND(cdt:contains(?l, ?k) AS ?ok) }";
    run_select_both_modes(&cdt_store, cdt_ctrl, 48 * 1024, "cdt:contains", true);
    run_select_both_modes(&cdt_store, cdt_tgt, 48 * 1024, "cdt:contains", false);
}

#[cfg(feature = "text")]
#[test]
fn text_highlight_strict_decode() {
    // Text highlighting (`text::search::search_in` / `text_bound`) strict decode refusal.
    let big_text = format!("needle {}", "w".repeat(16 * 1024));
    let text_store = Store::in_memory(StoreOptions::default());
    text_store
        .load(&[Source::from_bytes(
            format!(
                "@prefix ex: <http://example.org/> .\n\
                 ex:d_small ex:body \"needle small\" .\n\
                 ex:d_big ex:body \"{big_text}\" .\n"
            )
            .into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    text_store.compact().unwrap();
    text_store
        .enable_text(crate::text::TextConfig {
            predicates: crate::text::PredicateSet::Only(vec!["http://example.org/body".into()]),
            ..Default::default()
        })
        .unwrap();
    let hl_q = "PREFIX ex: <http://example.org/> \
        PREFIX text: <http://jena.apache.org/text#> \
        SELECT ?s ?lit WHERE { (?s ?sc ?lit) text:query (ex:body \"needle\" \"highlight:\") }";
    let hl_ok = super::query(
        text_store.snapshot(),
        hl_q,
        &QueryOptions {
            max_memory_bytes: Some(MIB),
            no_cache: true,
            ..QueryOptions::default()
        },
    )
    .expect("text highlight control");
    assert_eq!(hl_ok.len(), 2);
    assert_memory_budget_error(
        super::query(
            text_store.snapshot(),
            hl_q,
            &QueryOptions {
                max_memory_bytes: Some(48 * 1024),
                no_cache: true,
                ..QueryOptions::default()
            },
        ),
        "text highlight refuses oversized body literal",
    );
}

// =========================================================================
// T16: GeoSPARQL cumulative credit for search/recheck/join & KNN/spatialF (Sol #6, Neon #6, #9)
// =========================================================================

#[cfg(feature = "geo")]
#[test]
fn geo_search_join_knn_and_spatialf_cumulative_credit_strict_decode() {
    // 180 distinct moderate LINESTRING geometries (~700 B WKT each, individually fitting
    // easily under 128 KiB, collectively exhausting the budget when retained in
    // `Literals::memo`, `Rechecked::memo`, or `geo::join::Side`).
    let make_line = |idx: usize| -> String {
        let mut s = String::from("LINESTRING(");
        for j in 0..40 {
            if j > 0 {
                s.push_str(", ");
            }
            let x = (idx % 20) as f64 * 0.01 + j as f64 * 0.0001;
            let y = (idx / 20) as f64 * 0.01 + j as f64 * 0.0001;
            s.push_str(&format!("{x:.4} {y:.4}"));
        }
        s.push(')');
        s
    };

    let mut small_ttl = String::from(
        "@prefix ex: <http://example.org/> .\n\
         @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n",
    );
    let mut many_ttl = small_ttl.clone();
    for i in 0..5 {
        let wkt = make_line(i);
        small_ttl.push_str(&format!("ex:f{i} geo:asWKT \"{wkt}\"^^geo:wktLiteral .\n"));
    }
    for i in 0..180 {
        let wkt = make_line(i);
        many_ttl.push_str(&format!("ex:f{i} geo:asWKT \"{wkt}\"^^geo:wktLiteral .\n"));
    }

    // 1. Fallback `Literals::entry` cumulative retention (pause geo build -> scan_fallback).
    let small_store = store_with_turtle(&small_ttl, true);
    small_store.pause_geo_build(true);
    small_store
        .enable_geo(crate::geo::GeoConfig::default())
        .unwrap();

    let many_store = store_with_turtle(&many_ttl, true);
    many_store.pause_geo_build(true);
    many_store
        .enable_geo(crate::geo::GeoConfig::default())
        .unwrap();

    let budget = 160 * 1024;
    let search_q = "PREFIX spatial: <http://jena.apache.org/spatial#> \
        PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        SELECT ?f WHERE { ?f spatial:nearbyGeom (\"POINT(0.05 0.05)\"^^geo:wktLiteral 50000 <http://www.opengis.net/def/uom/OGC/1.0/metre>) }";
    run_select_both_modes(
        &small_store,
        search_q,
        budget,
        "geo fallback Literals cumulative control",
        true,
    );
    run_select_both_modes(
        &many_store,
        search_q,
        budget,
        "geo fallback Literals cumulative refusal",
        false,
    );

    // 2. `Rechecked::entry` cumulative retention (low max_geometry_bytes forces every
    //    LINESTRING into Slot::Skipped(TooLarge) -> Rechecked::entry).
    let recheck_store = store_with_turtle(&many_ttl, true);
    recheck_store
        .enable_geo(crate::geo::GeoConfig {
            max_geometry_bytes: 64,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(recheck_store.wait_geo().unwrap().state, "ready");
    run_select_both_modes(
        &recheck_store,
        search_q,
        budget,
        "geo Rechecked cumulative refusal",
        false,
    );

    // 3. `geo::join::Side::of_table` cumulative retention across two sides (ls stays live
    //    while rs is built).
    let join_q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
        SELECT ?f1 ?f2 WHERE { ?f1 geo:asWKT ?g1 . ?f2 geo:asWKT ?g2 . FILTER(geof:sfIntersects(?g1, ?g2)) }";
    run_select_both_modes(
        &small_store,
        join_q,
        budget,
        "geo join Side cumulative control",
        true,
    );
    run_select_both_modes(
        &many_store,
        join_q,
        budget,
        "geo join Side cumulative refusal",
        false,
    );

    // 4. `geo::knn` (spatial:nearbyGeom with limit / ORDER BY distance) refusal on oversized WKT.
    let mut big_line = String::from("LINESTRING(");
    for i in 0..800 {
        if i > 0 {
            big_line.push_str(", ");
        }
        big_line.push_str(&format!("{:.4} {:.4}", 1.0 + i as f64 * 0.0001, 2.0));
    }
    big_line.push(')');
    let knn_store = store_with_turtle(
        &format!(
            "@prefix ex: <http://example.org/> .\n\
             @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n\
             ex:f1 geo:asWKT \"{big_line}\"^^geo:wktLiteral .\n"
        ),
        true,
    );
    knn_store.pause_geo_build(true);
    knn_store
        .enable_geo(crate::geo::GeoConfig::default())
        .unwrap();
    let knn_q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
        PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/metre> \
        SELECT ?f ?d WHERE { \
            ?f geo:asWKT ?g . \
            BIND(geof:distance(?g, \"POINT(1 2)\"^^geo:wktLiteral, uom:) AS ?d) \
        } ORDER BY ?d LIMIT 1";
    run_select_both_modes(&knn_store, knn_q, 64 * 1024, "geo knn refusal", false);
}
