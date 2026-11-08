use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, pool::QueryPool};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shex::{Association, NoImports, NodeSelector, ShapeLabel, ShapeMap, ValidateOptions};

#[test]
fn selected_validation_node_retains_original_credit_through_last_clone() {
    let store = Store::in_memory(StoreOptions::default());
    let payload = "x".repeat(64 << 10);
    let input = format!("<urn:s> <urn:p> \"{payload}\" .");
    store
        .load(&[Source::from_bytes(
            input.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let schema = sparkles_shex::parse_schema("START = LITERAL", None, None).unwrap();
    let compiled = sparkles_shex::compile(&schema, &NoImports).unwrap();
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Sparql("SELECT ?focus WHERE { <urn:s> <urn:p> ?focus }".into()),
        shape: ShapeLabel::Start,
    }]);
    let pool = QueryPool::new(16 << 20);
    let options = ValidateOptions {
        selector_query: Some(QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = sparkles_shex::validate(&store.snapshot(), &compiled, &map, &options).unwrap();
    assert!(result.conforms);
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.conformant, 1);
    assert_eq!(result.results[0].node.to_string(), format!("\"{payload}\""));
    let last = result.results[0].node.clone();
    drop(result);
    drop(store);
    assert_eq!(last.to_string(), format!("\"{payload}\""));
    assert!(
        pool.granted() >= 64 << 10,
        "the actual returned 64KiB node must retain original query credit; live={}",
        pool.granted()
    );
    drop(last);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn returned_failure_value_keeps_its_actual_original_credit_until_last_clone() {
    let store = Store::in_memory(StoreOptions::default());
    let payload = "x".repeat(64 << 10);
    store
        .load(&[Source::from_bytes(
            format!("<urn:s> <urn:p> \"{payload}\" .").into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let schema = sparkles_shex::parse_schema("START = IRI", None, None).unwrap();
    let compiled = sparkles_shex::compile(&schema, &NoImports).unwrap();
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Sparql("SELECT ?focus WHERE { <urn:s> <urn:p> ?focus }".into()),
        shape: ShapeLabel::Start,
    }]);
    let pool = QueryPool::new(16 << 20);
    let options = ValidateOptions {
        selector_query: Some(QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = sparkles_shex::validate(&store.snapshot(), &compiled, &map, &options).unwrap();
    assert!(!result.conforms);
    assert_eq!(result.nonconformant, 1);
    let last = result.results[0]
        .failures
        .iter()
        .find_map(|failure| match failure {
            sparkles_shex::ShexFailure::NodeKind { value, constraint } if constraint == "IRI" => {
                Some(value.clone())
            }
            _ => None,
        })
        .expect("unchanged literal-vs-IRI failure");
    drop(result);
    drop(store);
    assert_eq!(last.to_string(), format!("\"{payload}\""));
    assert!(pool.granted() >= 64 << 10);
    drop(last);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn many_selected_nodes_refuse_the_original_aggregate_limit_and_refund_partials() {
    let store = Store::in_memory(StoreOptions::default());
    let payload = "x".repeat(64 << 10);
    let mut input = String::new();
    for n in 0..128 {
        input.push_str(&format!("<urn:s{n}> <urn:p> \"{payload}{n}\" .\n"));
    }
    store
        .load(&[Source::from_bytes(
            input.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let schema = sparkles_shex::parse_schema("START = LITERAL", None, None).unwrap();
    let compiled = sparkles_shex::compile(&schema, &NoImports).unwrap();
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Sparql("SELECT ?focus WHERE { ?s <urn:p> ?focus }".into()),
        shape: ShapeLabel::Start,
    }]);
    let pool = QueryPool::new(16 << 20);
    let options = ValidateOptions {
        selector_query: Some(QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let error = sparkles_shex::validate(&store.snapshot(), &compiled, &map, &options).unwrap_err();
    assert!(
        matches!(error.downcast_ref::<sparkles_core::Error>(),
        Some(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::error::BudgetKind::Memory && b.limit == 4 << 20 && b.requested > b.limit),
        "{error:#}"
    );
    assert_eq!(
        pool.granted(),
        0,
        "no partial selected-node owner survives refusal"
    );
}

#[test]
fn enclosing_validation_cancel_propagates_as_typed_error_without_publishing_nodes() {
    use std::sync::{Arc, atomic::AtomicBool};
    let store = Store::in_memory(StoreOptions::default());
    let schema = sparkles_shex::parse_schema("START = LITERAL", None, None).unwrap();
    let compiled = sparkles_shex::compile(&schema, &NoImports).unwrap();
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Sparql("SELECT ?focus WHERE { VALUES ?focus { \"x\" } }".into()),
        shape: ShapeLabel::Start,
    }]);
    let pool = QueryPool::new(16 << 20);
    let options = ValidateOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        selector_query: Some(QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            ..Default::default()
        }),
        ..Default::default()
    };
    let error = sparkles_shex::validate(&store.snapshot(), &compiled, &map, &options).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<sparkles_core::Error>(),
            Some(sparkles_core::Error::Cancelled)
        ),
        "{error:#}"
    );
    assert_eq!(pool.granted(), 0);
}
