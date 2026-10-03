//! Custom aggregates: extension IRIs that the parser reads as aggregate calls
//! (`SELECT (geof:aggUnion(?w) AS ?u)` groups like `SUM`), and their evaluation.
//!
//! The GeoSPARQL aggregates (`geof:aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`,
//! `aggConcaveHull`, `aggConvexHull`, `aggUnion`) are registered in every build, so a
//! query groups the same way with or without the `geo` feature; without it their value
//! is unbound, like an unknown function's.
//!
//! Jena ARQ's statistical aggregates follow ARQ's results exactly. The parser reads the
//! keywords `MEDIAN`, `MODE`, `STDEV`, `STDEV_SAMP`, `STDEV_POP`, `VARIANCE`, `VAR_SAMP`
//! and `VAR_POP` as custom aggregates in ARQ's aggregate namespace (`agg:`), and the six
//! variance and deviation aggregates are also registered under `afn:`, as ARQ registers
//! them:
//!
//! - `MEDIAN` and `MODE` convert each value to a double and return an `xsd:decimal`.
//!   `MEDIAN` is the middle value, or the mean of the two middle ones. `MODE` is the most
//!   frequent value; among equally frequent values it is the one that reached that count
//!   first in row order, as in ARQ. Over no rows both are `0`.
//! - `STDEV` / `STDEV_SAMP`, `STDEV_POP`, `VARIANCE` / `VAR_SAMP` and `VAR_POP` return an
//!   `xsd:double` computed with ARQ's shifted sums, so the rounding matches. The sample
//!   forms of one value are an error, and over no rows all are unbound.
//! - A value that is not a number, or an expression error in any row, makes the
//!   aggregate unbound.

use super::ctx::Ctx;
use super::value::{Num, Value};
use crate::geo::vocab::{AGGREGATES, GEOF};
use crate::id::{Id, Tag};
use oxrdf::NamedNode;
use oxsdatatypes::{Decimal, Double};
use rustc_hash::FxHashMap;
use spargebra::SparqlParser;
use spargebra::algebra::{ARQ_AGGREGATE_KEYWORDS, ARQ_AGGREGATE_NAMESPACE};
use std::str::FromStr;

/// ARQ's function library namespace (`afn:`), where ARQ registers its variance and
/// deviation aggregates a second time.
pub const AFN: &str = "http://jena.apache.org/ARQ/function#";

/// The local names of ARQ's variance and deviation aggregates.
const STATS: [&str; 6] = [
    "stdev",
    "stdev_samp",
    "stdev_pop",
    "variance",
    "var_samp",
    "var_pop",
];

/// The parser with every custom aggregate IRI of this build registered.
pub fn register(mut p: SparqlParser) -> SparqlParser {
    for local in AGGREGATES {
        p = p.with_custom_aggregate_function(NamedNode::new_unchecked(format!("{GEOF}{local}")));
    }
    for (_, local) in ARQ_AGGREGATE_KEYWORDS {
        p = p.with_custom_aggregate_function(NamedNode::new_unchecked(format!(
            "{ARQ_AGGREGATE_NAMESPACE}{local}"
        )));
    }
    for local in STATS {
        p = p.with_custom_aggregate_function(NamedNode::new_unchecked(format!("{AFN}{local}")));
    }
    p
}

/// One of ARQ's statistical aggregates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arq {
    Median,
    Mode,
    /// variance or standard deviation, of the population or of a sample
    Stat {
        population: bool,
        root: bool,
    },
}

impl Arq {
    /// The ARQ aggregate an IRI names: `agg:median`, `agg:stdev`, `afn:stdev`, ….
    pub(crate) fn of(iri: &str) -> Option<Arq> {
        let local = iri
            .strip_prefix(ARQ_AGGREGATE_NAMESPACE)
            .or_else(|| iri.strip_prefix(AFN).filter(|l| STATS.contains(l)))?;
        let stat = |population, root| Arq::Stat { population, root };
        Some(match local {
            "median" => Arq::Median,
            "mode" => Arq::Mode,
            "stdev" | "stdev_samp" => stat(false, true),
            "stdev_pop" => stat(true, true),
            "variance" | "var_samp" => stat(false, false),
            "var_pop" => stat(true, false),
            _ => return None,
        })
    }
}

/// The value of the custom aggregate `iri` over the evaluated values of one group
/// (after DISTINCT; `Err` for a row whose expression was an error); unbound when `iri`
/// is not a known aggregate or the aggregate is an error.
pub(crate) fn aggregate(ctx: &Ctx, iri: &str, vals: &[std::result::Result<Id, ()>]) -> Id {
    if let Some(arq) = Arq::of(iri) {
        return arq_aggregate(ctx, arq, vals);
    }
    #[cfg(feature = "geo")]
    if let Some(id) = iri
        .strip_prefix(GEOF)
        .and_then(|local| crate::geo::aggregates::evaluate(ctx, local, vals))
    {
        return id;
    }
    let _ = (ctx, iri, vals);
    Id::UNDEF
}

/// A value as a double, as ARQ's `NodeValue.getDouble`; `None` for a value that is not a
/// number (and for unbound).
fn double_of(ctx: &Ctx, id: Id) -> Option<f64> {
    match id.tag() {
        Tag::Int => Some(id.as_i64() as f64),
        Tag::Double => Some(id.as_f64()),
        Tag::Undef => None,
        _ => Some(f64::from(Num::of(&ctx.value(id)?).ok()?.to_double())),
    }
}

/// `x` as an `xsd:decimal`, as ARQ's `NodeValue.makeDecimal(double)` (the shortest
/// decimal that reads back as `x`); unbound for NaN, the infinities and a value out of
/// the decimal range.
fn decimal_id(ctx: &Ctx, x: f64) -> Id {
    if !x.is_finite() {
        return Id::UNDEF;
    }
    let d = Decimal::from_str(&format!("{x}"))
        .ok()
        .or_else(|| Decimal::try_from(Double::from(x)).ok());
    d.map_or(Id::UNDEF, |d| ctx.intern_value(&Value::Decimal(d)))
}

fn arq_aggregate(ctx: &Ctx, arq: Arq, vals: &[std::result::Result<Id, ()>]) -> Id {
    if let Arq::Stat { .. } = arq {
        let mut acc = StatAcc::default();
        for v in vals {
            acc.add(ctx, v.ok());
        }
        return acc.finish(ctx, arq);
    }
    let mut xs = Vec::with_capacity(vals.len());
    for v in vals {
        match v.ok().and_then(|id| double_of(ctx, id)) {
            Some(x) => xs.push(x),
            None => return Id::UNDEF,
        }
    }
    if xs.is_empty() {
        return Id::from_i64(0).unwrap_or(Id::UNDEF);
    }
    match arq {
        Arq::Median => {
            xs.sort_unstable_by(f64::total_cmp);
            let n = xs.len();
            let m = if n % 2 == 1 {
                xs[n / 2]
            } else {
                (xs[n / 2] + xs[n / 2 - 1]) / 2.0
            };
            decimal_id(ctx, m)
        }
        Arq::Mode => decimal_id(ctx, mode(&xs)),
        Arq::Stat { .. } => unreachable!(),
    }
}

/// The most frequent value; among equally frequent values the one that reached that
/// count first. Values are equal as Java's `Double.equals` has them (all NaNs equal,
/// `0.0` and `-0.0` different).
fn mode(xs: &[f64]) -> f64 {
    let key = |x: f64| {
        if x.is_nan() {
            f64::NAN.to_bits()
        } else {
            x.to_bits()
        }
    };
    let mut counts: FxHashMap<u64, u64> = FxHashMap::default();
    let (mut best, mut max) = (xs[0], 0);
    for &x in xs {
        let c = counts.entry(key(x)).or_insert(0);
        *c += 1;
        if *c > max {
            max = *c;
            best = x;
        }
    }
    best
}

/// The running state of ARQ's variance and deviation aggregates: the sums of the values
/// and of their squares, shifted by the first value (ARQ's `AccStatBase`).
#[derive(Default, Clone, Debug)]
pub(crate) struct StatAcc {
    count: u64,
    k: f64,
    sum: f64,
    sum_sq: f64,
    error: bool,
}

impl StatAcc {
    /// Add one row's value: `None` (an expression error) or a value that is not a number
    /// makes the aggregate an error.
    #[inline]
    pub(crate) fn add(&mut self, ctx: &Ctx, id: Option<Id>) {
        if self.error {
            return;
        }
        let Some(d) = id.and_then(|id| double_of(ctx, id)) else {
            self.error = true;
            return;
        };
        self.count += 1;
        if self.count == 1 {
            self.k = d;
            self.sum = d - self.k;
            self.sum_sq = (d - self.k) * (d - self.k);
        } else {
            let dk = d - self.k;
            self.sum += dk;
            self.sum_sq += dk * dk;
        }
    }

    pub(crate) fn finish(&self, ctx: &Ctx, arq: Arq) -> Id {
        let Arq::Stat { population, root } = arq else {
            return Id::UNDEF;
        };
        if self.error || self.count == 0 {
            return Id::UNDEF;
        }
        let n = self.count;
        let n1 = if population { n } else { n - 1 };
        if n1 == 0 {
            return Id::UNDEF;
        }
        let var = (self.sum_sq - (self.sum * self.sum) / n as f64) / n1 as f64;
        let x = if root { var.sqrt() } else { var };
        ctx.intern_value(&Value::Double(x.into()))
    }
}
