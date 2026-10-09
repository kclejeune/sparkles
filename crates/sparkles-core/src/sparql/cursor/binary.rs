//! Build one input, then resume compatible probe rows without materializing the
//! join's output or a Cartesian vector of matching row pairs.
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::expr::{Expr, Row, ebv};
use crate::sparql::plan::{JoinAlgo, Kind, Node};
use crate::sparql::table::{Table, VarId};
use rustc_hash::FxHashMap;
use std::sync::Arc;

pub(super) fn eligible(node: &Node) -> bool {
    node.children.len() == 2
        && match &node.kind {
            Kind::Join { .. } | Kind::HalfJoin { .. } | Kind::Minus => true,
            Kind::LeftJoin { expr } => expr.as_ref().is_none_or(|e| !e.has_exists()),
            _ => false,
        }
}

/// The input that is streamed. It matches the planner's hash probe order: the larger
/// input of an inner hash join, and child zero of every other join.
pub(super) fn probe_side(node: &Node) -> usize {
    usize::from(
        matches!(
            node.kind,
            Kind::Join {
                algo: JoinAlgo::Hash,
                ..
            }
        ) && node.children[0].est < node.children[1].est,
    )
}

#[derive(Clone, Copy)]
enum Mode {
    Inner,
    Optional,
    Semi,
    Anti,
    Minus,
}

pub(super) struct Binary {
    mode: Mode,
    probe_side: usize,
    columns: Vec<(Option<usize>, Option<usize>)>,
    shared: Vec<(usize, usize)>,
    hash_key: Option<(usize, usize)>,
    hash: FxHashMap<Id, Vec<usize>>,
    build: Option<Buffer>,
    probe: Option<Buffer>,
    row: usize,
    candidate: usize,
    matched: bool,
    expression: Option<Expr>,
    _charge: Option<RetainedCharge>,
    _hash_charge: Option<RetainedCharge>,
}

impl Binary {
    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let mode = match &node.kind {
            Kind::LeftJoin { .. } => Mode::Optional,
            Kind::HalfJoin { anti: false } => Mode::Semi,
            Kind::HalfJoin { anti: true } => Mode::Anti,
            Kind::Minus => Mode::Minus,
            _ => Mode::Inner,
        };
        let probe_side = probe_side(node);
        let (probe, build) = (&node.children[probe_side], &node.children[1 - probe_side]);
        let charge = ctx.retained_charge(node.vars.len() as u64 * 96 + 1024)?;
        let columns = node
            .vars
            .iter()
            .map(|v| {
                (
                    probe.vars.iter().position(|x| x == v),
                    build.vars.iter().position(|x| x == v),
                )
            })
            .collect();
        let shared: Vec<_> = probe
            .vars
            .iter()
            .enumerate()
            .filter_map(|(p, v)| build.vars.iter().position(|x| x == v).map(|b| (p, b)))
            .collect();
        let hash_key = shared
            .iter()
            .find(|&&(p, b)| {
                probe.certain.contains(&probe.vars[p]) && build.certain.contains(&build.vars[b])
            })
            .copied();
        let expression = match &node.kind {
            Kind::LeftJoin { expr } => expr.clone(),
            _ => None,
        };
        Ok(Self {
            mode,
            probe_side,
            columns,
            shared,
            hash_key,
            hash: Default::default(),
            build: None,
            probe: None,
            row: 0,
            candidate: 0,
            matched: false,
            expression,
            _charge: charge,
            _hash_charge: None,
        })
    }

    fn ensure_probe(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        cap: usize,
    ) -> Result<bool> {
        if self.probe.as_ref().is_some_and(|b| self.row < b.table.len) {
            return Ok(true);
        }
        self.probe = None;
        self.row = 0;
        self.candidate = 0;
        self.matched = false;
        self.probe = children[self.probe_side].next(ctx, options, cap)?;
        Ok(self.probe.is_some())
    }

    fn build(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
    ) -> Result<()> {
        let child = &mut children[1 - self.probe_side];
        let mut build = Buffer::new(ctx, &child.vars, 0)?;
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            let n = batch.table.len;
            merge::append(ctx, &mut build, &batch.table, 0..n)?;
        }
        if let Some((_, column)) = self.hash_key {
            // Includes simultaneous old/new buckets and per-key row vectors.
            self._hash_charge = ctx.retained_charge(build.table.len as u64 * 384 + 1024)?;
            for (row, &key) in build.table.cols[column].iter().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                self.hash.entry(key).or_default().push(row);
            }
        }
        self.build = Some(build);
        Ok(())
    }

    fn advance(&mut self) {
        self.row += 1;
        self.candidate = 0;
        self.matched = false;
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if !self.ensure_probe(ctx, children, options, cap)? {
            return Ok(None);
        }
        if self.build.is_none() {
            self.build(ctx, children, options)?;
        }
        let mut output = Buffer::new(ctx, vars, cap)?;
        let _predicate_charge =
            ctx.charge(vars.len() as u64 * 48 + ctx.nvars() as u64 * 16 + 128)?;
        let mut predicate = Table::new(vars.to_vec());
        for column in &mut predicate.cols {
            column.push(Id::UNDEF);
        }
        predicate.len = 1;
        let map = predicate.var_map(ctx.nvars());
        let mut steps = 0usize;
        while output.table.len < cap {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            if !self.ensure_probe(ctx, children, options, cap)? {
                break;
            }
            let probe = &self.probe.as_ref().expect("available probe").table;
            let build = &self.build.as_ref().expect("captured build").table;
            let candidates = self
                .hash_key
                .and_then(|(column, _)| self.hash.get(&probe.cols[column][self.row]));
            let count = if self.hash_key.is_some() {
                candidates.map_or(0, Vec::len)
            } else {
                build.len
            };
            if self.candidate == count {
                let keep =
                    !self.matched && matches!(self.mode, Mode::Optional | Mode::Anti | Mode::Minus);
                if keep {
                    for (column, &(p, _)) in output.table.cols.iter_mut().zip(&self.columns) {
                        column.push(p.map_or(Id::UNDEF, |c| probe.cols[c][self.row]));
                    }
                    output.table.len += 1;
                }
                self.advance();
                continue;
            }
            let other = candidates.map_or(self.candidate, |rows| rows[self.candidate]);
            self.candidate += 1;
            if !self.shared.iter().all(|&(p, b)| {
                let (p, b) = (probe.cols[p][self.row], build.cols[b][other]);
                p == b || p.is_undef() || b.is_undef()
            }) {
                continue;
            }
            // MINUS requires an intersection of the rows' bound domains.
            if matches!(self.mode, Mode::Minus)
                && !self.shared.iter().any(|&(p, b)| {
                    !probe.cols[p][self.row].is_undef() && !build.cols[b][other].is_undef()
                })
            {
                continue;
            }
            for (column, &(p, b)) in predicate.cols.iter_mut().zip(&self.columns) {
                let p = p.map_or(Id::UNDEF, |c| probe.cols[c][self.row]);
                column[0] = if p.is_undef() {
                    b.map_or(p, |c| build.cols[c][other])
                } else {
                    p
                };
            }
            if self.expression.as_ref().is_some_and(|e| {
                !ebv(
                    e,
                    &Row {
                        table: &predicate,
                        i: 0,
                        map: &map,
                        dec: None,
                    },
                    ctx,
                )
                .unwrap_or(false)
            }) {
                ctx.check()?;
                continue;
            }
            self.matched = true;
            if matches!(self.mode, Mode::Anti | Mode::Minus) {
                self.advance();
                continue;
            }
            for ((column, value), &(p, _)) in output
                .table
                .cols
                .iter_mut()
                .zip(&predicate.cols)
                .zip(&self.columns)
            {
                column.push(if matches!(self.mode, Mode::Semi) {
                    p.map_or(Id::UNDEF, |c| probe.cols[c][self.row])
                } else {
                    value[0]
                });
            }
            output.table.len += 1;
            if matches!(self.mode, Mode::Semi) {
                self.advance();
            }
        }
        output.reconcile()?;
        Ok((!output.table.is_empty()).then_some(output))
    }
}
