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
        SELECT ?o WHERE { ex:r_small rdf:reifies ?t . <<( ex:s ex:p ?o )>> }";
    let _ = unpack_ctrl; // also test explicit reifier unpack via ?t bound to triple term
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
