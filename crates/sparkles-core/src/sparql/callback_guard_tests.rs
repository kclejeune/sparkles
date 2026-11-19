//! A registered extension reached from a context that was not configured for it fails
//! the query with an error. It never panics, and so never panics a second time in a
//! destructor while unwinding, which would abort the process (roadmap item 1198).

use super::extensions::{
    ArgumentShape, CallbackGuard, ExtensionRegistry, PropertyContext, PropertyDescriptor,
    PropertyFunction, PropertyInput, PropertyPosition, PropertyStream, ScalarError,
};
use super::plan::Planner;
use super::*;
use crate::store::{Store, StoreOptions};

struct Never;
impl PropertyFunction for Never {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        _: PropertyInput,
    ) -> std::result::Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        panic!("an unconfigured context must not call the function")
    }
}

/// A context with the registry and without `configure_extensions`, as a rewrite that
/// adds a call after configuration would leave it.
fn unconfigured() -> Ctx {
    let mut r = ExtensionRegistry::builder();
    r.register_property(
        PropertyDescriptor::new(
            "urn:callback",
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![PropertyPosition::Subject(0)],
            vec![PropertyPosition::Object(0)],
            Never,
        )
        .unwrap(),
    )
    .unwrap();
    let opts = QueryOptions {
        extensions: Some(r.build()),
        no_cache: true,
        ..Default::default()
    };
    let store = Store::in_memory(StoreOptions::default());
    make_ctx(store.snapshot(), &opts, None, None).unwrap()
}

#[test]
fn the_guard_refuses_an_unconfigured_context() {
    let ctx = unconfigured();
    assert!(!ctx.calls_extensions);
    let error = CallbackGuard::enter(&ctx)
        .err()
        .expect("an error, not a guard");
    assert!(error.to_string().contains("outside the calls"), "{error}");
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
    let ctx = unconfigured();
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
    let ctx = unconfigured();
    CallbackGuard::drop_owned(&ctx, Some(PanicsWithPayload), "destructor panicked");
}

#[test]
fn the_planner_refuses_a_call_its_context_was_not_configured_for() {
    let ctx = unconfigured();
    let parsed = parse_query("SELECT * { <urn:s> <urn:callback> ?x }", None, &[]).unwrap();
    let (pattern, _, _) = split(&parsed);
    let error = Planner::new(&ctx)
        .plan(pattern, &ActiveGraph::Default, Vec::new())
        .err()
        .expect("a refusal");
    assert!(error.to_string().contains("outside the calls"), "{error}");
}
