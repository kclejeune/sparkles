//! Charged construction of the checked GRAPH diagnostics.

use super::{
    Ctx, Expr, Kind, Node, PlanFidelity, PlanInfo, PlanPattern, PlanRefs, PreparedDiagnostics,
    VarId,
};
use crate::access::{
    Preparation, PreparationBuffer, PreparationText, Reservation, sort_by_prepared,
};
use crate::error::{Error, Result};
use std::sync::Arc;

pub(in crate::sparql) fn owner(preparation: &Preparation) -> Result<Reservation> {
    preparation.reserve((size_of::<PreparedDiagnostics>() + 2 * size_of::<usize>()) as u64)
}

pub(in crate::sparql) fn leaf(
    ctx: &Ctx,
    node: &Node,
    held: &mut Reservation,
    runtime_note: Option<&str>,
) -> Result<PlanInfo> {
    let description = text(held, |out| {
        out.write_str(&node.desc)?;
        if let Some(note) = runtime_note {
            out.write_char(' ')?;
            out.write_str(note)?;
        }
        Ok(())
    })?;
    let operator = text(held, |out| out.write_str(node.operator()))?;
    let columns = names(ctx, &node.vars, held)?;
    let sorted_on = names(ctx, &node.sorted, held)?;
    let fidelity = fidelity(ctx, node, held)?;
    let refs = refs(ctx, node, held)?;
    ctx.check()?;
    Ok(PlanInfo {
        operator,
        description,
        columns,
        sorted_on,
        estimated_rows: node.est.round(),
        estimated_cost: node.cost.round(),
        actual_rows: -1,
        fidelity,
        refs,
        ..Default::default()
    })
}

pub(in crate::sparql) fn text(
    held: &mut Reservation,
    write: impl FnOnce(&mut dyn std::fmt::Write) -> std::fmt::Result,
) -> Result<String> {
    let mut out = PreparationText::new(held.preparation())?;
    let written = write(&mut out);
    let (text, credit) = out.finish()?;
    if written.is_err() {
        drop(text);
        return Err(Error::invalid("could not render graph diagnostics"));
    }
    held.merge(credit);
    Ok(text)
}

fn names(ctx: &Ctx, vars: &[VarId], held: &mut Reservation) -> Result<Vec<String>> {
    let preparation = held.preparation().clone();
    let variables = ctx.var_names.read();
    let mut names = PreparationBuffer::new(&preparation)?;
    for &var in vars {
        preparation.check()?;
        let name = &variables[var as usize];
        if !name.starts_with(' ') {
            names.push(text(held, |out| out.write_str(name))?)?;
        }
    }
    let (names, storage) = names.into_parts();
    held.merge(storage);
    Ok(names)
}

fn refs(ctx: &Ctx, node: &Node, held: &mut Reservation) -> Result<Option<Box<PlanRefs>>> {
    if !ctx.plan_patterns {
        return Ok(None);
    }
    let preparation = held.preparation().clone();
    let mut patterns = PreparationBuffer::<Arc<PlanPattern>>::new(&preparation)?;
    let mut pattern = |source: Option<&Arc<PlanPattern>>| -> Result<()> {
        if let Some(source) = source {
            for existing in &*patterns {
                preparation.check()?;
                if existing == source {
                    return Ok(());
                }
            }
            patterns.push(source.clone())?;
        }
        Ok(())
    };
    let mut filters = PreparationBuffer::new(&preparation)?;
    let mut filter = |exprs: &[Expr]| -> Result<()> {
        for expr in exprs {
            preparation.check()?;
            expr.visit_vars(&mut |var| filters.push(var))?;
        }
        Ok(())
    };
    let mut join = PreparationBuffer::new(&preparation)?;
    match &node.kind {
        Kind::Scan(scan)
        | Kind::RangeScan(scan, _)
        | Kind::CountScan { spec: scan, .. }
        | Kind::CountDistinctScan { spec: scan, .. }
        | Kind::GroupCountScan { spec: scan, .. } => pattern(scan.src.as_ref())?,
        Kind::CountFilterScan {
            spec, filter: expr, ..
        } => {
            pattern(spec.src.as_ref())?;
            filter(expr)?;
        }
        Kind::OrderedTopK(spec) => pattern(spec.scan.src.as_ref())?,
        Kind::SpatialScan(spec) => pattern(spec.scan.src.as_ref())?,
        Kind::IndexJoin(index) => {
            for probe in &index.probes {
                pattern(probe.scan.src.as_ref())?;
            }
            join.push(index.key)?;
        }
        Kind::Path { spec, .. } => pattern(spec.src.as_ref())?,
        Kind::PatternCount(spec) => pattern(spec.src.as_ref())?,
        Kind::PatternScan(spec) => pattern(spec.src.as_ref())?,
        Kind::Filter(exprs) => filter(exprs)?,
        Kind::Join { keys, .. } => {
            for &key in keys {
                join.push(key)?;
            }
        }
        Kind::CountJoin { .. }
        | Kind::CountJoinRuns { .. }
        | Kind::LeftJoin { .. }
        | Kind::KeyedLeftJoin { .. }
        | Kind::KeyedJoin { .. }
        | Kind::Minus
        | Kind::HalfJoin { .. }
            if let [left, right] = node.children.as_slice() =>
        {
            for &var in &left.vars {
                preparation.check()?;
                if right.vars.contains(&var) {
                    join.push(var)?;
                }
            }
        }
        _ => {}
    }
    if patterns.is_empty() && filters.is_empty() && join.is_empty() {
        return Ok(None);
    }
    sort_by_prepared(&mut filters, &preparation, |a, b| Ok(a.cmp(b)))?;
    filters.dedup();
    let filter_vars = names(ctx, &filters, held)?;
    let join_vars = names(ctx, &join, held)?;
    if patterns.is_empty() && filter_vars.is_empty() && join_vars.is_empty() {
        return Ok(None);
    }
    held.grow_by(size_of::<PlanRefs>() as u64)?;
    let (patterns, storage) = patterns.into_parts();
    held.merge(storage);
    Ok(Some(Box::new(PlanRefs {
        patterns,
        filter_vars,
        join_vars,
    })))
}

fn fidelity(ctx: &Ctx, node: &Node, held: &mut Reservation) -> Result<Option<Box<PlanFidelity>>> {
    let preparation = held.preparation().clone();
    let mut pushed = PreparationBuffer::new(&preparation)?;
    let mut push = |exprs: &[Expr]| -> Result<()> {
        for expr in exprs {
            preparation.stage("describing checked graph filters")?;
            pushed.push(text(held, |out| expr.write_description(ctx, out))?)?;
        }
        Ok(())
    };
    match &node.kind {
        Kind::RangeScan(_, range) => push(&range.filter)?,
        Kind::IndexJoin(index) => {
            for probe in &index.probes {
                push(&probe.filter)?;
            }
        }
        _ => {}
    };
    let estimate_guessed = matches!(node.kind, Kind::Service { .. });
    if pushed.is_empty() && !estimate_guessed {
        return Ok(None);
    }
    held.grow_by(size_of::<PlanFidelity>() as u64)?;
    let (pushed_filters, storage) = pushed.into_parts();
    held.merge(storage);
    Ok(Some(Box::new(PlanFidelity {
        pushed_filters,
        estimate_guessed,
        ..Default::default()
    })))
}

pub(in crate::sparql) fn tree(
    ctx: &Ctx,
    mut node: &Node,
    preparation: &Preparation,
) -> Result<PlanInfo> {
    while let Some(branch) = node.graph_singleton() {
        preparation.check()?;
        node = branch;
    }
    preparation.stage("describing checked graph node")?;
    let mut held = owner(preparation)?;
    let mut info = leaf(ctx, node, &mut held, None)?;
    let mut children = PreparationBuffer::new(preparation)?;
    for child in node.graph_diagnostic_children() {
        children.push(tree(ctx, child, preparation)?)?;
    }
    let (children, storage) = children.into_parts();
    held.merge(storage);
    info.children = children;
    PreparedDiagnostics::attach(&mut info, held);
    Ok(info)
}

/// A saved branch description awaiting execution or skipped by an earlier result.
pub(super) fn pending(ctx: &Ctx, node: &Node, preparation: &Preparation) -> Result<PlanInfo> {
    let mut info = tree(ctx, node, preparation)?;
    let held = &mut Arc::get_mut(info.prepared_diagnostics.as_mut().expect("prepared plan"))
        .expect("fresh plan has one owner")
        .0;
    let reason = text(held, |out| out.write_str("the query stopped before it ran"))?;
    if info.fidelity.is_none() {
        held.grow_by(size_of::<PlanFidelity>() as u64)?;
    }
    info.fidelity_mut().skipped = Some(reason);
    Ok(info)
}
