//! Grouping with `COUNT`, `SUM` and `AVG`: per group, the counts and sums move with the
//! change rows, and the group's old row is retracted and its new one emitted.
//!
//! Integer and decimal sums are kept exactly, so removing a value is exact. Float and
//! double values are kept as value counts, and a changed group sums them again: such a
//! sum can differ in its last bits from a one-off evaluation, which adds the values in
//! the order the rows arrive. A sum whose integer part leaves the 64-bit range, or a
//! decimal sum outside the decimal range, is unbound, as an overflow in a one-off
//! evaluation is. A one-off evaluation can also overflow on the way to a sum in the
//! range, depending on the order of the rows, where the circuit's exact sum does not. `DISTINCT` aggregates keep the
//! counts of the group's distinct values.
//!
//! `MIN`, `MAX`, `GROUP_CONCAT` and `SAMPLE` keep the group's values as an ordered
//! multiset ([`Multiset`]): the distinct values in the order of `ORDER BY` with their
//! counts, 16 bytes each. `MIN` and `MAX` read its ends, so removing the extreme value
//! costs one search and no scan. `GROUP_CONCAT` joins the values again for a changed
//! group, in that order, where a one-off evaluation joins them in the order its rows
//! arrive. `SAMPLE` reports one value and keeps it while the group holds it, then takes
//! the smallest. Adding or removing a value decodes the values it is compared with on
//! the way, about the logarithm of the group's distinct values, and shifts the entries
//! after it. A hash map of counts would take the same memory and make removing the
//! extreme value scan the group, which this trades for the shift.

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::prov::{self, AChanges, Ann, AnnMap, Provenance};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::Node;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::value::order_cmp;
use crate::sparql::value::{NumOp, Value, arith};
use oxsdatatypes::{Decimal, Double, Float};
use rustc_hash::FxHashMap;
use spargebra::algebra::Expression;
use std::cmp::Ordering;

/// A 256-bit two's complement integer, for exact sums of 128-bit products.
#[derive(Default)]
struct Wide {
    hi: u128,
    lo: u128,
}

impl Wide {
    fn add(&mut self, hi: u128, lo: u128) {
        let (lo, carry) = self.lo.overflowing_add(lo);
        self.lo = lo;
        self.hi = self.hi.wrapping_add(hi).wrapping_add(u128::from(carry));
    }

    /// Add `a × b`.
    fn add_product(&mut self, a: i128, b: i64) {
        let negative = (a < 0) != (b < 0);
        let (m, n) = (a.unsigned_abs(), u128::from(b.unsigned_abs()));
        // |a| × |b| as two 128-bit limbs, from the 64-bit halves of |a|
        let low = (m & u128::from(u64::MAX)) * n;
        let high = (m >> 64) * n;
        let (lo, carry) = low.overflowing_add(high << 64);
        let hi = (high >> 64) + u128::from(carry);
        if negative {
            // two's complement: invert and add one
            let (lo, carry) = (!lo).overflowing_add(1);
            self.add((!hi).wrapping_add(u128::from(carry)), lo);
        } else {
            self.add(hi, lo);
        }
    }

    /// The value when it fits in 128 bits.
    fn to_i128(&self) -> Option<i128> {
        let v = self.lo as i128;
        let sign = if v < 0 { u128::MAX } else { 0 };
        (self.hi == sign).then_some(v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggKind {
    /// `COUNT(*)`
    CountAll,
    Count,
    Sum,
    Avg,
    Min,
    Max,
    /// `GROUP_CONCAT`
    Concat,
    Sample,
}

impl AggKind {
    /// Whether the aggregate's state is the group's values as a [`Multiset`].
    fn ordered(self) -> bool {
        matches!(
            self,
            AggKind::Min | AggKind::Max | AggKind::Concat | AggKind::Sample
        )
    }
}

#[derive(Clone, Debug)]
pub struct AggSpec {
    pub kind: AggKind,
    pub distinct: bool,
    pub expr: Option<Expression>,
    /// the expression is this input column (`Some(None)`: a variable the input lacks)
    pub column: Option<Option<usize>>,
    /// the separator of `GROUP_CONCAT` (`None`: a space)
    pub separator: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct SumState {
    rows: i64,
    errs: i64,
    /// the integer sum, in wrapping arithmetic so that removals stay exact
    int: i128,
    /// the decimal values as counts by their 128-bit encoding
    decs: FxHashMap<i128, i64>,
    floats: FxHashMap<u32, i64>,
    doubles: FxHashMap<u64, i64>,
}

impl SumState {
    /// Add `w` copies of a value (`None`: an error or unbound).
    fn add(&mut self, v: Option<&Value>, w: i64) {
        self.rows += w;
        match v {
            Some(Value::Integer(i)) => {
                self.int = self
                    .int
                    .wrapping_add((i64::from(*i) as i128).wrapping_mul(w as i128))
            }
            Some(Value::Decimal(d)) => {
                let k = i128::from_be_bytes(d.to_be_bytes());
                let n = self.decs.entry(k).or_insert(0);
                *n += w;
                if *n == 0 {
                    self.decs.remove(&k);
                }
            }
            Some(Value::Float(f)) => *self.floats.entry(f32::from(*f).to_bits()).or_insert(0) += w,
            Some(Value::Double(d)) => {
                *self.doubles.entry(f64::from(*d).to_bits()).or_insert(0) += w
            }
            _ => self.errs += w,
        }
        self.floats.retain(|_, n| *n != 0);
        self.doubles.retain(|_, n| *n != 0);
    }

    fn sum(&self) -> Option<Value> {
        if self.errs != 0 {
            return None;
        }
        let mut acc = if !self.decs.is_empty() {
            // summed exactly in 256 bits, so that values which cancel out do not
            // overflow on the way; the integers join the decimals, as a one-off
            // evaluation promotes them
            let mut acc = Wide::default();
            for (k, n) in &self.decs {
                acc.add_product(*k, *n);
            }
            acc.add_product(self.int, 1_000_000_000_000_000_000);
            Value::Decimal(Decimal::from_be_bytes(acc.to_i128()?.to_be_bytes()))
        } else {
            Value::Integer(i64::try_from(self.int).ok()?.into())
        };
        let mut floats: Vec<(f32, i64)> = self
            .floats
            .iter()
            .map(|(b, n)| (f32::from_bits(*b), *n))
            .collect();
        floats.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (f, n) in floats {
            acc = arith(NumOp::Add, &acc, &Value::Float(Float::from(f * n as f32))).ok()?;
        }
        let mut doubles: Vec<(f64, i64)> = self
            .doubles
            .iter()
            .map(|(b, n)| (f64::from_bits(*b), *n))
            .collect();
        doubles.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (d, n) in doubles {
            acc = arith(NumOp::Add, &acc, &Value::Double(Double::from(d * n as f64))).ok()?;
        }
        Some(acc)
    }
}

/// The values of a group as an ordered multiset: the distinct values in the order of
/// `ORDER BY` (then by id, for values the order does not tell apart) with their counts.
/// The state of `MIN`, `MAX`, `GROUP_CONCAT` and `SAMPLE`.
#[derive(Clone, Debug, Default)]
struct Multiset {
    vals: Vec<(Id, i64)>,
    /// the count of arguments whose evaluation failed, which make a `GROUP_CONCAT`
    /// unbound
    errs: i64,
    /// `SAMPLE`: the value reported, kept while the group holds it
    sample: Option<Id>,
}

impl Multiset {
    /// Where `v`, whose decoded value is `value`, is or belongs in `vals`.
    fn position(
        &self,
        p: &Pass,
        v: Id,
        value: Option<&Value>,
    ) -> std::result::Result<usize, usize> {
        self.vals.binary_search_by(|(x, _)| {
            if *x == v {
                return Ordering::Equal;
            }
            match order_cmp(p.value(*x).as_ref(), value) {
                Ordering::Equal => x.0.cmp(&v.0),
                o => o,
            }
        })
    }

    /// Add `w` copies of `v`.
    fn add(&mut self, p: &Pass, v: Id, w: i64) {
        let value = p.value(v);
        match self.position(p, v, value.as_ref()) {
            Ok(i) => {
                self.vals[i].1 += w;
                if self.vals[i].1 == 0 {
                    self.vals.remove(i);
                    if self.sample == Some(v) {
                        self.sample = None;
                    }
                }
            }
            Err(i) => self.vals.insert(i, (v, w)),
        }
    }

    /// The smallest value the group holds.
    fn min(&self) -> Option<Id> {
        self.vals.iter().find(|(_, n)| *n > 0).map(|(v, _)| *v)
    }

    fn max(&self) -> Option<Id> {
        self.vals
            .iter()
            .rev()
            .find(|(_, n)| *n > 0)
            .map(|(v, _)| *v)
    }

    /// Give `SAMPLE` a value when the group has values and its value left.
    fn settle(&mut self) {
        if self.sample.is_none() {
            self.sample = self.min();
        }
    }
}

#[derive(Clone, Debug)]
enum AState {
    Count(i64),
    /// the counts of the distinct values (and of the errors) of a DISTINCT aggregate
    Values {
        vals: FxHashMap<Id, i64>,
        errs: i64,
        sum: SumState,
    },
    /// `COUNT(DISTINCT *)`: the counts of the distinct rows
    Rows(FxHashMap<Row, i64>),
    Sum(SumState),
    /// `MIN`, `MAX`, `GROUP_CONCAT` and `SAMPLE`, with or without `DISTINCT`
    Multi(Multiset),
}

#[derive(Clone, Debug)]
struct GState {
    rows: i64,
    aggs: Vec<AState>,
    out: Option<Row>,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub inner: Node,
    /// the position of each key in the input (`None`: never bound)
    pub keys: Vec<Option<usize>>,
    pub aggs: Vec<AggSpec>,
    /// no `GROUP BY`: one group, present even without rows
    pub implicit: bool,
    groups: FxHashMap<Row, GState>,
    /// each group's annotation, the sum of its members', when the circuit keeps them
    /// (spec F13 §5.2)
    gann: Option<AnnMap>,
    /// the number of the condition of an implicit group without rows
    pub cond: u32,
    prov: Provenance,
}

impl Group {
    pub fn new(inner: Node, keys: Vec<Option<usize>>, aggs: Vec<AggSpec>, implicit: bool) -> Group {
        Group {
            inner,
            keys,
            aggs,
            implicit,
            groups: FxHashMap::default(),
            gann: None,
            cond: u32::MAX,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance, cond: u32) {
        self.prov = mode;
        self.cond = cond;
        self.gann = (!mode.is_none()).then(AnnMap::default);
    }

    fn key_of(&self, r: &[Id]) -> Row {
        self.keys
            .iter()
            .map(|k| k.map_or(Id::UNDEF, |k| r[k]))
            .collect()
    }

    /// The annotation of group `key`'s row: its members' sum, or, for an implicit
    /// group without rows, the condition that the pattern has no match.
    fn row_ann(&self, p: &mut Pass, key: &Row) -> Ann {
        let mode = p.prov;
        match self.groups.get(key) {
            Some(g) if g.rows == 0 && self.implicit => p.absent_ann(self.cond, &[]),
            Some(_) => self
                .gann
                .as_ref()
                .map_or_else(|| Ann::zero(mode), |m| prov::get(m, key, mode)),
            None => Ann::zero(mode),
        }
    }

    /// [`apply`](Self::apply) with annotations. The weights move the groups as
    /// without them, and the annotations of the rows that the groups retract, emit or
    /// keep follow the groups' annotations before and after.
    fn apply_a(&mut self, p: &mut Pass, c: AChanges) -> Result<AChanges> {
        let mode = p.prov;
        let mut before: FxHashMap<Row, Ann> = FxHashMap::default();
        for (r, _, _) in &c {
            let k = self.key_of(r);
            if let std::collections::hash_map::Entry::Vacant(e) = before.entry(k) {
                let a = self.row_ann(p, e.key());
                e.insert(a);
            }
        }
        let plain = consolidate(c.iter().map(|(r, w, _)| (r.clone(), *w)).collect());
        let mut gann = self.gann.take().unwrap_or_default();
        for (r, _, a) in c {
            let k = self.key_of(&r);
            prov::absorb_row(&mut gann, &k, a, mode);
        }
        self.gann = Some(gann);
        let moved = if plain.is_empty() {
            Changes::new()
        } else {
            self.apply(p, plain)?
        };
        let n = self.keys.len();
        let mut out = AChanges::new();
        let mut emitted: rustc_hash::FxHashSet<Row> = Default::default();
        for (row, w) in moved {
            let k: Row = row[..n].into();
            let a = if w < 0 {
                before
                    .get(&k)
                    .cloned()
                    .unwrap_or_else(|| Ann::zero(mode))
                    .scaled(w)
            } else {
                self.row_ann(p, &k).scaled(w)
            };
            emitted.insert(k);
            out.push((row, w, a));
        }
        for (k, old) in before {
            if emitted.contains(&k) {
                continue;
            }
            let Some(row) = self.groups.get(&k).and_then(|g| g.out.clone()) else {
                continue;
            };
            let d = self.row_ann(p, &k).sub(&old);
            if !d.is_zero() {
                out.push((row, 0, d));
            }
        }
        if let Some(m) = self.gann.as_mut() {
            m.retain(|k, _| self.groups.contains_key(k));
        }
        Ok(out)
    }

    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        self.groups.clear();
        if let Some(m) = self.gann.as_mut() {
            m.clear();
        }
        let c = prov::consolidate(self.inner.init_a(p)?);
        let mut out = self.apply_a(p, c)?;
        if self.implicit && self.groups.is_empty() {
            let g = self.fresh();
            let mut row = Vec::new();
            row.extend(self.finish(p, &g));
            let row: Row = row.into();
            let mut g = g;
            g.out = Some(row.clone());
            self.groups.insert(Row::default(), g);
            let a = self.row_ann(p, &Row::default());
            out.push((row, 1, a));
        }
        Ok(out)
    }

    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let c = prov::consolidate(self.inner.delta_a(p)?);
        if c.is_empty() {
            return Ok(c);
        }
        self.apply_a(p, c)
    }

    fn fresh(&self) -> GState {
        GState {
            rows: 0,
            aggs: self
                .aggs
                .iter()
                .map(|a| match (a.kind, a.distinct) {
                    (k, _) if k.ordered() => AState::Multi(Multiset::default()),
                    (AggKind::CountAll, true) => AState::Rows(FxHashMap::default()),
                    (AggKind::CountAll | AggKind::Count, false) => AState::Count(0),
                    (_, true) => AState::Values {
                        vals: FxHashMap::default(),
                        errs: 0,
                        sum: SumState::default(),
                    },
                    (_, false) => AState::Sum(SumState::default()),
                })
                .collect(),
            out: None,
        }
    }

    pub fn describe(&self, vars: &[String]) -> String {
        let keys: Vec<String> = self
            .keys
            .iter()
            .map(|k| match k {
                Some(i) => format!("?{}", vars[self.inner.schema[*i] as usize]),
                None => "UNDEF".into(),
            })
            .collect();
        let aggs: Vec<String> = self
            .aggs
            .iter()
            .map(|a| {
                let f = match a.kind {
                    AggKind::CountAll | AggKind::Count => "COUNT",
                    AggKind::Sum => "SUM",
                    AggKind::Avg => "AVG",
                    AggKind::Min => "MIN",
                    AggKind::Max => "MAX",
                    AggKind::Concat => "GROUP_CONCAT",
                    AggKind::Sample => "SAMPLE",
                };
                let d = if a.distinct { "DISTINCT " } else { "" };
                let sep = match &a.separator {
                    Some(s) => format!("; SEPARATOR = {:?}", s),
                    None => String::new(),
                };
                match &a.expr {
                    Some(e) => format!("{f}({d}{e}{sep})"),
                    None => format!("{f}({d}*)"),
                }
            })
            .collect();
        format!("by [{}] {}", keys.join(" "), aggs.join(" "))
    }

    /// The aggregates' values in a group's state.
    fn finish(&self, p: &mut Pass, g: &GState) -> Vec<Id> {
        let mut out = Vec::with_capacity(self.aggs.len());
        for (spec, st) in self.aggs.iter().zip(&g.aggs) {
            let v = match st {
                AState::Count(n) => Id::from_i64(*n).unwrap_or(Id::UNDEF),
                AState::Rows(rows) => Id::from_i64(rows.len() as i64).unwrap_or(Id::UNDEF),
                AState::Values { vals, errs, sum } => match spec.kind {
                    AggKind::Count => Id::from_i64(vals.len() as i64).unwrap_or(Id::UNDEF),
                    _ => {
                        let mut s = sum.clone();
                        s.errs += errs;
                        s.rows = vals.len() as i64 + errs;
                        self.sum_value(p, spec.kind, &s)
                    }
                },
                AState::Sum(s) => self.sum_value(p, spec.kind, s),
                AState::Multi(m) => match spec.kind {
                    AggKind::Min => m.min().unwrap_or(Id::UNDEF),
                    AggKind::Max => m.max().unwrap_or(Id::UNDEF),
                    AggKind::Sample => m.sample.unwrap_or(Id::UNDEF),
                    _ => Self::concat(p, spec, m),
                },
            };
            out.push(v);
        }
        out
    }

    /// `GROUP_CONCAT` of a group's values in their order, each as often as the group
    /// holds it, or once with `DISTINCT`. An argument that failed, or a value without a
    /// lexical form, makes the result unbound, as in a one-off evaluation. The result is a
    /// simple literal.
    fn concat(p: &mut Pass, spec: &AggSpec, m: &Multiset) -> Id {
        if m.errs != 0 {
            return Id::UNDEF;
        }
        let sep = spec.separator.as_deref().unwrap_or(" ");
        let mut out = String::new();
        let mut first = true;
        for (v, n) in &m.vals {
            if *n <= 0 {
                continue;
            }
            let Some(s) = p.value(*v).and_then(|v| v.lexical().ok()) else {
                return Id::UNDEF;
            };
            let reps = if spec.distinct { 1 } else { *n };
            for _ in 0..reps {
                if !first {
                    out.push_str(sep);
                }
                first = false;
                out.push_str(&s);
            }
        }
        p.value_id(&Value::Str(out.into()))
    }

    fn sum_value(&self, p: &mut Pass, kind: AggKind, s: &SumState) -> Id {
        let Some(sum) = s.sum() else {
            return Id::UNDEF;
        };
        let v = if kind == AggKind::Avg {
            if s.rows == 0 {
                Some(Value::Integer(0.into()))
            } else {
                arith(NumOp::Div, &sum, &Value::Integer(s.rows.into())).ok()
            }
        } else {
            Some(sum)
        };
        v.map_or(Id::UNDEF, |v| p.value_id(&v))
    }

    fn apply(&mut self, p: &mut Pass, c: Changes) -> Result<Changes> {
        // each aggregate's argument for each change row
        let rows: Vec<&[Id]> = c.iter().map(|(r, _)| &r[..]).collect();
        let mut args: Vec<Vec<Option<Id>>> = Vec::with_capacity(self.aggs.len());
        for a in &self.aggs {
            args.push(match (&a.expr, a.column) {
                (None, _) => vec![None; rows.len()],
                (_, Some(Some(i))) => rows
                    .iter()
                    .map(|r| Some(r[i]).filter(|v| !v.is_undef()))
                    .collect(),
                (_, Some(None)) => vec![None; rows.len()],
                (Some(e), None) => p.eval(&self.inner.schema, &rows, e, false)?,
            });
        }
        let mut touched: Vec<Row> = Vec::new();
        let mut seen: rustc_hash::FxHashSet<Row> = Default::default();
        for (i, (r, w)) in c.iter().enumerate() {
            let key: Row = self
                .keys
                .iter()
                .map(|k| k.map_or(Id::UNDEF, |k| r[k]))
                .collect();
            if !self.groups.contains_key(&key) {
                let f = self.fresh();
                self.groups.insert(key.clone(), f);
            }
            if seen.insert(key.clone()) {
                touched.push(key.clone());
            }
            let w = *w;
            let mut vals: Vec<Option<Value>> = Vec::with_capacity(self.aggs.len());
            for (ai, spec) in self.aggs.iter().enumerate() {
                let arg = args[ai][i];
                vals.push(match spec.kind {
                    AggKind::Sum | AggKind::Avg => arg.and_then(|v| p.value(v)),
                    _ => None,
                });
            }
            let g = self.groups.get_mut(&key).expect("inserted above");
            g.rows += w;
            for (ai, st) in g.aggs.iter_mut().enumerate() {
                let arg = args[ai][i];
                match st {
                    AState::Count(n) => {
                        if self.aggs[ai].kind == AggKind::CountAll || arg.is_some() {
                            *n += w;
                        }
                    }
                    AState::Rows(m) => {
                        let e = m.entry(r.clone()).or_insert(0);
                        *e += w;
                        if *e == 0 {
                            m.remove(r);
                        }
                    }
                    AState::Values { vals: m, errs, sum } => match arg {
                        None => *errs += w,
                        Some(v) => {
                            let e = m.entry(v).or_insert(0);
                            let before = *e;
                            *e += w;
                            let after = *e;
                            if after == 0 {
                                m.remove(&v);
                            }
                            let val = vals[ai].as_ref();
                            if before <= 0 && after > 0 {
                                sum.add(val, 1);
                            } else if before > 0 && after <= 0 {
                                sum.add(val, -1);
                            }
                        }
                    },
                    AState::Sum(s) => s.add(vals[ai].as_ref(), w),
                    AState::Multi(m) => match arg {
                        Some(v) => m.add(p, v, w),
                        // a failed expression is an error, and so is an unbound variable
                        // under GROUP_CONCAT, where a one-off evaluation answers an
                        // error; MIN, MAX and SAMPLE leave an unbound variable out, as a
                        // one-off evaluation does
                        None if self.aggs[ai].column.is_none()
                            || self.aggs[ai].kind == AggKind::Concat =>
                        {
                            m.errs += w
                        }
                        None => {}
                    },
                }
            }
        }
        let mut out = Changes::new();
        for key in touched {
            if let Some(g) = self.groups.get_mut(&key) {
                for st in g.aggs.iter_mut() {
                    if let AState::Multi(m) = st {
                        m.settle();
                    }
                }
            }
            let g = self.groups.get(&key).expect("touched groups exist").clone();
            let new = if g.rows > 0 || self.implicit {
                let mut row = key.to_vec();
                row.extend(self.finish(p, &g));
                Some(row.into_boxed_slice())
            } else {
                None
            };
            if g.out != new {
                if let Some(o) = &g.out {
                    out.push((o.clone(), -1));
                }
                if let Some(n) = &new {
                    out.push((n.clone(), 1));
                }
            }
            if g.rows <= 0 && !self.implicit {
                self.groups.remove(&key);
            } else if let Some(s) = self.groups.get_mut(&key) {
                s.out = new;
            }
        }
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.groups.clear();
        let c = consolidate(self.inner.init(p)?);
        let mut out = self.apply(p, c)?;
        if self.implicit && self.groups.is_empty() {
            let g = self.fresh();
            let mut row = Vec::new();
            row.extend(self.finish(p, &g));
            let row: Row = row.into();
            let mut g = g;
            g.out = Some(row.clone());
            self.groups.insert(Row::default(), g);
            out.push((row, 1));
        }
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = consolidate(self.inner.delta(p)?);
        if c.is_empty() {
            return Ok(c);
        }
        self.apply(p, c)
    }

    pub fn bytes(&self) -> u64 {
        let width = self.keys.len() + self.aggs.len();
        self.groups
            .values()
            .map(|g| {
                row_bytes(width) * 2
                    + g.aggs
                        .iter()
                        .map(|a| match a {
                            AState::Count(_) => 8,
                            AState::Rows(m) => m.len() as u64 * row_bytes(self.inner.schema.len()),
                            AState::Values { vals, .. } => vals.len() as u64 * 24 + 96,
                            AState::Sum(s) => {
                                96 + (s.floats.len() + s.doubles.len()) as u64 * 24
                                    + s.decs.len() as u64 * 40
                            }
                            AState::Multi(m) => 40 + m.vals.len() as u64 * 16,
                        })
                        .sum::<u64>()
            })
            .sum::<u64>()
            + self
                .gann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.keys.len()))
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        fn sum(w: &mut Writer, s: &SumState) {
            w.i64(s.rows);
            w.i64(s.errs);
            w.i128(s.int);
            w.u64(s.decs.len() as u64);
            for (k, n) in &s.decs {
                w.i128(*k);
                w.i64(*n);
            }
            w.u64(s.floats.len() as u64);
            for (b, n) in &s.floats {
                w.u64(*b as u64);
                w.i64(*n);
            }
            w.u64(s.doubles.len() as u64);
            for (b, n) in &s.doubles {
                w.u64(*b);
                w.i64(*n);
            }
        }
        w.u64(self.groups.len() as u64);
        for (k, g) in &self.groups {
            w.row(k)?;
            w.i64(g.rows);
            match &g.out {
                Some(o) => {
                    w.u64(1);
                    w.row(o)?;
                }
                None => w.u64(0),
            }
            for a in &g.aggs {
                match a {
                    AState::Count(n) => w.i64(*n),
                    AState::Rows(m) => {
                        w.u64(m.len() as u64);
                        for (r, n) in m {
                            w.row(r)?;
                            w.i64(*n);
                        }
                    }
                    AState::Values { vals, errs, sum: s } => {
                        w.u64(vals.len() as u64);
                        for (v, n) in vals {
                            w.id(*v)?;
                            w.i64(*n);
                        }
                        w.i64(*errs);
                        sum(w, s);
                    }
                    AState::Sum(s) => sum(w, s),
                    AState::Multi(m) => {
                        w.u64(m.vals.len() as u64);
                        for (v, n) in &m.vals {
                            w.id(*v)?;
                            w.i64(*n);
                        }
                        w.i64(m.errs);
                        match m.sample {
                            Some(v) => {
                                w.u64(1);
                                w.id(v)?;
                            }
                            None => w.u64(0),
                        }
                    }
                }
            }
        }
        if let Some(m) = &self.gann {
            prov::save_map(w, m)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        fn sum(r: &mut Reader) -> Result<SumState> {
            let mut s = SumState {
                rows: r.i64()?,
                errs: r.i64()?,
                int: r.i128()?,
                ..Default::default()
            };
            for _ in 0..r.u64()? {
                let k = r.i128()?;
                s.decs.insert(k, r.i64()?);
            }
            for _ in 0..r.u64()? {
                let b = r.u64()? as u32;
                s.floats.insert(b, r.i64()?);
            }
            for _ in 0..r.u64()? {
                let b = r.u64()?;
                s.doubles.insert(b, r.i64()?);
            }
            Ok(s)
        }
        self.groups.clear();
        let n = r.u64()?;
        for _ in 0..n {
            let k = r.row()?;
            let mut g = self.fresh();
            g.rows = r.i64()?;
            g.out = match r.u64()? {
                0 => None,
                _ => Some(r.row()?),
            };
            for a in g.aggs.iter_mut() {
                match a {
                    AState::Count(n) => *n = r.i64()?,
                    AState::Rows(m) => {
                        for _ in 0..r.u64()? {
                            let row = r.row()?;
                            m.insert(row, r.i64()?);
                        }
                    }
                    AState::Values { vals, errs, sum: s } => {
                        for _ in 0..r.u64()? {
                            let v = r.id()?;
                            vals.insert(v, r.i64()?);
                        }
                        *errs = r.i64()?;
                        *s = sum(r)?;
                    }
                    AState::Sum(s) => *s = sum(r)?,
                    AState::Multi(m) => {
                        // the entries were written in their order, which the terms keep
                        for _ in 0..r.u64()? {
                            let v = r.id()?;
                            m.vals.push((v, r.i64()?));
                        }
                        m.errs = r.i64()?;
                        m.sample = match r.u64()? {
                            0 => None,
                            _ => Some(r.id()?),
                        };
                    }
                }
            }
            self.groups.insert(k, g);
        }
        let mode = self.prov;
        if let Some(m) = self.gann.as_mut() {
            *m = prov::load_map(r, mode)?;
        }
        Ok(())
    }
}
