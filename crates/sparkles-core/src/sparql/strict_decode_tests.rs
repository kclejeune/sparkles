//! Regression tests for roadmap item 1308 Wave 1: strict term and value decoding
//! across SPARQL UPDATE, ARQ property functions, registered extensions, custom
//! aggregates, hybrid search, history service, path search, vector top-k, remote
//! enhancers, GeoSPARQL planner rewrites, planner range/lexical probes and RDFS
//! schema/explanation decoding.

use super::extensions::{
    AggregateAccumulator, AggregateContext, AggregateDescriptor, AggregateFactory, ArgumentShape,
    PropertyContext, PropertyDescriptor, PropertyFunction, PropertyInput, PropertyPosition,
    PropertyRow, PropertyStream, ScalarError, ScalarResult,
};
use super::lateral::LateralSpec;
use super::plan::{
    ActiveGraph, Agg, GraphFilter, Kind, Node, PT, PathEnd, Planner, ScanSpec, TextSpec,
};
use super::table::Table;
use super::*;
use crate::access::{GraphAccess, GraphRule, Graphs};
use crate::error::{BudgetKind, Error};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};
use oxrdf::vocab::{rdf, rdfs, xsd};
use oxrdf::{Literal, NamedNode, Term};
use spargebra::algebra::{AggregateFunction, GraphPattern};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern, Variable};
use std::sync::Arc;

const MIB: u64 = 1 << 20;

fn assert_memory_budget_error<T>(res: Result<T>, what: &str) {
    match res {
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory => {}
        Err(other) => panic!("{what}: expected Memory BudgetExceeded, got Err({other:?})"),
        Ok(_) => panic!("{what}: expected Memory BudgetExceeded, got Ok(_)"),
    }
}

/// Reserve all but `headroom` bytes of `ctx.mem_limit`, returning the guard charge.
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

#[test]
fn ctx_checked_decoders_refuse_under_tight_budget_in_both_modes() {
    let payload = "v".repeat(2048);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{payload}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let delta_lit = Term::Literal(Literal::new_simple_literal("d".repeat(2048)));
    store
        .load(&[Source::from_bytes(
            format!(
                "@prefix ex: <http://example.org/> .\n\
                 ex:s ex:q \"{}\" .\n",
                "d".repeat(2048)
            )
            .into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let vocab_lit = Term::Literal(Literal::new_simple_literal(payload));
    let snap = store.snapshot();
    let vocab_id = snap.lookup_term(&vocab_lit).unwrap();
    let delta_id = snap.lookup_term(&delta_lit).unwrap();
    assert_eq!(vocab_id.tag(), Tag::Vocab);
    assert_eq!(delta_id.tag(), Tag::Delta);

    for cursor in [false, true] {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        if cursor {
            ctx.enable_cursor_accounting().unwrap();
        }
        let local_id = ctx.intern_term(&Term::Literal(Literal::new_simple_literal(
            "l".repeat(2048),
        )));
        assert_eq!(local_id.tag(), Tag::Local);
        let _held = hold_all_but(&ctx, 100);
        for (label, id) in [
            ("vocab", vocab_id),
            ("delta", delta_id),
            ("local", local_id),
        ] {
            assert_memory_budget_error(
                ctx.term_checked(id),
                &format!("term_checked {label} cursor={cursor}"),
            );
            assert_memory_budget_error(
                ctx.value_checked(id),
                &format!("value_checked {label} cursor={cursor}"),
            );
        }
    }
}

#[test]
fn update_check_graph_id_propagates_memory_error_instead_of_default_graph() {
    let graph_iri = format!("http://example.org/graph/{}", "g".repeat(1024));
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let graph_id = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(&graph_iri)));
    assert_eq!(graph_id.tag(), Tag::Local);

    // Policy allows writing ONLY the default graph and forbids every named graph.
    let default_only = Graphs::Only(GraphRule {
        default_graph: true,
        ..Default::default()
    });
    let access = GraphAccess {
        read: Graphs::All,
        write: default_only,
        triples: None,
    };
    assert!(access.writable(None));
    assert!(!access.writable(Some(&Term::NamedNode(NamedNode::new_unchecked(&graph_iri)))));

    let _held = hold_all_but(&ctx, 100);
    let err = super::update::check_graph_id(&access, &ctx, graph_id);
    assert_memory_budget_error(err, "update::check_graph_id on local named graph");
}

#[test]
fn update_check_graph_id_propagates_memory_error_for_compacted_vocab_graph() {
    let graph_iri = format!("http://example.org/graph/{}", "v".repeat(1024));
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            format!(
                "<http://example.org/s> <http://example.org/p> <http://example.org/o> <{graph_iri}> .\n"
            )
            .into_bytes(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    let snap = store.snapshot();
    let graph_id = snap.lookup_iri(&graph_iri).unwrap();
    assert_eq!(graph_id.tag(), Tag::Vocab);

    let mut ctx = Ctx::new(snap);
    ctx.mem_limit = MIB;
    let access = GraphAccess {
        read: Graphs::All,
        write: Graphs::Only(GraphRule {
            default_graph: true,
            ..Default::default()
        }),
        triples: None,
    };
    let _held = hold_all_but(&ctx, 100);
    let err = super::update::check_graph_id(&access, &ctx, graph_id);
    assert_memory_budget_error(err, "update::check_graph_id on vocab named graph");
}

#[test]
fn update_delete_insert_propagates_memory_error_on_decode_instead_of_dropping_quad() {
    // 4 KiB * 2 = 8 KiB local string from CONCAT:
    // - WHERE interning (`retain_bytes(8194 + 96)`) fits easily inside 32 KiB.
    // - Template `to_store` checked decode (`8 * 8194 + 128 = 65,680` bytes) exceeds 32 KiB.
    let big = "u".repeat(4 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{big}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let before = store.snapshot().len();

    let opts = QueryOptions {
        max_memory_bytes: Some(32 * 1024),
        ..Default::default()
    };
    let where_select = "PREFIX ex: <http://example.org/> \
                        SELECT ?out WHERE { ex:s ex:p ?v . BIND(CONCAT(?v, ?v) AS ?out) }";
    let select_res = query(store.snapshot(), where_select, &opts)
        .expect("WHERE clause must fit inside 32 KiB so UPDATE fails specifically in to_store");
    assert_eq!(select_res.len(), 1);

    let res = super::update::update(
        &store,
        "PREFIX ex: <http://example.org/> \
         INSERT { ex:s ex:q ?out } \
         WHERE { ex:s ex:p ?v . BIND(CONCAT(?v, ?v) AS ?out) }",
        &opts,
    );
    assert_memory_budget_error(res, "update INSERT local term decode in to_store");
    assert_eq!(store.snapshot().len(), before);
}

#[test]
fn update_delete_and_insert_triple_templates_and_unknown_graph_id() {
    // 1. DELETE (`gt`) and INSERT (`tp`) nested triple templates under tight memory budget.
    let big = "t".repeat(6 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         ex:s ex:p \"{big}\" .\n\
         ex:r rdf:reifies <<( ex:s ex:p \"{big}\" )>> .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let before = store.snapshot().len();

    // WHERE binds stored ids without decoding the 6 KiB literal (~50 KiB checked decode > 32 KiB).
    let opts = QueryOptions {
        max_memory_bytes: Some(32 * 1024),
        ..Default::default()
    };
    let where_res = query(
        store.snapshot(),
        "PREFIX ex: <http://example.org/> SELECT ?s ?o WHERE { ?s ex:p ?o }",
        &opts,
    )
    .expect("WHERE must fit in 32 KiB so DELETE/INSERT fail specifically in triple-term decode");
    assert_eq!(where_res.len(), 1);

    let del_res = super::update::update(
        &store,
        "PREFIX ex: <http://example.org/> \
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
         DELETE { ex:r rdf:reifies <<( ?s ex:p ?o )>> } \
         WHERE { ?s ex:p ?o }",
        &opts,
    );
    assert_memory_budget_error(del_res, "update DELETE nested triple-term decode (gt)");
    assert_eq!(store.snapshot().len(), before);

    let ins_res = super::update::update(
        &store,
        "PREFIX ex: <http://example.org/> \
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
         INSERT { ex:r2 rdf:reifies <<( ?s ex:p ?o )>> } \
         WHERE { ?s ex:p ?o }",
        &opts,
    );
    assert_memory_budget_error(ins_res, "update INSERT nested triple-term decode (tp)");
    assert_eq!(store.snapshot().len(), before);

    // 2. check_graph_id under a default-allowed policy must reject missing/special graph IDs
    //    with Error::Invalid instead of falling back to default-graph authorization.
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let default_only = GraphAccess {
        read: Graphs::All,
        write: Graphs::Only(GraphRule {
            default_graph: true,
            ..Default::default()
        }),
        triples: None,
    };
    assert!(super::update::check_graph_id(&default_only, &ctx, Id::DEFAULT_GRAPH).is_ok());
    for (label, bad_id) in [
        ("undef", Id::UNDEF),
        ("special", Id::new(Tag::Special, 7)),
        ("missing_vocab", Id::vocab(999_999)),
        ("missing_delta", Id::delta(999_999)),
        ("missing_local", Id::local(999_999)),
    ] {
        let res = super::update::check_graph_id(&default_only, &ctx, bad_id);
        assert!(
            matches!(res, Err(Error::Invalid(_))),
            "check_graph_id({label}) should return Error::Invalid, got {res:?}"
        );
    }
    ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        super::update::check_graph_id(&default_only, &ctx, Id::DEFAULT_GRAPH),
        Err(Error::Cancelled)
    ));
}

#[test]
fn arqpf_propagates_memory_error_instead_of_empty_result() {
    let long = "a".repeat(2048);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         ex:bag a rdf:Bag ; rdf:_1 ex:item1 .\n\
         ex:s ex:val \"{long}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let bag_id = snap.lookup_iri("http://example.org/bag").unwrap();
    let lit_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&long)))
        .unwrap();
    let iri_id = snap.lookup_iri("http://example.org/s").unwrap();

    let mut ctx = Ctx::new(snap);
    ctx.mem_limit = MIB;
    let v_out = ctx.var("out");
    let v_ns = ctx.var("ns");
    let v_loc = ctx.var("loc");
    let sep_id = ctx.intern_term(&Term::Literal(Literal::new_simple_literal(",")));
    let _held = hold_all_but(&ctx, 100);

    // apf:concat
    let spec = super::arqpf::PfSpec {
        kind: super::arqpf::PfKind::Concat,
        slots: vec![PathEnd::Var(v_out), PathEnd::Const(lit_id)],
        graph: GraphFilter::Default,
        inputs: Vec::new(),
    };
    assert_memory_budget_error(
        super::arqpf::run(&ctx, &spec, None, &[v_out]),
        "arqpf Concat",
    );

    // apf:str
    let spec = super::arqpf::PfSpec {
        kind: super::arqpf::PfKind::Str,
        slots: vec![PathEnd::Var(v_out), PathEnd::Const(lit_id)],
        graph: GraphFilter::Default,
        inputs: Vec::new(),
    };
    assert_memory_budget_error(super::arqpf::run(&ctx, &spec, None, &[v_out]), "arqpf Str");

    // apf:splitIRI
    let spec = super::arqpf::PfSpec {
        kind: super::arqpf::PfKind::SplitIri,
        slots: vec![
            PathEnd::Const(iri_id),
            PathEnd::Var(v_ns),
            PathEnd::Var(v_loc),
        ],
        graph: GraphFilter::Default,
        inputs: Vec::new(),
    };
    assert_memory_budget_error(
        super::arqpf::run(&ctx, &spec, None, &[v_ns, v_loc]),
        "arqpf SplitIri",
    );

    // apf:strSplit
    let spec = super::arqpf::PfSpec {
        kind: super::arqpf::PfKind::StrSplit,
        slots: vec![
            PathEnd::Var(v_out),
            PathEnd::Const(lit_id),
            PathEnd::Const(sep_id),
        ],
        graph: GraphFilter::Default,
        inputs: Vec::new(),
    };
    assert_memory_budget_error(
        super::arqpf::run(&ctx, &spec, None, &[v_out]),
        "arqpf StrSplit",
    );

    // apf:assign (distinct constants compare values)
    let spec = super::arqpf::PfSpec {
        kind: super::arqpf::PfKind::Assign,
        slots: vec![PathEnd::Const(lit_id), PathEnd::Const(iri_id)],
        graph: GraphFilter::Default,
        inputs: Vec::new(),
    };
    assert_memory_budget_error(super::arqpf::run(&ctx, &spec, None, &[]), "arqpf Assign");

    // rdfs:container (Lists::number decodes rdf:_1 predicate)
    let spec = super::arqpf::PfSpec {
        kind: super::arqpf::PfKind::Container(None),
        slots: vec![PathEnd::Const(bag_id), PathEnd::Var(v_out)],
        graph: GraphFilter::Default,
        inputs: Vec::new(),
    };
    assert_memory_budget_error(
        super::arqpf::run(&ctx, &spec, None, &[v_out]),
        "arqpf Container Lists::number",
    );
}

#[test]
fn propertyext_propagates_memory_error_instead_of_invalid() {
    let long = "p".repeat(2048);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{long}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let lit_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&long)))
        .unwrap();

    struct ViewReader;
    impl PropertyFunction for ViewReader {
        fn open<'q>(
            &self,
            ctx: PropertyContext<'q>,
            _args: PropertyInput,
        ) -> std::result::Result<Box<dyn PropertyStream + 'q>, ScalarError> {
            let mut cursor = ctx.view().scan(None, None, None)?;
            let _ = cursor.next_batch()?;
            struct Empty;
            impl PropertyStream for Empty {
                fn next(
                    &mut self,
                    _context: &PropertyContext<'_>,
                ) -> std::result::Result<Option<PropertyRow>, ScalarError> {
                    Ok(None)
                }
            }
            Ok(Box::new(Empty))
        }
    }

    let desc = Arc::new(
        PropertyDescriptor::new(
            "urn:test:prop",
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![PropertyPosition::Subject(0)],
            vec![PropertyPosition::Object(0)],
            ViewReader,
        )
        .unwrap(),
    );

    // 1. Property input decode refusal (propertyext.rs:736) must return Memory error, not Error::invalid.
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let v_o = ctx.var("o");
        let spec = super::propertyext::Spec::for_test(
            desc.clone(),
            vec![PT::C(lit_id)],
            vec![PT::V(v_o)],
            Arc::new(GraphFilter::Default),
        );
        let _held = hold_all_but(&ctx, 1024);
        let res = super::propertyext::run(&ctx, &spec, &Table::unit(), &[v_o]);
        assert_memory_budget_error(res, "propertyext input decode");
    }

    // 2. Captured triple read decode refusal (propertyext.rs:289) must return Memory error, not Error::invalid.
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        ctx.calls_extensions = true;
        ctx.extension_failure = Some(Default::default());
        let s_id = store.snapshot().lookup_iri("http://example.org/s").unwrap();
        let v_o = ctx.var("o");
        let spec = super::propertyext::Spec::for_test(
            desc,
            vec![PT::C(s_id)],
            vec![PT::V(v_o)],
            Arc::new(GraphFilter::Default),
        );
        // Leave enough headroom for `run_inner` setup + PropertyContext + input decode of ex:s + open(256),
        // but not enough to decode the 2048-byte literal object (~16 KiB).
        let _held = hold_all_but(&ctx, 4096);
        let res = super::propertyext::run(&ctx, &spec, &Table::unit(), &[v_o]);
        assert_memory_budget_error(res, "propertyext captured triple decode");
    }
}

#[test]
fn registeredagg_propagates_memory_error_instead_of_domain_error() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    ctx.calls_extensions = true;
    ctx.extension_failure = Some(Default::default());
    let v_x = ctx.var("x");
    let lit_id = ctx.intern_term(&Term::Literal(Literal::new_simple_literal(
        "r".repeat(2048),
    )));

    struct NoopAcc;
    impl AggregateAccumulator for NoopAcc {
        fn add(
            &mut self,
            _: &AggregateContext<'_>,
            _: &Term,
        ) -> std::result::Result<(), ScalarError> {
            Ok(())
        }
        fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
            Ok(Term::Literal(Literal::new_simple_literal("ok")))
        }
    }
    struct NoopFactory;
    impl AggregateFactory for NoopFactory {
        fn create(
            &self,
            _: &AggregateContext<'_>,
        ) -> std::result::Result<Box<dyn AggregateAccumulator>, ScalarError> {
            Ok(Box::new(NoopAcc))
        }
    }
    let desc = AggregateDescriptor::new("urn:test:agg", 1, NoopFactory).unwrap();

    let mut table = Table::new(vec![v_x]);
    table.push_row(&[lit_id]);
    let map = table.var_map(ctx.nvars());
    let agg = Agg {
        func: AggregateFunction::Custom(NamedNode::new_unchecked("urn:test:agg")),
        expr: Some(super::expr::Expr::Var(v_x)),
        distinct: false,
        fold: None,
        registered: None,
    };

    // Enough headroom for accumulator setup charges, not enough to decode the 2 KiB local literal (~16 KiB).
    let _held = hold_all_but(&ctx, 1024);
    let res = super::registeredagg::aggregate(&ctx, &table, &map, &[0], &agg, &desc);
    assert_memory_budget_error(res, "registeredagg term decode");
}

#[test]
fn aggext_latches_memory_error_instead_of_silent_undef() {
    let store = Store::in_memory(StoreOptions::default());
    let big_dec = Term::Literal(Literal::new_typed_literal(
        "123456789012345678901234567890.5",
        xsd::DECIMAL,
    ));

    // 1. agg:median via aggext::aggregate
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let id = ctx.intern_term(&big_dec);
        assert_eq!(id.tag(), Tag::Local);
        let _held = hold_all_but(&ctx, 64);
        let median_iri = format!("{}median", spargebra::algebra::ARQ_AGGREGATE_NAMESPACE);
        let out = super::aggext::aggregate(&ctx, &median_iri, &[Ok(id)]);
        assert_eq!(out, Id::UNDEF);
        assert_memory_budget_error(ctx.check(), "aggext::aggregate median latches memory error");
    }

    // 2. StatAcc::add
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let id = ctx.intern_term(&big_dec);
        let _held = hold_all_but(&ctx, 64);
        let mut acc = super::aggext::StatAcc::default();
        acc.add(&ctx, Some(id));
        assert_memory_budget_error(ctx.check(), "aggext::StatAcc::add latches memory error");
    }
}

#[test]
fn hybrid_ranks_and_bound_text_propagate_memory_error() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let v_s = ctx.var("s");
    let v_score = ctx.var("score");
    let s_id = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked("urn:s")));
    let score_id = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
        "0.5",
        xsd::FLOAT,
    )));
    let query_id = ctx.intern_term(&Term::Literal(Literal::new_simple_literal(
        "q".repeat(2048),
    )));
    assert_eq!(score_id.tag(), Tag::Local);

    let mut table = Table::new(vec![v_s, v_score]);
    table.push_row(&[s_id, score_id]);

    let ts = TextSpec {
        subject: PathEnd::Var(v_s),
        predicates: Vec::new(),
        query: String::new(),
        query_var: Some(v_score),
        limit: Some(10),
        lang: None,
        score: None,
        literal: None,
        graph_out: None,
        prop: None,
        rank: None,
        highlight: None,
        graph: GraphFilter::Default,
        graph_var: None,
        dedup: false,
    };

    let _held = hold_all_but(&ctx, 64);
    assert_memory_budget_error(
        super::hybrid::ranks(&ctx, &table, v_s, v_score, None, false),
        "hybrid::ranks score decode",
    );
    assert_memory_budget_error(
        super::hybrid::bound_text(&ctx, &ts, query_id),
        "hybrid::bound_text query decode",
    );
}

#[test]
fn history_svc_slot_and_bind_propagate_memory_error() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let v_s = ctx.var("s");
    let iri_id = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(format!(
        "http://example.org/{}",
        "h".repeat(1024)
    ))));

    let mut planner = Planner::new(&ctx);
    planner.subst.insert(v_s, iri_id);

    let spec = super::history_svc::HistorySpec {
        s: super::history_svc::Slot::Var(v_s),
        p: super::history_svc::Slot::Any,
        o: super::history_svc::Slot::Any,
        g: super::history_svc::Slot::Any,
        op: super::history_svc::Slot::Any,
        commit: None,
        time: None,
        kind: None,
        author: None,
        message: None,
        from: None,
        to: None,
        from_var: None,
        to_var: None,
        limit: None,
        descending: false,
        inputs: vec![v_s],
    };

    let _held = hold_all_but(&ctx, 100);
    let tp = TermPattern::Variable(Variable::new_unchecked("s"));
    assert_memory_budget_error(
        super::history_svc::slot(&planner, &tp, "subject"),
        "history_svc::slot substituted constant decode",
    );
    assert_memory_budget_error(
        super::history_svc::bind(&ctx, &spec, &[iri_id]),
        "history_svc::bind input decode",
    );
}

#[test]
fn pathsearch_weight_and_edge_triple_term_propagate_memory_error() {
    // 1 KiB node IRIs (~8.5 KiB decode each) with 4 KiB headroom:
    // search setup, visits and path output fit inside 4 KiB, while term decoding refuses.
    let long_a = format!("http://example.org/a/{}", "x".repeat(1024));
    let long_b = format!("http://example.org/b/{}", "y".repeat(1024));
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         <{long_a}> ex:e <{long_b}> .\n\
         ex:r rdf:reifies <<( <{long_a}> ex:e <{long_b}> )>> ; ex:w \"123456789012345678901234567890.5\"^^xsd:decimal .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let a_id = snap.lookup_iri(&long_a).unwrap();
    let b_id = snap.lookup_iri(&long_b).unwrap();
    let e_id = snap.lookup_iri("http://example.org/e").unwrap();
    let w_id = snap.lookup_iri("http://example.org/w").unwrap();
    let reifies_id = snap.lookup_iri(rdf::REIFIES.as_str()).unwrap();
    const HEADROOM: u64 = 4096;

    // 1. Weighted path search: verify Dijkstra setup fits inside HEADROOM when reifies is None,
    //    then assert lookup_weight decode refuses with BudgetKind::Memory when reifies is Some.
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let v_cost = ctx.var("cost");
        let base_spec = super::pathsearch::PathSearchSpec {
            algorithm: super::pathsearch::Algorithm::Shortest,
            source: PathEnd::Const(a_id),
            target: PathEnd::Const(b_id),
            predicates: Some(vec![e_id.0]),
            directions: vec![super::pathsearch::Direction::Forward],
            direction: super::pathsearch::Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: Some(3),
            k: 1,
            limit: None,
            max_visited: 1000,
            weight: Some(Some(w_id.0)),
            default_weight: 1.0,
            reifies: None,
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: Some(v_cost),
            graph: GraphFilter::Default,
        };
        let _held = hold_all_but(&ctx, HEADROOM);
        let control = super::pathsearch::run(&ctx, &base_spec, None, None, &[v_cost])
            .expect("weighted search setup without lookup_weight must fit inside HEADROOM");
        assert_eq!(control.0.len(), 1);

        let mut weighted_spec = base_spec;
        weighted_spec.reifies = Some(reifies_id.0);
        let res = super::pathsearch::run(&ctx, &weighted_spec, None, None, &[v_cost]);
        assert_memory_budget_error(res, "pathsearch lookup_weight decode");
    }

    // 2. Unweighted path search: verify bidirectional BFS fits inside HEADROOM without edge
    //    triple-term decoding, then assert triple_term edge decode refuses with BudgetKind::Memory.
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let v_len = ctx.var("len");
        let v_edge = ctx.var("edge");
        let base_spec = super::pathsearch::PathSearchSpec {
            algorithm: super::pathsearch::Algorithm::Shortest,
            source: PathEnd::Const(a_id),
            target: PathEnd::Const(b_id),
            predicates: Some(vec![e_id.0]),
            directions: vec![super::pathsearch::Direction::Forward],
            direction: super::pathsearch::Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: Some(3),
            k: 1,
            limit: None,
            max_visited: 1000,
            weight: None,
            default_weight: 1.0,
            reifies: None,
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: Some(v_len),
            cost: None,
            graph: GraphFilter::Default,
        };
        let _held = hold_all_but(&ctx, HEADROOM);
        let control = super::pathsearch::run(&ctx, &base_spec, None, None, &[v_len])
            .expect("unweighted BFS without edge decode must fit inside HEADROOM");
        assert_eq!(control.0.len(), 1);

        let mut edge_spec = base_spec;
        edge_spec.length = None;
        edge_spec.edge = Some(v_edge);
        let res = super::pathsearch::run(&ctx, &edge_spec, None, None, &[v_edge]);
        assert_memory_budget_error(res, "pathsearch triple_term edge decode");
    }
}

#[test]
fn vectortopk_propagates_memory_error_instead_of_skipping_rewrite() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let v_s = ctx.var("s");
    let v_vec = ctx.var("v");
    let pred_id = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(
        "http://example.org/vec",
    )));
    let query_vec = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
        format!("[{}]", vec!["1.0"; 256].join(",")),
        NamedNode::new_unchecked("http://sparkles.dev/ns/vector#f32"),
    )));

    let scan = Node::leaf(
        Kind::Scan(ScanSpec {
            perm: Perm::Pso,
            prefix: vec![pred_id.0],
            cols: vec![(1, v_s), (2, v_vec)],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: 3,
            dedup: false,
            src: None,
        }),
        vec![v_s, v_vec],
        10.0,
        "scan".into(),
    );
    let key = super::expr::Expr::Call(
        super::expr::Func::Ext(format!("{}cosine", crate::vector::NS)),
        vec![
            super::expr::Expr::Var(v_vec),
            super::expr::Expr::Const(query_vec),
        ],
    );
    let order = Node {
        kind: Kind::OrderBy {
            keys: vec![(key, false)],
            limit: Some(5),
        },
        vars: scan.vars.clone(),
        certain: scan.certain.clone(),
        sorted: Vec::new(),
        est: scan.est,
        cost: scan.cost,
        dist: scan.dist.clone(),
        desc: "order".into(),
        children: vec![scan],
    };

    let _held = hold_all_but(&ctx, 100);
    let res = super::vectortopk::vector_topk(order, &ctx);
    assert_memory_budget_error(res, "vectortopk constant vector decode");
}

#[test]
fn enhancer_planner_subst_and_remote_run_propagate_memory_error() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    ctx.allow_service = true;
    let v_x = ctx.var("x");
    let v_y = ctx.var("y");
    let long_id = ctx.intern_term(&Term::Literal(Literal::new_simple_literal(
        "e".repeat(2048),
    )));

    let pattern = GraphPattern::Bgp {
        patterns: vec![TriplePattern {
            subject: TermPattern::Variable(Variable::new_unchecked("x")),
            predicate: NamedNodePattern::NamedNode(NamedNode::new_unchecked(
                "http://example.org/p",
            )),
            object: TermPattern::Variable(Variable::new_unchecked("y")),
        }],
    };

    // 1. planner_subst
    {
        let mut planner = Planner::new(&ctx);
        planner.subst.insert(v_x, long_id);
        let _held = hold_all_but(&ctx, 100);
        assert_memory_budget_error(
            super::enhancer::planner_subst(&planner, &pattern),
            "enhancer::planner_subst",
        );
    }

    let rl = super::enhancer::RemoteLoop {
        endpoint: NamedNode::new_unchecked("http://example.org/sparql"),
        bulk: 10,
        cache: super::enhancer::CacheMode::None,
        silent: false,
        values: true,
        idx: "__idx".into(),
    };

    // 2. RemoteRun::new (outer constant decode)
    {
        let spec = LateralSpec {
            pattern: pattern.clone(),
            keys: vec![v_y],
            outer: vec![(v_x, long_id)],
            outer_scoped: Vec::new(),
            graph: ActiveGraph::Default,
            optional: None,
            unscoped: false,
            service: Some(Box::new(rl.clone())),
        };
        let _held = hold_all_but(&ctx, 100);
        let res = super::enhancer::RemoteRun::new(&ctx, &spec, &rl).map(|_| ());
        assert_memory_budget_error(res, "enhancer::RemoteRun::new outer decode");
    }

    // 3. RemoteRun::add (input key decode)
    {
        let spec = LateralSpec {
            pattern,
            keys: vec![v_x],
            outer: Vec::new(),
            outer_scoped: Vec::new(),
            graph: ActiveGraph::Default,
            optional: None,
            unscoped: false,
            service: Some(Box::new(rl.clone())),
        };
        let mut run = super::enhancer::RemoteRun::new(&ctx, &spec, &rl).unwrap();
        let mut left = Table::new(vec![v_x]);
        left.push_row(&[long_id]);
        let _held = hold_all_but(&ctx, 100);
        assert_memory_budget_error(
            run.add(&ctx, &spec, &rl, &left),
            "enhancer::RemoteRun::add input key decode",
        );
    }
}

#[cfg(feature = "geo")]
#[test]
fn geopf_and_georewrite_propagate_memory_error() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = MIB;
    let v_w = ctx.var("w");
    let v_s = ctx.var("s");
    let wkt_id = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
        format!("POINT(0 0) /* {} */", "z".repeat(2048)),
        NamedNode::new_unchecked(crate::geo::vocab::WKT_LITERAL),
    )));
    let pred_id = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(
        crate::geo::vocab::AS_WKT,
    )));

    // 1. geopf::fold_const_checked
    {
        let _held = hold_all_but(&ctx, 100);
        assert_memory_budget_error(
            super::geopf::fold_const_checked(&super::expr::Expr::Const(wkt_id), &ctx),
            "geopf::fold_const_checked",
        );
    }

    // 2. geopf::spatial_leaf -> on::decode (geopf.rs:401)
    {
        let mut planner = Planner::new(&ctx);
        planner.subst.insert(v_w, wkt_id);
        let call = super::geopf::SpatialCall {
            func: crate::geo::SpatialPfKind::Nearby,
            subjects: vec![TermPattern::Variable(Variable::new_unchecked("s"))],
            args: vec![TermPattern::Variable(Variable::new_unchecked("w"))],
        };
        let _held = hold_all_but(&ctx, 100);
        let res = super::geopf::spatial_leaf(&planner, call, &ActiveGraph::Default).map(|_| ());
        assert_memory_budget_error(res, "geopf::spatial_leaf decode");
    }

    // 3. geopf::push_spatial_checked
    {
        let scan = Node::leaf(
            Kind::Scan(ScanSpec {
                perm: Perm::Pso,
                prefix: vec![pred_id.0],
                cols: vec![(1, v_s), (2, v_w)],
                eqs: Vec::new(),
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            }),
            vec![v_s, v_w],
            100.0,
            "scan".into(),
        );
        let expr = super::expr::Expr::Call(
            super::expr::Func::Ext(format!("{}sfWithin", crate::geo::vocab::GEOF)),
            vec![
                super::expr::Expr::Var(v_w),
                super::expr::Expr::Const(wkt_id),
            ],
        );
        let _held = hold_all_but(&ctx, 100);
        let res = super::geopf::push_spatial_checked(scan, vec![expr], &ctx).map(|_| ());
        assert_memory_budget_error(res, "geopf::push_spatial_checked");
    }

    // 4. georewrite::rewrite_leaf
    {
        let mut planner = Planner::new(&ctx);
        planner.subst.insert(v_w, wkt_id);
        let call = super::georewrite::RewriteCall {
            rel: crate::geo::Relation::SfContains,
            subject: TermPattern::Variable(Variable::new_unchecked("s")),
            object: TermPattern::Variable(Variable::new_unchecked("w")),
            asserted: true,
        };
        let _held = hold_all_but(&ctx, 100);
        let res =
            super::georewrite::rewrite_leaf(&planner, call, &ActiveGraph::Default).map(|_| ());
        assert_memory_budget_error(res, "georewrite::rewrite_leaf");
    }
}

#[test]
fn plan_range_atom_and_lexical_probe_propagate_memory_error() {
    let long = "m".repeat(2048);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s1 ex:label \"{long}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let pred_id = snap.lookup_iri("http://example.org/label").unwrap();
    let lit_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&long)))
        .unwrap();
    assert_eq!(lit_id.tag(), Tag::Vocab);

    let mut ctx = Ctx::new(snap);
    ctx.mem_limit = MIB;
    let v_s = ctx.var("s");
    let v_o = ctx.var("o");
    let big_dec_id = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
        "123456789012345678901234567890.5",
        xsd::DECIMAL,
    )));
    let scan = ScanSpec {
        perm: Perm::Pos,
        prefix: vec![pred_id.0],
        cols: vec![(1, v_o), (2, v_s)],
        eqs: Vec::new(),
        graph: GraphFilter::Default,
        graph_col: 3,
        dedup: false,
        src: None,
    };
    let scan_node = Node::leaf(
        Kind::Scan(scan.clone()),
        vec![v_o, v_s],
        10.0,
        "scan".into(),
    );
    let cmp = super::expr::Expr::Cmp(
        Box::new(super::expr::Expr::Var(v_o)),
        Box::new(super::expr::Expr::Const(big_dec_id)),
        super::expr::CmpOp::Lt,
    );

    let _held = hold_all_but(&ctx, 100);
    // 1. range_atom_checked
    assert_memory_budget_error(
        super::plan::range_atom_checked(&cmp, v_o, &ctx),
        "plan::range_atom_checked",
    );
    // 2. filter_checked (push_range)
    assert_memory_budget_error(
        super::plan::filter_checked(scan_node, vec![cmp], &ctx).map(|_| ()),
        "plan::filter_checked push_range",
    );
    // 3. plain_strings_first (vocab key probe)
    assert_memory_budget_error(
        super::plan::plain_strings_first(&ctx, &scan, lit_id.0, lit_id.0),
        "plan::plain_strings_first vocab probe",
    );
}

#[test]
fn rdfs_schema_and_explain_propagate_memory_error() {
    let long_a = format!("http://example.org/A/{}", "a".repeat(1024));
    let long_b = format!("http://example.org/B/{}", "b".repeat(1024));
    let long_c = format!("http://example.org/C/{}", "c".repeat(1024));
    let long_p = format!("http://example.org/p/{}", "p".repeat(1024));
    let long_o = format!("http://example.org/o/{}", "o".repeat(1024));
    let long_g = format!("http://example.org/g/{}", "g".repeat(1024));
    let nq = format!(
        "<{long_a}> <{}> <{long_b}> <{long_g}> .\n\
         <{long_b}> <{}> <{long_c}> <{long_g}> .\n\
         <{long_p}> <{}> <{long_c}> <{long_g}> .\n\
         <http://example.org/s> <{long_p}> <{long_o}> <{long_g}> .\n",
        rdfs::SUB_CLASS_OF.as_str(),
        rdfs::SUB_CLASS_OF.as_str(),
        rdfs::DOMAIN.as_str(),
    );
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
        .unwrap();
    store.compact().unwrap();

    // 1. RdfsOnRead::schema_in_ctx under tight query memory budget.
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let rdfs_on =
            super::rdfs::RdfsOnRead::new(super::rdfs::SchemaSource::Graph(Some(long_g.clone())));
        let _held = hold_all_but(&ctx, 100);
        let res = rdfs_on.schema_in_ctx(&ctx);
        assert_memory_budget_error(res, "RdfsOnRead::schema_in_ctx");
    }

    // 2. RdfsSchema::explain under tight ExplainLimits::max_memory.
    {
        let snap = store.snapshot();
        let schema = super::rdfs::schema_of_graph(&snap, Some(&long_g)).unwrap();
        let gid = snap.lookup_iri(&long_g).unwrap();
        let limits = crate::explain::ExplainLimits {
            max_trees: 4,
            max_depth: 8,
            deadline: None,
            cancel: None,
            max_memory: Some(256),
        };

        // Trigger object() decode (rdfs2 domain derivation on <http://example.org/s> a <long_c>).
        let triple = [
            Term::NamedNode(NamedNode::new_unchecked("http://example.org/s")),
            Term::NamedNode(rdf::TYPE.into_owned()),
            Term::NamedNode(NamedNode::new_unchecked(&long_c)),
        ];
        let res = schema.explain(
            &snap,
            super::rdfs::SchemaLeaves::Graph(gid),
            &[gid],
            &triple,
            limits.clone(),
        );
        assert!(
            matches!(
                &res,
                Err(Error::Component(c)) if c.code == "reasoning-budget"
            ),
            "rdfs::explain object decode should fail with reasoning-budget, got {res:?}"
        );

        // Trigger chain() decode (rdfs11 subclass chain <long_a> rdfs:subClassOf <long_c>).
        let chain_triple = [
            Term::NamedNode(NamedNode::new_unchecked(&long_a)),
            Term::NamedNode(rdfs::SUB_CLASS_OF.into_owned()),
            Term::NamedNode(NamedNode::new_unchecked(&long_c)),
        ];
        let res = schema.explain(
            &snap,
            super::rdfs::SchemaLeaves::Graph(gid),
            &[gid],
            &chain_triple,
            limits,
        );
        assert!(
            matches!(
                &res,
                Err(Error::Component(c)) if c.code == "reasoning-budget"
            ),
            "rdfs::explain chain decode should fail with reasoning-budget, got {res:?}"
        );
    }

    // 3. RdfsOnRead::schema_controlled_with_memory bounds schema loading under tight max_memory
    //    and leaves the schema cache unpopulated after refusal.
    {
        let snap = store.snapshot();
        let rdfs_on =
            super::rdfs::RdfsOnRead::new(super::rdfs::SchemaSource::Graph(Some(long_g.clone())));
        let err = rdfs_on
            .schema_controlled_with_memory(&snap, Some(256), &mut || Ok(()))
            .unwrap_err();
        assert!(
            matches!(&err, Error::Component(c) if c.code == "reasoning-budget"),
            "schema_controlled_with_memory should fail with reasoning-budget, got {err:?}"
        );
        // Subsequent unbounded load succeeds (failed build was not cached).
        let loaded = rdfs_on
            .schema_controlled_with_memory(&snap, None, &mut || Ok(()))
            .unwrap();
        assert!(!loaded.is_empty());
    }

    // 4. Positive tight-limit test: ExplainLimits::max_memory stays an answer-memory limit,
    //    so a short asserted triple in a 4 KiB named graph succeeds inside an 8 KiB budget.
    {
        let graph_4k = format!("http://example.org/g4k/{}", "g".repeat(4070));
        let nq = format!(
            "<http://example.org/s> <http://example.org/p> <http://example.org/o> <{graph_4k}> .\n"
        );
        let s4k = Store::in_memory(StoreOptions::default());
        s4k.load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();
        s4k.compact().unwrap();
        let snap = s4k.snapshot();
        let gid = snap.lookup_iri(&graph_4k).unwrap();
        let triple = [
            Term::NamedNode(NamedNode::new_unchecked("http://example.org/s")),
            Term::NamedNode(NamedNode::new_unchecked("http://example.org/p")),
            Term::NamedNode(NamedNode::new_unchecked("http://example.org/o")),
        ];
        let schema = super::rdfs::RdfsSchema::default();
        let explained = schema
            .explain(
                &snap,
                super::rdfs::SchemaLeaves::Document,
                &[gid],
                &triple,
                crate::explain::ExplainLimits {
                    max_trees: 1,
                    max_depth: 4,
                    deadline: None,
                    cancel: None,
                    max_memory: Some(8192),
                },
            )
            .expect("4 KiB named graph + short asserted triple must fit an 8 KiB answer limit");
        match explained {
            crate::explain::Explained::Found { trees, .. } => {
                assert_eq!(trees.len(), 1);
                assert_eq!(trees[0].graph.as_deref(), Some(graph_4k.as_str()));
                assert!(trees[0].heap_bytes() <= 8192);
            }
            other => panic!("expected Explained::Found, got {other:?}"),
        }
    }
}

#[test]
fn ctx_checked_decoders_propagate_live_cancel_timeout_missing_and_latched_refusals() {
    let payload = "c".repeat(1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{payload}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let pred_id = snap.lookup_iri("http://example.org/p").unwrap();
    let vocab_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(&payload)))
        .unwrap();
    assert_eq!(vocab_id.tag(), Tag::Vocab);
    let missing_vocab = Id::vocab(999_999);
    let missing_delta = Id::delta(999_999);
    let missing_local = Id::local(999_999);

    // 1. Genuine missing IDs return Ok(None) across all checked decoders.
    assert_eq!(
        snap.term_controlled(missing_vocab, &mut |_| Ok(()), &mut || Ok(()))
            .unwrap(),
        None
    );
    assert_eq!(
        snap.term_controlled(missing_delta, &mut |_| Ok(()), &mut || Ok(()))
            .unwrap(),
        None
    );
    assert_eq!(
        snap.term_controlled(Id::UNDEF, &mut |_| Ok(()), &mut || Ok(()))
            .unwrap(),
        None
    );

    for cursor in [false, true] {
        // Genuine Ok(None) + live cancellation + live timeout across valid, cached, missing and lexical probe paths.
        {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let local_id = ctx.intern_term(&Term::Literal(Literal::new_simple_literal("local")));
            for id in [
                Id::UNDEF,
                Id::new(Tag::Special, 1),
                missing_vocab,
                missing_delta,
                missing_local,
            ] {
                assert_eq!(ctx.term_checked(id).unwrap(), None);
                assert!(ctx.value_checked(id).unwrap().is_none());
            }
            // Populate ctx.values cache so we also test value-cache hit under live controls.
            assert!(ctx.value_checked(vocab_id).unwrap().is_some());

            let scan = ScanSpec {
                perm: Perm::Pos,
                prefix: vec![pred_id.0],
                cols: vec![(1, ctx.var("o")), (2, ctx.var("s"))],
                eqs: Vec::new(),
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            };

            // Live cancellation
            ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            for id in [
                vocab_id,
                local_id,
                Id::from_i64(42).unwrap(),
                missing_vocab,
                Id::UNDEF,
            ] {
                assert!(
                    matches!(ctx.term_checked(id), Err(Error::Cancelled)),
                    "term_checked cancel cursor={cursor} id={id:?}"
                );
                assert!(
                    matches!(ctx.value_checked(id), Err(Error::Cancelled)),
                    "value_checked cancel cursor={cursor} id={id:?}"
                );
            }
            assert!(matches!(
                super::plan::plain_strings_first(&ctx, &scan, vocab_id.0, vocab_id.0),
                Err(Error::Cancelled)
            ));
        }

        // Live expired deadline
        {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            assert!(ctx.value_checked(vocab_id).unwrap().is_some());
            ctx.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
            for id in [
                vocab_id,
                Id::from_i64(42).unwrap(),
                missing_vocab,
                Id::UNDEF,
            ] {
                assert!(
                    matches!(ctx.term_checked(id), Err(Error::Timeout)),
                    "term_checked timeout cursor={cursor} id={id:?}"
                );
                assert!(
                    matches!(ctx.value_checked(id), Err(Error::Timeout)),
                    "value_checked timeout cursor={cursor} id={id:?}"
                );
            }
        }

        // Already-latched non-memory refusal is preserved verbatim even under tight memory headroom.
        for latched in [Error::Cancelled, Error::Timeout] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let _held = hold_all_but(&ctx, 64);
            ctx.fail_memory(&latched);
            let expected_cancel = matches!(latched, Error::Cancelled);
            let assert_preserved = |err: Error| match err {
                Error::Cancelled if expected_cancel => {}
                Error::Timeout if !expected_cancel => {}
                other => panic!("expected preserved latched refusal, got {other:?}"),
            };
            assert_preserved(ctx.term_checked(vocab_id).unwrap_err());
            assert_preserved(ctx.value_checked(vocab_id).unwrap_err());
        }
    }
}

#[test]
fn multi_term_decodes_retain_credit_across_coexisting_terms() {
    // 1. Ctx::term_checked_into accumulates credit across coexisting terms.
    let long_a = format!("http://example.org/a/{}", "a".repeat(1024));
    let long_b = format!("http://example.org/b/{}", "b".repeat(1024));
    let long_c = format!("http://example.org/c/{}", "c".repeat(1024));
    let ttl = format!("<{long_a}> <{long_b}> <{long_c}> .\n");
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let a_id = snap.lookup_iri(&long_a).unwrap();
    let b_id = snap.lookup_iri(&long_b).unwrap();
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        // ~12 KiB fits one 1 KiB IRI (~8.5 KiB + 2 KiB scratch) but not two simultaneously (~17 KiB).
        let _held = hold_all_but(&ctx, 12 * 1024);
        let credit = ctx.charge(0).unwrap();
        assert!(ctx.term_checked_into(a_id, &credit).unwrap().is_some());
        assert_memory_budget_error(
            ctx.term_checked_into(b_id, &credit),
            "second coexisting term_checked_into must fail when combined bytes exceed budget",
        );
    }

    // 2. RdfsOnRead::schema_in_ctx accumulates credit across all schema triples in Vec<Triple>.
    {
        let mut ttl = String::new();
        for i in 0..8 {
            let s = format!("http://example.org/S{i}/{}", "s".repeat(1024));
            let o = format!("http://example.org/O{i}/{}", "o".repeat(1024));
            ttl.push_str(&format!(
                "<{s}> <{}> <{o}> .\n",
                rdfs::SUB_CLASS_OF.as_str()
            ));
        }
        let s_store = store_with_turtle(&ttl, true);
        let rdfs_on = super::rdfs::RdfsOnRead::new(super::rdfs::SchemaSource::Graph(None));
        let mut ctx = Ctx::new(s_store.snapshot());
        ctx.mem_limit = MIB;
        // 32 KiB fits any individual IRI (~8.5 KiB + 2 KiB scratch), but not 16 accumulated IRIs (~136 KiB).
        let _held = hold_all_but(&ctx, 32 * 1024);
        assert_memory_budget_error(
            rdfs_on.schema_in_ctx(&ctx),
            "RdfsOnRead::schema_in_ctx must fail on accumulated decoded schema triples",
        );
        // Failed build must not populate cache: a fresh context with full budget succeeds.
        let full_ctx = Ctx::new(s_store.snapshot());
        assert!(!rdfs_on.schema_in_ctx(&full_ctx).unwrap().is_empty());
    }

    // 3. propertyext captured triple scan holds credit across all 3 decoded terms simultaneously.
    {
        struct ViewReader;
        impl PropertyFunction for ViewReader {
            fn open<'q>(
                &self,
                ctx: PropertyContext<'q>,
                _args: PropertyInput,
            ) -> std::result::Result<Box<dyn PropertyStream + 'q>, ScalarError> {
                let mut cursor = ctx.view().scan(None, None, None)?;
                let _ = cursor.next_batch()?;
                struct Empty;
                impl PropertyStream for Empty {
                    fn next(
                        &mut self,
                        _context: &PropertyContext<'_>,
                    ) -> std::result::Result<Option<PropertyRow>, ScalarError> {
                        Ok(None)
                    }
                }
                Ok(Box::new(Empty))
            }
        }
        let desc = Arc::new(
            PropertyDescriptor::new(
                "urn:test:prop-coexist",
                ArgumentShape::Term,
                ArgumentShape::Term,
                vec![PropertyPosition::Subject(0)],
                vec![PropertyPosition::Object(0)],
                ViewReader,
            )
            .unwrap(),
        );
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        ctx.calls_extensions = true;
        ctx.extension_failure = Some(Default::default());
        let short_in = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked("urn:in")));
        let v_o = ctx.var("o");
        let spec = super::propertyext::Spec::for_test(
            desc,
            vec![PT::C(short_in)],
            vec![PT::V(v_o)],
            Arc::new(GraphFilter::Default),
        );
        // 20 KiB fits setup + any single 1 KiB IRI (~8.5 KiB), but not all 3 coexisting (~25.5 KiB).
        let _held = hold_all_but(&ctx, 20 * 1024);
        let res = super::propertyext::run(&ctx, &spec, &Table::unit(), &[v_o]);
        assert_memory_budget_error(
            res,
            "propertyext captured triple scan must hold credit across all 3 coexisting terms",
        );
    }
}

#[cfg(feature = "geo")]
#[test]
fn geo_aggregates_propagate_memory_error_on_second_input_and_output() {
    let store = Store::in_memory(StoreOptions::default());
    let short_pt = Term::Literal(Literal::new_typed_literal(
        "POINT(0 0)",
        NamedNode::new_unchecked(crate::geo::vocab::WKT_LITERAL),
    ));
    let long_valid_wkt = Term::Literal(Literal::new_typed_literal(
        format!("POINT(1 1) {}", " ".repeat(2048)),
        NamedNode::new_unchecked(crate::geo::vocab::WKT_LITERAL),
    ));
    let long_malformed_wkt = Term::Literal(Literal::new_typed_literal(
        format!("POINT(1 {}", " ".repeat(2048)),
        NamedNode::new_unchecked(crate::geo::vocab::WKT_LITERAL),
    ));

    // In eager mode, a long second geometry (both valid and malformed) must latch Memory error
    // instead of decoding unchecked or silently returning unbound on parse failure.
    for (label, second_term) in [
        ("valid_second", &long_valid_wkt),
        ("malformed_second", &long_malformed_wkt),
    ] {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let first_id = ctx.intern_term(&short_pt);
        let second_id = ctx.intern_term(second_term);
        // Leave enough room to decode first_id (~400 B), not enough for second_id (~16 KiB).
        let _held = hold_all_but(&ctx, 2048);
        let out =
            crate::geo::aggregates::evaluate(&ctx, "aggUnion", &[Ok(first_id), Ok(second_id)]);
        assert_eq!(out, Some(Id::UNDEF), "{label}");
        assert_memory_budget_error(ctx.check(), &format!("geo::aggregates {label} eager mode"));
    }
}

#[cfg(feature = "geo")]
#[test]
fn geopf_fold_const_checked_propagates_memory_error_on_nested_const_in_eager_mode() {
    let payload = "w".repeat(2048);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"POINT(0 0) {payload}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let long_id = snap
        .lookup_term(&Term::Literal(Literal::new_simple_literal(format!(
            "POINT(0 0) {payload}"
        ))))
        .unwrap();
    assert_eq!(long_id.tag(), Tag::Vocab);

    // Eager mode (`cursor=false`): CONCAT over a nested Expr::Const must refuse before eval,
    // not slip through the legacy eager decode bypass.
    let mut ctx = Ctx::new(snap);
    ctx.mem_limit = MIB;
    let suffix = Term::Literal(Literal::new_simple_literal(" "));
    let suffix_id = ctx.intern_term(&suffix);
    let expr = super::expr::Expr::Call(
        super::expr::Func::Builtin(spargebra::algebra::Function::Concat),
        vec![
            super::expr::Expr::Const(long_id),
            super::expr::Expr::Lit(suffix_id, super::value::Value::Str(" ".into())),
        ],
    );
    let _held = hold_all_but(&ctx, 100);
    assert_memory_budget_error(
        super::geopf::fold_const_checked(&expr, &ctx),
        "geopf::fold_const_checked nested Expr::Const in eager mode",
    );
}

#[cfg(feature = "geo")]
#[test]
fn geo_planner_scope_covers_and_geojoin_propagate_memory_error() {
    let long_g = format!("http://example.org/geo-graph/{}", "g".repeat(1024));
    let nq = format!(
        "<http://example.org/a> <{}> \"POINT(0 0)\"^^<{}> <{long_g}> .\n\
         <http://example.org/b> <{}> \"POINT(1 1)\"^^<{}> <{long_g}> .\n",
        crate::geo::vocab::AS_WKT,
        crate::geo::vocab::WKT_LITERAL,
        crate::geo::vocab::AS_WKT,
        crate::geo::vocab::WKT_LITERAL,
    );
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
        .unwrap();
    store.compact().unwrap();
    store
        .enable_geo(crate::geo::GeoConfig {
            graphs: crate::text::GraphScope {
                include: crate::text::PredicateSet::Only(vec![long_g.clone()]),
                exclude: Vec::new(),
            },
            ..crate::geo::GeoConfig::default()
        })
        .unwrap();

    let snap = store.snapshot();
    let pred_id = snap.lookup_iri(crate::geo::vocab::AS_WKT).unwrap();
    let g_id = snap.lookup_iri(&long_g).unwrap();
    assert_eq!(g_id.tag(), Tag::Vocab);

    let pt_val = super::value::Value::Other {
        lex: "POINT(0 0)".into(),
        dt: crate::geo::vocab::WKT_LITERAL.into(),
    };
    let pt_id = snap
        .lookup_term(&Term::Literal(Literal::new_typed_literal(
            "POINT(0 0)",
            NamedNode::new_unchecked(crate::geo::vocab::WKT_LITERAL),
        )))
        .unwrap();

    for cursor in [false, true] {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        if cursor {
            ctx.enable_cursor_accounting().unwrap();
        }
        let v_s1 = ctx.var("s1");
        let v_w1 = ctx.var("w1");
        let v_s2 = ctx.var("s2");
        let v_w2 = ctx.var("w2");
        let make_scan = |vs, vw| {
            Node::leaf(
                Kind::Scan(ScanSpec {
                    perm: Perm::Pso,
                    prefix: vec![pred_id.0],
                    cols: vec![(1, vs), (2, vw)],
                    eqs: Vec::new(),
                    graph: GraphFilter::One(g_id.0),
                    graph_col: 3,
                    dedup: false,
                    src: None,
                }),
                vec![vs, vw],
                100.0,
                "scan".into(),
            )
        };
        let _held = hold_all_but(&ctx, 100);

        // 1. push_spatial_checked -> scope_covers -> graph_iri_checked
        let push_expr = super::expr::Expr::Call(
            super::expr::Func::Ext(format!("{}sfWithin", crate::geo::vocab::GEOF)),
            vec![
                super::expr::Expr::Var(v_w1),
                super::expr::Expr::Lit(pt_id, pt_val.clone()),
            ],
        );
        assert_memory_budget_error(
            super::geopf::push_spatial_checked(make_scan(v_s1, v_w1), vec![push_expr], &ctx)
                .map(|_| ()),
            &format!("push_spatial_checked scope_covers cursor={cursor}"),
        );

        // 2. geojoin::spatial_joins -> index_side -> scope_covers -> graph_iri_checked
        let mut parts = vec![make_scan(v_s1, v_w1), make_scan(v_s2, v_w2)];
        let mut filters = vec![super::expr::Expr::Call(
            super::expr::Func::Ext(format!("{}sfIntersects", crate::geo::vocab::GEOF)),
            vec![super::expr::Expr::Var(v_w1), super::expr::Expr::Var(v_w2)],
        )];
        assert_memory_budget_error(
            super::geojoin::spatial_joins(&mut parts, &mut filters, &ctx),
            &format!("geojoin::spatial_joins scope_covers cursor={cursor}"),
        );

        // 3. geojoin::spatial_knn -> knn_node -> scope_covers -> graph_iri_checked
        let scan = make_scan(v_s1, v_w1);
        let dist_call = super::expr::Expr::Call(
            super::expr::Func::Ext(format!("{}metricDistance", crate::geo::vocab::GEOF)),
            vec![
                super::expr::Expr::Var(v_w1),
                super::expr::Expr::Lit(pt_id, pt_val.clone()),
            ],
        );
        let order = Node {
            kind: Kind::OrderBy {
                keys: vec![(dist_call, true)],
                limit: Some(5),
            },
            vars: scan.vars.clone(),
            certain: scan.certain.clone(),
            sorted: Vec::new(),
            est: scan.est,
            cost: scan.cost,
            dist: scan.dist.clone(),
            desc: "order".into(),
            children: vec![scan],
        };
        assert_memory_budget_error(
            super::geojoin::spatial_knn(order, &ctx).map(|_| ()),
            &format!("geojoin::spatial_knn scope_covers cursor={cursor}"),
        );
    }
}

#[test]
fn diagnostic_rendering_uses_plan_term_without_latching_refusal() {
    let long_pred1 = format!("http://example.org/pred1/{}", "p".repeat(1024));
    let long_pred2 = format!("http://example.org/pred2/{}", "q".repeat(1024));
    let long_ent = format!("http://example.org/ent/{}", "e".repeat(1024));
    let ttl = format!(
        "<{long_ent}> <{long_pred1}> \"[1.0, 0.0]\"^^<http://sparkles.dev/ns/vector#f32> .\n\
         <{long_ent}> <{long_pred1}> \"[0.0, 1.0]\"^^<http://sparkles.dev/ns/vector#f32> .\n\
         <{long_ent}> <{long_pred2}> \"v\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let snap = store.snapshot();
    let pred1_id = snap.lookup_iri(&long_pred1).unwrap();
    let pred2_id = snap.lookup_iri(&long_pred2).unwrap();
    let ent_id = snap.lookup_iri(&long_ent).unwrap();
    assert_eq!(pred1_id.tag(), Tag::Vocab);
    assert_eq!(pred2_id.tag(), Tag::Vocab);
    assert_eq!(ent_id.tag(), Tag::Vocab);

    let mut ctx = Ctx::new(snap);
    ctx.mem_limit = MIB;
    ctx.enable_cursor_accounting().unwrap();
    let v_s = ctx.var("s");
    let v_o1 = ctx.var("o1");
    let v_o2 = ctx.var("o2");
    let _held = hold_all_but(&ctx, 64);

    // 1. Expr::display must use plan_term: renders actual IRI, never UNDEF, never latches refusal.
    let rendered = super::expr::Expr::Const(pred1_id).display(&ctx);
    assert!(
        rendered.contains(&long_pred1),
        "Expr::display should render real IRI, got {rendered}"
    );
    assert!(!rendered.contains("UNDEF"));
    ctx.check()
        .expect("Expr::display must not latch a decode refusal");

    // 2. indexjoin::fuse_stars / star_desc must use plan_term without latching refusal.
    let drive = Node::leaf(
        Kind::Values(Table::new(vec![v_s])),
        vec![v_s],
        10.0,
        "drive".into(),
    );
    let probe1 = super::indexjoin::Probe {
        scan: ScanSpec {
            perm: Perm::Pso,
            prefix: vec![pred1_id.0],
            cols: vec![(1, v_s), (2, v_o1)],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: 3,
            dedup: false,
            src: None,
        },
        filter: Vec::new(),
        star: Some(super::indexjoin::StarPattern {
            p: pred1_id.0,
            o: None,
            graph: GraphFilter::Default,
            dedup: false,
        }),
    };
    let probe2 = super::indexjoin::Probe {
        scan: ScanSpec {
            perm: Perm::Pso,
            prefix: vec![pred2_id.0],
            cols: vec![(1, v_s), (2, v_o2)],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: 3,
            dedup: false,
            src: None,
        },
        filter: Vec::new(),
        star: Some(super::indexjoin::StarPattern {
            p: pred2_id.0,
            o: None,
            graph: GraphFilter::Default,
            dedup: false,
        }),
    };
    let low = Node {
        kind: Kind::IndexJoin(Box::new(super::indexjoin::IndexJoinSpec {
            key: v_s,
            probes: vec![probe1],
        })),
        vars: vec![v_s, v_o1],
        certain: vec![v_s, v_o1],
        sorted: Vec::new(),
        est: 10.0,
        cost: 10.0,
        dist: Default::default(),
        desc: "low".into(),
        children: vec![drive],
    };
    let top = Node {
        kind: Kind::IndexJoin(Box::new(super::indexjoin::IndexJoinSpec {
            key: v_s,
            probes: vec![probe2],
        })),
        vars: vec![v_s, v_o1, v_o2],
        certain: vec![v_s, v_o1, v_o2],
        sorted: Vec::new(),
        est: 10.0,
        cost: 20.0,
        dist: Default::default(),
        desc: "top".into(),
        children: vec![low],
    };
    let fused = super::indexjoin::fuse_stars(top, &ctx);
    assert!(
        matches!(fused.kind, Kind::IndexJoin(_)),
        "expected fused IndexJoin star, got {:?}",
        fused.desc
    );
    assert!(
        fused.desc.contains(&"p".repeat(100)) && fused.desc.contains(&"q".repeat(100)),
        "star_desc should render real predicates via plan_term, got {}",
        fused.desc
    );
    ctx.check()
        .expect("indexjoin::star_desc must not latch a decode refusal");

    // 3. Vector multi-entity diagnostic must render the real entity IRI, not "?", and not latch.
    let vspec = super::plan::VectorSpec {
        pred: Some(pred1_id),
        query: super::plan::VectorQuery::Entity(ent_id),
        k: 5,
        metric: crate::vector::Metric::Cosine,
        subject: PathEnd::Var(v_s),
        score: None,
        vector: None,
        graph: GraphFilter::Default,
        graph_var: None,
        dedup: false,
        mode: crate::vector::SearchMode::default(),
        distinct_subject: false,
        candidates: false,
        order_fallback: false,
    };
    let err = super::exec::entity_vector(&ctx, &vspec, pred1_id, ent_id).unwrap_err();
    match &err {
        Error::Invalid(msg) => {
            assert!(
                msg.contains(&long_ent),
                "vector diagnostic should contain entity IRI, got {msg}"
            );
        }
        other => panic!("expected Error::Invalid, got {other:?}"),
    }
    ctx.check()
        .expect("vector diagnostic must not latch a decode refusal");
}
