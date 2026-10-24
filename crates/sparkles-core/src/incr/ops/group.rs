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

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::Node;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::value::{NumOp, Value, arith};
use oxsdatatypes::{Decimal, Double, Float};
use rustc_hash::FxHashMap;
use spargebra::algebra::Expression;

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
}

#[derive(Clone, Debug)]
pub struct AggSpec {
    pub kind: AggKind,
    pub distinct: bool,
    pub expr: Option<Expression>,
    /// the expression is this input column (`Some(None)`: a variable the input lacks)
    pub column: Option<Option<usize>>,
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
}

impl Group {
    pub fn new(inner: Node, keys: Vec<Option<usize>>, aggs: Vec<AggSpec>, implicit: bool) -> Group {
        Group {
            inner,
            keys,
            aggs,
            implicit,
            groups: FxHashMap::default(),
        }
    }

    fn fresh(&self) -> GState {
        GState {
            rows: 0,
            aggs: self
                .aggs
                .iter()
                .map(|a| match (a.kind, a.distinct) {
                    (AggKind::CountAll, true) => AState::Rows(FxHashMap::default()),
                    (AggKind::CountAll | AggKind::Count, false) => AState::Count(0),
                    (_, true) => AState::Values {
                        vals: FxHashMap::default(),
                        errs: 0,
                        sum: SumState::default(),
                    },
                    (AggKind::Sum | AggKind::Avg, false) => AState::Sum(SumState::default()),
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
                };
                let d = if a.distinct { "DISTINCT " } else { "" };
                match &a.expr {
                    Some(e) => format!("{f}({d}{e})"),
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
            };
            out.push(v);
        }
        out
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
                }
            }
        }
        let mut out = Changes::new();
        for key in touched {
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
                        })
                        .sum::<u64>()
            })
            .sum()
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
                }
            }
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
                }
            }
            self.groups.insert(k, g);
        }
        Ok(())
    }
}
