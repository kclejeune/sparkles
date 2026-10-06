//! Immutable, per-query application scalar functions and group aggregates.
//!
//! Both contracts accept batches, but initial execution supplies singleton batches.
//! It does not combine calls from independent solution rows or merge partial groups.

use super::ctx::{Charge, Ctx};
use crate::error::{Budget, Error, Result};
use oxrdf::{NamedNode, Term};
use rustc_hash::FxHashMap;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) type Failure = Arc<parking_lot::Mutex<Option<ScalarError>>>;

#[derive(Clone)]
struct CallbackFrame {
    families: Vec<uuid::Uuid>,
    failure: Failure,
    writers: Vec<(uuid::Uuid, Failure)>,
    ancestors: Vec<Failure>,
}

thread_local! {
    static CALLBACKS: std::cell::RefCell<Vec<CallbackFrame>> = const { std::cell::RefCell::new(Vec::new()) };
    static WRITERS: std::cell::RefCell<Vec<(u64, uuid::Uuid, Failure)>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) struct WriterOwner {
    token: u64,
    failure: Failure,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl WriterOwner {
    pub(crate) fn check(&self) -> Result<()> {
        match self.failure.lock().as_ref() {
            Some(failure) => Err(failure.engine()),
            None => Ok(()),
        }
    }

    pub(crate) fn enter(family: uuid::Uuid) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let token = NEXT.fetch_add(1, Ordering::Relaxed);
        let failure: Failure = Default::default();
        WRITERS.with(|w| w.borrow_mut().push((token, family, failure.clone())));
        Self {
            token,
            failure,
            _thread: std::marker::PhantomData,
        }
    }
}

impl Drop for WriterOwner {
    fn drop(&mut self) {
        WRITERS.with(|w| w.borrow_mut().retain(|(token, _, _)| *token != self.token));
    }
}

pub(crate) fn captured_writers() -> Vec<(uuid::Uuid, Failure)> {
    let mut writers: Vec<_> = WRITERS.with(|w| {
        w.borrow()
            .iter()
            .map(|(_, id, failure)| (*id, failure.clone()))
            .collect()
    });
    CALLBACKS.with(|c| {
        for frame in c.borrow().iter() {
            for (id, failure) in &frame.writers {
                if !writers.iter().any(|(_, known)| Arc::ptr_eq(known, failure)) {
                    writers.push((*id, failure.clone()));
                }
            }
        }
    });
    writers
}

pub(crate) fn captured_families(family: uuid::Uuid) -> Vec<uuid::Uuid> {
    let mut families = vec![family];
    WRITERS.with(|w| families.extend(w.borrow().iter().map(|(_, id, _)| *id)));
    CALLBACKS.with(|c| {
        for frame in c.borrow().iter() {
            families.extend(&frame.families);
        }
    });
    families.sort_unstable();
    families.dedup();
    families
}

pub(crate) fn captured_ancestors() -> Vec<Failure> {
    let mut failures = Vec::new();
    CALLBACKS.with(|c| {
        for frame in c.borrow().iter() {
            for failure in std::iter::once(&frame.failure).chain(&frame.ancestors) {
                if !failures.iter().any(|known| Arc::ptr_eq(known, failure)) {
                    failures.push(failure.clone());
                }
            }
        }
    });
    failures
}

/// Installed on the actual callback thread, including rayon workers. A nested query
/// on another family keeps both owner tokens until the corresponding calls return.
pub(crate) struct CallbackGuard;

impl CallbackGuard {
    pub(crate) fn enter(ctx: &Ctx) -> Self {
        CALLBACKS.with(|s| {
            s.borrow_mut().push(CallbackFrame {
                families: ctx.extension_families.clone(),
                failure: ctx
                    .extension_failure
                    .as_ref()
                    .expect("active callback context")
                    .clone(),
                writers: ctx.extension_writers.clone(),
                ancestors: ctx.extension_ancestors.clone(),
            })
        });
        Self
    }
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        CALLBACKS.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

pub(crate) fn check_family(dataset: uuid::Uuid) -> Result<()> {
    CALLBACKS.with(|s| {
        let owners = s.borrow();
        let mut forbidden = false;
        for frame in owners
            .iter()
            .filter(|frame| frame.families.contains(&dataset))
        {
            forbidden = true;
            let error = ScalarError::Execution(
                "nested operation on the callback's dataset family is forbidden".into(),
            );
            frame.failure.lock().get_or_insert(error.clone());
            for ancestor in &frame.ancestors {
                ancestor.lock().get_or_insert(error.clone());
            }
            for (_, writer) in frame.writers.iter().filter(|(id, _)| *id == dataset) {
                writer.lock().get_or_insert(error.clone());
            }
        }
        if forbidden {
            Err(Error::invalid(
                "nested operation on the callback's dataset family is forbidden",
            ))
        } else {
            Ok(())
        }
    })
}

struct ReentrantOperation;

pub(crate) fn assert_family(dataset: uuid::Uuid) {
    if check_family(dataset).is_err() {
        // Existing infallible storage APIs retain their signatures. This private
        // payload is caught by the callback boundary and skips the panic hook.
        std::panic::resume_unwind(Box::new(ReentrantOperation));
    }
}

pub const MAX_ARGUMENTS: usize = 128;
pub const MAX_BATCH_ROWS: usize = 1024;
pub const MAX_BATCH_BYTES: u64 = 1 << 20;

pub(crate) fn term_bytes(t: &Term) -> std::result::Result<u64, ScalarError> {
    use oxrdf::TermRef;
    let mut pending = vec![(t.as_ref(), 0)];
    let mut bytes = 0u64;
    while let Some((term, depth)) = pending.pop() {
        if depth > 128 {
            return Err(ScalarError::Execution(
                "callback RDF term exceeds nesting ceiling".into(),
            ));
        }
        bytes = bytes.saturating_add(64);
        let payload = match term {
            TermRef::NamedNode(n) => {
                NamedNode::new(n.as_str()).map_err(|_| {
                    ScalarError::Execution("callback returned an invalid IRI".into())
                })?;
                n.as_str().len()
            }
            TermRef::BlankNode(b) => {
                oxrdf::BlankNode::new(b.as_str()).map_err(|_| {
                    ScalarError::Execution("callback returned an invalid blank node label".into())
                })?;
                b.as_str().len()
            }
            TermRef::Literal(l) => {
                NamedNode::new(l.datatype().as_str()).map_err(|_| {
                    ScalarError::Execution("callback returned an invalid datatype IRI".into())
                })?;
                if let Some(language) = l.language() {
                    oxrdf::Literal::new_language_tagged_literal("", language).map_err(|_| {
                        ScalarError::Execution("callback returned an invalid language tag".into())
                    })?;
                }
                l.value()
                    .len()
                    .saturating_add(l.datatype().as_str().len())
                    .saturating_add(l.language().map_or(0, str::len))
            }
            TermRef::Triple(t) => {
                pending.push((t.subject.as_ref().into(), depth + 1));
                pending.push((t.predicate.as_ref().into(), depth + 1));
                pending.push((t.object.as_ref(), depth + 1));
                0
            }
        };
        bytes = bytes.saturating_add(payload as u64);
        if bytes > MAX_BATCH_BYTES {
            return Err(ScalarError::Execution(
                "callback RDF term exceeds batch byte ceiling".into(),
            ));
        }
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Volatility {
    Immutable,
    Stable,
    #[default]
    Volatile,
}

/// A domain error is a SPARQL expression error. All other variants abort execution.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ScalarError {
    #[error("application function expression error")]
    Expression,
    #[error("application function execution error: {0}")]
    Execution(String),
    #[error("query cancelled")]
    Cancelled,
    #[error("query timed out")]
    Timeout,
    #[error("{0}")]
    BudgetExceeded(Budget),
}

impl ScalarError {
    pub(crate) fn bounded(self) -> Self {
        match self {
            Self::Execution(message) => Self::Execution(bounded(&message)),
            other => other,
        }
    }

    pub(crate) fn from_engine(e: &Error) -> Self {
        match e {
            Error::Cancelled => Self::Cancelled,
            Error::Timeout => Self::Timeout,
            Error::BudgetExceeded(b) => Self::BudgetExceeded(*b),
            _ => Self::Execution(bounded(&e.to_string())),
        }
    }

    pub(crate) fn engine(&self) -> Error {
        match self {
            Self::Cancelled => Error::Cancelled,
            Self::Timeout => Error::Timeout,
            Self::BudgetExceeded(b) => Error::BudgetExceeded(*b),
            Self::Expression => Error::invalid("application function expression error"),
            Self::Execution(s) => Error::invalid(format!(
                "application function execution error: {}",
                bounded(s)
            )),
        }
    }
}

impl From<Error> for ScalarError {
    fn from(e: Error) -> Self {
        match e {
            Error::Cancelled => Self::Cancelled,
            Error::Timeout => Self::Timeout,
            Error::BudgetExceeded(b) => Self::BudgetExceeded(b),
            _ => Self::Execution(bounded(&e.to_string())),
        }
    }
}

fn bounded(s: &str) -> String {
    s.chars().take(512).collect()
}

pub type ScalarResult = std::result::Result<Term, ScalarError>;

/// Read-only query context. It exposes neither storage nor a nested query executor.
pub struct ScalarContext<'a> {
    pub(crate) ctx: &'a Ctx,
}

impl ScalarContext<'_> {
    pub fn timestamp(&self) -> oxsdatatypes::DateTime {
        self.ctx.now
    }

    /// Cancellation, deadline and prior execution failures remain fatal even if the
    /// callback catches this error and returns an RDF term.
    pub fn check(&self) -> std::result::Result<(), ScalarError> {
        self.ctx.check().map_err(|e| {
            let e = ScalarError::from(e);
            self.ctx.fail_extension(e.clone());
            e
        })
    }

    /// Charge application buffers while the returned guard remains alive.
    pub fn charge(&self, bytes: u64) -> std::result::Result<Charge<'_>, ScalarError> {
        self.ctx.charge(bytes).map_err(|e| {
            let e = ScalarError::from(e);
            self.ctx.fail_extension(e.clone());
            e
        })
    }
}

pub trait ScalarFunction: Send + Sync + 'static {
    fn call(&self, context: &ScalarContext<'_>, arguments: &[Term]) -> ScalarResult;

    /// Results must have the same length and order as `arguments`. Overriding this
    /// method expresses batch capability; current execution supplies one row per call.
    fn call_batch(
        &self,
        context: &ScalarContext<'_>,
        arguments: &[Vec<Term>],
    ) -> Vec<ScalarResult> {
        arguments.iter().map(|a| self.call(context, a)).collect()
    }
}

impl<F> ScalarFunction for F
where
    F: Fn(&ScalarContext<'_>, &[Term]) -> ScalarResult + Send + Sync + 'static,
{
    fn call(&self, context: &ScalarContext<'_>, arguments: &[Term]) -> ScalarResult {
        self(context, arguments)
    }
}

#[derive(Clone)]
pub struct ScalarDescriptor {
    pub iri: NamedNode,
    pub arity: std::ops::RangeInclusive<usize>,
    pub volatility: Volatility,
    pub description: Option<String>,
    pub(crate) implementation: Arc<dyn ScalarFunction>,
}

impl fmt::Debug for ScalarDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScalarDescriptor")
            .field("iri", &self.iri)
            .field("arity", &self.arity)
            .field("volatility", &self.volatility)
            .finish_non_exhaustive()
    }
}

impl ScalarDescriptor {
    pub fn new(
        iri: impl Into<String>,
        arity: std::ops::RangeInclusive<usize>,
        implementation: impl ScalarFunction,
    ) -> Result<Self> {
        let iri = NamedNode::new(iri.into()).map_err(|e| Error::invalid(e.to_string()))?;
        if arity.is_empty() || *arity.end() > MAX_ARGUMENTS {
            return Err(Error::invalid(
                "invalid application function arity (maximum 128)",
            ));
        }
        Ok(Self {
            iri,
            arity,
            volatility: Volatility::default(),
            description: None,
            implementation: Arc::new(implementation),
        })
    }
}

/// Context for one aggregate group. Retained charges last until its accumulator
/// is destroyed. Application state is owned by that accumulator, never this view.
pub struct AggregateContext<'a> {
    pub(crate) scalar: ScalarContext<'a>,
    pub(crate) retained: &'a Charge<'a>,
}

impl AggregateContext<'_> {
    pub fn timestamp(&self) -> oxsdatatypes::DateTime {
        self.scalar.timestamp()
    }

    pub fn check(&self) -> std::result::Result<(), ScalarError> {
        self.scalar.check()
    }

    pub fn charge(&self, bytes: u64) -> std::result::Result<Charge<'_>, ScalarError> {
        self.scalar.charge(bytes)
    }

    /// Account additional retained state before allocating it. The engine releases
    /// these charges when the group accumulator drops, including error paths.
    pub fn retain(&self, bytes: u64) -> std::result::Result<(), ScalarError> {
        self.retained.add(bytes).map_err(|e| {
            let e = ScalarError::from(e);
            self.scalar.ctx.fail_extension(e.clone());
            e
        })
    }
}

pub trait AggregateAccumulator: Send + 'static {
    /// Arguments are owned RDF terms valid for this call. Copying them into the
    /// accumulator requires charging retained state with `context.retain`.
    fn add(
        &mut self,
        context: &AggregateContext<'_>,
        argument: &Term,
    ) -> std::result::Result<(), ScalarError>;

    /// Current execution supplies singleton batches. Override this to express
    /// batch capability; no parallel partial aggregation is performed.
    fn add_batch(
        &mut self,
        context: &AggregateContext<'_>,
        arguments: &[Term],
    ) -> std::result::Result<(), ScalarError> {
        for argument in arguments {
            self.add(context, argument)?;
        }
        Ok(())
    }

    /// Finalize one group. An expression error makes its value unbound; execution
    /// errors are fatal. No finalization occurs after an argument or add domain
    /// error, but destruction always occurs inside the guarded callback boundary.
    fn finish(&mut self, context: &AggregateContext<'_>) -> ScalarResult;
}

pub trait AggregateFactory: Send + Sync + 'static {
    /// Called once per group, including an empty ungrouped aggregate. Explicitly
    /// grouped empty input creates no accumulator. Instances are never shared.
    fn create(
        &self,
        context: &AggregateContext<'_>,
    ) -> std::result::Result<Box<dyn AggregateAccumulator>, ScalarError>;
}

impl<F> AggregateFactory for F
where
    F: Fn(&AggregateContext<'_>) -> std::result::Result<Box<dyn AggregateAccumulator>, ScalarError>
        + Send
        + Sync
        + 'static,
{
    fn create(
        &self,
        context: &AggregateContext<'_>,
    ) -> std::result::Result<Box<dyn AggregateAccumulator>, ScalarError> {
        self(context)
    }
}

#[derive(Clone)]
pub struct AggregateDescriptor {
    pub iri: NamedNode,
    /// Explicit `AGG <iri>(expr)` syntax currently supports exactly one argument.
    pub arity: usize,
    pub volatility: Volatility,
    pub description: Option<String>,
    pub(crate) implementation: Arc<dyn AggregateFactory>,
}

impl fmt::Debug for AggregateDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AggregateDescriptor")
            .field("iri", &self.iri)
            .field("arity", &self.arity)
            .field("volatility", &self.volatility)
            .finish_non_exhaustive()
    }
}

impl AggregateDescriptor {
    pub fn new(
        iri: impl Into<String>,
        arity: usize,
        implementation: impl AggregateFactory,
    ) -> Result<Self> {
        let iri = NamedNode::new(iri.into()).map_err(|e| Error::invalid(e.to_string()))?;
        if arity != 1 {
            return Err(Error::invalid(
                "application aggregates require exactly one argument",
            ));
        }
        Ok(Self {
            iri,
            arity,
            volatility: Volatility::default(),
            description: None,
            implementation: Arc::new(implementation),
        })
    }
}

/// A property-function side is a single RDF term or a query-written RDF list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgumentShape {
    Term,
    List(std::ops::RangeInclusive<usize>),
}

impl ArgumentShape {
    pub(crate) fn accepts(&self, length: usize) -> bool {
        match self {
            Self::Term => length == 1,
            Self::List(range) => range.contains(&length),
        }
    }
    fn maximum(&self) -> usize {
        match self {
            Self::Term => 1,
            Self::List(r) => *r.end(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PropertyPosition {
    Subject(usize),
    Object(usize),
}

/// One invocation's owned bound/unbound arguments. Returned rows have this shape;
/// None preserves the input binding. New bindings require declared output positions.
#[derive(Clone, Debug)]
pub struct PropertyInput {
    pub subject: Vec<Option<Term>>,
    pub object: Vec<Option<Term>>,
}
pub type PropertyRow = PropertyInput;

pub use super::propertyext::{PropertyContext, PropertyReadBatch, PropertyScan, PropertyView};

pub trait PropertyFunction: Send + Sync + 'static {
    fn open<'query>(
        &self,
        context: PropertyContext<'query>,
        input: PropertyInput,
    ) -> std::result::Result<Box<dyn PropertyStream + 'query>, ScalarError>;
}

/// Pulled synchronously once per input solution, without memoizing duplicate inputs.
/// Expression errors discard every provisional row of this invocation; other errors
/// abort the query. Destruction always runs inside the guarded callback boundary.
pub trait PropertyStream: Send {
    fn next(
        &mut self,
        context: &PropertyContext<'_>,
    ) -> std::result::Result<Option<PropertyRow>, ScalarError>;
}

#[derive(Clone)]
pub struct PropertyDescriptor {
    pub iri: NamedNode,
    pub subject: ArgumentShape,
    pub object: ArgumentShape,
    pub required: Vec<PropertyPosition>,
    pub produces: Vec<PropertyPosition>,
    pub volatility: Volatility,
    pub description: Option<String>,
    pub cardinality_factor: Option<f64>,
    pub(crate) implementation: Arc<dyn PropertyFunction>,
}

impl fmt::Debug for PropertyDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PropertyDescriptor")
            .field("iri", &self.iri)
            .field("subject", &self.subject)
            .field("object", &self.object)
            .field("required", &self.required)
            .field("produces", &self.produces)
            .field("volatility", &self.volatility)
            .finish_non_exhaustive()
    }
}

impl PropertyDescriptor {
    pub fn new(
        iri: impl Into<String>,
        subject: ArgumentShape,
        object: ArgumentShape,
        required: Vec<PropertyPosition>,
        produces: Vec<PropertyPosition>,
        implementation: impl PropertyFunction,
    ) -> Result<Self> {
        let descriptor = Self {
            iri: NamedNode::new(iri.into()).map_err(|e| Error::invalid(e.to_string()))?,
            subject,
            object,
            required,
            produces,
            implementation: Arc::new(implementation),
            volatility: Volatility::default(),
            description: None,
            cardinality_factor: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
    fn validate(&self) -> Result<()> {
        NamedNode::new(self.iri.as_str())
            .map_err(|_| Error::invalid("invalid property function IRI"))?;
        for shape in [&self.subject, &self.object] {
            if matches!(shape, ArgumentShape::List(r) if r.is_empty()) {
                return Err(Error::invalid("invalid property argument shape"));
            }
        }
        if self.subject.maximum().saturating_add(self.object.maximum()) > MAX_ARGUMENTS {
            return Err(Error::invalid("property arguments exceed maximum 128"));
        }
        let mut seen = rustc_hash::FxHashSet::default();
        for position in self.required.iter().chain(&self.produces) {
            let (shape, index) = match position {
                PropertyPosition::Subject(i) => (&self.subject, *i),
                PropertyPosition::Object(i) => (&self.object, *i),
            };
            if index >= shape.maximum() || !seen.insert(*position) {
                return Err(Error::invalid("invalid or duplicate property position"));
            }
        }
        if self
            .cardinality_factor
            .is_some_and(|f| !f.is_finite() || f < 0.0)
        {
            return Err(Error::invalid("invalid property cardinality factor"));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ExtensionRegistryBuilder {
    scalars: FxHashMap<String, Arc<ScalarDescriptor>>,
    aggregates: FxHashMap<String, Arc<AggregateDescriptor>>,
    properties: FxHashMap<String, Arc<PropertyDescriptor>>,
}

impl ExtensionRegistryBuilder {
    pub fn register_scalar(&mut self, descriptor: ScalarDescriptor) -> Result<&mut Self> {
        let iri = descriptor.iri.as_str();
        NamedNode::new(iri).map_err(|_| Error::invalid("invalid application function IRI"))?;
        if descriptor.arity.is_empty() || *descriptor.arity.end() > MAX_ARGUMENTS {
            return Err(Error::invalid(
                "invalid application function arity (maximum 128)",
            ));
        }
        if super::expr::is_cast(iri)
            || super::catalog::extension_functions()
                .iter()
                .any(|s| s == iri)
            || super::catalog::extension_aggregates()
                .iter()
                .any(|s| s == iri)
        {
            return Err(Error::invalid(
                "application function cannot replace a built-in function or aggregate",
            ));
        }
        if self.scalars.contains_key(iri) || self.aggregates.contains_key(iri) {
            return Err(Error::invalid("duplicate application function IRI"));
        }
        self.scalars.insert(iri.to_owned(), Arc::new(descriptor));
        Ok(self)
    }

    pub fn register_aggregate(&mut self, descriptor: AggregateDescriptor) -> Result<&mut Self> {
        let iri = descriptor.iri.as_str();
        NamedNode::new(iri).map_err(|_| Error::invalid("invalid application aggregate IRI"))?;
        if descriptor.arity != 1 {
            return Err(Error::invalid(
                "application aggregates require exactly one argument",
            ));
        }
        if super::expr::is_cast(iri)
            || super::catalog::extension_functions()
                .iter()
                .any(|s| s == iri)
            || super::catalog::extension_aggregates()
                .iter()
                .any(|s| s == iri)
        {
            return Err(Error::invalid(
                "application aggregate cannot replace a built-in function or aggregate",
            ));
        }
        if self.scalars.contains_key(iri) || self.aggregates.contains_key(iri) {
            return Err(Error::invalid("duplicate application expression IRI"));
        }
        self.aggregates.insert(iri.to_owned(), Arc::new(descriptor));
        Ok(self)
    }

    pub fn register_property(&mut self, descriptor: PropertyDescriptor) -> Result<&mut Self> {
        descriptor.validate()?;
        let iri = descriptor.iri.as_str();
        if super::catalog::property_functions()
            .iter()
            .any(|s| s == iri)
            || crate::geo::vocab::SpatialPfKind::from_iri(iri).is_some()
        {
            return Err(Error::invalid(
                "application property cannot replace a built-in property function",
            ));
        }
        if self.properties.contains_key(iri) {
            return Err(Error::invalid("duplicate application property IRI"));
        }
        self.properties.insert(iri.to_owned(), Arc::new(descriptor));
        Ok(self)
    }

    pub fn build(self) -> Arc<ExtensionRegistry> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Arc::new(ExtensionRegistry {
            identity: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            scalars: self.scalars,
            aggregates: self.aggregates,
            properties: self.properties,
        })
    }
}

/// Immutable; building another registry gives it a distinct process-local identity.
#[derive(Debug)]
pub struct ExtensionRegistry {
    identity: u64,
    scalars: FxHashMap<String, Arc<ScalarDescriptor>>,
    aggregates: FxHashMap<String, Arc<AggregateDescriptor>>,
    properties: FxHashMap<String, Arc<PropertyDescriptor>>,
}

impl ExtensionRegistry {
    pub fn builder() -> ExtensionRegistryBuilder {
        ExtensionRegistryBuilder::default()
    }

    pub fn identity(&self) -> u64 {
        self.identity
    }

    pub fn scalar(&self, iri: &str) -> Option<&Arc<ScalarDescriptor>> {
        self.scalars.get(iri)
    }

    pub fn aggregate(&self, iri: &str) -> Option<&Arc<AggregateDescriptor>> {
        self.aggregates.get(iri)
    }

    pub fn property(&self, iri: &str) -> Option<&Arc<PropertyDescriptor>> {
        self.properties.get(iri)
    }

    pub fn is_empty(&self) -> bool {
        self.scalars.is_empty() && self.aggregates.is_empty() && self.properties.is_empty()
    }

    pub(crate) fn references(&self, p: &GraphPattern) -> bool {
        use GraphPattern as P;
        let expr = |e: &Expression| self.references_expr(e);
        let order = |o: &OrderExpression| match o {
            OrderExpression::Asc(e) | OrderExpression::Desc(e) => expr(e),
        };
        match p {
            P::Bgp { patterns } => patterns.iter().any(|t| matches!(&t.predicate,
                spargebra::term::NamedNodePattern::NamedNode(p) if self.properties.contains_key(p.as_str()))),
            P::Path { .. } | P::Values { .. } => false,
            P::Join { left, right }
            | P::Lateral { left, right }
            | P::Union { left, right }
            | P::Minus { left, right }
            | P::SemiJoin { left, right }
            | P::AntiJoin { left, right } => self.references(left) || self.references(right),
            P::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.references(left)
                    || self.references(right)
                    || expression.as_ref().is_some_and(expr)
            }
            P::Filter { inner, expr: e } => self.references(inner) || expr(e),
            P::Extend {
                inner, expression, ..
            }
            | P::Assign {
                inner, expression, ..
            }
            | P::Unfold {
                inner, expression, ..
            } => self.references(inner) || expr(expression),
            P::OrderBy { inner, expression } => {
                self.references(inner) || expression.iter().any(order)
            }
            P::Group {
                inner, aggregates, ..
            } => {
                self.references(inner)
                    || aggregates.iter().any(|(_, a)| match a {
                        AggregateExpression::CountSolutions { .. } => false,
                        AggregateExpression::FunctionCall { name, expr: e, .. } => {
                            matches!(name, AggregateFunction::Custom(n) if self.aggregates.contains_key(n.as_str()))
                                || expr(e)
                        }
                        AggregateExpression::Fold {
                            expr: e,
                            value,
                            order: os,
                            ..
                        } => expr(e) || value.iter().any(expr) || os.iter().any(order),
                    })
            }
            P::Graph { inner, .. }
            | P::Project { inner, .. }
            | P::Distinct { inner }
            | P::Reduced { inner }
            | P::Slice { inner, .. }
            | P::Service { inner, .. } => self.references(inner),
        }
    }

    fn references_expr(&self, e: &Expression) -> bool {
        use Expression as E;
        match e {
            E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => false,
            E::FunctionCall(f, args) => {
                matches!(f, Function::Custom(n) if self.scalars.contains_key(n.as_str()))
                    || args.iter().any(|e| self.references_expr(e))
            }
            E::Or(a, b)
            | E::And(a, b)
            | E::Equal(a, b)
            | E::SameTerm(a, b)
            | E::Greater(a, b)
            | E::GreaterOrEqual(a, b)
            | E::Less(a, b)
            | E::LessOrEqual(a, b)
            | E::Add(a, b)
            | E::Subtract(a, b)
            | E::Multiply(a, b)
            | E::Divide(a, b) => self.references_expr(a) || self.references_expr(b),
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => self.references_expr(a),
            E::In(a, list) => {
                self.references_expr(a) || list.iter().any(|e| self.references_expr(e))
            }
            E::If(a, b, c) => {
                self.references_expr(a) || self.references_expr(b) || self.references_expr(c)
            }
            E::Coalesce(list) => list.iter().any(|e| self.references_expr(e)),
            E::Exists(p) => self.references(p),
        }
    }
}
