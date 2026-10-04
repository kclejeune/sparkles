//! FILTER selectivity measured on a sample of the index.
//!
//! Without better knowledge the planner assumes that a FILTER conjunct keeps 30% of its
//! input. A conjunct whose variables are all bound by one triple pattern of its group is
//! instead tested on a sample of that pattern's rows, and the share of the sample it
//! keeps becomes its selectivity. The sample is read from the sorted index:
//!
//! * A pattern of at most [`WHOLE_RANGE`] rows, or in at most [`SAMPLE_BLOCKS`] blocks
//!   ([`SORTED_BLOCKS`] when it is sorted on the conjunct's variable, see below), is
//!   read whole with the snapshot's delta merged in, and [`SAMPLE_ROWS`] rows spread over
//!   it are tested.
//! * A larger pattern is sampled per block. The first and last keys of its blocks, which
//!   the block metadata holds in memory, cost nothing to read. When they are fewer than
//!   half of [`SAMPLE_ROWS`], one or two blocks are decoded as well, preferably ones the
//!   block cache holds, and give [`DECODED_ROWS`] rows each. The delta is left out, which
//!   only matters for an estimate when it is large next to the base.
//!
//! Rows are taken at a position within each equal share of a range that a hash picks, so
//! that a sample does not fall into step with values that repeat at a fixed distance.
//!
//! The pattern is read from a permutation sorted on another variable when it has one.
//! Its rows then hold the conjunct's values in no particular order, and every sampled row
//! counts the same. A pattern sorted on the conjunct's variable holds a narrow slice of
//! its values per block, so each block's rows count in proportion to the block's size. A
//! conjunct that keeps no sampled row is taken to keep half a row of the sample.
//!
//! Selectivities are kept with the snapshot, under the pattern and the conjunct's text,
//! so later queries at the same commit reuse them. Within one query they are kept by the
//! conjunct's text (see [`Ctx::sampled`]), so that every plan the join ordering compares
//! sees the same estimate for a conjunct.

use super::ctx::Ctx;
use super::expr::Expr;
use super::plan::{Kind, Node, ScanSpec};
use super::table::{Table, VarId};
use crate::error::Result;
use crate::id::Id;
use crate::index::{ColMask, Key, bound_cols, pad};
use crate::store::{Chunk, Snapshot};

/// A FILTER conjunct's selectivity measured on a sample of `rows` rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampled {
    pub sel: f64,
    pub rows: u32,
}

/// Rows a sample of a small pattern tests at most.
const SAMPLE_ROWS: usize = 256;
/// Blocks a sample decodes at most, except a sorted pattern read whole (see
/// [`SORTED_BLOCKS`]). Decoding a column of a block takes 150 to 250 µs when its pages are
/// in memory, as long as planning a typical query.
const SAMPLE_BLOCKS: usize = 2;
/// A pattern sorted on the filtered variable in at most this many blocks is read whole:
/// its blocks hold narrow slices of the values, which their first and last keys alone
/// would place too coarsely.
const SORTED_BLOCKS: usize = 4;
/// The blocks of a larger pattern whose first and last keys, which the block metadata
/// holds in memory, join its sample, at most.
const STRATA: usize = SAMPLE_ROWS / 2;
/// Rows taken from each decoded block of a larger pattern.
const DECODED_ROWS: usize = 128;
/// A pattern with at most this many rows is read whole.
const WHOLE_RANGE: u64 = 4096;

/// Measure the selectivity of the conjuncts in `filters` that one triple pattern of the
/// group binds (`leaves` are the patterns' access paths), keeping it in the query's
/// context for the planner.
pub(super) fn prepare(ctx: &Ctx, leaves: &[Vec<Node>], filters: &[Expr]) {
    for f in filters {
        if f.has_exists() {
            continue;
        }
        let vars = f.var_set();
        if vars.is_empty() {
            continue;
        }
        let text = f.display(ctx);
        if ctx.sampled(&text).is_some() {
            continue;
        }
        // the smallest pattern binding every variable of the conjunct
        let Some(opts) = leaves
            .iter()
            .filter(|l| {
                matches!(l[0].kind, Kind::Scan(_)) && vars.iter().all(|v| l[0].vars.contains(v))
            })
            .min_by(|a, b| a[0].est.total_cmp(&b[0].est))
        else {
            continue;
        };
        // read sorted on another variable when the pattern has one
        let spec = opts
            .iter()
            .filter_map(|o| match &o.kind {
                Kind::Scan(s) => Some(s),
                _ => None,
            })
            .min_by_key(|s| s.cols.first().is_some_and(|(_, v)| vars.contains(v)));
        let Some(spec) = spec else { continue };
        match selectivity(ctx, spec, f, &vars, &text) {
            Ok(Some(s)) => ctx.set_sampled(text, s),
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(target: "sparkles::sparql::sample", "no sampled selectivity for {text}: {e}")
            }
        }
    }
}

/// The selectivity of `f` (with variables `vars`, shown as `text`) on a sample of the
/// rows of `spec`, from the snapshot's cache or measured now.
fn selectivity(
    ctx: &Ctx,
    spec: &ScanSpec,
    f: &Expr,
    vars: &[VarId],
    text: &str,
) -> Result<Option<Sampled>> {
    let mut cols = Vec::with_capacity(vars.len());
    for v in vars {
        match spec.cols.iter().find(|(_, x)| x == v) {
            Some(&(kc, _)) => cols.push((kc, *v)),
            None => return Ok(None),
        }
    }
    let key = format!(
        "{}|{:?}|{:?}|{:?}|{}|{}",
        spec.perm.name(),
        spec.prefix,
        spec.eqs,
        spec.graph,
        cols.iter()
            .map(|(kc, v)| format!("{kc}={}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(","),
        text
    );
    if let Some(s) = ctx.snap.counts.sampled(&key) {
        return Ok(Some(s));
    }
    let mut mask: ColMask = 0;
    for &(kc, _) in &cols {
        mask |= 1 << kc;
    }
    for &(a, b) in &spec.eqs {
        mask |= 1 << a | 1 << b;
    }
    if spec.graph != super::plan::GraphFilter::All {
        mask |= 1 << spec.graph_col;
    }
    // a scan sorted on another variable holds the conjunct's values in no particular
    // order, so every sampled row counts the same; one sorted on them holds a narrow
    // slice of them per block, so each block counts by its rows
    let pooled = spec.cols.first().is_some_and(|(_, v)| !vars.contains(v));
    let rows = sample(&ctx.snap, spec, mask, pooled)?;
    let rows: Vec<(Key, f64)> = rows
        .into_iter()
        .filter(|(k, _)| {
            spec.graph.accepts(k[spec.graph_col]) && spec.eqs.iter().all(|&(a, b)| k[a] == k[b])
        })
        .collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let pass = test(ctx, &rows, &cols, f)?;
    let total: f64 = rows.iter().map(|(_, w)| w).sum();
    let kept: f64 = rows
        .iter()
        .zip(&pass)
        .filter(|(_, p)| **p)
        .map(|((_, w), _)| w)
        .sum();
    let n = rows.len();
    let sel = if kept > 0.0 {
        (kept / total).min(1.0)
    } else {
        0.5 / n as f64
    };
    let s = Sampled {
        sel,
        rows: n as u32,
    };
    ctx.snap.counts.set_sampled(key, s);
    Ok(Some(s))
}

/// Whether each sampled row passes `f`, whose variables are in the key columns `cols`.
fn test(ctx: &Ctx, rows: &[(Key, f64)], cols: &[(usize, VarId)], f: &Expr) -> Result<Vec<bool>> {
    let exprs = std::slice::from_ref(f);
    if let [(kc, v)] = cols {
        // once per distinct value, on vocabulary keys where the conjunct allows it
        let mut uniq: Vec<Id> = rows.iter().map(|(k, _)| Id(k[*kc])).collect();
        uniq.sort_unstable();
        uniq.dedup();
        let (hit, _) = super::exprcache::filter_values(ctx, &uniq, *v, exprs)?;
        return Ok(rows
            .iter()
            .map(|(k, _)| hit[uniq.binary_search(&Id(k[*kc])).unwrap_or(0)])
            .collect());
    }
    let mut t = Table::new(cols.iter().map(|(_, v)| *v).collect());
    for (k, _) in rows {
        let row: Vec<Id> = cols.iter().map(|(kc, _)| Id(k[*kc])).collect();
        t.push_row(&row);
    }
    super::exec::filter_mask(ctx, &t, exprs)
}

/// Rows of the scan `spec` spread over its range, each with the number of rows it stands
/// for, or with the same weight when `pooled`. Only the key columns in `mask` (and those
/// of the range) are certain to be read; the others may read as 0.
fn sample(
    snap: &Snapshot,
    spec: &ScanSpec,
    mask: ColMask,
    pooled: bool,
) -> Result<Vec<(Key, f64)>> {
    let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
    let base = snap.perm(spec.perm);
    let (b0, b1) = base.key_block_range(&lo, &hi);
    let read_whole = if pooled { SAMPLE_BLOCKS } else { SORTED_BLOCKS };
    if b1 - b0 <= read_whole || snap.estimate(spec.perm, &spec.prefix) <= WHOLE_RANGE {
        return whole(snap, spec, mask | bound_cols(&lo, &hi));
    }
    // the strata: every block of the range, or STRATA of them evenly spread
    let nb = b1 - b0;
    let strata: Vec<usize> = if nb <= STRATA {
        (b0..b1).collect()
    } else {
        (0..STRATA)
            .map(|i| b0 + (2 * i + 1) * nb / (2 * STRATA))
            .collect()
    };
    let scale = nb as f64 / strata.len() as f64;
    let inside = |k: &Key| *k >= lo && *k <= hi;
    let free = strata
        .iter()
        .map(|&b| {
            let m = &base.blocks[b];
            usize::from(inside(&m.first)) + usize::from(inside(&m.last) && m.last != m.first)
        })
        .sum::<usize>();
    // blocks inside the range to decode, preferring ones the cache holds
    let want = if free >= SAMPLE_ROWS / 2 {
        0
    } else if free >= SAMPLE_ROWS / 4 {
        1
    } else {
        SAMPLE_BLOCKS
    };
    let interior: Vec<usize> = strata
        .iter()
        .copied()
        .filter(|&b| inside(&base.blocks[b].first) && inside(&base.blocks[b].last))
        .collect();
    let mut decode: Vec<usize> = interior
        .iter()
        .copied()
        .filter(|&b| snap.cache.has_cols(base, b, mask))
        .take(want)
        .collect();
    for i in 0..want {
        if decode.len() >= want || interior.is_empty() {
            break;
        }
        let b = interior[(2 * i + 1) * interior.len() / (2 * want)];
        if !decode.contains(&b) {
            decode.push(b);
        }
    }
    let mut out = Vec::with_capacity(free + decode.len() * DECODED_ROWS);
    for &b in &strata {
        let m = &base.blocks[b];
        // a block at the range's ends holds some rows outside it
        let rows = if inside(&m.first) && inside(&m.last) {
            m.rows as f64
        } else {
            m.rows as f64 / 2.0
        } * scale;
        let weight = |n: usize| if pooled { 1.0 } else { rows / n as f64 };
        if decode.contains(&b) {
            let blk = snap.cache.get_cols(base, b, mask)?;
            let k = DECODED_ROWS.min(blk.len());
            for i in 0..k {
                out.push((blk.key(spot(i, k, blk.len(), b as u64)), weight(k)));
            }
            continue;
        }
        let keys: Vec<Key> = [m.first, m.last]
            .into_iter()
            .filter(inside)
            .collect::<Vec<_>>();
        let keys = if keys.len() == 2 && keys[0] == keys[1] {
            &keys[..1]
        } else {
            &keys[..]
        };
        for &k in keys {
            out.push((k, weight(keys.len())));
        }
    }
    Ok(out)
}

/// The row of `n` that stands for the `i`-th of `k` equal shares of them: a position in
/// the share picked by a hash of `i` and `seed`, so that a sample does not fall into step
/// with values that repeat at a fixed distance, yet is the same in every run.
fn spot(i: usize, k: usize, n: usize, seed: u64) -> usize {
    let (lo, hi) = (i * n / k, (i + 1) * n / k);
    let mut x = (seed << 32 ^ i as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    lo + (x % (hi - lo).max(1) as u64) as usize
}

/// Up to [`SAMPLE_ROWS`] rows of a small scan spread over it, read whole with the delta.
fn whole(snap: &Snapshot, spec: &ScanSpec, mask: ColMask) -> Result<Vec<(Key, f64)>> {
    let n = snap.count(spec.perm, &spec.prefix)? as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    let k = SAMPLE_ROWS.min(n);
    let w = n as f64 / k as f64;
    let pos = |i: usize| spot(i, k, n, 0);
    let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
    let mut out = Vec::with_capacity(k);
    let mut row = 0;
    snap.scan_between_cols(spec.perm, lo, hi, mask, |c| {
        match c {
            Chunk::Block(b, s, e) => {
                while out.len() < k && pos(out.len()) < row + (e - s) {
                    out.push((b.key(s + pos(out.len()) - row), w));
                }
                row += e - s;
            }
            Chunk::Row(key) => {
                if out.len() < k && pos(out.len()) == row {
                    out.push((key, w));
                }
                row += 1;
            }
        }
        Ok(out.len() < k)
    })?;
    Ok(out)
}

/// The planner's selectivity of FILTER conjuncts: the product of the sampled ones, and
/// [`FILTER_SELECTIVITY`](super::plan::FILTER_SELECTIVITY) for each of the others.
pub(super) fn combine(sampled: impl IntoIterator<Item = Option<f64>>) -> f64 {
    let (mut p, mut n) = (1.0, 0);
    for s in sampled {
        match s {
            Some(s) => p *= s,
            None => n += 1,
        }
    }
    p * super::plan::FILTER_SELECTIVITY.powi(n)
}

/// A note for EXPLAIN on the selectivity of conjuncts of which at least one was sampled.
pub(super) fn note(sampled: &[Option<Sampled>]) -> Option<String> {
    if sampled.iter().all(Option::is_none) {
        return None;
    }
    let parts: Vec<String> = sampled
        .iter()
        .map(|s| match s {
            Some(s) => format!("{} of {} sampled rows", round(s.sel), s.rows),
            None => format!("{} assumed", super::plan::FILTER_SELECTIVITY),
        })
        .collect();
    Some(format!("[selectivity {}]", parts.join(" × ")))
}

/// A selectivity with three significant digits.
fn round(x: f64) -> String {
    if x >= 0.1 || x <= 0.0 {
        format!("{x:.3}")
    } else {
        let digits = (-x.log10()).ceil() as usize + 2;
        format!("{x:.digits$}")
    }
}
