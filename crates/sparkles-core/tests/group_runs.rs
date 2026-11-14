//! Cursor groupings whose aggregates need each group's rows in input order: FOLD and
//! registered aggregates (roadmap item 7). They run under the strict fallback policy,
//! read their input in key order, through a stable sort when the input does not keep
//! it, and give the answers of eager execution, including the order in which each
//! group's rows reach the aggregate.
use oxrdf::{Literal, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::extensions::{
    AggregateAccumulator, AggregateContext, AggregateDescriptor, ExtensionRegistry, ScalarError,
    ScalarResult,
};
use sparkles_core::sparql::{CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;

const SEQ: &str = "urn:test:sequence";

/// The arguments of a group joined in the order they arrive, which shows whether the
/// group's rows reach the accumulator in input order.
struct Sequence(Vec<String>);

impl AggregateAccumulator for Sequence {
    fn add(&mut self, ctx: &AggregateContext<'_>, argument: &Term) -> Result<(), ScalarError> {
        let text = match argument {
            Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        };
        ctx.retain(text.len() as u64 + 32)?;
        self.0.push(text);
        Ok(())
    }

    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        Ok(Literal::new_simple_literal(self.0.join(",")).into())
    }
}

fn options(memory: Option<u64>, spill: Option<Arc<Spill>>) -> QueryOptions {
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_aggregate(
            AggregateDescriptor::new(SEQ, 1, |_: &AggregateContext<'_>| {
                Ok(Box::new(Sequence(Vec::new())) as Box<dyn AggregateAccumulator>)
            })
            .unwrap(),
        )
        .unwrap();
    QueryOptions {
        extensions: Some(registry.build()),
        max_memory_bytes: memory,
        spill,
        ..Default::default()
    }
}

/// `groups` subjects with `per` values each under `<urn:p>`, written in an order
/// unrelated to the subjects, and the same values under `<urn:q>` for every other
/// subject.
fn store(groups: usize, per: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let n = groups * per;
    let mut data = String::new();
    for i in 0..n {
        let k = (i * 7919) % n;
        data.push_str(&format!("<urn:s:{}> <urn:p> {} .\n", k % groups, k));
        if (k % groups).is_multiple_of(2) {
            data.push_str(&format!("<urn:s:{}> <urn:q> {} .\n", k % groups, k % 13));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn spill(dir: &std::path::Path) -> Arc<Spill> {
    Spill::open(&SpillOptions {
        dir: dir.to_path_buf(),
        max_bytes: 4 << 30,
        query_bytes: 4 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap()
}

fn bag(rows: impl IntoIterator<Item = impl std::fmt::Debug>) -> Vec<String> {
    let mut rows: Vec<String> = rows.into_iter().map(|r| format!("{r:?}")).collect();
    rows.sort();
    rows
}

/// The rows of `q` from a strict cursor, and its plan.
fn cursor(
    s: &Store,
    q: &str,
    opts: &QueryOptions,
    batch_rows: usize,
) -> Result<(Vec<String>, String), Error> {
    let cursor = CursorOptions {
        batch_rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, opts, &cursor)?;
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch()? {
        for i in 0..b.len() {
            rows.push(b.row(i)?.values().to_vec());
        }
    }
    Ok((bag(rows), c.plan_json()?))
}

const QUERIES: &[&str] = &[
    // the scan of `<urn:p>` comes in subject order
    "SELECT ?s (AGG <urn:test:sequence>(?v) AS ?a) { ?s <urn:p> ?v } GROUP BY ?s",
    // the object order of the scan is not the key order, so the input is sorted
    "SELECT ?v (AGG <urn:test:sequence>(?s) AS ?a) { ?s <urn:p> ?v BIND(?v / 7 AS ?w) } GROUP BY ?v",
    "SELECT ?w (AGG <urn:test:sequence>(?v) AS ?a) (COUNT(*) AS ?n) { ?s <urn:p> ?v BIND(FLOOR(?v / 50) AS ?w) } GROUP BY ?w",
    "SELECT ?s (FOLD(?v ORDER BY DESC(?v)) AS ?l) { ?s <urn:p> ?v } GROUP BY ?s",
    "SELECT ?s ?x (FOLD(?v) AS ?l) (SUM(?v) AS ?t) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?x } } GROUP BY ?s ?x",
    "SELECT (AGG <urn:test:sequence>(?v) AS ?a) { ?s <urn:q> ?v }",
    "SELECT (AGG <urn:test:sequence>(?v) AS ?a) { ?s <urn:absent> ?v }",
    "SELECT ?s (AGG <urn:test:sequence>(?v) AS ?a) { ?s <urn:absent> ?v } GROUP BY ?s",
    "SELECT ?s (AGG <urn:test:sequence>(DISTINCT ?x) AS ?a) { ?s <urn:p> ?v ; <urn:q> ?x } GROUP BY ?s HAVING (COUNT(*) > 3)",
];

#[test]
fn ordered_aggregates_stream_with_the_eager_answers() {
    let s = store(1500, 6);
    let opts = options(None, None);
    for q in QUERIES {
        let expected = bag(query(s.snapshot(), q, &opts)
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .rows()
            .unwrap());
        for batch_rows in [7, 1000] {
            let (rows, plan) = cursor(&s, q, &opts, batch_rows)
                .unwrap_or_else(|e| panic!("{q}; {batch_rows}: {e}"));
            assert_eq!(rows, expected, "{q}; {batch_rows}\n{plan}");
            assert!(!plan.contains("\"materializes\":true"), "{q}\n{plan}");
        }
    }
}

#[test]
fn a_sort_on_the_keys_keeps_input_order_within_each_group() {
    let s = store(1500, 6);
    let opts = options(None, None);
    let q = QUERIES[2];
    let (_, plan) = cursor(&s, q, &opts, 1000).unwrap();
    assert!(plan.contains("on the group keys"), "{plan}");
    let q = QUERIES[0];
    let (_, plan) = cursor(&s, q, &opts, 1000).unwrap();
    assert!(!plan.contains("on the group keys"), "{plan}");
}

/// Under a small budget the sort spills and the answers stay the same. Without spill
/// the query fails on memory instead of exceeding its budget.
#[test]
fn ordered_aggregates_spill_their_sort() {
    let s = store(30_000, 4);
    let dir = tempfile::tempdir().unwrap();
    let q = "SELECT ?v (AGG <urn:test:sequence>(?s) AS ?a) (COUNT(*) AS ?n) { ?s <urn:p> ?w BIND(?w - FLOOR(?w / 3) * 3 AS ?v) } GROUP BY ?v";
    let expected = bag(query(s.snapshot(), q, &options(None, None))
        .unwrap()
        .rows()
        .unwrap());
    let memory = 24 << 20;
    let (rows, plan) = cursor(&s, q, &options(Some(memory), Some(spill(dir.path()))), 1000)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows, expected);
    assert!(plan.contains("spilledBytes"), "{plan}");
    match cursor(&s, q, &options(Some(memory), None), 1000) {
        Ok((rows, _)) => assert_eq!(rows, expected),
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
        Err(e) => panic!("{e}"),
    }
}

/// One group that spans hundreds of batches, beside small ones, gives the eager
/// answer.
#[test]
fn a_group_spanning_many_batches() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..40_000 {
        let g = if i % 10 == 0 { i } else { 1 };
        data.push_str(&format!("<urn:s:{g}> <urn:p> {i} .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let opts = options(None, None);
    let q = "SELECT ?s (AGG <urn:test:sequence>(?v) AS ?a) (COUNT(*) AS ?n) { ?s <urn:p> ?v } GROUP BY ?s";
    let expected = bag(query(s.snapshot(), q, &opts).unwrap().rows().unwrap());
    let (rows, _) = cursor(&s, q, &opts, 64).unwrap();
    assert_eq!(rows, expected);
}
