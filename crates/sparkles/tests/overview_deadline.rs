//! A graph overview's relative timeout bounds the whole answer, not each of its queries.
use oxrdf::Term;
use serde_json::json;
use sparkles::overview::{FacetsRequest, GroupsRequest};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::extensions::{
    ExtensionRegistry, ScalarContext, ScalarDescriptor, Volatility,
};
use sparkles::{Dataset, Error};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const PAUSE: &str = "urn:test:overview-pause";
// Every overview query embeds the caller's set query once and pauses this long. Two
// queries exceed the timeout together while each one stays well within it.
const STEP: Duration = Duration::from_millis(150);
const TIMEOUT: Duration = Duration::from_millis(250);

fn options(calls: &Arc<AtomicUsize>, timeout: Duration) -> QueryOptions {
    let count = calls.clone();
    let mut pause = ScalarDescriptor::new(
        PAUSE,
        1..=1,
        move |_: &ScalarContext<'_>, arguments: &[Term]| {
            count.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(STEP);
            Ok(arguments[0].clone())
        },
    )
    .unwrap();
    pause.volatility = Volatility::Volatile;
    let mut registry = ExtensionRegistry::builder();
    registry.register_scalar(pause).unwrap();
    QueryOptions {
        timeout: Some(timeout),
        no_cache: true,
        extensions: Some(registry.build()),
        ..Default::default()
    }
}

fn dataset() -> Dataset {
    let ds = Dataset::memory();
    ds.update(
        "INSERT DATA { <urn:a> a <urn:T> ; <urn:p> 1 ; <urn:q> <urn:b> . <urn:b> a <urn:U> }",
    )
    .unwrap();
    ds
}

fn facets() -> FacetsRequest {
    FacetsRequest::from_json(&json!({
        "within": {
            "query": format!("SELECT ?s WHERE {{ ?s a <urn:T> FILTER(isIRI(<{PAUSE}>(?s))) }}"),
            "variable": "s",
        },
        "facets": [{ "property": "urn:p" }],
        "filters": [{ "property": "urn:p", "range": [0, 2] }],
    }))
    .unwrap()
}

#[test]
fn facets_timeout_covers_every_query_of_the_answer() {
    let ds = dataset();
    let calls = Arc::new(AtomicUsize::new(0));
    let answer = ds
        .graph()
        .facets(&facets(), &options(&calls, Duration::from_secs(600)))
        .unwrap();
    assert_eq!(answer["size"], 1);
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "the fixture needs several paused queries"
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let result = ds.graph().facets(&facets(), &options(&calls, TIMEOUT));
    assert!(
        matches!(result, Err(Error::Timeout)),
        "each overview query renewed the relative timeout: {result:?}"
    );
}

#[test]
fn facets_at_anchors_the_timeout_once() {
    let ds = dataset();
    let calls = Arc::new(AtomicUsize::new(0));
    let result =
        sparkles::overview::facets_at(ds.store().snapshot(), &facets(), &options(&calls, TIMEOUT));
    assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
}

#[test]
fn groups_keep_answers_under_an_ample_timeout() {
    let ds = dataset();
    let req = GroupsRequest::from_json(&json!({"within": {"class": "urn:T"}})).unwrap();
    let opts = QueryOptions {
        timeout: Some(Duration::from_secs(600)),
        no_cache: true,
        ..Default::default()
    };
    let answer = ds.graph().groups(&req, &opts).unwrap();
    assert_eq!(answer["within"]["size"], 1);
}
