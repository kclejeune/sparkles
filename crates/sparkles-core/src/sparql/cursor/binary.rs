//! Build one input, then resume compatible probe rows without materializing the
//! join's output or a Cartesian vector of matching row pairs.
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::{Error, Result};
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
    /// Probe columns in order of preference for the hash key: certain on the probe
    /// side first. The build data decides which one is used.
    preferred: Vec<(usize, usize)>,
    index: Option<HashIndex>,
    build: Option<Buffer>,
    probe: Option<Buffer>,
    row: usize,
    candidates: Candidates,
    matched: bool,
    expression: Option<Expr>,
    _charge: Option<RetainedCharge>,
}

/// The end of a row chain.
const NONE: u32 = u32::MAX;

/// Build rows grouped on one shared column. Each bound key has a chain of its rows in
/// build order. Rows that leave the key unbound are compatible with every probe value,
/// so every bound probe also visits them, and an unbound probe visits every build row.
struct HashIndex {
    /// The probe column that carries the key.
    probe: usize,
    heads: FxHashMap<Id, u32>,
    next: Vec<u32>,
    unbound: Vec<u32>,
    _charge: Option<RetainedCharge>,
}

impl HashIndex {
    /// Index `build` on its column `column`. Row numbers are 32 bits wide, and the
    /// chains, the unbound rows and the key table are charged while they grow.
    fn new(ctx: &Ctx, build: &Table, column: usize, probe: usize) -> Result<Self> {
        let rows = u32::try_from(build.len)
            .ok()
            .filter(|&n| n < NONE)
            .ok_or_else(|| Error::invalid("a hash join build input has too many rows"))?;
        // A growing key table holds its old and new buckets at once.
        let mut charge = ctx.retained_charge(u64::from(rows) * 72 + 1024)?;
        let mut heads: FxHashMap<Id, u32> = FxHashMap::default();
        let mut next = vec![NONE; rows as usize];
        let mut unbound = Vec::new();
        // Walk backwards so that every chain lists its rows in build order.
        for row in (0..rows).rev() {
            if row % 1024 == 0 {
                ctx.check()?;
            }
            let key = build.cols[column][row as usize];
            if key.is_undef() {
                unbound.push(row);
            } else if let Some(head) = heads.insert(key, row) {
                next[row as usize] = head;
            }
        }
        unbound.reverse();
        unbound.shrink_to_fit();
        if let Some(charge) = &mut charge {
            charge.resize(
                (next.capacity() as u64 + unbound.capacity() as u64) * 4
                    + heads.capacity() as u64 * 24
                    + 1024,
            )?;
        }
        Ok(Self {
            probe,
            heads,
            next,
            unbound,
            _charge: charge,
        })
    }
}

/// Where the build rows of the current probe row are being visited.
#[derive(Clone, Copy)]
enum Candidates {
    Start,
    Chain(u32),
    Unbound(usize),
    All(usize),
    Done,
}

/// The next build row that may match the probe row, or None when it has none left.
fn candidate(
    state: &mut Candidates,
    index: Option<&HashIndex>,
    probe: &Table,
    row: usize,
    rows: usize,
) -> Option<usize> {
    loop {
        match *state {
            Candidates::Start => {
                *state = match index {
                    Some(index) => {
                        let key = probe.cols[index.probe][row];
                        if key.is_undef() {
                            Candidates::All(0)
                        } else {
                            Candidates::Chain(index.heads.get(&key).copied().unwrap_or(NONE))
                        }
                    }
                    None => Candidates::All(0),
                }
            }
            Candidates::Chain(NONE) => *state = Candidates::Unbound(0),
            Candidates::Chain(at) => {
                let index = index.expect("a chain belongs to an index");
                *state = Candidates::Chain(index.next[at as usize]);
                return Some(at as usize);
            }
            Candidates::Unbound(at) => {
                let index = index.expect("unbound rows belong to an index");
                match index.unbound.get(at) {
                    Some(&other) => {
                        *state = Candidates::Unbound(at + 1);
                        return Some(other as usize);
                    }
                    None => *state = Candidates::Done,
                }
            }
            Candidates::All(at) if at < rows => {
                *state = Candidates::All(at + 1);
                return Some(at);
            }
            Candidates::All(_) | Candidates::Done => {
                *state = Candidates::Done;
                return None;
            }
        }
    }
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
        let mut preferred = shared.clone();
        preferred.sort_by_key(|&(p, _)| !probe.certain.contains(&probe.vars[p]));
        let expression = match &node.kind {
            Kind::LeftJoin { expr } => expr.clone(),
            _ => None,
        };
        Ok(Self {
            mode,
            probe_side,
            columns,
            shared,
            preferred,
            index: None,
            build: None,
            probe: None,
            row: 0,
            candidates: Candidates::Start,
            matched: false,
            expression,
            _charge: charge,
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
        self.candidates = Candidates::Start;
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
        // Key on the shared column that the build data leaves unbound least often.
        // A column that is unbound in every build row selects nothing.
        let table = &build.table;
        let unbound = |column: usize| table.cols[column].iter().filter(|x| x.is_undef()).count();
        let mut best: Option<((usize, usize), usize)> = None;
        for &(p, b) in &self.preferred {
            ctx.check()?;
            let n = unbound(b);
            if n < table.len && best.is_none_or(|(_, m)| n < m) {
                best = Some(((p, b), n));
            }
            if n == 0 {
                break;
            }
        }
        if let Some(((p, b), _)) = best
            && table.len > 1
        {
            self.index = Some(HashIndex::new(ctx, table, b, p)?);
        }
        self.build = Some(build);
        Ok(())
    }

    fn advance(&mut self) {
        self.row += 1;
        self.candidates = Candidates::Start;
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
            let Some(other) = candidate(
                &mut self.candidates,
                self.index.as_ref(),
                probe,
                self.row,
                build.len,
            ) else {
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
            };
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
