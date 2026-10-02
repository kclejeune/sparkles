//! Join estimates from probing a sample of the keys of a small input.
//!
//! When one input of a group is small, a VALUES table or a triple pattern of few rows,
//! the values its join variable takes can be read at planning time. A sample of them is
//! looked up in each triple pattern of the group that binds the variable, by an exact
//! count of the pattern's rows for that value, as an index join would read them. The
//! mean count per sampled row of the small input is the number of rows of the pattern
//! that join each of its rows, and the share of sampled values with a count above zero is
//! the share of its values that the pattern holds. Both replace the assumptions of the
//! estimate from distinct values: that every value of the side with fewer distinct values
//! is on the other side (less QLever's correction of 0.7), and that each has the pattern's
//! average number of rows. This is index-based join sampling as Leis et al. describe it
//! ("Cardinality Estimation Done Right: Index-Based Join Sampling", CIDR 2017), from one
//! small input.
//!
//! A join on the variable of a plan holding the small input with a plan holding one other
//! pattern on the variable that was probed is estimated as the rows of the first, times
//! the mean count, times the share of the probed pattern's rows that the second plan
//! keeps. Whatever else restricts either plan is taken to be independent of the values
//! the small input holds. When the small input is itself a pattern the characteristic
//! sets count, a join that the sets estimate keeps their estimate, since they hold how
//! the predicates of a star occur together where a probe sees one pattern. Otherwise the
//! small input's values are a subset the sets know nothing of, and the probes come
//! first. Other joins keep the estimate from distinct values.
//!
//! The values are counted in order, so each block of a pattern is read at most once, and
//! blocks wholly inside one value's range are counted from their metadata. The blocks to
//! read that the block cache does not hold count against a budget of [`DECODES`] per
//! group, so that planning decodes few blocks; a pattern that would exceed it is not
//! probed. A small pattern whose blocks the cache does not hold is not read for its
//! values at all: the query that plans with the other estimates reads them, and later
//! queries probe. Measurements are kept with the snapshot, so later queries at the same
//! commit reuse them.

use super::ctx::Ctx;
use super::plan::{Kind, Node, ScanSpec};
use super::table::{Table, VarId};
use crate::error::Result;
use crate::id::Id;
use crate::index::{Key, Perm, bound_cols, pad};
use crate::store::{Chunk, Delta, Snapshot};
use std::hash::{Hash, Hasher};

/// A triple pattern of at most this many rows is a small input.
const SOURCE_ROWS: f64 = 1024.0;
/// Rows of the small input whose values are probed at most.
const KEYS: usize = 32;
/// Blocks outside the block cache that the probes of a group may decode, at about 50 to
/// 200 µs each.
const DECODES: usize = 2;

/// What probing a pattern for the values of a small input measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    /// mean rows of the pattern per row of the small input
    pub m: f64,
    /// share of the small input's rows whose value the pattern holds
    pub f: f64,
    /// the pattern's rows and distinct values of the variable, as the planner estimates
    /// them
    pub rows: f64,
    pub d: f64,
}

/// A small input of a group: its signature, for recognizing it in plans.
#[derive(Clone, Debug, PartialEq)]
enum Sig {
    /// a scan of a triple pattern, by its access paths
    Scan(Vec<ScanSig>),
    /// a VALUES table: its rows and three values of the variable's column
    Values(usize, [u64; 3]),
}

type ScanSig = (Perm, Vec<u64>, Vec<(usize, VarId)>);

fn scan_sig(s: &ScanSpec) -> ScanSig {
    (s.perm, s.prefix.clone(), s.cols.clone())
}

fn values_sig(t: &Table, v: VarId) -> Option<(usize, [u64; 3])> {
    let c = t.col_of(v)?;
    let col = &t.cols[c];
    if col.is_empty() {
        return None;
    }
    Some((
        col.len(),
        [col[0].0, col[col.len() / 2].0, col[col.len() - 1].0],
    ))
}

/// The small input of one variable and the patterns probed with its values.
pub(super) struct VarProbe {
    source: Sig,
    /// the small input is not a star pattern on the variable, so its values are a subset
    /// the characteristic sets know nothing of, and probes come before the sets
    first: bool,
    /// the probed patterns, by their access paths
    targets: Vec<(Vec<ScanSig>, Measure)>,
}

/// How a plan holds the leaves on a probed variable.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Side {
    /// it holds the small input
    pub source: bool,
    /// the probed pattern it holds, if one
    pub target: Option<u32>,
    /// it holds another leaf on the variable, or more than one probed pattern
    pub other: bool,
}

impl Side {
    pub(super) fn merge(self, o: Side) -> Side {
        Side {
            source: self.source || o.source,
            target: self.target.or(o.target),
            other: self.other || o.other || (self.target.is_some() && o.target.is_some()),
        }
    }
}

/// Find the small input of each variable that several inputs of a group bind, and probe
/// the group's other patterns on the variable with a sample of its values.
pub(super) fn prepare(ctx: &Ctx, leaves: &[Vec<Node>]) {
    let mut vars: Vec<VarId> = leaves.iter().flat_map(|l| l[0].vars.clone()).collect();
    vars.sort_unstable();
    let shared: Vec<VarId> = vars
        .chunk_by(|a, b| a == b)
        .filter(|c| c.len() > 1)
        .map(|c| c[0])
        .collect();
    let mut budget = DECODES;
    for v in shared {
        if ctx.probes.lock().contains_key(&v) {
            continue;
        }
        match probe_var(ctx, leaves, v, &mut budget) {
            Ok(Some(p)) => {
                ctx.probes.lock().insert(v, p);
            }
            Ok(None) => {}
            Err(e) => tracing::debug!("no key probes for ?{}: {e}", ctx.var_name(v)),
        }
    }
}

/// The values of `v` in the small input `src`, with their rows: from a VALUES table or
/// read from the pattern's scan.
fn source_values(ctx: &Ctx, src: &Node, v: VarId) -> Result<Option<Vec<Id>>> {
    let mut vals: Vec<Id> = match &src.kind {
        Kind::Values(t) => {
            let Some(c) = t.col_of(v) else {
                return Ok(None);
            };
            t.cols[c]
                .iter()
                .copied()
                .filter(|id| !id.is_undef())
                .collect()
        }
        Kind::Scan(spec) => {
            let Some(&(kc, _)) = spec.cols.iter().find(|(_, x)| *x == v) else {
                return Ok(None);
            };
            let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
            let mut mask = bound_cols(&lo, &hi) | 1 << kc;
            for &(a, b) in &spec.eqs {
                mask |= 1 << a | 1 << b;
            }
            mask |= 1 << spec.graph_col;
            // a pattern whose blocks are not cached yet is left to a later query, after
            // this one has read them
            let base = ctx.snap.perm(spec.perm);
            let (b0, b1) = base.key_block_range(&lo, &hi);
            if (b0..b1).any(|b| !ctx.snap.cache.has_cols(base, b, mask)) {
                return Ok(None);
            }
            let limit = 4 * SOURCE_ROWS as usize;
            let mut out = Vec::new();
            let keep = |k: &Key| {
                spec.graph.accepts(k[spec.graph_col]) && spec.eqs.iter().all(|&(a, b)| k[a] == k[b])
            };
            ctx.snap.scan_between_cols(spec.perm, lo, hi, mask, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            let k = b.key(i);
                            if keep(&k) {
                                out.push(Id(k[kc]));
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if keep(&k) {
                            out.push(Id(k[kc]));
                        }
                    }
                }
                Ok(out.len() < limit)
            })?;
            out
        }
        _ => return Ok(None),
    };
    if vals.is_empty() {
        return Ok(None);
    }
    vals.sort_unstable();
    Ok(Some(vals))
}

fn probe_var(
    ctx: &Ctx,
    leaves: &[Vec<Node>],
    v: VarId,
    budget: &mut usize,
) -> Result<Option<VarProbe>> {
    // the small input: the VALUES table or the pattern of fewest rows that always binds v
    let small = leaves
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let n = &l[0];
            n.certain.contains(&v)
                && match &n.kind {
                    Kind::Values(_) => true,
                    Kind::Scan(_) => n.est <= SOURCE_ROWS,
                    _ => false,
                }
        })
        .min_by(|a, b| a.1[0].est.total_cmp(&b.1[0].est));
    let Some((si, src)) = small else {
        return Ok(None);
    };
    let src = &src[0];
    let source = match &src.kind {
        Kind::Values(t) => match values_sig(t, v) {
            Some((n, s)) => Sig::Values(n, s),
            None => return Ok(None),
        },
        Kind::Scan(_) => Sig::Scan(
            leaves[si]
                .iter()
                .filter_map(|o| match &o.kind {
                    Kind::Scan(s) => Some(scan_sig(s)),
                    _ => None,
                })
                .collect(),
        ),
        _ => return Ok(None),
    };
    let Some(vals) = source_values(ctx, src, v)? else {
        return Ok(None);
    };
    // rows of the small input spread over its values in order
    let k = KEYS.min(vals.len());
    let keys: Vec<Id> = (0..k)
        .map(|i| vals[(2 * i + 1) * vals.len() / (2 * k)])
        .collect();
    let mut targets = Vec::new();
    for (i, opts) in leaves.iter().enumerate() {
        if i == si || !opts[0].vars.contains(&v) {
            continue;
        }
        // an access path sorted on v, read for one value of it by its key prefix
        let Some(spec) = opts.iter().find_map(|o| match &o.kind {
            Kind::Scan(s) if s.cols.first().is_some_and(|c| c.1 == v) && s.eqs.is_empty() => {
                Some(s)
            }
            _ => None,
        }) else {
            continue;
        };
        if let Some(m) = measure(ctx, spec, &keys, budget)? {
            let sigs = opts
                .iter()
                .filter_map(|o| match &o.kind {
                    Kind::Scan(s) => Some(scan_sig(s)),
                    _ => None,
                })
                .collect();
            let n = &opts[0];
            targets.push((
                sigs,
                Measure {
                    rows: n.est,
                    d: n.d(v),
                    ..m
                },
            ));
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let first = !matches!(&src.kind, Kind::Scan(s) if super::charsets::is_star(ctx, s, v));
    Ok(Some(VarProbe {
        source,
        first,
        targets,
    }))
}

/// Count the rows of the pattern `spec` (sorted on the probed variable) for each of
/// `keys`, within the `budget` of blocks to decode.
fn measure(ctx: &Ctx, spec: &ScanSpec, keys: &[Id], budget: &mut usize) -> Result<Option<Measure>> {
    // the pattern's range and the values probed decide the measure
    let mut h = rustc_hash::FxHasher::default();
    keys.hash(&mut h);
    let key = format!(
        "probe|{}|{:?}|{:?}|{}|{:x}",
        spec.perm.name(),
        spec.prefix,
        spec.graph,
        keys.len(),
        h.finish()
    );
    if let Some(m) = ctx.snap.counts.probed(&key) {
        return Ok(m);
    }
    let Some(counts) = count_keys(&ctx.snap, spec, keys, budget)? else {
        // too many blocks to decode now; they may be cached for a later query
        return Ok(None);
    };
    let n = counts.len() as f64;
    let m = Some(Measure {
        m: counts.iter().sum::<u64>() as f64 / n,
        f: counts.iter().filter(|&&c| c > 0).count() as f64 / n,
        rows: 0.0,
        d: 0.0,
    });
    ctx.snap.counts.set_probed(key, m);
    Ok(m)
}

/// The rows of the pattern `spec` (sorted on the probed variable) for each of the sorted
/// `keys`, reading each block of the pattern at most once and counting the blocks wholly
/// inside a key's range from their metadata. `None` when the blocks to read that the
/// cache does not hold are more than `budget`, which the ones read are taken from.
fn count_keys(
    snap: &Snapshot,
    spec: &ScanSpec,
    keys: &[Id],
    budget: &mut usize,
) -> Result<Option<Vec<u64>>> {
    let base = snap.perm(spec.perm);
    let pi = spec.perm.index();
    let mut prefix = spec.prefix.clone();
    prefix.push(0);
    let delta = Delta::range(&snap.delta.ins[pi], &spec.prefix)
        .next()
        .is_some()
        || Delta::range(&snap.delta.del[pi], &spec.prefix)
            .next()
            .is_some();
    let mut cur: Option<(usize, crate::index::Block)> = None;
    let mut out = Vec::with_capacity(keys.len());
    for &k in keys {
        *prefix.last_mut().unwrap() = k.0;
        let (lo, hi) = (pad(&prefix, 0), pad(&prefix, u64::MAX));
        let (b0, b1) = base.key_block_range(&lo, &hi);
        let mask = bound_cols(&lo, &hi);
        let mut c = 0u64;
        for b in b0..b1 {
            let m = &base.blocks[b];
            if m.first >= lo && m.last <= hi {
                c += m.rows as u64;
                continue;
            }
            if cur.as_ref().is_none_or(|x| x.0 != b) {
                if !snap.cache.has_cols(base, b, mask) {
                    if *budget == 0 {
                        return Ok(None);
                    }
                    *budget -= 1;
                }
                cur = Some((b, snap.cache.get_cols(base, b, mask)?));
            }
            let blk = &cur.as_ref().unwrap().1;
            let (s, e) = blk.key_range(&lo, &hi);
            c += (e - s) as u64;
        }
        if delta {
            // the updates since the base was built, counted exactly
            c = snap.count(spec.perm, &prefix)?;
        }
        out.push(c);
    }
    Ok(Some(out))
}

/// How plan `n` holds the leaves on `v`, the variable of `p`.
fn side_of(p: &VarProbe, n: &Node, v: VarId, out: &mut Side) {
    let leaf = |spec: &ScanSpec, out: &mut Side| {
        if !spec.cols.iter().any(|c| c.1 == v) {
            return;
        }
        let sig = scan_sig(spec);
        if matches!(&p.source, Sig::Scan(s) if s.contains(&sig)) {
            out.source = true;
        } else if let Some(t) = p.targets.iter().position(|(s, _)| s.contains(&sig)) {
            *out = out.merge(Side {
                target: Some(t as u32),
                ..Default::default()
            });
        } else {
            out.other = true;
        }
    };
    match &n.kind {
        Kind::Scan(spec) | Kind::RangeScan(spec, _) => leaf(spec, out),
        Kind::SpatialScan(s) => leaf(&s.scan, out),
        Kind::Values(t) => {
            if t.vars.contains(&v) {
                match (&p.source, values_sig(t, v)) {
                    (Sig::Values(a, b), Some((c, d))) if *a == c && *b == d => out.source = true,
                    _ => out.other = true,
                }
            }
        }
        Kind::IndexJoin(j) => {
            for pr in &j.probes {
                leaf(&pr.scan, out);
            }
            side_of(p, &n.children[0], v, out);
        }
        Kind::Join { .. } | Kind::Filter(_) | Kind::Sort(_) => {
            for c in &n.children {
                side_of(p, c, v, out);
            }
        }
        _ => {
            if n.vars.contains(&v) {
                out.other = true;
            }
        }
    }
}

/// Whether joins on `v` are estimated from probes before characteristic sets: when its
/// small input is a VALUES table or a pattern that the sets do not count (a constant
/// object, or `v` not its subject).
pub(super) fn first(ctx: &Ctx, v: VarId) -> bool {
    ctx.probes.lock().get(&v).is_some_and(|p| p.first)
}

/// How plan `n` holds the leaves on `v`, when `v` was probed.
pub(super) fn side(ctx: &Ctx, n: &Node, v: VarId) -> Option<Side> {
    let probes = ctx.probes.lock();
    let p = probes.get(&v)?;
    let mut s = Side::default();
    side_of(p, n, v, &mut s);
    Some(s)
}

/// The measurement for a join on `v` of plans holding the leaves `a` and `b`: when one
/// holds the small input and the other one probed pattern and no other leaf on `v`, the
/// pattern's measure and whether `a` is the side with the small input.
pub(super) fn applies(ctx: &Ctx, v: VarId, a: Side, b: Side) -> Option<(Measure, bool)> {
    let (src, tgt, a_src) = match (a.source, b.source) {
        (true, false) => (a, b, true),
        (false, true) => (b, a, false),
        _ => return None,
    };
    let _ = src;
    if tgt.other {
        return None;
    }
    let t = tgt.target?;
    let probes = ctx.probes.lock();
    Some((probes.get(&v)?.targets[t as usize].1, a_src))
}

/// The estimated rows of a join of `s_est` rows holding the small input with `t_est`
/// rows holding a probed pattern measured as `m`.
#[inline]
pub(super) fn est(s_est: f64, t_est: f64, m: &Measure) -> f64 {
    let e = s_est * m.m * (t_est / m.rows.max(1.0));
    e.max(if s_est > 0.0 && t_est > 0.0 { 1.0 } else { 0.0 })
}

/// The distinct values of the join variable after such a join, from `ds` and `dt`.
#[inline]
pub(super) fn distinct(ds: f64, dt: f64, m: &Measure) -> f64 {
    (ds * m.f * (dt / m.d.max(1.0)).min(1.0)).min(ds).min(dt)
}
