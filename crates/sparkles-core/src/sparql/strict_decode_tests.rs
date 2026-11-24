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
    let big = "u".repeat(16 * 1024);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p \"{big}\" .\n"
    );
    let store = store_with_turtle(&ttl, true);
    let before = store.snapshot().len();

    // Local term (CONCAT) in INSERT template exceeds tight memory budget during to_store decode.
    let opts = QueryOptions {
        max_memory_bytes: Some(32 * 1024),
        ..Default::default()
    };
    let res = super::update::update(
        &store,
        "PREFIX ex: <http://example.org/> \
         INSERT { ex:s ex:q ?out } \
         WHERE { ex:s ex:p ?v . BIND(CONCAT(?v, ?v) AS ?out) }",
        &opts,
    );
    assert_memory_budget_error(res, "update INSERT local term decode");
    assert_eq!(store.snapshot().len(), before);
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
    let long_a = format!("http://example.org/a/{}", "x".repeat(512));
    let long_b = format!("http://example.org/b/{}", "y".repeat(512));
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

    // 1. Weighted path search: lookup_weight decodes edge terms / weight value under tight budget.
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let v_cost = ctx.var("cost");
        let spec = super::pathsearch::PathSearchSpec {
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
            reifies: Some(reifies_id.0),
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
        let _held = hold_all_but(&ctx, 100);
        let res = super::pathsearch::run(&ctx, &spec, None, None, &[v_cost]);
        assert_memory_budget_error(res, "pathsearch lookup_weight decode");
    }

    // 2. Unweighted path search with edge triple-term binding (pathsearch.rs:3433).
    {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = MIB;
        let v_edge = ctx.var("edge");
        let spec = super::pathsearch::PathSearchSpec {
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
            edge: Some(v_edge),
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };
        let _held = hold_all_but(&ctx, 100);
        let res = super::pathsearch::run(&ctx, &spec, None, None, &[v_edge]);
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
}
