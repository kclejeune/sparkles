use crate::sparql::extensions::{
    AggregateAccumulator, AggregateContext, AggregateDescriptor, AggregateFactory,
    ExtensionRegistry, ScalarError, ScalarResult,
};
use crate::sparql::{QueryOptions, explain, query};
use crate::store::{Store, StoreOptions};
use oxrdf::{Literal, Term};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
include!("../support/fixture_rows_eq.rs");
const IRI: &str = "urn:test:aggregate";

#[derive(Default)]
struct Counts {
    create: AtomicUsize,
    add: AtomicUsize,
    finish: AtomicUsize,
    drop: AtomicUsize,
}

struct Counting {
    counts: Arc<Counts>,
    count: i64,
}

impl AggregateAccumulator for Counting {
    fn add(&mut self, ctx: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        ctx.check()?;
        self.counts.add.fetch_add(1, Ordering::SeqCst);
        self.count += 1;
        Ok(())
    }

    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        self.counts.finish.fetch_add(1, Ordering::SeqCst);
        Ok(Literal::from(self.count).into())
    }
}

impl Drop for Counting {
    fn drop(&mut self) {
        self.counts.drop.fetch_add(1, Ordering::SeqCst);
    }
}

fn factory(counts: Arc<Counts>, offset: i64) -> impl AggregateFactory {
    move |_: &AggregateContext<'_>| {
        counts.create.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Counting {
            counts: counts.clone(),
            count: offset,
        }) as Box<dyn AggregateAccumulator>)
    }
}

fn integer(value: i64) -> Option<Term> {
    Some(Literal::from(value).into())
}

#[test]
fn large_eligible_builtin_arguments_are_not_precomputed_for_registered_aggregates() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let mut registry = ExtensionRegistry::builder();
    let mut descriptor = AggregateDescriptor::new(IRI, 1, factory(counts.clone(), 0)).unwrap();
    descriptor.volatility = crate::sparql::extensions::Volatility::Immutable;
    descriptor.description = Some("application description is private".into());
    registry.register_aggregate(descriptor).unwrap();
    let opts = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    let values = std::iter::repeat_n("1", 256).collect::<Vec<_>>().join(" ");
    let text = format!("SELECT (AGG <{IRI}>(STR(?v)) AS ?n) {{VALUES ?v {{{values}}}}}");
    let plan = serde_json::to_string(&explain(store.snapshot(), &text, &opts).unwrap().1).unwrap();
    assert!(plan.contains("aggregate Immutable") && plan.contains("singleton batch"));
    assert!(!plan.contains("application description"));
    assert_eq!(counts.create.load(Ordering::SeqCst), 0);
    let result = query(store.snapshot(), &text, &opts).unwrap();
    assert_fixture_rows_eq!(result.rows().unwrap(), vec![vec![integer(256)]]);
    assert_eq!(counts.add.load(Ordering::SeqCst), 256);
    assert!(!result.inner.ctx.opt.expr_cache);
    let plan = serde_json::to_string(&result.plan()).unwrap();
    assert!(!plan.contains("exprCacheHits") && !plan.contains("once per"));
}
