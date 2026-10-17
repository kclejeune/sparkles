//! The plan depth the engine measures and enforces: what the walk of the parsed query
//! bounds, what [`Node::depth`] reports, what the plan gate refuses and the stack that
//! execution asks for.

use super::depth::{MAX_PLAN_DEPTH, check_query, probe};
use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<urn:s> <urn:p> <urn:o> . <urn:o> <urn:p> <urn:s> .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn chain_body(n: usize) -> String {
    (0..n)
        .map(|i| format!("?a{i} <urn:p> ?a{} . ", i + 1))
        .collect()
}

fn chain(n: usize) -> String {
    format!("SELECT * {{ {}}}", chain_body(n))
}

fn parsed(q: &str) -> Query {
    spargebra::SparqlParser::new().parse_query(q).unwrap()
}

/// A chain of `n` nodes, built without recursion.
fn node_chain(n: usize) -> plan::Node {
    let mut node = plan::Node::unit();
    for _ in 1..n {
        let mut parent = plan::Node::unit();
        parent.children.push(node);
        node = parent;
    }
    node
}

#[test]
fn a_node_reports_the_depth_of_its_deepest_branch() {
    assert_eq!(plan::Node::unit().depth(), 1);
    assert_eq!(node_chain(7).depth(), 7);
    // a shallow branch beside a deep one, at the root and below it
    let mut root = node_chain(3);
    root.children.push(plan::Node::unit());
    root.children[0].children.push(node_chain(4));
    assert_eq!(root.depth(), 1 + 1 + 4);
    // counted without recursion: no stack is needed to measure a deep chain
    let deep = node_chain(MAX_PLAN_DEPTH + 1);
    assert_eq!(deep.depth(), MAX_PLAN_DEPTH + 1);
    depth::with_stack(deep.depth(), move || drop(deep));
}

#[test]
fn a_node_and_its_description_have_the_same_depth() {
    let s = store();
    let opts = QueryOptions::default();
    for q in [chain(5), chain(20), chain(60)] {
        let (_, info) = explain(s.snapshot(), &q, &opts).unwrap();
        assert!(info.depth() >= 5, "{q}");
        probe::STACKS.with_borrow_mut(Vec::clear);
        query(s.snapshot(), &q, &opts).unwrap();
        // the evaluation asks for the stack of the plan it runs, after the stack of the
        // query that bounds the plan
        let stacks = probe::STACKS.with_borrow(Clone::clone);
        assert_eq!(stacks.last(), Some(&info.depth()), "{q}: {stacks:?}");
        assert!(stacks[0] >= info.depth(), "{q}: {stacks:?}");
    }
}

#[test]
fn a_plan_deeper_than_the_limit_is_refused_after_planning() {
    let s = store();
    let opts = QueryOptions::default();
    // the walk of this query passes, and the planner builds more than 12 levels for it
    let q = chain(10);
    check_query(&parsed(&q)).unwrap();
    let (_, info) = explain(s.snapshot(), &q, &opts).unwrap();
    assert!(info.depth() > 12, "{}", info.depth());
    probe::PLAN_LIMIT.set(Some(12));
    let refused = |r: Result<()>, what: &str| {
        let e = r.expect_err(what).to_string();
        assert!(e.contains("query plan nested deeper than"), "{what}: {e}");
    };
    refused(explain(s.snapshot(), &q, &opts).map(|_| ()), "explain");
    refused(query(s.snapshot(), &q, &opts).map(|_| ()), "query");
    refused(
        update::update(
            &s,
            &format!("DELETE {{ ?a0 <urn:q> ?a1 }} WHERE {{ {}}}", chain_body(10)),
            &opts,
        )
        .map(|_| ()),
        "update",
    );
    // a plan at the limit is not
    probe::PLAN_LIMIT.set(Some(info.depth()));
    explain(s.snapshot(), &q, &opts).unwrap();
    query(s.snapshot(), &q, &opts).unwrap();
    probe::PLAN_LIMIT.set(None);
    query(s.snapshot(), &q, &opts).unwrap();
}

/// Describing a plan for `explain` checks the deadline and the cancellation for each node,
/// which a clock cannot test reliably: here the deadline has passed before the description
/// starts, and the check count shows that every node is checked.
#[test]
fn describing_a_plan_observes_a_deadline_that_has_passed() {
    use std::time::{Duration, Instant};
    let s = store();
    let q = chain(40);
    let parsed = parsed(&q);
    let (pattern, dataset, base) = split(&parsed);
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let mut ctx = make_ctx(s.snapshot(), &opts, dataset, base).unwrap();
    let checks = || ctx::CHECKS.with(|n| n.get());
    let before = checks();
    let node = Planner::new(&ctx)
        .plan(pattern, &ActiveGraph::Default, Vec::new())
        .unwrap();
    let planning = checks() - before;
    let nodes = {
        let mut n = 0;
        let mut todo = vec![&node];
        while let Some(x) = todo.pop() {
            n += 1;
            todo.extend(&x.children);
        }
        n
    };
    assert!(nodes > 40, "{nodes}");
    // a deadline in the future describes every node, checking at each one
    ctx.deadline = Some(Instant::now() + Duration::from_secs(600));
    let before = checks();
    let info = exec::try_describe(&ctx, &node).unwrap();
    assert!(checks() - before >= nodes);
    assert_eq!(info.depth(), node.depth());
    // `explain` describes the plan this way: it checks at least once for each node beyond
    // the checks that planning makes
    let before = checks();
    explain(s.snapshot(), &q, &opts).unwrap();
    let explaining = checks() - before;
    assert!(
        explaining >= planning + nodes,
        "{explaining} checks for {planning} in planning and {nodes} nodes"
    );
    // one that has passed stops the description at the root
    ctx.deadline = Instant::now().checked_sub(Duration::from_secs(1));
    assert!(ctx.deadline.is_some());
    let before = checks();
    let e = exec::try_describe(&ctx, &node).unwrap_err();
    assert!(matches!(e, Error::Timeout), "{e}");
    assert_eq!(checks() - before, 1);
    // and so does a cancellation
    ctx.deadline = None;
    ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let e = exec::try_describe(&ctx, &node).unwrap_err();
    assert!(matches!(e, Error::Cancelled), "{e}");
}

#[test]
fn the_walk_bounds_the_plan_of_a_group_without_a_triple_pattern() {
    let s = store();
    let opts = QueryOptions::default();
    for q in [
        "SELECT * { BIND(1 AS ?z) }",
        "ASK { BIND(1 AS ?z) }",
        "ASK { FILTER(true) }",
        "CONSTRUCT { ?a <urn:p> ?a } WHERE { FILTER(BOUND(?a)) }",
        "SELECT * { { } }",
        "SELECT * { OPTIONAL { BIND(1 AS ?z) } }",
        "SELECT * { VALUES ?x { 1 } }",
    ] {
        let measured = check_query(&parsed(q)).unwrap();
        let (_, info) = explain(s.snapshot(), q, &opts).unwrap();
        assert!(
            measured >= info.depth(),
            "{q}: {measured} < {}",
            info.depth()
        );
    }
}
