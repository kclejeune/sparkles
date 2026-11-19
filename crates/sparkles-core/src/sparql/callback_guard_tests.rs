//! A registered extension reached from a context that was not configured for it fails
//! the query with an error. It never panics, and so never panics a second time in a
//! destructor while unwinding, which would abort the process (roadmap item 1198).

use super::extensions::{
    ArgumentShape, CallbackGuard, ExtensionRegistry, PropertyContext, PropertyDescriptor,
    PropertyFunction, PropertyInput, PropertyPosition, PropertyStream, ScalarContext,
    ScalarDescriptor, ScalarError, ScalarFunction, ScalarResult,
};
use super::plan::Planner;
use super::*;
use crate::store::{Store, StoreOptions};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) const EXISTS_QUERY: &str = "SELECT ?b WHERE {
    BIND(COALESCE(EXISTS { <urn:s> <urn:callback> ?x }, false) AS ?b)
}";
pub(super) const FALLBACK_QUERY: &str = "SELECT ?b ?c WHERE {
    BIND(IF(false, EXISTS { <urn:s> <urn:callback> ?x }, true) AS ?b)
    BIND(COALESCE(?missing, false) AS ?c)
}";

struct Never(Arc<AtomicUsize>);
impl PropertyFunction for Never {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        _: PropertyInput,
    ) -> std::result::Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("an unconfigured context must not call the function")
    }
}
impl ScalarFunction for Never {
    fn call(&self, _: &ScalarContext<'_>, _: &[Term]) -> ScalarResult {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("an unconfigured context must not call the function")
    }
}

/// A context with the registry and without `configure_extensions`, as a rewrite that
/// adds a call after configuration would leave it.
fn unconfigured() -> (Ctx, Arc<AtomicUsize>) {
    let opens = Arc::new(AtomicUsize::new(0));
    let mut r = ExtensionRegistry::builder();
    r.register_property(
        PropertyDescriptor::new(
            "urn:callback",
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![PropertyPosition::Subject(0)],
            vec![PropertyPosition::Object(0)],
            Never(opens.clone()),
        )
        .unwrap(),
    )
    .unwrap();
    r.register_scalar(ScalarDescriptor::new("urn:scalar", 0..=0, Never(opens.clone())).unwrap())
        .unwrap();
    let opts = QueryOptions {
        extensions: Some(r.build()),
        no_cache: true,
        ..Default::default()
    };
    let store = Store::in_memory(StoreOptions::default());
    (
        make_ctx(store.snapshot(), &opts, None, None).unwrap(),
        opens,
    )
}

pub(super) fn prepared(parsed: &Query, cursor: bool) -> (PreparedQuery, Arc<AtomicUsize>) {
    let (mut ctx, opens) = unconfigured();
    if cursor {
        ctx.enable_cursor_accounting().unwrap();
    }
    let ctx = Arc::new(ctx);
    ctx.attach_owner();
    let (pattern, _, _) = split(parsed);
    let node = Planner::new(&ctx)
        .plan(pattern, &ActiveGraph::Default, Vec::new())
        .unwrap();
    let plan_depth = depth::check_plan(node.depth()).unwrap();
    (
        PreparedQuery {
            ctx,
            node,
            pattern: pattern.clone(),
            bound: Vec::new(),
            kind: QueryKind::Select,
            plan_depth,
        },
        opens,
    )
}

pub(super) fn assert_unconfigured(error: Error) {
    assert!(extensions::is_unconfigured(&error), "{error:?}");
}

#[test]
fn the_guard_refuses_an_unconfigured_context() {
    let (ctx, _) = unconfigured();
    assert!(!ctx.calls_extensions);
    let error = CallbackGuard::enter(&ctx)
        .err()
        .expect("an error, not a guard");
    assert_unconfigured(error);
}

#[test]
fn an_owned_value_drops_without_a_guard_or_an_unwind() {
    struct Panics(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Panics {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            panic!("application destructor")
        }
    }
    let (ctx, _) = unconfigured();
    // nothing opened: nothing to drop and no guard to enter
    CallbackGuard::drop_owned::<Panics>(&ctx, None, "unused");
    // an unguarded drop still contains the application's panic
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    CallbackGuard::drop_owned(&ctx, Some(Panics(dropped.clone())), "destructor panicked");
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}

/// A destructor that panics with a payload whose own destructor panics: the payload is
/// leaked, so neither panic leaves `drop_owned`.
#[test]
fn a_panicking_payload_does_not_escape_the_drop() {
    struct Payload;
    impl Drop for Payload {
        fn drop(&mut self) {
            panic!("payload destructor")
        }
    }
    struct PanicsWithPayload;
    impl Drop for PanicsWithPayload {
        fn drop(&mut self) {
            std::panic::panic_any(Payload)
        }
    }
    let (ctx, _) = unconfigured();
    CallbackGuard::drop_owned(&ctx, Some(PanicsWithPayload), "destructor panicked");
}

#[test]
fn the_planner_refuses_a_call_its_context_was_not_configured_for() {
    let (ctx, _) = unconfigured();
    let parsed = parse_query("SELECT * { <urn:s> <urn:callback> ?x }", None, &[]).unwrap();
    let (pattern, _, _) = split(&parsed);
    let error = Planner::new(&ctx)
        .plan(pattern, &ActiveGraph::Default, Vec::new())
        .err()
        .expect("a refusal");
    assert_unconfigured(error);
}

#[test]
fn eager_exists_cannot_hide_the_unconfigured_call() {
    assert_eager_unconfigured(EXISTS_QUERY);
}

#[test]
fn eager_scalar_fallback_cannot_hide_the_unconfigured_call() {
    assert_eager_unconfigured("SELECT ?b WHERE { BIND(COALESCE(<urn:scalar>(), false) AS ?b) }");
}

fn assert_eager_unconfigured(query: &str) {
    let parsed = parse_query(query, None, &[]).unwrap();
    let (prepared, opens) = prepared(&parsed, false);
    let result = execute_prepared(&parsed, &Default::default(), 0.0, 0.0, prepared);
    assert_eq!(opens.load(Ordering::Relaxed), 0);
    let error = match result {
        Err(error) => error,
        Ok(result) => panic!(
            "unexpected eager rows: {:?}",
            result.test_application_rows()
        ),
    };
    assert_unconfigured(error.error);
}

#[test]
fn eager_untaken_exists_and_expression_fallback_succeed() {
    let parsed = parse_query(FALLBACK_QUERY, None, &[]).unwrap();
    let (prepared, opens) = prepared(&parsed, false);
    let result = execute_prepared(&parsed, &Default::default(), 0.0, 0.0, prepared).unwrap();
    assert_eq!(
        result.test_application_rows(),
        [vec![
            Some(Term::Literal(oxrdf::Literal::from(true))),
            Some(Term::Literal(oxrdf::Literal::from(false))),
        ]]
    );
    assert_eq!(opens.load(Ordering::Relaxed), 0);
}
