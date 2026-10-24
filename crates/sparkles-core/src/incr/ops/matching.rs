//! Optional, minus and `FILTER (NOT) EXISTS`: operators that keep a left row according to
//! how many right rows match it.
//!
//! The state holds each left row with its weight and its match count `m`, the summed
//! weight of the right rows that are compatible with it (and, for `OPTIONAL`, satisfy
//! its filter, for `MINUS`, share a bound variable with it). A change on the right moves
//! the counts of the left rows it matches. A left row whose count crosses zero moves
//! between the parts of the output: for `OPTIONAL` between the joined rows and the row
//! alone, for `MINUS` and `NOT EXISTS` in or out, and for `EXISTS` out or in.

use super::super::pass::{Pass, Which};
use super::super::persist::{Reader, Writer};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::{Arrangement, Merge, Node, side_matches};
use crate::error::Result;
use crate::id::Id;
use rustc_hash::FxHashMap;
use spargebra::algebra::Expression;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    LeftJoin,
    Minus,
    NotExists,
    Exists,
}

impl MatchMode {
    pub fn name(self) -> &'static str {
        match self {
            MatchMode::LeftJoin => "Optional",
            MatchMode::Minus => "Minus",
            MatchMode::NotExists => "NotExists",
            MatchMode::Exists => "Exists",
        }
    }

    /// Whether a left row with match count `m` is in the output alone.
    fn alone(self, m: i64) -> bool {
        match self {
            MatchMode::Exists => m > 0,
            _ => m == 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MatchOp {
    pub mode: MatchMode,
    pub left: Node,
    pub right: Node,
    /// the filter of an `OPTIONAL`, over the merged row
    pub expr: Option<Expression>,
    pub m: Merge,
    /// left rows by join key: weight and match count
    lstate: FxHashMap<Row, FxHashMap<Row, (i64, i64)>>,
    lrows: usize,
    rarr: Option<Arrangement>,
}

impl MatchOp {
    pub fn new(mode: MatchMode, left: Node, right: Node, expr: Option<Expression>) -> MatchOp {
        let m = Merge::new(&left, &right);
        let rarr = right
            .as_bgp()
            .is_none()
            .then(|| Arrangement::new(m.key.iter().map(|k| k.1).collect(), right.schema.len()));
        MatchOp {
            mode,
            left,
            right,
            expr,
            m,
            lstate: FxHashMap::default(),
            lrows: 0,
            rarr,
        }
    }

    fn get(&self, a: &Row) -> (i64, i64) {
        self.lstate
            .get(&self.m.key_left(a))
            .and_then(|b| b.get(a))
            .copied()
            .unwrap_or((0, 0))
    }

    fn set(&mut self, a: &Row, v: (i64, i64)) {
        let k = self.m.key_left(a);
        let bucket = self.lstate.entry(k.clone()).or_default();
        let had = bucket.contains_key(a);
        if v.0 == 0 {
            if had {
                bucket.remove(a);
                self.lrows -= 1;
            }
            if bucket.is_empty() {
                self.lstate.remove(&k);
            }
        } else {
            if !had {
                self.lrows += 1;
            }
            bucket.insert(a.clone(), v);
        }
    }

    /// Which of the pairs (left row, right row) satisfy the operator's condition beyond
    /// compatibility.
    fn conditions(&self, p: &mut Pass, pairs: &[(Row, Row)]) -> Result<Vec<bool>> {
        let mut keep: Vec<bool> = pairs
            .iter()
            .map(|(a, b)| {
                self.m.compatible(a, b) && (self.mode != MatchMode::Minus || self.m.overlap(a, b))
            })
            .collect();
        if let Some(e) = &self.expr {
            let merged: Vec<Row> = pairs
                .iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|((a, b), _)| self.m.merge(a, b))
                .collect();
            let rows: Vec<&[Id]> = merged.iter().map(|r| &r[..]).collect();
            let vals = p.eval(&self.m.out, &rows, e, true)?;
            let mut it = vals.into_iter();
            for k in keep.iter_mut() {
                if *k {
                    *k = it.next().flatten() == Some(Id::from_bool(true));
                }
            }
        }
        Ok(keep)
    }

    /// The right rows at snapshot `w` that satisfy the condition with left row `a`.
    fn matching(&self, p: &mut Pass, w: Which, a: &Row) -> Result<Changes> {
        let key = self.m.key_left(a);
        let seed = self.m.seed_right(a);
        let cands = side_matches(p, &self.right, self.rarr.as_ref(), w, &key, &seed)?;
        let pairs: Vec<(Row, Row)> = cands.iter().map(|(b, _)| (a.clone(), b.clone())).collect();
        let keep = self.conditions(p, &pairs)?;
        Ok(cands
            .into_iter()
            .zip(keep)
            .filter(|(_, k)| *k)
            .map(|(c, _)| c)
            .collect())
    }

    /// The left rows (with weight and count) that can match right row `b`.
    fn left_for(&self, b: &Row) -> Vec<(Row, (i64, i64))> {
        self.lstate
            .get(&self.m.key_right(b))
            .into_iter()
            .flat_map(|bk| bk.iter().map(|(a, v)| (a.clone(), *v)))
            .collect()
    }

    fn out_row(&self, a: &Row) -> Row {
        match self.mode {
            MatchMode::LeftJoin => self.m.pad_left(a),
            _ => a.clone(),
        }
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let l = consolidate(self.left.init(p)?);
        let r = consolidate(self.right.init(p)?);
        let mut rindex = Arrangement::new(
            self.m.key.iter().map(|k| k.1).collect(),
            self.right.schema.len(),
        );
        rindex.apply(&r);
        if let Some(a) = &mut self.rarr {
            a.clear();
            a.apply(&r);
        }
        self.lstate.clear();
        self.lrows = 0;
        let mut out = Changes::new();
        for (a, wa) in &l {
            p.tick()?;
            let cands: Changes = rindex
                .get(&self.m.key_left(a))
                .map(|(b, w)| (b.clone(), w))
                .collect();
            let pairs: Vec<(Row, Row)> =
                cands.iter().map(|(b, _)| (a.clone(), b.clone())).collect();
            let keep = self.conditions(p, &pairs)?;
            let mut m = 0;
            for ((b, wb), k) in cands.iter().zip(keep) {
                if k {
                    m += wb;
                    if self.mode == MatchMode::LeftJoin {
                        out.push((self.m.merge(a, b), wa * wb));
                    }
                }
            }
            self.set(a, (*wa, m));
            if self.mode.alone(m) {
                out.push((self.out_row(a), *wa));
            }
        }
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let dl = consolidate(self.left.delta(p)?);
        let dr = consolidate(self.right.delta(p)?);
        let mut out = Changes::new();
        let lj = self.mode == MatchMode::LeftJoin;
        // the joined part: ΔL ⋈ R_old
        if lj {
            for (a, wa) in &dl {
                for (b, wb) in self.matching(p, Which::Old, a)? {
                    out.push((self.m.merge(a, &b), wa * wb));
                }
            }
        }
        // the counts of the left rows already present move with the right's changes
        let mut touched: FxHashMap<Row, (i64, i64)> = FxHashMap::default();
        let mut dm: FxHashMap<Row, i64> = FxHashMap::default();
        for (b, wb) in &dr {
            p.tick()?;
            let cands = self.left_for(b);
            let pairs: Vec<(Row, Row)> =
                cands.iter().map(|(a, _)| (a.clone(), b.clone())).collect();
            let keep = self.conditions(p, &pairs)?;
            for ((a, v), k) in cands.into_iter().zip(keep) {
                if k {
                    *dm.entry(a.clone()).or_insert(0) += wb;
                    touched.entry(a).or_insert(v);
                }
            }
        }
        if let Some(arr) = &mut self.rarr {
            arr.apply(&dr);
        }
        // the left's own changes; a row that appears counts its matches anew
        let mut dw: FxHashMap<Row, i64> = FxHashMap::default();
        for (a, wa) in &dl {
            *dw.entry(a.clone()).or_insert(0) += wa;
            if !touched.contains_key(a) {
                let v = self.get(a);
                touched.insert(a.clone(), v);
            }
        }
        #[allow(clippy::type_complexity)]
        let mut new: Vec<(Row, (i64, i64), (i64, i64))> = Vec::with_capacity(touched.len());
        for (a, (ow, om)) in touched {
            let nw = ow + dw.get(&a).copied().unwrap_or(0);
            let nm = if ow != 0 {
                om + dm.get(&a).copied().unwrap_or(0)
            } else if nw != 0 {
                self.matching(p, Which::New, &a)?
                    .iter()
                    .map(|(_, w)| w)
                    .sum()
            } else {
                0
            };
            new.push((a, (ow, om), (nw, nm)));
        }
        for (a, _, v) in &new {
            self.set(a, *v);
        }
        // the joined part: L_new ⋈ ΔR
        if lj {
            for (b, wb) in &dr {
                let cands = self.left_for(b);
                let pairs: Vec<(Row, Row)> =
                    cands.iter().map(|(a, _)| (a.clone(), b.clone())).collect();
                let keep = self.conditions(p, &pairs)?;
                for ((a, (wa, _)), k) in cands.into_iter().zip(keep) {
                    if k {
                        out.push((self.m.merge(&a, b), wa * wb));
                    }
                }
            }
        }
        // rows that move in or out of the part without a match
        for (a, (ow, om), (nw, nm)) in new {
            let before = if ow != 0 && self.mode.alone(om) {
                ow
            } else {
                0
            };
            let after = if nw != 0 && self.mode.alone(nm) {
                nw
            } else {
                0
            };
            if before != after {
                out.push((self.out_row(&a), after - before));
            }
        }
        Ok(out)
    }

    pub fn bytes(&self) -> u64 {
        self.lrows as u64 * (row_bytes(self.left.schema.len()) + 8)
            + self.lstate.len() as u64 * row_bytes(self.m.key.len())
            + self.rarr.as_ref().map_or(0, |a| a.bytes())
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.u64(self.lrows as u64);
        for bucket in self.lstate.values() {
            for (a, (n, m)) in bucket {
                w.row(a)?;
                w.i64(*n);
                w.i64(*m);
            }
        }
        if let Some(a) = &self.rarr {
            a.save(w)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.lstate.clear();
        self.lrows = 0;
        let n = r.u64()?;
        for _ in 0..n {
            let a = r.row()?;
            let w = r.i64()?;
            let m = r.i64()?;
            self.set(&a, (w, m));
        }
        if let Some(a) = &mut self.rarr {
            a.load(r)?;
        }
        Ok(())
    }
}
