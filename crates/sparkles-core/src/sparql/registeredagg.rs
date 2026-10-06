//! Registered, sequential per-group aggregates. Every application lifecycle call
//! runs inside the same guarded boundary as application scalar calls.

use super::ctx::{Charge, Ctx};
use super::expr::{Row, eval};
use super::extensions::{
    AggregateAccumulator, AggregateContext, AggregateDescriptor, CallbackGuard, ScalarContext,
    ScalarError, term_bytes,
};
use super::plan::Agg;
use super::table::Table;
use crate::error::Result;
use crate::id::Id;
use oxrdf::Term;
use rustc_hash::FxHashSet;

fn dispatch<T>(
    ctx: &Ctx,
    call: impl FnOnce() -> std::result::Result<T, ScalarError>,
) -> Result<Option<T>> {
    ctx.check().inspect_err(|error| {
        ctx.fail_extension(ScalarError::from_engine(error));
    })?;
    let _owner = CallbackGuard::enter(ctx);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err(ScalarError::Execution("aggregate callback panicked".into())));
    if let Err(error) = &result {
        ctx.fail_extension(error.clone());
    }
    // A callback cannot turn cancellation, a budget failure or forbidden nested
    // operations into a successful aggregate by catching their errors itself.
    ctx.check().inspect_err(|error| {
        ctx.fail_extension(ScalarError::from_engine(error));
    })?;
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ScalarError::Expression) => Ok(None),
        Err(error) => Err(error.engine()),
    }
}

struct OwnedAccumulator<'a> {
    ctx: &'a Ctx,
    inner: Option<Box<dyn AggregateAccumulator>>,
    retained: Charge<'a>,
}

impl Drop for OwnedAccumulator<'_> {
    fn drop(&mut self) {
        // Drop must run even when check() already fails. It must also remain
        // guarded: application destructors may otherwise reenter the writer.
        let _owner = CallbackGuard::enter(self.ctx);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(self.inner.take())))
            .is_err()
        {
            self.ctx.fail_extension(ScalarError::Execution(
                "aggregate accumulator destructor panicked".into(),
            ));
        }
    }
}

/// Stored blank nodes returned by finalization may refer to any argument received
/// by this group. Keep only the unique received nodes, not all input RDF values.
fn remember_blank_nodes(
    ctx: &Ctx,
    term: &Term,
    remembered: &mut FxHashSet<Term>,
    charge: &Charge<'_>,
) -> Result<()> {
    match term {
        Term::BlankNode(_) => {
            if !remembered.contains(term) {
                charge.add(term_bytes(term).map_err(|e| e.engine())?.saturating_add(32))?;
                remembered.insert(term.clone());
            }
        }
        Term::Triple(t) => {
            remember_blank_nodes(ctx, &t.subject.clone().into(), remembered, charge)?;
            remember_blank_nodes(ctx, &t.object, remembered, charge)?;
        }
        Term::Literal(l) if super::cdt::may_name_bnodes(l) => {
            let mut error = None;
            let _parsed = super::cdt::callback_relabel_term(ctx, term, &mut |label| {
                if error.is_none() {
                    let term = Term::BlankNode(oxrdf::BlankNode::new_unchecked(label));
                    error = remember_blank_nodes(ctx, &term, remembered, charge).err();
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

pub(super) fn aggregate(
    ctx: &Ctx,
    table: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    agg: &Agg,
    descriptor: &AggregateDescriptor,
) -> Result<Id> {
    let result = aggregate_inner(ctx, table, map, rows, agg, descriptor);
    if let Err(error) = &result {
        ctx.fail_extension(ScalarError::from_engine(error));
    }
    result
}

fn aggregate_inner(
    ctx: &Ctx,
    table: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    agg: &Agg,
    descriptor: &AggregateDescriptor,
) -> Result<Id> {
    let mut owned = OwnedAccumulator {
        ctx,
        inner: None,
        retained: ctx.charge(128)?,
    };
    let context = AggregateContext {
        scalar: ScalarContext { ctx },
        retained: &owned.retained,
    };
    // Install returned application state into the guarded owner before the
    // post-call cancellation/fatal check. Otherwise rejecting a successful
    // factory result could drop its box outside the destructor panic boundary.
    dispatch(ctx, || {
        owned.inner = Some(descriptor.implementation.create(&context)?);
        Ok(())
    })?;
    if owned.inner.is_none() {
        drop(owned);
        ctx.check()?;
        return Ok(Id::UNDEF);
    }
    let Some(expr) = &agg.expr else {
        unreachable!("registered aggregate has one expression")
    };
    let distinct_charge = ctx.charge(0)?;
    let blank_charge = ctx.charge(0)?;
    let mut seen = FxHashSet::default();
    let mut blanks = FxHashSet::default();
    let mut domain_error = false;
    for &i in rows {
        ctx.check()?;
        let value = eval(
            expr,
            &Row {
                table,
                i: i as usize,
                map,
                dec: None,
            },
            ctx,
        );
        // eval's TypeError also represents a sticky fatal callback error. Check
        // it before following the ordinary aggregate expression-error rules.
        ctx.check()?;
        let Ok(value) = value else {
            domain_error = true;
            continue;
        };
        // Continue evaluating every argument expression after a domain error,
        // preserving scalar callback observability, but retain/decode no unused
        // values or DISTINCT state for a result that is already unbound.
        if domain_error {
            continue;
        }
        let id = value.into_id(ctx);
        if agg.distinct {
            if seen.contains(&id) {
                continue;
            }
            distinct_charge.add(32)?;
            seen.insert(id);
        }
        let Some(term) = ctx.term(id) else {
            domain_error = true;
            continue;
        };
        let bytes = term_bytes(&term).map_err(|error| {
            ctx.fail_extension(error.clone());
            error.engine()
        })?;
        let _input = ctx.charge(bytes)?;
        remember_blank_nodes(ctx, &term, &mut blanks, &blank_charge)?;
        let context = AggregateContext {
            scalar: ScalarContext { ctx },
            retained: &owned.retained,
        };
        let accumulator = owned.inner.as_mut().unwrap();
        if dispatch(ctx, || accumulator.add_batch(&context, &[term]))?.is_none() {
            domain_error = true;
        }
    }
    let output = if domain_error {
        None
    } else {
        let context = AggregateContext {
            scalar: ScalarContext { ctx },
            retained: &owned.retained,
        };
        let accumulator = owned.inner.as_mut().unwrap();
        dispatch(ctx, || accumulator.finish(&context))?
    };
    // A failing destructor is fatal before a result is returned or admitted to
    // the parent table, including if finalization returned an expression error.
    drop(owned);
    ctx.check()?;
    let Some(term) = output else {
        return Ok(Id::UNDEF);
    };
    let bytes = term_bytes(&term).map_err(|error| {
        ctx.fail_extension(error.clone());
        error.engine()
    })?;
    let _output = ctx.charge(bytes)?;
    let _arguments = ctx.charge(
        (blanks.len() * std::mem::size_of::<Term>() + std::mem::size_of::<Vec<Term>>()) as u64,
    )?;
    let arguments: Vec<_> = blanks.into_iter().collect();
    ctx.intern_callback_term(&term, &arguments)
}
