//! A keyed join that reads both of its inputs alternately, for an ASK whose first
//! matching pair ends the query ([`Kind::KeyedJoin`] with `ripple` set).
//!
//! An ordinary hash join reads its whole build input before its first output row. When
//! an ASK needs one pair and both inputs are large, that first read is most of the work.
//! This join instead reads a batch from one input, then from the other, with batches
//! that grow four times at each turn up to the cursor's batch size. Each new row is
//! tested against the rows of the other input read so far, through the candidate keys of
//! the equality ([`crate::sparql::eqkey`]), and then indexed for the other input's later
//! rows, so every pair is a candidate exactly once, when the later of its rows arrives.
//! This is the symmetric hash join of Wilschut and Apers. Once an input is exhausted, the
//! rows of the other one are only probed and no longer indexed.
//!
//! It holds prefixes of both inputs while both can produce rows, where a hash join
//! holds one complete input. At EOF it drops the other input's history and retains
//! only that input's active probe batch. The extra prefix storage runs only under ASK.
use super::binary::{gather, map_bytes};
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::eqkey::{EqKey, eq_key};
use crate::sparql::exec;
use crate::sparql::expr::Expr;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::{Table, VarId};
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// The rows of the first batch read from each input.
const FIRST_BATCH: usize = 64;

/// The rows of one input read so far, indexed by candidate key.
struct Side {
    held: Option<Buffer>,
    keyed: FxHashMap<Id, Vec<u32>>,
    loose: Vec<u32>,
    /// The input has no more rows.
    done: bool,
    /// The rows of the next read.
    want: usize,
    charge: Option<RetainedCharge>,
    /// Capacity bytes of row-position vectors, excluding the map's buckets.
    position_bytes: u64,
}

impl Side {
    fn rows(&self) -> usize {
        self.held.as_ref().map_or(0, |b| b.table.len)
    }

    fn discard_history(&mut self) -> Result<()> {
        self.held = None;
        self.keyed = FxHashMap::default();
        self.loose = Vec::new();
        self.position_bytes = 0;
        if let Some(charge) = &mut self.charge {
            charge.resize(1024)?;
        }
        Ok(())
    }
}

/// The new rows of one input that are being tested against the other input.
struct Probe {
    side: usize,
    row: usize,
    end: usize,
    /// How many candidates of `row` have been visited.
    offset: usize,
}

pub(super) struct Ripple {
    sides: [Side; 2],
    /// The equality's column in each input.
    key: [usize; 2],
    columns: Vec<(Option<usize>, Option<usize>)>,
    expression: Expr,
    turn: usize,
    probe: Option<Probe>,
}

pub(super) fn eligible(node: &Node) -> bool {
    node.children.len() == 2 && matches!(node.kind, Kind::KeyedJoin { ripple: true, .. })
}

impl Ripple {
    fn finish_side(&mut self, side: usize) -> Result<()> {
        self.sides[side].done = true;
        if self.sides[side].rows() == 0 {
            self.sides[1 - side].done = true;
        } else {
            // Every retained row of the other side has already met this side's
            // complete input. Only its future probe batches can find new pairs.
            self.sides[1 - side].discard_history()?;
        }
        Ok(())
    }

    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let Kind::KeyedJoin {
            expr, left, right, ..
        } = &node.kind
        else {
            unreachable!("a ripple join is a keyed join");
        };
        let (l, r) = (&node.children[0], &node.children[1]);
        let position = |vars: &[VarId], v: VarId| vars.iter().position(|x| *x == v);
        let key = [
            position(&l.vars, *left).expect("the left key"),
            position(&r.vars, *right).expect("the right key"),
        ];
        let columns = node
            .vars
            .iter()
            .map(|v| (position(&l.vars, *v), position(&r.vars, *v)))
            .collect();
        let side = || -> Result<Side> {
            Ok(Side {
                held: None,
                keyed: FxHashMap::default(),
                loose: Vec::new(),
                done: false,
                want: FIRST_BATCH,
                charge: ctx.retained_charge(1024)?,
                position_bytes: 0,
            })
        };
        Ok(Self {
            sides: [side()?, side()?],
            key,
            columns,
            expression: expr.clone(),
            turn: 1,
            probe: None,
        })
    }

    /// Read the next batch of the input whose turn it is, keep its rows and index them
    /// unless the other input is exhausted, and start probing them. `false` once both
    /// inputs are exhausted.
    fn read(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
    ) -> Result<bool> {
        loop {
            // A child may report EOF together with its final nonempty batch.
            // Its last batch has finished probing before another read begins.
            for (side, child) in children.iter().enumerate() {
                if child.done && !self.sides[side].done {
                    self.finish_side(side)?;
                }
            }
            if self.sides.iter().all(|s| s.done) {
                return Ok(false);
            }
            let s = if self.sides[self.turn].done {
                1 - self.turn
            } else {
                self.turn
            };
            self.turn = 1 - s;
            let want = self.sides[s].want;
            self.sides[s].want = want.saturating_mul(4).min(options.batch_rows.max(1));
            let Some(batch) = children[s].next(ctx, options, want)? else {
                self.finish_side(s)?;
                continue;
            };
            let n = batch.table.len;
            if n == 0 {
                continue;
            }
            if self.sides[1 - s].done {
                // The previous probe batch has no remaining consumer.
                self.sides[s].held = None;
            }
            let side = &mut self.sides[s];
            let start = side.rows();
            match &mut side.held {
                Some(held) => merge::append(ctx, held, &batch.table, 0..n)?,
                None => side.held = Some(batch),
            }
            // Rows that no later row of the other input will meet are not indexed.
            if !self.sides[1 - s].done {
                let side = &mut self.sides[s];
                let held = &side.held.as_ref().expect("held rows").table;
                // Reserve before either the map or its row vectors can allocate.
                // Each u32 vector's geometric capacity is at most four times its
                // populated rows. The old index remains charged while an allocation
                // moves, and reserving the map for every incoming key prevents any
                // unplanned bucket growth during the fill.
                if let Some(charge) = &mut side.charge {
                    charge.resize(
                        map_bytes(side.keyed.capacity(), 32)
                            .saturating_add(side.position_bytes)
                            .saturating_add(map_bytes(side.keyed.len().saturating_add(n), 32))
                            .saturating_add((start.saturating_add(n) as u64).saturating_mul(16))
                            .saturating_add(1024),
                    )?;
                }
                side.keyed.reserve(n);
                for row in start..start + n {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    match eq_key(ctx, held.cols[self.key[s]][row])? {
                        EqKey::Unbound => {}
                        EqKey::Key(k) => {
                            let rows = side.keyed.entry(k).or_default();
                            let before = rows.capacity();
                            rows.push(row as u32);
                            side.position_bytes += (rows.capacity() - before) as u64 * 4;
                        }
                        EqKey::Loose => {
                            let before = side.loose.capacity();
                            side.loose.push(row as u32);
                            side.position_bytes += (side.loose.capacity() - before) as u64 * 4;
                        }
                    }
                }
                if let Some(charge) = &mut side.charge {
                    charge.resize(
                        map_bytes(side.keyed.capacity(), 32)
                            .saturating_add(side.position_bytes)
                            .saturating_add(1024),
                    )?;
                }
            }
            self.probe = Some(Probe {
                side: s,
                row: start,
                end: start + n,
                offset: 0,
            });
            return Ok(true);
        }
    }

    /// The candidate pairs of the probe from where it stopped, at most `room` of them,
    /// as (left row, right row).
    fn candidates(&mut self, ctx: &Ctx, room: usize, pairs: &mut Vec<(u32, u32)>) -> Result<()> {
        let Some(probe) = &mut self.probe else {
            return Ok(());
        };
        let (s, o) = (probe.side, 1 - probe.side);
        let held = &self.sides[s].held.as_ref().expect("probed rows").table;
        let other = &self.sides[o];
        while probe.row < probe.end && pairs.len() < room {
            if probe.row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let rows: &[u32] = match eq_key(ctx, held.cols[self.key[s]][probe.row])? {
                EqKey::Unbound => &[],
                EqKey::Key(k) => other.keyed.get(&k).map_or(&[], Vec::as_slice),
                EqKey::Loose => &other.loose,
            };
            while probe.offset < rows.len() && pairs.len() < room {
                let (mine, theirs) = (probe.row as u32, rows[probe.offset]);
                pairs.push(if s == 0 {
                    (mine, theirs)
                } else {
                    (theirs, mine)
                });
                probe.offset += 1;
            }
            if probe.offset < rows.len() {
                break;
            }
            probe.row += 1;
            probe.offset = 0;
        }
        if probe.row >= probe.end {
            self.probe = None;
        }
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let mut output = Buffer::new(ctx, vars, 0)?;
        // Candidate pairs, both original and filtered row-position arrays, the
        // merged table, its mask and output capacity coexist before reconciliation.
        // Include vector growth and the columns' metadata before allocating them.
        let _pairs = ctx.charge(
            (cap as u64)
                .saturating_mul(64 + vars.len() as u64 * 32)
                .saturating_add(vars.len() as u64 * 96 + 512),
        )?;
        let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(cap.min(1 << 16));
        while output.table.len == 0 {
            ctx.check()?;
            if self.probe.is_none() && !self.read(ctx, children, options)? {
                break;
            }
            pairs.clear();
            self.candidates(ctx, cap, &mut pairs)?;
            if pairs.is_empty() {
                continue;
            }
            let empty = Table::new(Vec::new());
            let table = |s: usize| self.sides[s].held.as_ref().map_or(&empty, |b| &b.table);
            let (lp, rp): (Vec<u32>, Vec<u32>) = pairs.iter().copied().unzip();
            let mut merged = Table::new(vars.to_vec());
            gather(&mut merged, &self.columns, table(0), table(1), &lp, &rp);
            let keep = exec::filter_mask(ctx, &merged, std::slice::from_ref(&self.expression))?;
            let (lp, rp): (Vec<u32>, Vec<u32>) = lp
                .iter()
                .zip(&rp)
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|((l, r), _)| (*l, *r))
                .unzip();
            gather(
                &mut output.table,
                &self.columns,
                table(0),
                table(1),
                &lp,
                &rp,
            );
        }
        output.reconcile()?;
        Ok((!output.table.is_empty()).then_some(output))
    }

    /// Release both inputs' rows and indexes.
    pub(super) fn release_working_sets(&mut self) {
        for side in &mut self.sides {
            side.held = None;
            side.keyed = FxHashMap::default();
            side.loose = Vec::new();
            side.position_bytes = 0;
            if let Some(charge) = &mut side.charge {
                let _ = charge.resize(0);
            }
        }
        self.probe = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};
    use crate::{BudgetKind, Error};

    #[test]
    fn refused_index_growth_leaves_its_allocations_unchanged() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 7 << 10;
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_cursor_owner();
        let values = |v| {
            let mut t = Table::new(vec![v]);
            for row in 0..64 {
                t.push_row(&[Id::bnode(row)]);
            }
            Node::leaf(Kind::Values(t), vec![v], 64.0, "values".into())
        };
        let (left, right) = (values(x), values(y));
        let mut join = Node::leaf(
            Kind::KeyedJoin {
                expr: Expr::Eq(Box::new(Expr::Var(x)), Box::new(Expr::Var(y))),
                left: x,
                right: y,
                ripple: true,
            },
            vec![x, y],
            64.0,
            "keyed".into(),
        );
        join.children = vec![left.clone(), right.clone()];
        let mut children = vec![
            Operator::build(
                &ctx,
                left,
                super::super::FallbackPolicy::RejectMaterialization,
            )
            .unwrap(),
            Operator::build(
                &ctx,
                right,
                super::super::FallbackPolicy::RejectMaterialization,
            )
            .unwrap(),
        ];
        let mut ripple = Ripple::new(&ctx, &join).unwrap();
        assert!(matches!(
            ripple.read(&ctx, &mut children, &CursorOptions::default()),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
        ));
        let side = &ripple.sides[1];
        assert_eq!(side.keyed.capacity(), 0);
        assert_eq!(side.loose.capacity(), 0);
        assert_eq!(side.position_bytes, 0);
        assert_eq!(side.charge.as_ref().unwrap().bytes(), 1024);
    }
}
