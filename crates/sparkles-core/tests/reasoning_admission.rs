//! Refusal propagation across the allocation-capable reasoning predicate.
use oxrdf::Term;
use sparkles_core::sparql::reasoning::{
    Demand, DemandInfo, Derived, ReasoningProvider, ReasoningSession, Report, SessionRequest,
};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{Budget, BudgetKind, Error, Result};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Calls {
    intensional: AtomicUsize,
    prepare: AtomicUsize,
    derive: AtomicUsize,
}
struct RefusingProvider {
    calls: Arc<Calls>,
    path: bool,
}
impl ReasoningProvider for RefusingProvider {
    fn label(&self) -> String {
        "refusal-control".into()
    }
    fn start(&self, _: SessionRequest) -> Result<Box<dyn ReasoningSession>> {
        Ok(Box::new(RefusingSession {
            calls: self.calls.clone(),
            path: self.path,
        }))
    }
}
struct RefusingSession {
    calls: Arc<Calls>,
    path: bool,
}
impl ReasoningSession for RefusingSession {
    fn intensional(&self, s: Option<&Term>, p: Option<&Term>, o: Option<&Term>) -> Result<bool> {
        self.calls.intensional.fetch_add(1, Ordering::SeqCst);
        if self.path {
            assert!(
                s.is_none() && p.is_some() && o.is_none(),
                "actual property-path branch"
            );
        } else {
            assert!(s.is_some(), "actual triple-pattern branch");
        }
        Err(Error::BudgetExceeded(Budget {
            kind: BudgetKind::ServerMemory,
            limit: 17,
            requested: 23,
        }))
    }
    fn prepare(&mut self, _: &[Demand]) -> Result<Vec<DemandInfo>> {
        self.calls.prepare.fetch_add(1, Ordering::SeqCst);
        Err(Error::invalid(
            "prepare must not follow intensional refusal",
        ))
    }
    fn derive(&mut self, _: Vec<(usize, Vec<u64>)>) -> Result<Derived> {
        self.calls.derive.fetch_add(1, Ordering::SeqCst);
        Err(Error::invalid("derive must not follow intensional refusal"))
    }
    fn report(&self) -> Report {
        Report::default()
    }
}
fn check(text: &str, path: bool) {
    let store = Store::in_memory(StoreOptions::default());
    let calls = Arc::new(Calls::default());
    let options = QueryOptions {
        reasoning: Some(Arc::new(RefusingProvider {
            calls: calls.clone(),
            path,
        })),
        ..Default::default()
    };
    let error = query(store.snapshot(), text, &options)
        .err()
        .expect("must refuse, not answer empty/extensional success");
    let Error::BudgetExceeded(budget) = error else {
        panic!("wrong refusal: {error:?}");
    };
    assert_eq!(
        (budget.kind, budget.limit, budget.requested),
        (BudgetKind::ServerMemory, 17, 23)
    );
    assert_eq!(
        calls.intensional.load(Ordering::SeqCst),
        1,
        "later patterns must not run"
    );
    assert_eq!(calls.prepare.load(Ordering::SeqCst), 0);
    assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
}
#[test]
fn triple_intensional_refusal_short_circuits_before_prepare_or_later_patterns() {
    check(
        "SELECT ?o WHERE { <urn:a> <urn:p> ?o . <urn:z> <urn:q> ?o }",
        false,
    );
}
#[test]
fn property_path_intensional_refusal_propagates_before_prepare() {
    check("SELECT ?o WHERE { <urn:a> <urn:p>+ ?o }", true);
}
