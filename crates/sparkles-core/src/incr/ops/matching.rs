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
use super::super::prov::{self, AChanges, Ann, AnnMap, Provenance};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::{Arrangement, Merge, Node, absorb_side, side_matches, side_matches_a};
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
    /// the annotations of the left rows, when the circuit keeps them (spec F13 §5)
    lann: Option<AnnMap>,
    /// for `EXISTS`, the sum of the annotations of each left row's matches
    sann: Option<AnnMap>,
    rann: Option<AnnMap>,
    /// the number of the operator's negative condition (spec F13 §5.2)
    pub cond: u32,
    prov: Provenance,
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
            lann: None,
            sann: None,
            rann: None,
            cond: u32::MAX,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance, cond: u32) {
        self.prov = mode;
        self.cond = cond;
        self.lann = Some(AnnMap::default());
        self.sann = (self.mode == MatchMode::Exists).then(AnnMap::default);
        self.rann = self.rarr.is_some().then(AnnMap::default);
    }

    /// [`matching`](Self::matching) with the annotations of the right rows.
    fn matching_a(&self, p: &mut Pass, w: Which, a: &Row) -> Result<AChanges> {
        let key = self.m.key_left(a);
        let seed = self.m.seed_right(a);
        let cands = side_matches_a(
            p,
            &self.right,
            self.rarr.as_ref(),
            self.rann.as_ref(),
            w,
            &key,
            &seed,
        )?;
        let pairs: Vec<(Row, Row)> = cands
            .iter()
            .map(|(b, _, _)| (a.clone(), b.clone()))
            .collect();
        let keep = self.conditions(p, &pairs)?;
        Ok(cands
            .into_iter()
            .zip(keep)
            .filter(|(_, k)| *k)
            .map(|(c, _)| c)
            .collect())
    }

    /// The annotation of left row `a` alone in the output, from its annotation `la`
    /// and, for `EXISTS`, the sum `sa` of its matches': the product with its matches
    /// for `EXISTS`, and with the condition that nothing matches otherwise.
    fn alone_ann(&self, p: &mut Pass, a: &Row, la: &Ann, sa: &Ann) -> Ann {
        match self.mode {
            MatchMode::Exists => la.mul(sa),
            _ => {
                let vals: Vec<(u32, Id)> = self
                    .m
                    .shared
                    .iter()
                    .map(|&(i, _)| (self.left.schema[i], a[i]))
                    .collect();
                la.mul(&p.absent_ann(self.cond, &vals))
            }
        }
    }

    /// [`init`](Self::init) with annotations.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let l = prov::consolidate(self.left.init_a(p)?);
        let r = prov::consolidate(self.right.init_a(p)?);
        let r = match (&mut self.rarr, &mut self.rann) {
            (Some(a), Some(m)) => {
                a.clear();
                m.clear();
                absorb_side(a, m, r, mode)
            }
            _ => r,
        };
        let mut rindex: FxHashMap<Row, Vec<usize>> = FxHashMap::default();
        for (i, (b, _, _)) in r.iter().enumerate() {
            rindex.entry(self.m.key_right(b)).or_default().push(i);
        }
        self.lstate.clear();
        self.lrows = 0;
        let mut lann = std::mem::take(&mut self.lann).unwrap_or_default();
        let mut sann = std::mem::take(&mut self.sann);
        lann.clear();
        if let Some(s) = sann.as_mut() {
            s.clear();
        }
        let mut out = AChanges::new();
        for (a, wa, aa) in l {
            p.tick()?;
            prov::absorb_row(&mut lann, &a, aa, mode);
            if wa == 0 {
                continue;
            }
            let la = prov::get(&lann, &a, mode);
            let cands: Vec<&(Row, i64, Ann)> = rindex
                .get(&self.m.key_left(&a))
                .into_iter()
                .flatten()
                .map(|&i| &r[i])
                .collect();
            let pairs: Vec<(Row, Row)> = cands
                .iter()
                .map(|(b, _, _)| (a.clone(), b.clone()))
                .collect();
            let keep = self.conditions(p, &pairs)?;
            let mut m = 0;
            let mut sum = Ann::zero(mode);
            for ((b, wb, ab), k) in cands.into_iter().zip(keep) {
                if k {
                    m += wb;
                    if self.mode == MatchMode::LeftJoin {
                        out.push((self.m.merge(&a, b), wa * wb, la.mul(ab)));
                    } else if self.mode == MatchMode::Exists {
                        sum.add_assign(ab);
                    }
                }
            }
            let sa = match sann.as_mut() {
                Some(s) => {
                    prov::absorb_row(s, &a, sum, mode);
                    prov::get(s, &a, mode)
                }
                None => sum,
            };
            self.set(&a, (wa, m));
            if self.mode.alone(m) {
                let ann = self.alone_ann(p, &a, &la, &sa);
                out.push((self.out_row(&a), wa, ann));
            }
        }
        self.lann = Some(lann);
        self.sann = sann;
        Ok(out)
    }

    /// [`delta`](Self::delta) with annotations. Each input's change is absorbed into
    /// the state that keeps it, and the output's change is the change of what the
    /// states give: the joined rows of an `OPTIONAL` as a join's are, and the rows
    /// alone as the difference of their annotations before and after.
    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let dl = prov::consolidate(self.left.delta_a(p)?);
        let dr = prov::consolidate(self.right.delta_a(p)?);
        let lj = self.mode == MatchMode::LeftJoin;
        let ex = self.mode == MatchMode::Exists;
        // the left rows each right change matches, by the counts before the changes
        let mut touched: FxHashMap<Row, (i64, i64)> = FxHashMap::default();
        let mut hits: Vec<(Row, usize)> = Vec::new();
        for (i, (b, _, _)) in dr.iter().enumerate() {
            p.tick()?;
            let cands = self.left_for(b);
            let pairs: Vec<(Row, Row)> =
                cands.iter().map(|(a, _)| (a.clone(), b.clone())).collect();
            let keep = self.conditions(p, &pairs)?;
            for ((a, v), k) in cands.into_iter().zip(keep) {
                if k {
                    hits.push((a.clone(), i));
                    touched.entry(a).or_insert(v);
                }
            }
        }
        for (a, _, _) in &dl {
            if !touched.contains_key(a) {
                let v = self.get(a);
                touched.insert(a.clone(), v);
            }
        }
        let mut lann = std::mem::take(&mut self.lann).unwrap_or_default();
        let mut sann = std::mem::take(&mut self.sann);
        // the annotations before the changes
        let before: FxHashMap<Row, (Ann, Ann)> = touched
            .keys()
            .map(|a| {
                let la = prov::get(&lann, a, mode);
                let sa = sann
                    .as_ref()
                    .map_or_else(|| Ann::zero(mode), |s| prov::get(s, a, mode));
                (a.clone(), (la, sa))
            })
            .collect();
        let mut dw: FxHashMap<Row, i64> = FxHashMap::default();
        let dl: AChanges = dl
            .into_iter()
            .map(|(a, wa, aa)| {
                *dw.entry(a.clone()).or_insert(0) += wa;
                let e = prov::absorb_row(&mut lann, &a, aa, mode);
                (a, wa, e)
            })
            .collect();
        let mut out = AChanges::new();
        // the joined part: ΔL ⋈ R_old
        if lj {
            for (a, wa, aa) in &dl {
                for (b, wb, ab) in self.matching_a(p, Which::Old, a)? {
                    out.push((self.m.merge(a, &b), wa * wb, aa.mul(&ab)));
                }
            }
        }
        let dr = match (&mut self.rarr, &mut self.rann) {
            (Some(a), Some(m)) => absorb_side(a, m, dr, mode),
            _ => dr,
        };
        let mut dm: FxHashMap<Row, i64> = FxHashMap::default();
        let mut ds: FxHashMap<Row, Ann> = FxHashMap::default();
        for (a, i) in hits {
            let (_, wb, ab) = &dr[i];
            *dm.entry(a.clone()).or_insert(0) += wb;
            if ex {
                ds.entry(a)
                    .or_insert_with(|| Ann::zero(mode))
                    .add_assign(ab);
            }
        }
        #[allow(clippy::type_complexity)]
        let mut new: Vec<(Row, (i64, i64), (i64, i64))> = Vec::with_capacity(touched.len());
        for (a, (ow, om)) in touched {
            let nw = ow + dw.get(&a).copied().unwrap_or(0);
            let nm = if ow != 0 {
                if let (Some(s), Some(d)) = (sann.as_mut(), ds.remove(&a)) {
                    prov::absorb_row(s, &a, d, mode);
                }
                om + dm.get(&a).copied().unwrap_or(0)
            } else if nw != 0 {
                let ms = self.matching_a(p, Which::New, &a)?;
                if let Some(s) = sann.as_mut() {
                    let mut sum = Ann::zero(mode);
                    for (_, _, ab) in &ms {
                        sum.add_assign(ab);
                    }
                    s.remove(&a);
                    prov::absorb_row(s, &a, sum, mode);
                }
                ms.iter().map(|(_, w, _)| w).sum()
            } else {
                0
            };
            if nw == 0
                && let Some(s) = sann.as_mut()
            {
                s.remove(&a);
            }
            new.push((a, (ow, om), (nw, nm)));
        }
        for (a, _, v) in &new {
            self.set(a, *v);
        }
        // the joined part: L_new ⋈ ΔR
        if lj {
            for (b, wb, ab) in &dr {
                let cands = self.left_for(b);
                let pairs: Vec<(Row, Row)> =
                    cands.iter().map(|(a, _)| (a.clone(), b.clone())).collect();
                let keep = self.conditions(p, &pairs)?;
                for ((a, (wa, _)), k) in cands.into_iter().zip(keep) {
                    if k {
                        let la = prov::get(&lann, &a, mode);
                        out.push((self.m.merge(&a, b), wa * wb, la.mul(ab)));
                    }
                }
            }
        }
        // rows that move in or out of the part without a match, or whose annotation
        // there changes
        for (a, (ow, om), (nw, nm)) in new {
            let was = ow != 0 && self.mode.alone(om);
            let is = nw != 0 && self.mode.alone(nm);
            let before_w = if was { ow } else { 0 };
            let after_w = if is { nw } else { 0 };
            let zero = Ann::zero(mode);
            let (ola, osa) = &before[&a];
            let old = if was {
                self.alone_ann(p, &a, ola, osa)
            } else {
                zero.clone()
            };
            let now = if is {
                let la = prov::get(&lann, &a, mode);
                let sa = sann
                    .as_ref()
                    .map_or_else(|| Ann::zero(mode), |s| prov::get(s, &a, mode));
                self.alone_ann(p, &a, &la, &sa)
            } else {
                zero
            };
            let d = now.sub(&old);
            if before_w != after_w || !d.is_zero() {
                out.push((self.out_row(&a), after_w - before_w, d));
            }
        }
        self.lann = Some(lann);
        self.sann = sann;
        Ok(out)
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
        let lw = self.left.schema.len();
        self.lrows as u64 * (row_bytes(lw) + 8)
            + self.lstate.len() as u64 * row_bytes(self.m.key.len())
            + self.rarr.as_ref().map_or(0, |a| a.bytes())
            + [&self.lann, &self.sann]
                .into_iter()
                .flatten()
                .map(|m| prov::map_bytes(m, lw))
                .sum::<u64>()
            + self
                .rann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.right.schema.len()))
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
        for m in [&self.lann, &self.sann, &self.rann].into_iter().flatten() {
            prov::save_map(w, m)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.lstate.clear();
        self.lrows = 0;
        let n = r.u64()?;
        for _ in 0..n {
            let a = r.row()?;
            if a.len() != self.left.schema.len() {
                return Err(crate::error::Error::Corrupt(
                    "a view's state holds a row of another width".into(),
                ));
            }
            let w = r.i64()?;
            let m = r.i64()?;
            self.set(&a, (w, m));
        }
        if let Some(a) = &mut self.rarr {
            a.load(r)?;
        }
        let mode = self.prov;
        for m in [&mut self.lann, &mut self.sann, &mut self.rann]
            .into_iter()
            .flatten()
        {
            *m = prov::load_map(r, mode)?;
        }
        Ok(())
    }
}
