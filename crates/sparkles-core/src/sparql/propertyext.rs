//! Registered property callbacks and their query-borrowed, access-filtered view.

use super::ctx::{Charge, Ctx};
use super::extensions::{
    ArgumentShape, CallbackGuard, MAX_BATCH_BYTES, MAX_BATCH_ROWS, PropertyDescriptor,
    PropertyInput, PropertyPosition, PropertyStream, ScalarError, term_bytes,
};
use super::plan::{ActiveGraph, GraphFilter, Kind, Node, PT, Planner};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{ALL_COLS, Perm};
use crate::store::Chunk;
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use parking_lot::Mutex;
use rustc_hash::FxHashSet;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use std::sync::Arc;

struct Scope<'q> {
    ctx: &'q Ctx,
    graph: Arc<GraphFilter>,
    retained: Mutex<Charge<'q>>,
    blanks: Mutex<FxHashSet<Term>>,
}

/// Invocation-borrowed context. Retaining application copies requires retain();
/// temporary buffers may instead hold a charge guard. No nested executor is exposed.
#[derive(Clone)]
pub struct PropertyContext<'q> {
    scope: Arc<Scope<'q>>,
}

impl<'q> PropertyContext<'q> {
    pub fn timestamp(&self) -> oxsdatatypes::DateTime {
        self.scope.ctx.now
    }
    pub fn check(&self) -> std::result::Result<(), ScalarError> {
        self.guarded(|| self.scope.ctx.check())
    }
    pub fn charge(&self, bytes: u64) -> std::result::Result<Charge<'q>, ScalarError> {
        self.guarded(|| self.scope.ctx.charge(bytes))
    }
    pub fn retain(&self, bytes: u64) -> std::result::Result<(), ScalarError> {
        self.guarded(|| self.scope.retained.lock().add(bytes))
    }
    pub fn view(&self) -> PropertyView<'q> {
        PropertyView {
            context: self.clone(),
        }
    }
    fn guarded<T>(&self, f: impl FnOnce() -> Result<T>) -> std::result::Result<T, ScalarError> {
        let ctx = self.scope.ctx;
        let _owner = CallbackGuard::enter(ctx);
        ctx.check().and_then(|()| f()).map_err(|e| {
            let error = ScalarError::from_engine(&e);
            ctx.fail_extension(error.clone());
            error
        })
    }
    fn remember(&self, term: &Term) -> Result<()> {
        match term {
            Term::BlankNode(_) => {
                let mut blanks = self.scope.blanks.lock();
                if !blanks.contains(term) {
                    self.scope
                        .retained
                        .lock()
                        .add(term_bytes(term).map_err(|e| e.engine())? + 32)?;
                    blanks.insert(term.clone());
                }
            }
            Term::Triple(t) => {
                self.remember(&t.subject.clone().into())?;
                self.remember(&t.object)?;
            }
            Term::Literal(l) if super::cdt::is_cdt(l.datatype().as_str()) => {
                let mut error = None;
                let _parsed =
                    super::cdt::callback_relabel_term(self.scope.ctx, term, &mut |label| {
                        if error.is_none() {
                            let blank = Term::BlankNode(oxrdf::BlankNode::new_unchecked(label));
                            error = self.remember(&blank).err();
                        }
                        label.to_owned()
                    })?;
                if let Some(error) = error {
                    return Err(error);
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn intern(&self, term: &Term) -> Result<Id> {
        // Project permissions onto this term rather than cloning the full scan's
        // growing blank ledger on every output (which would be quadratic).
        fn allowed(
            ctx: &Ctx,
            term: &Term,
            ledger: &FxHashSet<Term>,
            out: &mut FxHashSet<Term>,
            charge: &Charge<'_>,
        ) -> Result<()> {
            match term {
                Term::BlankNode(_) if ledger.contains(term) && !out.contains(term) => {
                    charge.add(term_bytes(term).map_err(|e| e.engine())? + 32)?;
                    out.insert(term.clone());
                }
                Term::Triple(t) => {
                    allowed(ctx, &t.subject.clone().into(), ledger, out, charge)?;
                    allowed(ctx, &t.object, ledger, out, charge)?;
                }
                Term::Literal(l) if super::cdt::is_cdt(l.datatype().as_str()) => {
                    let mut failure = None;
                    let _parsed = super::cdt::callback_relabel_term(ctx, term, &mut |label| {
                        if failure.is_none() {
                            let blank = Term::BlankNode(oxrdf::BlankNode::new_unchecked(label));
                            failure = allowed(ctx, &blank, ledger, out, charge).err();
                        }
                        label.to_owned()
                    })?;
                    if let Some(error) = failure {
                        return Err(error);
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let temporary = self.scope.ctx.charge(0)?;
        let mut blanks = FxHashSet::default();
        allowed(
            self.scope.ctx,
            term,
            &self.scope.blanks.lock(),
            &mut blanks,
            &temporary,
        )?;
        temporary.add(
            (blanks.len() * std::mem::size_of::<Term>() + std::mem::size_of::<Vec<Term>>()) as u64,
        )?;
        let blanks: Vec<_> = blanks.into_iter().collect();
        self.scope.ctx.intern_callback_term(term, &blanks)
    }
}

/// Captured, read-only SPO view of the active graph and caller's restricted dataset.
/// It exposes no graph enumeration, raw IDs, unfiltered statistics or storage handle.
/// The captured view cannot be retained beyond its query:
///
/// ```compile_fail
/// use sparkles_core::sparql::extensions::PropertyView;
/// fn retain_for_later<'query>(view: PropertyView<'query>) -> PropertyView<'static> {
///     view
/// }
/// ```
#[derive(Clone)]
pub struct PropertyView<'q> {
    context: PropertyContext<'q>,
}

impl<'q> PropertyView<'q> {
    pub fn scan(
        &self,
        subject: Option<NamedOrBlankNode>,
        predicate: Option<NamedNode>,
        object: Option<Term>,
    ) -> std::result::Result<PropertyScan<'q>, ScalarError> {
        self.context.guarded(|| {
            let terms = [subject.map(Term::from), predicate.map(Term::from), object];
            let bytes = terms.iter().flatten().try_fold(256u64, |n, t| {
                term_bytes(t)
                    .map(|b| n.saturating_add(b))
                    .map_err(|e| e.engine())
            })?;
            if bytes > MAX_BATCH_BYTES {
                return Err(Error::invalid("property scan pattern exceeds byte ceiling"));
            }
            let charge = self.context.scope.ctx.charge(bytes)?;
            let ids = terms
                .iter()
                .map(|t| t.as_ref().map(|t| self.context.intern(t)).transpose())
                .collect::<Result<Vec<_>>>()?;
            let perm = [
                Perm::Spo,
                Perm::Sop,
                Perm::Pso,
                Perm::Pos,
                Perm::Osp,
                Perm::Ops,
            ]
            .into_iter()
            .max_by_key(|p| {
                p.order()[..3]
                    .iter()
                    .take_while(|&&i| ids[i].is_some())
                    .count()
            })
            .unwrap();
            let mut lo = [0; 4];
            let mut hi = [u64::MAX; 4];
            for (column, &component) in perm.order()[..3].iter().enumerate() {
                let Some(id) = ids[component] else { break };
                lo[column] = id.0;
                hi[column] = id.0;
            }
            Ok(PropertyScan {
                context: self.context.clone(),
                perm,
                ids: [ids[0], ids[1], ids[2]],
                lo: Some(lo),
                hi,
                previous: None,
                _charge: charge,
            })
        })
    }
}

/// A bounded pull cursor. All returned batches and cursors remain query-borrowed.
pub struct PropertyScan<'q> {
    context: PropertyContext<'q>,
    perm: Perm,
    ids: [Option<Id>; 3],
    lo: Option<[u64; 4]>,
    hi: [u64; 4],
    previous: Option<[Id; 3]>,
    _charge: Charge<'q>,
}

pub struct PropertyReadBatch<'q> {
    triples: Vec<Triple>,
    _charge: Charge<'q>,
}
impl PropertyReadBatch<'_> {
    pub fn triples(&self) -> &[Triple] {
        &self.triples
    }
}

fn successor(mut key: [u64; 4]) -> Option<[u64; 4]> {
    for i in (0..4).rev() {
        if key[i] != u64::MAX {
            key[i] += 1;
            return Some(key);
        }
        key[i] = 0;
    }
    None
}

impl<'q> PropertyScan<'q> {
    pub fn next_batch(
        &mut self,
    ) -> std::result::Result<Option<PropertyReadBatch<'q>>, ScalarError> {
        let context = self.context.clone();
        context.guarded(|| {
            let Some(lo) = self.lo else { return Ok(None) };
            let ctx = context.scope.ctx;
            let charge = ctx.charge(64)?;
            let mut triples = Vec::new();
            let mut bytes = 0u64;
            let mut last = None;
            let mut stopped = false;
            let mut consume = |key: [u64; 4]| -> Result<bool> {
                ctx.check()?;
                let q = self.perm.to_quad(&key);
                if !context.scope.graph.accepts(q[3].0)
                    || self
                        .ids
                        .iter()
                        .enumerate()
                        .any(|(i, id)| id.is_some_and(|id| id != q[i]))
                {
                    last = Some(key);
                    return Ok(true);
                }
                let spo = [q[0], q[1], q[2]];
                if self.previous == Some(spo) {
                    ctx.produced(1)?;
                    last = Some(key);
                    return Ok(true);
                }
                let decoded = ctx.charge(256)?;
                let terms = spo
                    .map(|id| {
                        ctx.term(id)
                            .ok_or_else(|| Error::invalid("invalid captured RDF term"))
                    })
                    .into_iter()
                    .collect::<Result<Vec<_>>>()?;
                let cost = terms.iter().try_fold(128u64, |n, t| {
                    term_bytes(t)
                        .map(|b| n.saturating_add(b))
                        .map_err(|e| e.engine())
                })?;
                decoded.add(cost.saturating_sub(128))?;
                if cost > MAX_BATCH_BYTES {
                    return Err(Error::invalid("property read triple exceeds byte ceiling"));
                }
                if !triples.is_empty() && bytes.saturating_add(cost) > MAX_BATCH_BYTES {
                    stopped = true;
                    return Ok(false);
                }
                // Consume work only after deciding this key fits the page. A key
                // deferred by the byte boundary is charged on its next pull, once.
                ctx.produced(1)?;
                charge.add(cost)?;
                for term in &terms {
                    context.remember(term)?;
                }
                let subject = NamedOrBlankNode::try_from(terms[0].clone())
                    .map_err(|_| Error::invalid("invalid captured subject"))?;
                let Term::NamedNode(predicate) = terms[1].clone() else {
                    return Err(Error::invalid("invalid captured predicate"));
                };
                triples.push(Triple::new(subject, predicate, terms[2].clone()));
                bytes += cost;
                self.previous = Some(spo);
                last = Some(key);
                ctx.check_rows(triples.len())?;
                if triples.len() >= MAX_BATCH_ROWS {
                    stopped = true;
                    Ok(false)
                } else {
                    Ok(true)
                }
            };
            ctx.snap
                .scan_between_cols(self.perm, lo, self.hi, ALL_COLS, |chunk| match chunk {
                    Chunk::Row(key) => consume(key),
                    Chunk::Block(block, start, end) => {
                        for row in start..end {
                            let key = std::array::from_fn(|c| block.cols[c][row]);
                            if !consume(key)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    }
                })?;
            self.lo = if stopped {
                last.and_then(successor).filter(|key| *key <= self.hi)
            } else {
                None
            };
            if triples.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PropertyReadBatch {
                    triples,
                    _charge: charge,
                }))
            }
        })
    }
}

#[derive(Clone)]
pub(super) struct Call {
    descriptor: Arc<PropertyDescriptor>,
    subject: Vec<TermPattern>,
    object: Vec<TermPattern>,
}
#[derive(Clone)]
pub struct Spec {
    descriptor: Arc<PropertyDescriptor>,
    subject: Vec<PT>,
    object: Vec<PT>,
    graph: Arc<GraphFilter>,
}

pub(super) fn has_calls(ctx: &Ctx, patterns: &[TriplePattern]) -> bool {
    ctx.extensions.as_ref().is_some_and(|r| {
        patterns.iter().any(|t|
        matches!(&t.predicate, NamedNodePattern::NamedNode(n) if r.property(n.as_str()).is_some()))
    })
}

// Query-written collection extraction is local to the registered contract. The
// built-in text/property parser's 64-cell limit remains unchanged.
fn take_list(
    patterns: &[TriplePattern],
    head: &TermPattern,
) -> Result<(Vec<TermPattern>, Vec<usize>)> {
    use oxrdf::vocab::rdf;
    let TermPattern::BlankNode(head) = head else {
        return Err(Error::invalid(
            "property argument must be a query-written RDF list",
        ));
    };
    let mut cell = head.clone();
    let mut used = FxHashSet::default();
    let mut elements = Vec::new();
    loop {
        let link = |predicate: oxrdf::NamedNodeRef<'_>| -> Result<usize> {
            let mut matches = patterns.iter().enumerate().filter(|(_, t)| {
                matches!(&t.subject, TermPattern::BlankNode(b) if b == &cell)
                    && matches!(&t.predicate, NamedNodePattern::NamedNode(n) if *n == predicate)
            });
            let (Some((i, _)), None) = (matches.next(), matches.next()) else {
                return Err(Error::invalid("malformed property argument list"));
            };
            Ok(i)
        };
        let first = link(rdf::FIRST)?;
        let rest = link(rdf::REST)?;
        if !used.insert(first)
            || !used.insert(rest)
            || elements.len() >= super::extensions::MAX_ARGUMENTS
        {
            return Err(Error::invalid("cyclic or oversized property argument list"));
        }
        elements.push(patterns[first].object.clone());
        match &patterns[rest].object {
            TermPattern::NamedNode(n) if *n == rdf::NIL => break,
            TermPattern::BlankNode(next) => cell = next.clone(),
            _ => return Err(Error::invalid("malformed property argument list")),
        }
    }
    Ok((elements, used.into_iter().collect()))
}

pub(super) fn take_calls(
    ctx: &Ctx,
    patterns: &[TriplePattern],
) -> Result<(Vec<Call>, Vec<TriplePattern>)> {
    let registry = ctx.extensions.as_ref().unwrap();
    let mut used = FxHashSet::default();
    let mut calls = Vec::new();
    for (index, triple) in patterns.iter().enumerate() {
        let NamedNodePattern::NamedNode(iri) = &triple.predicate else {
            continue;
        };
        let Some(descriptor) = registry.property(iri.as_str()) else {
            continue;
        };
        // Decode against the original BGP: shared heads are deterministic inputs,
        // and their cells are removed once after every side/call has read them.
        used.insert(index);
        let mut side = |head: &TermPattern, shape: &ArgumentShape| -> Result<Vec<TermPattern>> {
            let elements = match shape {
                ArgumentShape::Term => vec![head.clone()],
                ArgumentShape::List(_) if matches!(head, TermPattern::NamedNode(n) if *n == oxrdf::vocab::rdf::NIL) => {
                    Vec::new()
                }
                ArgumentShape::List(_) => {
                    let (elements, cells) = take_list(patterns, head)?;
                    used.extend(cells);
                    elements
                }
            };
            if !shape.accepts(elements.len()) {
                return Err(Error::invalid("property argument shape mismatch"));
            }
            Ok(elements)
        };
        let subject = side(&triple.subject, &descriptor.subject)?;
        let object = side(&triple.object, &descriptor.object)?;
        for p in descriptor.required.iter().chain(&descriptor.produces) {
            let (args, i) = match p {
                PropertyPosition::Subject(i) => (&subject, i),
                PropertyPosition::Object(i) => (&object, i),
            };
            if *i >= args.len() {
                return Err(Error::invalid(
                    "property position absent from argument list",
                ));
            }
        }
        calls.push(Call {
            descriptor: descriptor.clone(),
            subject,
            object,
        });
    }
    let rest = patterns
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(_, pattern)| pattern.clone())
        .collect();
    Ok((calls, rest))
}

fn slot<'a, T>(subject: &'a [T], object: &'a [T], p: &PropertyPosition) -> &'a T {
    match p {
        PropertyPosition::Subject(i) => &subject[*i],
        PropertyPosition::Object(i) => &object[*i],
    }
}

pub(super) fn attach(
    p: &Planner<'_>,
    mut input: Node,
    calls: Vec<Call>,
    graph: &ActiveGraph,
) -> Result<Node> {
    let mut pending: Vec<_> = calls
        .into_iter()
        .map(|call| Spec {
            subject: call.subject.iter().map(|t| p.term_pattern(t)).collect(),
            object: call.object.iter().map(|t| p.term_pattern(t)).collect(),
            descriptor: call.descriptor,
            graph: Arc::new(GraphFilter::All),
        })
        .collect();
    let graph = p.graph_filter(graph).map(|(filter, _)| Arc::new(filter));
    while !pending.is_empty() {
        let bound = |v: &VarId| input.vars.contains(v) || p.property_inputs.contains(v);
        let ready = pending.iter().position(|s| {
            s.descriptor.required.iter().all(|position| {
                match slot(&s.subject, &s.object, position) {
                    PT::C(_) => true,
                    PT::V(v) => bound(v),
                }
            })
        });
        let Some(i) = ready else {
            return Err(Error::invalid(
                "property required input is unavailable or cyclic",
            ));
        };
        let mut spec = pending.remove(i);
        let mut vars = input.vars.clone();
        for position in &spec.descriptor.produces {
            if let PT::V(v) = slot(&spec.subject, &spec.object, position)
                && !vars.contains(v)
            {
                vars.push(*v);
            }
        }
        let Some(filter) = &graph else {
            input = Node::leaf(Kind::Empty, vars, 0.0, "inaccessible property graph".into());
            continue;
        };
        spec.graph = filter.clone();
        let est = (input.est * spec.descriptor.cardinality_factor.unwrap_or(4.0)).max(1.0);
        let desc = format!(
            "{} {:?}; singleton, per input row",
            spec.descriptor.iri, spec.descriptor.volatility
        );
        input = Node {
            kind: Kind::RegisteredProperty(Box::new(spec)),
            certain: input.certain.clone(),
            sorted: Vec::new(),
            dist: vars.iter().map(|v| (*v, est)).collect(),
            vars,
            est,
            cost: input.cost + est,
            desc,
            children: vec![input],
        };
    }
    Ok(input)
}

pub(super) fn reads_left(p: &Planner<'_>, left: &GraphPattern, right: &GraphPattern) -> bool {
    fn variables(term: &TermPattern, out: &mut Vec<String>) {
        match term {
            TermPattern::Variable(v) => out.push(v.as_str().to_owned()),
            TermPattern::Triple(t) => {
                variables(&t.subject, out);
                variables(&t.object, out);
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    out.push(v.as_str().to_owned());
                }
            }
            _ => {}
        }
    }
    fn read_vars(ctx: &Ctx, pattern: &GraphPattern, out: &mut Vec<String>) {
        use GraphPattern as P;
        match pattern {
            P::Bgp { patterns } if has_calls(ctx, patterns) => {
                if let Ok((calls, _)) = take_calls(ctx, patterns) {
                    for call in calls {
                        for argument in call.subject.iter().chain(&call.object) {
                            variables(argument, out);
                        }
                    }
                }
            }
            P::Join { left, right }
            | P::Lateral { left, right }
            | P::Union { left, right }
            | P::Minus { left, right }
            | P::SemiJoin { left, right }
            | P::AntiJoin { left, right }
            | P::LeftJoin { left, right, .. } => {
                read_vars(ctx, left, out);
                read_vars(ctx, right, out);
            }
            P::Filter { inner, .. }
            | P::Graph { inner, .. }
            | P::Extend { inner, .. }
            | P::Assign { inner, .. }
            | P::Unfold { inner, .. }
            | P::Distinct { inner }
            | P::Reduced { inner }
            | P::Slice { inner, .. }
            | P::OrderBy { inner, .. } => read_vars(ctx, inner, out),
            // Subqueries and remote services retain their ordinary algebra boundaries.
            _ => {}
        }
    }
    if p.ctx.extensions.is_none() {
        return false;
    }
    let mut reads = Vec::new();
    read_vars(p.ctx, right, &mut reads);
    let mut found = false;
    left.on_in_scope_variable(|v| {
        found |=
            reads.iter().any(|n| n == v.as_str()) && !p.subst.contains_key(&p.ctx.var(v.as_str()));
    });
    found
}

fn dispatch<T>(
    ctx: &Ctx,
    call: impl FnOnce() -> std::result::Result<T, ScalarError>,
) -> Result<Option<T>> {
    ctx.check()
        .inspect_err(|e| ctx.fail_extension(ScalarError::from_engine(e)))?;
    let _owner = CallbackGuard::enter(ctx);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err(ScalarError::Execution("property callback panicked".into())));
    if let Err(e) = &result {
        ctx.fail_extension(e.clone());
    }
    ctx.check()
        .inspect_err(|e| ctx.fail_extension(ScalarError::from_engine(e)))?;
    match result {
        Ok(v) => Ok(Some(v)),
        Err(ScalarError::Expression) => Ok(None),
        Err(e) => Err(e.engine()),
    }
}

struct OwnedStream<'q> {
    ctx: &'q Ctx,
    inner: Option<Box<dyn PropertyStream + 'q>>,
}
impl Drop for OwnedStream<'_> {
    fn drop(&mut self) {
        let _owner = CallbackGuard::enter(self.ctx);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(self.inner.take())))
            .is_err()
        {
            self.ctx.fail_extension(ScalarError::Execution(
                "property stream destructor panicked".into(),
            ));
        }
    }
}

pub(super) fn run(ctx: &Ctx, spec: &Spec, input: &Table, vars: &[VarId]) -> Result<Table> {
    let result = run_inner(ctx, spec, input, vars);
    if let Err(e) = &result {
        ctx.fail_extension(ScalarError::from_engine(e));
    }
    result
}
fn run_inner(ctx: &Ctx, spec: &Spec, input: &Table, vars: &[VarId]) -> Result<Table> {
    let held = ctx.charge(
        (std::mem::size_of::<Table>()
            + vars.len() * (std::mem::size_of::<VarId>() + std::mem::size_of::<Vec<Id>>()))
            as u64,
    )?;
    let mut out = Table::new(vars.to_vec());
    let mut held_rows = 0;
    for row in 0..input.len() {
        ctx.check()?;
        let read = |slot: &PT| match slot {
            PT::C(id) => *id,
            PT::V(v) => input.col_of(*v).map_or(Id::UNDEF, |c| input.cols[c][row]),
        };
        let slots = spec.subject.len() + spec.object.len();
        let _ids_charge = ctx.charge(
            (slots * std::mem::size_of::<Id>() + 2 * std::mem::size_of::<Vec<Id>>()) as u64,
        )?;
        let subject: Vec<_> = spec.subject.iter().map(read).collect();
        let object: Vec<_> = spec.object.iter().map(read).collect();
        if spec
            .descriptor
            .required
            .iter()
            .any(|p| slot(&subject, &object, p).is_undef())
        {
            continue;
        }
        let context = PropertyContext {
            scope: Arc::new(Scope {
                ctx,
                graph: spec.graph.clone(),
                retained: Mutex::new(ctx.charge(256)?),
                blanks: Mutex::new(FxHashSet::default()),
            }),
        };
        let slot_bytes = std::mem::size_of::<Option<Term>>() as u64;
        let mut bytes = (slots as u64).saturating_mul(slot_bytes);
        let args_charge =
            ctx.charge(bytes + 2 * std::mem::size_of::<Vec<Option<Term>>>() as u64)?;
        let mut decode = |ids: &[Id]| -> Result<Vec<Option<Term>>> {
            ids.iter()
                .map(|id| {
                    if id.is_undef() {
                        return Ok(None);
                    }
                    let term = ctx
                        .term(*id)
                        .ok_or_else(|| Error::invalid("invalid property input RDF term"))?;
                    // term_bytes includes this value's inline RDF slot; add only
                    // its remainder because even None slots were charged up front.
                    let cost = term_bytes(&term)
                        .map_err(|e| e.engine())?
                        .saturating_sub(slot_bytes);
                    bytes = bytes.saturating_add(cost);
                    if bytes > MAX_BATCH_BYTES {
                        return Err(Error::invalid("property input exceeds batch byte ceiling"));
                    }
                    args_charge.add(cost)?;
                    context.remember(&term)?;
                    Ok(Some(term))
                })
                .collect()
        };
        let args = PropertyInput {
            subject: decode(&subject)?,
            object: decode(&object)?,
        };
        if bytes > MAX_BATCH_BYTES {
            return Err(Error::invalid("property input exceeds batch byte ceiling"));
        }

        let mut owned = OwnedStream { ctx, inner: None };
        dispatch(ctx, || {
            owned.inner = Some(spec.descriptor.implementation.open(context.clone(), args)?);
            Ok(())
        })?;
        let start = out.len();
        let mut domain = false;
        if let Some(stream) = &mut owned.inner {
            loop {
                let result = dispatch(ctx, || stream.next(&context))?;
                let Some(result) = result else {
                    domain = true;
                    break;
                };
                let Some(extension) = result else { break };
                if extension.subject.len() != subject.len()
                    || extension.object.len() != object.len()
                {
                    return Err(Error::invalid("property output shape mismatch"));
                }
                let bytes = extension
                    .subject
                    .iter()
                    .chain(&extension.object)
                    .flatten()
                    .try_fold((slots as u64).saturating_mul(slot_bytes), |n, t| {
                        term_bytes(t)
                            .map(|b| n.saturating_add(b.saturating_sub(slot_bytes)))
                            .map_err(|e| e.engine())
                    })?;
                if bytes > MAX_BATCH_BYTES {
                    return Err(Error::invalid("property output exceeds batch byte ceiling"));
                }
                let _output =
                    ctx.charge(bytes + 2 * std::mem::size_of::<Vec<Option<Term>>>() as u64)?;
                let _row = ctx.charge(super::ctx::table_bytes(1, vars.len()))?;
                let mut values: Vec<_> = vars
                    .iter()
                    .map(|v| input.col_of(*v).map_or(Id::UNDEF, |c| input.cols[c][row]))
                    .collect();
                let mut matches = true;
                for (is_subject, patterns, bound, returned) in [
                    (true, &spec.subject, &subject, &extension.subject),
                    (false, &spec.object, &object, &extension.object),
                ] {
                    for (i, ((pattern, old), term)) in
                        patterns.iter().zip(bound).zip(returned).enumerate()
                    {
                        let Some(term) = term else { continue };
                        let id = context.intern(term)?;
                        if !old.is_undef() {
                            if id != *old {
                                matches = false;
                            }
                            continue;
                        }
                        let position = if is_subject {
                            PropertyPosition::Subject(i)
                        } else {
                            PropertyPosition::Object(i)
                        };
                        if !spec.descriptor.produces.contains(&position) {
                            return Err(Error::invalid(
                                "property output binds undeclared position",
                            ));
                        }
                        let PT::V(v) = pattern else { unreachable!() };
                        let c = vars.iter().position(|x| x == v).unwrap();
                        if !values[c].is_undef() && values[c] != id {
                            matches = false;
                        }
                        values[c] = id;
                    }
                }
                // Even rejected/provisional rows are callback work; successes are
                // counted at the ordinary operator boundary, discarded ones here.
                if matches {
                    ctx.check_output(out.len() + 1, vars.len())?;
                    // Domain failures truncate the table without releasing its
                    // column capacities. Charge their reusable high-water mark,
                    // rather than accumulating every discarded provisional row.
                    if out.len() == held_rows {
                        let capacity = held_rows.max(2) * 2;
                        held.add(super::ctx::table_bytes(capacity - held_rows, vars.len()))?;
                        for col in &mut out.cols {
                            col.reserve_exact(capacity - col.len());
                        }
                        held_rows = capacity;
                    }
                    out.push_row(&values);
                } else {
                    ctx.produced(1)?;
                }
            }
        }
        drop(owned);
        ctx.check()?;
        if domain {
            ctx.produced(out.len() - start)?;
            for col in &mut out.cols {
                col.truncate(start);
            }
            out.len = start;
        }
    }
    Ok(out)
}
