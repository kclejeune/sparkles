//! The dataset's query defaults (RDFS on read, the inferred overlay and DESCRIBE) apply
//! to every way the library runs a query: `query`, `query_with`, a stored query, a
//! transaction's query and `explain`. A field the caller sets wins over its default.

use sparkles::Dataset;
use sparkles::io::RdfFormat;
use sparkles::reasoning::rdfs::NewSchema;
use sparkles::reasoning::{INFERRED_GRAPH, ReasoningRecord};
use sparkles::sparql::QueryOptions;
use std::collections::BTreeMap;

const ASK: &str = "ASK { <urn:s> a <urn:parent> }";

fn stored_ask(ds: &Dataset) -> bool {
    let def = serde_json::from_value(serde_json::json!({ "query": ASK })).unwrap();
    ds.queries().put("parent", def, Default::default()).unwrap();
    ds.queries()
        .run("parent", &BTreeMap::new(), &QueryOptions::default())
        .unwrap()
        .boolean
}

fn transaction_ask(ds: &Dataset, opts: &QueryOptions) -> bool {
    ds.transaction(|tx| Ok(tx.query_with(ASK, opts)?.boolean))
        .unwrap()
}

/// Every library path, with the defaults and with options that set no default.
fn assert_every_path(ds: &Dataset, expected: bool) {
    assert_eq!(ds.ask(ASK).unwrap(), expected, "Dataset::ask");
    assert_eq!(
        ds.query_with(ASK, &QueryOptions::default())
            .unwrap()
            .boolean,
        expected,
        "Dataset::query_with"
    );
    assert_eq!(stored_ask(ds), expected, "StoredQueries::run");
    assert_eq!(
        transaction_ask(ds, &QueryOptions::default()),
        expected,
        "Transaction::query_with"
    );
}

#[test]
fn configured_rdfs_applies_to_every_query() {
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .
         <urn:s> a <urn:child> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    assert_every_path(&ds, false);
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    assert_every_path(&ds, true);
    assert!(ds.query_options().rdfs.is_some());
    // an explicit field wins: RDFS on read off for one query
    let off = QueryOptions {
        rdfs: None,
        ..ds.query_options()
    };
    assert!(!ds.query_with(ASK, &off).unwrap().boolean);
    assert!(!transaction_ask(&ds, &off));
    ds.reasoning().rdfs().reset().unwrap();
    assert_every_path(&ds, false);
}

#[test]
fn materialized_inferences_are_part_of_the_default_graph() {
    let ds = Dataset::memory();
    ds.load_str("<urn:s> a <urn:child> .", RdfFormat::Turtle)
        .unwrap();
    ds.load_str_into(
        "<urn:s> a <urn:parent> .",
        RdfFormat::Turtle,
        INFERRED_GRAPH,
    )
    .unwrap();
    // without a reasoning record, the inferred graph is a named graph like any other
    assert_every_path(&ds, false);
    ds.state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    assert_every_path(&ds, true);
    assert_eq!(ds.query_options().default_graph_extra, [INFERRED_GRAPH]);
    // an explicit field wins: the inferences left out of one query
    let mut without = ds.query_options();
    without.default_graph_extra.clear();
    assert!(!ds.query_with(ASK, &without).unwrap().boolean);
    assert!(!transaction_ask(&ds, &without));
    let stored = ds
        .queries()
        .run("parent", &BTreeMap::new(), &without)
        .unwrap();
    assert!(!stored.boolean);
}

#[test]
fn describe_follows_the_setting_unless_the_caller_sets_it() {
    use sparkles::sparql::describe::{DescribeMode, DescribeOptions};
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:a> <urn:p> <urn:b> . <urn:c> <urn:q> <urn:a> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let q = "DESCRIBE <urn:a>";
    assert_eq!(ds.construct(q).unwrap().len(), 1);
    let symmetric = DescribeOptions {
        mode: DescribeMode::Scbd,
        ..Default::default()
    };
    ds.settings().describe().set(symmetric).unwrap();
    assert_eq!(ds.construct(q).unwrap().len(), 2);
    assert_eq!(
        ds.query_with(q, &QueryOptions::default())
            .unwrap()
            .triples()
            .len(),
        2
    );
    // options the caller built on the defaults are used as given
    let given = QueryOptions {
        describe: DescribeOptions::default(),
        ..ds.query_options()
    };
    assert_eq!(ds.query_with(q, &given).unwrap().triples().len(), 1);
}

/// One inference view per reasoning mode chooses what a query reads: the materialized
/// inferences and RDFS on read for materialized reasoning, the asserted data alone when
/// reasoning is off, and the same inferred data for incremental views. Automatic
/// reasoning is refused.
#[test]
fn inference_views_select_the_inferred_data() {
    use sparkles::reasoning::ReasoningMode;
    use sparkles::reasoning::explain::ExplainSource;
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .
         <urn:s> a <urn:child> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let view = |mode| ds.inference_view(mode).unwrap();
    let materialized = view(ReasoningMode::Materialized);
    assert!(
        materialized.inference().is_none(),
        "nothing to read beyond the quads"
    );
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    ds.state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    let materialized = view(ReasoningMode::Materialized);
    let opts = materialized.query_options();
    assert_eq!(opts.default_graph_extra, [INFERRED_GRAPH]);
    assert!(opts.rdfs.is_some());
    assert!(ds.query_with(ASK, &opts).unwrap().boolean);
    let inference = materialized.inference().unwrap();
    assert!(inference.rdfs.is_some());
    assert_eq!(inference.default_graph_extra, [INFERRED_GRAPH]);
    assert_eq!(
        materialized.explain_source(),
        Some(ExplainSource::Materialized)
    );
    // a caller's own RDFS on read survives materialized reasoning
    let mut own = QueryOptions {
        rdfs: None,
        ..ds.query_options()
    };
    materialized.apply(&mut own);
    assert!(own.rdfs.is_none());
    let off = view(ReasoningMode::Off);
    let opts = off.query_options();
    assert!(opts.default_graph_extra.is_empty() && opts.rdfs.is_none());
    assert!(!ds.query_with(ASK, &opts).unwrap().boolean);
    assert!(off.inference().is_none());
    assert_eq!(off.explain_source(), None);
    assert!(
        ds.stored_inference_view(false)
            .query_options()
            .rdfs
            .is_none()
    );
    assert!(
        ds.stored_inference_view(true)
            .query_options()
            .rdfs
            .is_some()
    );
    let auto = ds.inference_view(ReasoningMode::Auto).unwrap_err();
    assert!(auto.to_string().contains("reasoning=auto"), "{auto}");
    for (name, mode) in [
        ("false", ReasoningMode::Off),
        ("true", ReasoningMode::Materialized),
        ("materialized", ReasoningMode::Materialized),
        ("auto", ReasoningMode::Auto),
    ] {
        assert_eq!(ReasoningMode::parse(name), Some(mode.clone()));
        assert_eq!(ReasoningMode::from_param(Some(name)), mode);
    }
    assert!(matches!(
        ReasoningMode::parse("demand"),
        Some(ReasoningMode::Demand(_))
    ));
    assert_eq!(
        ReasoningMode::from_param(Some("off")),
        ReasoningMode::Materialized
    );
    assert_eq!(ReasoningMode::from_param(None), ReasoningMode::Materialized);
}

/// Demand reasoning installs the query-time reasoner, which derives what materialized
/// reasoning would read, and fails without a profile when the dataset has no record.
#[cfg(feature = "reasoning")]
#[test]
fn a_demand_view_installs_the_query_time_reasoner() {
    use sparkles::reasoning::explain::ExplainSource;
    use sparkles::reasoning::{Demand, ReasoningMode};
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .
         <urn:s> a <urn:child> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let missing = ds
        .inference_view(ReasoningMode::Demand(Demand::default()))
        .unwrap_err();
    assert!(missing.to_string().contains("needs a profile"), "{missing}");
    let demand = ds
        .inference_view(ReasoningMode::Demand(Demand {
            profile: Some(sparkles::reasoning::reasoner::Profile::Rdfs),
            max_derived: None,
        }))
        .unwrap();
    assert_eq!(demand.explain_source(), Some(ExplainSource::Demand));
    let opts = demand.query_options();
    assert!(opts.reasoning.is_some());
    assert!(ds.query_with(ASK, &opts).unwrap().boolean);
}

/// A query request starts from the dataset's defaults, takes budgets that replace its
/// limits and budgets that only lower them, reads the inferred data of a view and
/// carries a cancellation flag. Its row budget stops a query as the options' does.
#[test]
fn query_requests_apply_budgets_reasoning_and_cancellation() {
    use sparkles::reasoning::ReasoningMode;
    use sparkles::{Budgets, QueryRequest};
    use std::time::Duration;
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:s> a <urn:child> . <urn:t> a <urn:child> . <urn:u> a <urn:child> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.load_str_into(
        "<urn:s> a <urn:parent> .",
        RdfFormat::Turtle,
        INFERRED_GRAPH,
    )
    .unwrap();
    ds.state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    let server = Budgets {
        timeout: Some(Duration::from_secs(5)),
        max_rows: Some(100),
        max_memory_bytes: Some(1 << 30),
        max_rows_produced: None,
        max_spill_bytes: Some(1 << 20),
    };
    let asked = Budgets {
        timeout: Some(Duration::from_secs(1)),
        max_rows: Some(2),
        max_memory_bytes: Some(1 << 40),
        max_rows_produced: Some(50),
        max_spill_bytes: None,
    };
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opts = QueryRequest::new(&ds)
        .budgets(&server)
        .lower_budgets(&asked)
        .cancel(flag.clone())
        .build();
    // lowering keeps the timeout and the smaller of each limit, where none is no limit
    assert_eq!(opts.timeout, Some(Duration::from_secs(5)));
    assert_eq!(opts.max_rows, Some(2));
    assert_eq!(opts.max_memory_bytes, Some(1 << 30));
    assert_eq!(opts.max_rows_produced, Some(50));
    assert_eq!(opts.max_spill_bytes, Some(1 << 20));
    assert_eq!(opts.default_graph_extra, [INFERRED_GRAPH]);
    assert!(ds.query_with(ASK, &opts).unwrap().boolean);
    let all = "SELECT ?s { ?s a <urn:child> }";
    assert!(
        ds.query_with(all, &opts).is_err(),
        "three rows pass the row budget"
    );
    let off = QueryRequest::new(&ds)
        .with_reasoning(&ds.inference_view(ReasoningMode::Off).unwrap())
        .build();
    assert!(!ds.query_with(ASK, &off).unwrap().boolean);
    assert_eq!(ds.query_with(all, &off).unwrap().len(), 3);
    assert!(std::sync::Arc::ptr_eq(opts.cancel.as_ref().unwrap(), &flag));
}

/// A view replaces the reasoning that options had: off removes a query-time reasoner as
/// well as the inferred data, and options without the dataset's defaults get them before
/// the view applies, so the dataset does not fill the inferences back in.
#[cfg(feature = "reasoning")]
#[test]
fn views_replace_earlier_reasoning_and_resolve_defaults_first() {
    use sparkles::QueryRequest;
    use sparkles::reasoning::{Demand, ReasoningMode};
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .
         <urn:s> a <urn:child> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    let demand = ds
        .inference_view(ReasoningMode::Demand(Demand {
            profile: Some(sparkles::reasoning::reasoner::Profile::Rdfs),
            max_derived: None,
        }))
        .unwrap();
    let off = ds.inference_view(ReasoningMode::Off).unwrap();
    let materialized = ds.inference_view(ReasoningMode::Materialized).unwrap();
    let opts = QueryRequest::new(&ds)
        .with_reasoning(&demand)
        .with_reasoning(&off)
        .build();
    assert!(opts.reasoning.is_none() && opts.rdfs.is_none());
    assert!(!ds.query_with(ASK, &opts).unwrap().boolean);
    let opts = QueryRequest::new(&ds)
        .with_reasoning(&demand)
        .with_reasoning(&materialized)
        .build();
    assert!(opts.reasoning.is_none() && opts.rdfs.is_some());
    // options that do not hold the defaults yet
    let opts = QueryRequest::from_options(QueryOptions::default())
        .with_reasoning(&off)
        .build();
    assert!(!ds.query_with(ASK, &opts).unwrap().boolean);
    let opts = QueryRequest::from_options(QueryOptions::default())
        .with_reasoning(&materialized)
        .build();
    assert!(ds.query_with(ASK, &opts).unwrap().boolean);
}
