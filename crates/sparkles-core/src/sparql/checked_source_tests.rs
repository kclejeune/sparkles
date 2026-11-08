use super::*;
use crate::access::{GraphAccess, GraphRule, Graphs, Preparation};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use std::sync::atomic::Ordering;

fn store() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    store.load(&[crate::io::Source::from_bytes(
        b"<urn:s> <urn:p> 1 . <urn:allowed> { <urn:a> <urn:p> 2 . } <urn:hidden> { <urn:h> <urn:p> 3 . }".to_vec(),
        crate::io::RdfFormat::TriG, None,
    )]).unwrap();
    store
}

fn opts() -> QueryOptions {
    QueryOptions {
        no_cache: true,
        ..Default::default()
    }
}

#[test]
fn prepared_query_retains_genuine_source_credit_and_releases_on_final_result() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 2 << 20).unwrap();
    let baseline = preparation.inner_used();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let physical = Arc::downgrade(view.query_input().0);
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(Arc::ptr_eq(&result.ctx.snap, view.query_input().0));
    assert!(result.ctx.read_source.is_some());
    assert!(preparation.inner_used() > baseline);
    let context = Arc::downgrade(&result.ctx);
    drop(view);
    drop(store);
    assert!(physical.upgrade().is_some());
    assert_eq!(result.rows()[0][0].as_ref().unwrap().to_string(), "<urn:s>");
    drop(result);
    assert!(context.upgrade().is_none());
    assert!(physical.upgrade().is_none());
    assert_eq!(preparation.inner_used(), baseline);
}

#[test]
fn two_source_queries_share_the_original_preparation_cap_without_credit_gap() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 2 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let first = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    let used = preparation.inner_used();
    let pressure = preparation.reserve(preparation.room()).unwrap();
    assert!(matches!(
        query_source(
            view.read_source(),
            &store,
            "SELECT ?s WHERE { ?s <urn:p> ?o }",
            &opts()
        ),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(preparation.inner_used(), 2 << 20);
    assert_eq!(first.len(), 1);
    drop(pressure);
    assert_eq!(preparation.inner_used(), used);
    let second = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert!(preparation.inner_used() > used);
    drop(second);
    assert_eq!(preparation.inner_used(), used);
    drop(first);
}

#[test]
fn source_query_uses_original_pool_and_refuses_foreign_domain_without_leak() {
    let store = store();
    let pool = pool::QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let preparation = Preparation::with_grant(Control::none(), 2 << 20, grant.clone()).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let source_bytes = preparation.inner_used();
    let foreign = pool::QueryPool::new(64 << 20);
    let options = QueryOptions {
        pool: Some(foreign.clone()),
        ..opts()
    };
    let error = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .err()
    .unwrap();
    assert!(matches!(error, Error::Component(c) if c.code == "foreign-domain"));
    assert_eq!(
        (preparation.inner_used(), grant.used(), foreign.granted()),
        (source_bytes, source_bytes, 0)
    );
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(result.ctx.grant().unwrap(), &grant));
    assert_eq!(grant.used(), preparation.inner_used());
    drop(result);
    assert_eq!(
        (grant.used(), preparation.inner_used()),
        (source_bytes, source_bytes)
    );
    drop(view);
    drop(preparation);
    assert_eq!(grant.used(), 0);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn checked_query_cannot_replace_prepared_policy_and_applies_graph_restrictions() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 4 << 20).unwrap();
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    );
    let view = access
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let source_bytes = preparation.inner_used();
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    assert!(matches!(
        query_source(
            view.read_source(),
            &store,
            "SELECT ?g WHERE { GRAPH ?g { ?s <urn:p> ?o } }",
            &options
        ),
        Err(Error::Invalid(_))
    ));
    assert_eq!(preparation.inner_used(), source_bytes);
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?g WHERE { GRAPH ?g { ?s <urn:p> ?o } }",
        &opts(),
    )
    .unwrap();
    assert_eq!(
        result.rows(),
        vec![vec![Some(
            oxrdf::NamedNode::new("urn:allowed").unwrap().into()
        )]]
    );
    drop(result);
    let options = QueryOptions {
        default_graph_uris: vec!["urn:hidden".into()],
        ..opts()
    };
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .unwrap();
    assert!(result.is_empty());
    drop(result);
    let options = QueryOptions {
        union_default_graph: Some(true),
        ..opts()
    };
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .unwrap();
    assert_eq!(
        result.rows(),
        vec![vec![Some(oxrdf::NamedNode::new("urn:a").unwrap().into())]]
    );
    drop(result);
    assert_eq!(preparation.inner_used(), source_bytes);
}

#[test]
fn checked_query_preserves_source_and_option_cancellation_before_policy_work() {
    let store = store();
    let control = Control::none();
    let preparation = Preparation::new(control.clone(), 2 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let options = QueryOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    assert!(matches!(
        query_source(view.read_source(), &store, "ASK {}", &options),
        Err(Error::Cancelled)
    ));
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    control.cancel.cancel();
    assert!(matches!(result.ctx.check(), Err(Error::Cancelled)));
    assert!(matches!(
        query_source(view.read_source(), &store, "ASK {}", &opts()),
        Err(Error::Cancelled)
    ));
}

#[test]
fn checked_source_deadline_cannot_be_extended_by_cursor_wait_credit() {
    let store = store();
    let deadline = Instant::now() + Duration::from_secs(2);
    let control = Control {
        deadline: Some(deadline),
        ..Control::none()
    };
    let preparation = Preparation::new(control, 2 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    result
        .ctx
        .deadline_pause
        .store(60_000_000_000, Ordering::Relaxed);
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert!(matches!(result.ctx.check(), Err(Error::Timeout)));
}

#[test]
fn historical_source_requires_current_policy_and_real_runtime_instance() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let view = store
        .evidence_view(&crate::history::At::Head, &preparation)
        .unwrap();
    let foreign = Store::in_memory(Default::default());
    assert!(query_source(view.read_source(), &foreign, "ASK {}", &opts()).is_err());
    assert!(matches!(
        query_source(view.read_source(), &store, "ASK {}", &opts()),
        Err(Error::NotPermitted(_))
    ));
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.ctx.source_view.is_some());
    assert!(
        result
            .ctx
            .read_source
            .as_ref()
            .unwrap()
            .query_input()
            .1
            .is_none()
    );
    drop(view);
    drop(store);
    assert_eq!(result.rows()[0][0].as_ref().unwrap().to_string(), "<urn:s>");
}
