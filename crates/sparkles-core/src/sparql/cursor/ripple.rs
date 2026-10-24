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
//! It holds the rows of both inputs read so far, where a hash join holds one input. When
//! no pair matches, it reads and holds both inputs whole, so it runs only under ASK.
use super::binary::gather;
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
    /// Bytes of `keyed` and `loose`, as charged.
    index_bytes: u64,
}

impl Side {
    fn rows(&self) -> usize {
        self.held.as_ref().map_or(0, |b| b.table.len)
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
                index_bytes: 0,
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
                self.sides[s].done = true;
                continue;
            };
            let n = batch.table.len;
            if n == 0 {
                continue;
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
                for row in start..start + n {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    match eq_key(ctx, held.cols[self.key[s]][row])? {
                        EqKey::Unbound => {}
                        EqKey::Key(k) => {
                            // a key's entry and its first row, then 4 bytes a row
                            let rows = side.keyed.entry(k).or_default();
                            side.index_bytes += if rows.is_empty() { 52 } else { 4 };
                            rows.push(row as u32);
                        }
                        EqKey::Loose => {
                            side.index_bytes += 4;
                            side.loose.push(row as u32);
                        }
                    }
                }
                if let Some(charge) = &mut side.charge {
                    charge.resize(side.index_bytes.saturating_mul(2) + 1024)?;
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
        let _pairs = ctx.charge(cap as u64 * (8 + vars.len() as u64 * 8) + 256)?;
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
            side.index_bytes = 0;
            if let Some(charge) = &mut side.charge {
                let _ = charge.resize(0);
            }
        }
        self.probe = None;
    }
}
