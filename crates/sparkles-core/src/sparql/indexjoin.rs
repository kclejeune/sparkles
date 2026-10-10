//! Batched index joins and fused subject stars.
//!
//! An index join reads a triple pattern only where its key variable takes a value of the
//! input. The input's distinct keys, sorted, become key ranges of a permutation sorted
//! on the key, and ranges whose blocks are adjacent are read by one scan: a coordinated
//! seek per cluster of keys, which for dense keys is one sweep over the pattern. Each
//! input row is then joined with the rows of its key, so the input's order and its
//! duplicates are kept. The planner offers it next to the merge and hash joins and costs
//! it per key, per block touched and per row read (see the cost model below).
//!
//! Consecutive index joins on one subject variable whose patterns have constant
//! predicates (a star) are fused into one operator. It reads the keys once and, per
//! query, either walks each subject's run in SPO once, picking out the star's
//! predicates, or probes every pattern's own permutation, whichever touches fewer
//! blocks; the output is formed once, without the chain's intermediate tables.
//!
//! Every read goes through the snapshot's scans, so the delta, graph filters, the union
//! default graph and past snapshots behave exactly as in a plain scan.

use super::ctx::Ctx;
use super::expr::Expr;
use super::plan::{GraphFilter, Kind, Node, ScanSpec};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{ALL_COLS, BLOCK_ROWS, Block, ColMask, G, Key, O, P, Perm, S, bound_cols, pad};
use crate::store::Chunk;
use std::ops::Bound;

/// An index join: `children[0]` is the input, `probes` are read for its keys.
#[derive(Clone)]
pub struct IndexJoinSpec {
    /// the input's variable whose values select the rows of the patterns (always bound
    /// in the input)
    pub key: VarId,
    /// the patterns read per key; several make a fused star, joined left to right
    pub probes: Vec<Probe>,
}

/// One pattern of an index join.
#[derive(Clone)]
pub struct Probe {
    /// the pattern's scan, with the key as its first free column
    pub scan: ScanSpec,
    /// FILTER conjuncts over the pattern's variables, tested on its rows
    pub filter: Vec<Expr>,
    /// the pattern as part of a subject star (key subject, constant predicate)
    pub star: Option<StarPattern>,
}

/// A star pattern `?key <p> o` as read from the subject's SPO run.
#[derive(Clone, Debug, PartialEq)]
pub struct StarPattern {
    pub p: u64,
    /// a constant object (otherwise the object is a variable)
    pub o: Option<u64>,
    pub graph: GraphFilter,
    /// drop rows that differ only by graph (merged default graph)
    pub dedup: bool,
}

// ------------------------------------------------------------------------------
// cost model
// ------------------------------------------------------------------------------

// Costs are in the planner's unit, one row read by a scan. They were measured on warm
// stores of 1.05M and 10.5M triples (the ignored tests in `costcal_tests`), where a scan
// reads a row in 1.3 to 1.6 ns and a probe spends 160 to 270 ns per key.

/// Finding one key's rows: its range, the binary searches in its block and the spans of
/// the key in the rows read.
const KEY_COST: f64 = 140.0;
/// Added to a key's cost for each doubling of the pattern's rows per input key: keys far
/// apart search farther and miss the CPU caches more often.
const KEY_GAP_COST: f64 = 6.0;
/// The same with the galloping reader, which finds a key from the one before in the
/// block it already holds: 25 to 40 ns per key plus 3.5 to 11.5 ns per doubling of the
/// rows per key, where a scan reads a row in 2.2 ns (and the binary searches took 200 to
/// 280 ns per key), fitted on keys spread at random over patterns of 40k to 2.5M rows.
/// Keys close together cost less (35 to 90 ns at any count), which the estimate, knowing
/// only how many keys there are, does not see.
const GALLOP_KEY_COST: f64 = 15.0;
const GALLOP_KEY_GAP_COST: f64 = 3.5;
/// A block the keys touch: fetching it from the block cache and finding the first key.
const BLOCK_COST: f64 = 256.0;
/// A row read: copied out of its block, then joined with the input rows of its key, 25 to
/// 50 ns with its output row. Merge and hash joins count their output rows too, so this
/// is what probing adds per row, taken at the low end because the rows read are estimated
/// from the pattern's average rows per key.
const ROW_COST: f64 = 8.0;
/// Probing is offered when it costs less than this many times scanning the pattern, which
/// is about what reading it whole and merging its rows costs.
const PROBE_LIMIT: f64 = 2.0;
/// Starting one scan of a cluster of keys, and decoding a block, in rows read: these
/// decide how a fused star is read.
const SEEK_COST: f64 = 64.0;
const BLOCK_DECODE: f64 = (BLOCK_ROWS / 8) as f64;

#[cfg(test)]
thread_local! {
    /// tests: offer an index join whenever it applies, whatever it costs
    pub(crate) static FORCE_INDEX_JOIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// tests: read fused stars by subject walks (`Some(true)`) or per pattern
    pub(crate) static FORCE_WALK: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

fn forced() -> bool {
    #[cfg(test)]
    return FORCE_INDEX_JOIN.with(|f| f.get());
    #[cfg(not(test))]
    false
}

fn forced_walk() -> Option<bool> {
    #[cfg(test)]
    return FORCE_WALK.with(|f| f.get());
    #[cfg(not(test))]
    None
}

/// Estimated cost of probing a pattern of `rows` rows for `keys` distinct keys with
/// `per_key` rows each, and the share of the pattern's rows it reads.
fn probe_cost(keys: f64, rows: f64, per_key: f64, gallop: bool) -> (f64, f64) {
    let blocks = (rows / BLOCK_ROWS as f64).ceil().max(1.0);
    // blocks holding at least one of `keys` keys spread over the pattern
    let touched = blocks * (1.0 - (1.0 - 1.0 / blocks).powf(keys));
    let read = (keys * per_key).min(rows);
    let gap = (rows / keys).max(1.0);
    let (key, key_gap) = if gallop {
        (GALLOP_KEY_COST, GALLOP_KEY_GAP_COST)
    } else {
        (KEY_COST, KEY_GAP_COST)
    };
    let probe = keys * (key + key_gap * gap.log2()) + touched * BLOCK_COST + read * ROW_COST;
    (probe, if rows > 0.0 { read / rows } else { 0.0 })
}

// ------------------------------------------------------------------------------
// planning
// ------------------------------------------------------------------------------

/// Index join alternatives for joining `a` and `b` in the join ordering: either side
/// probing the other when that is a triple pattern scan (with filters) sorted on a
/// variable the first side always binds.
pub(super) fn candidates(a: &Node, b: &Node, ctx: &Ctx) -> Vec<Node> {
    if !ctx.opt.batched_join {
        return Vec::new();
    }
    [candidate(a, b, ctx), candidate(b, a, ctx)]
        .into_iter()
        .flatten()
        .collect()
}

/// The scan under a probe side, with the filters over it.
fn probe_leaf(n: &Node) -> Option<(&Node, &ScanSpec, Vec<Expr>)> {
    let mut filter = Vec::new();
    let mut n = n;
    loop {
        match &n.kind {
            Kind::Filter(es) if !es.iter().any(Expr::has_exists) => {
                filter.extend(es.iter().cloned());
                n = &n.children[0];
            }
            Kind::Scan(spec) => return Some((n, spec, filter)),
            // the ranges only narrow the rows the pushed filter accepts
            Kind::RangeScan(spec, range) => {
                filter.extend(range.filter.iter().cloned());
                return Some((n, spec, filter));
            }
            _ => return None,
        }
    }
}

fn var_str(ctx: &Ctx, v: VarId) -> String {
    let n = ctx.var_name(v);
    if n.starts_with(' ') {
        "?_".into()
    } else {
        format!("?{n}")
    }
}

fn filter_str(ctx: &Ctx, filter: &[Expr]) -> String {
    filter
        .iter()
        .map(|e| e.display(ctx))
        .collect::<Vec<_>>()
        .join(" && ")
}

fn candidate(drive: &Node, probe: &Node, ctx: &Ctx) -> Option<Node> {
    let offer = offer(drive, probe, ctx)?;
    Some(offer.build(drive.clone(), probe))
}

/// The index join of `drive` into the pattern `probe`, or `drive` back when none is
/// offered.
pub(super) fn index_join(
    drive: Node,
    probe: &Node,
    ctx: &Ctx,
) -> std::result::Result<Node, Box<Node>> {
    match offer(&drive, probe, ctx) {
        Some(offer) => Ok(offer.build(drive, probe)),
        None => Err(Box::new(drive)),
    }
}

/// What the join ordering needs to know about a pattern to cost probing it: the
/// variable it is read sorted on, its rows in the permutation, and its rows per key.
#[derive(Clone, Copy, Debug)]
pub(super) struct ProbeSide {
    pub key: VarId,
    rows: f64,
    per_key: f64,
    /// the keys are found by the galloping reader
    gallop: bool,
    /// a star pattern on the key, which a fused star reads with the index joins on the
    /// key below it (when fused stars are costed so)
    pub star: bool,
}

/// The pattern under `probe` as the probed side of an index join, if it can be one.
pub(super) fn probe_side(probe: &Node, ctx: &Ctx) -> Option<ProbeSide> {
    let (scan, spec, _) = probe_leaf(probe)?;
    let &key = probe.sorted.first()?;
    if spec.cols.first() != Some(&(spec.prefix.len(), key)) || spec.graph_col == spec.prefix.len() {
        return None;
    }
    Some(ProbeSide {
        key,
        rows: ctx.snap.estimate(spec.perm, &spec.prefix) as f64,
        per_key: scan.est / scan.d(key),
        gallop: ctx.opt.gallop_index_join,
        star: ctx.opt.fused_star_costs && ctx.opt.star_fusion && star_pattern(spec, key).is_some(),
    })
}

/// The keys a fused star would probe its patterns for, when plan `n` is (under its
/// filters) an index join on `key` of star patterns: the distinct keys of the input of
/// the lowest index join of the chain.
pub(super) fn chain_keys(n: &Node, key: VarId) -> Option<f64> {
    let mut n = n;
    while let Kind::Filter(es) = &n.kind {
        if es.iter().any(Expr::has_exists) {
            return None;
        }
        n = &n.children[0];
    }
    match &n.kind {
        Kind::IndexJoin(j) if j.key == key && j.probes.iter().all(|p| p.star.is_some()) => {
            let c = &n.children[0];
            Some(chain_keys(c, key).unwrap_or_else(|| c.d(key).min(c.est).max(1.0)))
        }
        _ => None,
    }
}

/// The key and the keys a fused star would probe for (see [`chain_keys`]), when plan
/// `n` is an index join of star patterns.
pub(super) fn chain_of(n: &Node) -> Option<(VarId, f64)> {
    let mut m = n;
    while let Kind::Filter(_) = &m.kind {
        m = &m.children[0];
    }
    let Kind::IndexJoin(j) = &m.kind else {
        return None;
    };
    chain_keys(n, j.key).map(|k| (j.key, k))
}

impl ProbeSide {
    /// The estimated cost of probing the pattern, which costs `pat_cost` to read whole
    /// with its filters, for `keys_in` distinct keys, if probing is worth offering: the
    /// probes, and the filters on the rows read.
    pub(super) fn cost(&self, keys_in: f64, pat_cost: f64) -> Option<f64> {
        let (probe, share) = probe_cost(keys_in, self.rows, self.per_key, self.gallop);
        if !(probe < PROBE_LIMIT * self.rows || forced()) {
            return None;
        }
        Some(probe + (pat_cost - self.rows).max(0.0) * share)
    }
}

/// The cost of an index join whose input costs `drive_cost` for `drive_est` rows and
/// whose pattern costs `probe_cost` to probe, with `est` output rows.
pub(super) fn join_cost(drive_cost: f64, drive_est: f64, probe_cost: f64, est: f64) -> f64 {
    if forced() {
        drive_cost + 1.0
    } else {
        drive_cost + drive_est + probe_cost + est
    }
}

/// An index join worth offering, before its input is moved into it.
struct Offer<'p> {
    spec: &'p ScanSpec,
    filter: Vec<Expr>,
    key: VarId,
    est: f64,
    /// the key with how the join was estimated, when not from distinct values
    star: Option<(VarId, super::plan::JoinModel)>,
    cost: f64,
    sorted: Vec<VarId>,
    desc: String,
}

fn offer<'p>(drive: &Node, probe: &'p Node, ctx: &Ctx) -> Option<Offer<'p>> {
    let (scan, spec, filter) = probe_leaf(probe)?;
    let side = probe_side(probe, ctx)?;
    let key = side.key;
    if !drive.vars.contains(&key) {
        return None;
    }
    if !drive.certain.contains(&key) {
        tracing::debug!(
            target: "sparkles::sparql::indexjoin",
            "index join on {} not offered: the input does not always bind it",
            var_str(ctx, key)
        );
        return None;
    }
    let keys_in = match chain_keys(drive, key) {
        Some(k) if side.star => k,
        _ => drive.d(key).min(drive.est).max(1.0),
    };
    let Some(probe_cost) = side.cost(keys_in, probe.cost) else {
        tracing::debug!(
            target: "sparkles::sparql::indexjoin",
            "index join on {} into {} not offered: probing {keys_in:.0} keys costs more than scanning {:.0} rows",
            var_str(ctx, key),
            scan.desc,
            side.rows
        );
        return None;
    };
    let mut keys = vec![key];
    keys.extend(
        probe
            .vars
            .iter()
            .filter(|v| **v != key && drive.vars.contains(v)),
    );
    let (est, star) = super::plan::join_est_with(drive, probe, &keys, ctx);
    // the input's order is kept, except where a shared variable unbound in the input
    // takes the pattern's value
    let sorted = drive
        .sorted
        .iter()
        .take_while(|v| !keys[1..].contains(v))
        .copied()
        .collect();
    let cost = join_cost(drive.cost, drive.est, probe_cost, est);
    let base = scan.desc.split(" | ").next().unwrap_or_default();
    let desc = if filter.is_empty() {
        format!("on {} | {base}", var_str(ctx, key))
    } else {
        format!(
            "on {} | {base} | {}",
            var_str(ctx, key),
            filter_str(ctx, &filter)
        )
    };
    Some(Offer {
        spec,
        filter,
        key,
        est,
        star,
        cost,
        sorted,
        desc,
    })
}

impl Offer<'_> {
    fn build(self, drive: Node, probe: &Node) -> Node {
        let mut vars = drive.vars.clone();
        let mut certain = drive.certain.clone();
        for &v in &probe.vars {
            if !vars.contains(&v) {
                vars.push(v);
            }
            if !certain.contains(&v) {
                certain.push(v);
            }
        }
        let dist = super::plan::merge_dist(&drive, probe, self.est, self.star);
        Node {
            kind: Kind::IndexJoin(Box::new(IndexJoinSpec {
                key: self.key,
                probes: vec![Probe {
                    scan: self.spec.clone(),
                    filter: self.filter,
                    star: star_pattern(self.spec, self.key),
                }],
            })),
            children: vec![drive],
            vars,
            certain,
            sorted: self.sorted,
            est: self.est,
            cost: self.cost,
            dist,
            desc: self.desc,
        }
    }
}

/// The scan as a star pattern: the key is its subject, the predicate a constant, the
/// object a constant or another variable, and the graph no variable.
fn star_pattern(spec: &ScanSpec, key: VarId) -> Option<StarPattern> {
    if !spec.eqs.is_empty() {
        return None;
    }
    let order = spec.perm.order();
    let (mut p, mut o, mut g) = (None, None, None);
    for (i, &v) in spec.prefix.iter().enumerate() {
        match order[i] {
            P => p = Some(v),
            O => o = Some(v),
            G => g = Some(v),
            _ => return None,
        }
    }
    for &(kc, v) in &spec.cols {
        match order[kc] {
            S if v == key => {}
            O => {}
            _ => return None,
        }
    }
    Some(StarPattern {
        p: p?,
        o,
        graph: match g {
            Some(g) => GraphFilter::One(g),
            None => spec.graph.clone(),
        },
        dedup: spec.dedup,
    })
}

/// Fuse chains of index joins on one subject variable over star patterns (with the
/// filters placed between them moved above) into one operator reading them together.
pub(super) fn fuse_stars(n: Node, ctx: &Ctx) -> Node {
    if !ctx.opt.star_fusion || !ctx.opt.batched_join {
        return n;
    }
    fuse(n, ctx)
}

fn fuse(mut n: Node, ctx: &Ctx) -> Node {
    if matches!(
        n.kind,
        Kind::Filter(_) | Kind::Join { .. } | Kind::IndexJoin(_) | Kind::Sort(_)
    ) {
        n.children = std::mem::take(&mut n.children)
            .into_iter()
            .map(|c| fuse(c, ctx))
            .collect();
    }
    let Kind::IndexJoin(top) = &n.kind else {
        return n;
    };
    let mut filters = Vec::new();
    let mut inner = &n.children[0];
    while let Kind::Filter(es) = &inner.kind {
        if es.iter().any(Expr::has_exists) {
            return n;
        }
        filters.extend(es.iter().cloned());
        inner = &inner.children[0];
    }
    let Kind::IndexJoin(low) = &inner.kind else {
        return n;
    };
    if low.key != top.key {
        return n;
    }
    if let Err(why) = fusable(low, top, inner) {
        tracing::debug!(target: "sparkles::sparql::indexjoin", "star on {} not fused: {why}", var_str(ctx, top.key));
        return n;
    }
    let spec = IndexJoinSpec {
        key: top.key,
        probes: low.probes.iter().chain(&top.probes).cloned().collect(),
    };
    let mut desc = format!("on {} | {}", var_str(ctx, spec.key), star_desc(&spec, ctx));
    let pf: Vec<Expr> = spec
        .probes
        .iter()
        .flat_map(|p| p.filter.iter().cloned())
        .collect();
    if !pf.is_empty() {
        desc += &format!(" | {}", filter_str(ctx, &pf));
    }
    let fused = Node {
        kind: Kind::IndexJoin(Box::new(spec)),
        children: inner.children.clone(),
        vars: n.vars.clone(),
        certain: n.certain.clone(),
        sorted: n.sorted.clone(),
        est: n.est,
        cost: n.cost,
        dist: n.dist.clone(),
        desc,
    };
    if filters.is_empty() {
        return fused;
    }
    // the filters read only variables bound below the star's later patterns, whose own
    // variables are new: they keep their outcome above them
    let desc = filter_str(ctx, &filters);
    Node {
        kind: Kind::Filter(filters),
        vars: fused.vars.clone(),
        certain: fused.certain.clone(),
        sorted: fused.sorted.clone(),
        est: fused.est,
        cost: fused.cost + fused.est,
        dist: fused.dist.clone(),
        desc,
        children: vec![fused],
    }
}

/// Whether the patterns of two index joins on one key (`low` below `top`) can be read
/// as one star, or why not.
fn fusable(
    low: &IndexJoinSpec,
    top: &IndexJoinSpec,
    inner: &Node,
) -> std::result::Result<(), &'static str> {
    let probes: Vec<&Probe> = low.probes.iter().chain(&top.probes).collect();
    let stars: Vec<&StarPattern> = probes
        .iter()
        .map(|p| p.star.as_ref())
        .collect::<Option<_>>()
        .ok_or("a pattern is not on the subject with a constant predicate")?;
    if stars
        .iter()
        .any(|s| s.graph != stars[0].graph || s.dedup != stars[0].dedup)
    {
        return Err("the patterns read different graphs");
    }
    // each pattern's rows form their own range of the subject's run
    for (i, a) in stars.iter().enumerate() {
        for b in &stars[i + 1..] {
            if a.p == b.p && (a.o.is_none() || b.o.is_none() || a.o == b.o) {
                return Err("two patterns overlap on one predicate");
            }
        }
    }
    // every pattern binds only new variables besides the key, so the patterns join on
    // the key alone and the input's values never change
    let drive = &inner.children[0];
    let mut seen: Vec<VarId> = drive.vars.clone();
    for p in &probes {
        for &(_, v) in &p.scan.cols[1..] {
            if seen.contains(&v) {
                return Err("a pattern shares a variable besides the subject");
            }
            seen.push(v);
        }
    }
    Ok(())
}

fn star_desc(spec: &IndexJoinSpec, ctx: &Ctx) -> String {
    let term = |id: u64| {
        ctx.term(Id(id))
            .map_or_else(|| format!("{:?}", Id(id)), |t| super::plan::short(&t))
    };
    spec.probes
        .iter()
        .map(|p| {
            let s = p.star.as_ref().expect("fused patterns are star patterns");
            let o = match (s.o, p.scan.cols.get(1)) {
                (Some(o), _) => term(o),
                (None, Some(&(_, v))) => var_str(ctx, v),
                (None, None) => "?_".into(),
            };
            format!("{} {o}", term(s.p))
        })
        .collect::<Vec<_>>()
        .join(" ; ")
}

// ------------------------------------------------------------------------------
// execution
// ------------------------------------------------------------------------------

/// What an index join read (EXPLAIN).
#[derive(Default, Debug)]
pub(super) struct Stats {
    /// distinct keys of the input
    pub keys: usize,
    /// scans started (one per cluster of keys in adjacent blocks)
    pub seeks: usize,
    /// base blocks the scans read
    pub blocks: usize,
    /// rows in the keys' ranges
    pub rows: usize,
    /// how a fused star was read
    pub mode: &'static str,
}

impl Stats {
    pub fn note(&self) -> String {
        let mode = if self.mode.is_empty() {
            String::new()
        } else {
            format!("{}: ", self.mode)
        };
        format!(
            "[{mode}{} keys, {} seeks, {} blocks, {} rows read]",
            self.keys, self.seeks, self.blocks, self.rows
        )
    }

    pub fn counters(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("keys".into(), self.keys.into());
        m.insert("seeks".into(), self.seeks.into());
        m.insert("blocks".into(), self.blocks.into());
        m.insert("rowsRead".into(), self.rows.into());
        if !self.mode.is_empty() {
            m.insert("mode".into(), self.mode.into());
        }
        m
    }
}

/// Key ranges of one permutation, sorted and disjoint, computed on demand (there is one
/// per key and pattern).
enum Ranges<'a> {
    /// `prefix + [key]` for every key
    Keys { prefix: &'a [u64], keys: &'a [u64] },
    /// `[s, p, o]` (`o` when constant) per subject and star pattern, the patterns in
    /// `(p, o)` order
    Star {
        keys: &'a [u64],
        pats: &'a [(u64, Option<u64>)],
    },
    /// every row under the prefix
    All(&'a [u64]),
}

impl Ranges<'_> {
    fn len(&self) -> usize {
        match self {
            Ranges::Keys { keys, .. } => keys.len(),
            Ranges::Star { keys, pats } => keys.len() * pats.len(),
            Ranges::All(_) => 1,
        }
    }

    fn get(&self, i: usize) -> (Key, Key) {
        let mut b = [0u64; 4];
        let n = match self {
            Ranges::Keys { prefix, keys } => {
                b[..prefix.len()].copy_from_slice(prefix);
                b[prefix.len()] = keys[i];
                prefix.len() + 1
            }
            Ranges::Star { keys, pats } => {
                let (p, o) = pats[i % pats.len()];
                b[0] = keys[i / pats.len()];
                b[1] = p;
                match o {
                    Some(o) => {
                        b[2] = o;
                        3
                    }
                    None => 2,
                }
            }
            Ranges::All(prefix) => {
                b[..prefix.len()].copy_from_slice(prefix);
                prefix.len()
            }
        };
        (pad(&b[..n], 0), pad(&b[..n], u64::MAX))
    }

    /// The number of leading key columns that range `i` fixes: its rows are those whose
    /// first `depth` columns equal its lower end's.
    fn depth(&self, i: usize) -> usize {
        match self {
            Ranges::Keys { prefix, .. } => prefix.len() + 1,
            Ranges::Star { pats, .. } => 2 + pats[i % pats.len()].1.is_some() as usize,
            Ranges::All(prefix) => prefix.len(),
        }
    }

    /// The pattern a range belongs to (its position in the star's `(p, o)` order).
    fn part(&self, i: usize) -> usize {
        match self {
            Ranges::Star { pats, .. } => i % pats.len(),
            _ => 0,
        }
    }

    /// Key columns that decide whether a row lies in a range.
    fn bound_mask(&self) -> ColMask {
        match self {
            Ranges::Star { pats, .. } => {
                if pats.iter().any(|p| p.1.is_some()) {
                    0b111
                } else {
                    0b11
                }
            }
            _ if self.len() == 0 => 0,
            _ => {
                let (lo, hi) = self.get(0);
                bound_cols(&lo, &hi)
            }
        }
    }

    /// First range index in `[from, to)` whose upper end is at least `k`.
    fn seek(&self, from: usize, to: usize, k: &Key) -> usize {
        let (mut a, mut b) = (from, to);
        while a < b {
            let m = a + (b - a) / 2;
            if self.get(m).1 < *k {
                a = m + 1;
            } else {
                b = m;
            }
        }
        a
    }
}

/// The first index in `[from, n)` for which `before` is false, or `n`, where `before`
/// holds for a prefix of the indices: steps of doubling length from `from`, then a
/// binary search in the last step. Finding a position `d` places on costs about
/// `2 log2 d` tests, so a sorted batch of keys searched from each previous position
/// costs little per key when the keys are close.
#[inline]
pub(super) fn gallop_to(from: usize, n: usize, before: impl Fn(usize) -> bool) -> usize {
    if from >= n || !before(from) {
        return from;
    }
    // `before(lo)` holds; the answer is in `(lo, hi]`
    let (mut lo, mut step) = (from, 1);
    let hi = loop {
        let hi = lo + step;
        if hi >= n {
            break n;
        }
        if !before(hi) {
            break hi;
        }
        lo = hi;
        step *= 2;
    };
    search(lo + 1, hi, before)
}

/// The first index in `[lo, hi)` for which `before` is false, or `hi`, by binary search.
#[inline]
fn search(mut lo: usize, mut hi: usize, before: impl Fn(usize) -> bool) -> usize {
    while lo < hi {
        let m = lo + (hi - lo) / 2;
        if before(m) {
            lo = m + 1;
        } else {
            hi = m;
        }
    }
    lo
}

/// The blocks `[b0, b1)` of `idx` that can hold keys in `[lo, hi]`, searched from the
/// blocks of the range before (`from`) when `gallop`, else in all blocks.
#[inline]
fn block_range(
    idx: &crate::index::PermIndex,
    lo: &Key,
    hi: &Key,
    from: (usize, usize),
    gallop: bool,
) -> (usize, usize) {
    if !gallop {
        return idx.key_block_range(lo, hi);
    }
    let m = &idx.blocks;
    let b0 = gallop_to(from.0, m.len(), |b| m[b].last < *lo);
    let b1 = gallop_to(from.1.max(b0), m.len(), |b| m[b].first <= *hi);
    (b0, b1)
}

/// The most ranges a join prefetches the blocks of: past it, finding every range's
/// blocks first costs more than the reads it would overlap.
const PREFETCH_RANGES: usize = 16_384;

/// Ask the kernel to read the blocks of `ranges` in `perm` that the cache lacks, all
/// at once, before the reader decodes them one by one (`Optimizations::prefetch_blocks`):
/// on a cold server their reads then overlap. `cols` are the key columns the reader
/// decodes besides the ones that bound the ranges.
fn prefetch(ctx: &Ctx, perm: Perm, ranges: &Ranges, cols: ColMask) {
    if !ctx.opt.prefetch_blocks || ranges.len() > PREFETCH_RANGES || ranges.len() < 2 {
        return;
    }
    let idx = ctx.snap.perm(perm);
    let mask = cols | ranges.bound_mask() | (1 << perm.col_of(G));
    let mut last = None;
    for i in 0..ranges.len() {
        let (lo, hi) = ranges.get(i);
        let (b0, b1) = idx.key_block_range(&lo, &hi);
        for b in b0..b1 {
            // consecutive ranges often share a block
            if last != Some(b) {
                idx.prefetch(&ctx.snap.cache, b, mask);
                last = Some(b);
            }
        }
    }
}

/// Clusters of consecutive ranges read by one scan each: a range joins the cluster when
/// its first block is no further than just past the cluster's blocks, and no delta
/// change lies between it and the cluster (the scan would merge those row by row).
/// Returns the clusters as range index spans and the base blocks they read.
fn clusters(ctx: &Ctx, perm: Perm, ranges: &Ranges) -> (Vec<(usize, usize)>, usize) {
    let idx = ctx.snap.perm(perm);
    let pi = perm.index();
    let (ins, del) = (&ctx.snap.delta.ins[pi], &ctx.snap.delta.del[pi]);
    let delta = !ins.is_empty() || !del.is_empty();
    let gallop = ctx.opt.gallop_index_join;
    let mut out = Vec::new();
    let mut blocks = 0;
    // (first range, first block, end block, upper end of the last range)
    let mut cur: Option<(usize, usize, usize, Key)> = None;
    let mut at = (0, 0);
    for i in 0..ranges.len() {
        let (lo, hi) = ranges.get(i);
        let (b0, b1) = block_range(idx, &lo, &hi, at, gallop);
        at = (b0, b1);
        let join = cur.is_some_and(|(_, _, c1, prev)| {
            b0 <= c1
                && !(delta && {
                    let gap = (Bound::Excluded(prev), Bound::Excluded(lo));
                    ins.intersects(gap) || del.intersects(gap)
                })
        });
        match &mut cur {
            Some((_, _, c1, prev)) if join => {
                *c1 = (*c1).max(b1);
                *prev = hi;
            }
            _ => {
                if let Some((s, c0, c1, _)) = cur {
                    out.push((s, i));
                    blocks += c1 - c0;
                }
                cur = Some((i, b0, b1, hi));
            }
        }
    }
    if let Some((s, c0, c1, _)) = cur {
        out.push((s, ranges.len()));
        blocks += c1 - c0;
    }
    (out, blocks)
}

/// How the rows of a permutation are checked: a scan's graph filter, repeated
/// variables and merged-graph duplicates.
struct RowRule<'a> {
    graph: &'a GraphFilter,
    graph_col: usize,
    eqs: &'a [(usize, usize)],
    dedup: bool,
}

/// First row in `[i, e)` of a sorted block for which `pred` holds (`pred` false, then
/// true).
fn first_row(b: &Block, mut i: usize, mut e: usize, pred: impl Fn(&Key) -> bool) -> usize {
    while i < e {
        let m = i + (e - i) / 2;
        if pred(&b.key(m)) {
            e = m;
        } else {
            i = m + 1;
        }
    }
    i
}

/// Append the rows of `ranges` in `perm`, read cluster by cluster, to `out[part(r)]`:
/// the key columns `cols[part(r)]` of every row of range `r` that passes `rule`.
#[allow(clippy::too_many_arguments)]
fn read_ranges(
    ctx: &Ctx,
    perm: Perm,
    ranges: &Ranges,
    clusters: &[(usize, usize)],
    cols: &[Vec<usize>],
    rule: &RowRule,
    out: &mut [Table],
    stats: &mut Stats,
) -> Result<()> {
    let mut mask = ranges.bound_mask();
    for cs in cols {
        for &c in cs {
            mask |= 1 << c;
        }
    }
    if !matches!(rule.graph, GraphFilter::All) {
        mask |= 1 << rule.graph_col;
    }
    for &(a, b) in rule.eqs {
        mask |= (1 << a) | (1 << b);
    }
    if !ctx.opt.selective_columns {
        mask = ALL_COLS;
    }
    let width: usize = cols.iter().map(Vec::len).sum::<usize>().max(1);
    let total = |out: &[Table]| out.iter().map(Table::len).sum::<usize>();
    let push = |k: &Key, r: usize, last: &mut Option<(usize, Key)>, out: &mut [Table]| {
        push_row(rule, ranges, cols, k, r, last, out)
    };
    for &(c0, c1) in clusters {
        ctx.check()?;
        stats.seeks += 1;
        let mut r = c0;
        let mut last: Option<(usize, Key)> = None;
        let mut seen = 0usize;
        let (lo, _) = ranges.get(c0);
        let (_, hi) = ranges.get(c1 - 1);
        ctx.snap.scan_between_cols(perm, lo, hi, mask, |chunk| {
            match chunk {
                Chunk::Block(b, s, e) => {
                    let mut i = s;
                    while i < e && r < c1 {
                        let (lo, hi) = ranges.get(r);
                        let k = b.key(i);
                        if k < lo {
                            i = first_row(b, i, e, |k| *k >= lo);
                            continue;
                        }
                        if k > hi {
                            r = ranges.seek(r, c1, &k);
                            continue;
                        }
                        // the range may go on in the next chunk: `r` moves on only once
                        // a larger key is seen
                        let j = first_row(b, i, e, |k| *k > hi);
                        stats.rows += j - i;
                        seen += j - i;
                        let plain = !rule.dedup
                            && rule.eqs.is_empty()
                            && (matches!(rule.graph, GraphFilter::All)
                                || b.cols[rule.graph_col][i..j]
                                    .iter()
                                    .all(|&g| rule.graph.accepts(g)));
                        if plain {
                            let t = ranges.part(r);
                            let tab = &mut out[t];
                            for (n, &c) in cols[t].iter().enumerate() {
                                tab.cols[n].extend(b.cols[c][i..j].iter().map(|&x| Id(x)));
                            }
                            tab.len += j - i;
                        } else {
                            for x in i..j {
                                push(&b.key(x), r, &mut last, out);
                            }
                        }
                        i = j;
                    }
                }
                Chunk::Row(k) => {
                    if r < c1 && k > ranges.get(r).1 {
                        r = ranges.seek(r, c1, &k);
                    }
                    if r < c1 && k >= ranges.get(r).0 {
                        stats.rows += 1;
                        seen += 1;
                        push(&k, r, &mut last, out);
                    }
                }
            }
            if seen > 1 << 16 {
                seen = 0;
                ctx.check()?;
                ctx.check_output(total(out), width)?;
            }
            Ok(r < c1)
        })?;
        ctx.check_output(total(out), width)?;
    }
    Ok(())
}

/// How row `i` of a block compares with the first `d` columns of `k`.
#[inline]
fn lead_cmp(b: &Block, i: usize, k: &Key, d: usize) -> std::cmp::Ordering {
    for (c, x) in k.iter().enumerate().take(d) {
        match b.cols[c][i].cmp(x) {
            std::cmp::Ordering::Equal => {}
            o => return o,
        }
    }
    std::cmp::Ordering::Equal
}

/// Append the rows of `ranges` in `perm` to `out[part(r)]`, like [`read_ranges`], with
/// one cursor over the permutation instead of a scan per cluster. Each range finds its
/// first block by galloping over the blocks' first and last keys from the range before,
/// and its rows by galloping in the block from where the range before ended, so keys
/// that lie close together cost a few comparisons each. The cursor keeps the block it
/// is in. A range with delta changes in it is read by a scan that merges them.
#[allow(clippy::too_many_arguments)]
fn read_ranges_gallop(
    ctx: &Ctx,
    perm: Perm,
    ranges: &Ranges,
    cols: &[Vec<usize>],
    rule: &RowRule,
    out: &mut [Table],
    stats: &mut Stats,
) -> Result<()> {
    let mut mask = ranges.bound_mask();
    for cs in cols {
        for &c in cs {
            mask |= 1 << c;
        }
    }
    if !matches!(rule.graph, GraphFilter::All) {
        mask |= 1 << rule.graph_col;
    }
    for &(a, b) in rule.eqs {
        mask |= (1 << a) | (1 << b);
    }
    if !ctx.opt.selective_columns {
        mask = ALL_COLS;
    }
    let idx = ctx.snap.perm(perm);
    let metas = &idx.blocks;
    let pi = perm.index();
    let (ins, del) = (&ctx.snap.delta.ins[pi], &ctx.snap.delta.del[pi]);
    let delta = !ins.is_empty() || !del.is_empty();
    let width: usize = cols.iter().map(Vec::len).sum::<usize>().max(1);
    let total = |out: &[Table]| out.iter().map(Table::len).sum::<usize>();
    let plain_rule = !rule.dedup && rule.eqs.is_empty();
    // the first block that can hold the next range, the block the cursor holds, and the
    // row of that block where the range before ended
    let mut b = 0usize;
    let mut cur: Option<(usize, Block)> = None;
    let mut row = 0usize;
    let mut seen = 0usize;
    for r in 0..ranges.len() {
        if r % 1024 == 1023 || seen > 1 << 16 {
            seen = 0;
            ctx.check()?;
            ctx.check_output(total(out), width)?;
        }
        let (lo, hi) = ranges.get(r);
        b = gallop_to(b, metas.len(), |x| metas[x].last < lo);
        if delta && {
            let rng = (Bound::Included(lo), Bound::Included(hi));
            ins.intersects(rng) || del.intersects(rng)
        } {
            if b == metas.len() || metas[b].first > hi {
                // no base block reaches the range, so no deletion is in it either: its
                // rows are the delta's inserted keys, read straight from the delta
                stats.seeks += 1;
                let mut last: Option<(usize, Key)> = None;
                let mut it = ins.range(lo..=hi);
                loop {
                    let run = it.next_run();
                    if run.is_empty() {
                        break;
                    }
                    stats.rows += run.len();
                    seen += run.len();
                    for k in run {
                        push_row(rule, ranges, cols, k, r, &mut last, out);
                    }
                }
            } else {
                read_ranges(ctx, perm, ranges, &[(r, r + 1)], cols, rule, out, stats)?;
            }
            continue;
        }
        let d = ranges.depth(r);
        let t = ranges.part(r);
        let mut last: Option<(usize, Key)> = None;
        let mut x = b;
        while x < metas.len() && metas[x].first <= hi {
            let from = match &cur {
                Some((cb, _)) if *cb == x => Some(row),
                _ => {
                    // a block other than the next one is found by a search
                    if !cur.as_ref().is_some_and(|(cb, _)| cb + 1 == x) {
                        stats.seeks += 1;
                    }
                    cur = Some((x, ctx.snap.cache.get_cols(idx, x, mask)?));
                    stats.blocks += 1;
                    None
                }
            };
            let blk = &cur.as_ref().expect("the cursor holds a block").1;
            let n = blk.len();
            let before = |i: usize| lead_cmp(blk, i, &lo, d).is_lt();
            // in a new block, with no position to start from, a binary search
            let i = match from {
                Some(from) => gallop_to(from, n, before),
                None => search(0, n, before),
            };
            let j = gallop_to(i, n, |i| lead_cmp(blk, i, &lo, d).is_le());
            stats.rows += j - i;
            seen += j - i;
            let plain = plain_rule
                && (matches!(rule.graph, GraphFilter::All)
                    || blk.cols[rule.graph_col][i..j]
                        .iter()
                        .all(|&g| rule.graph.accepts(g)));
            if plain {
                let tab = &mut out[t];
                for (k, &c) in cols[t].iter().enumerate() {
                    tab.cols[k].extend(blk.cols[c][i..j].iter().map(|&v| Id(v)));
                }
                tab.len += j - i;
            } else {
                for y in i..j {
                    push_row(rule, ranges, cols, &blk.key(y), r, &mut last, out);
                }
            }
            row = j;
            if j < n {
                break;
            }
            // the range may go on in the next block
            x += 1;
        }
    }
    ctx.check_output(total(out), width)?;
    Ok(())
}

/// Append row `k` of range `r` to its pattern's table if it passes `rule` (see
/// [`read_ranges`]); `last` holds the range's last row for dropping merged-graph
/// duplicates.
fn push_row(
    rule: &RowRule,
    ranges: &Ranges,
    cols: &[Vec<usize>],
    k: &Key,
    r: usize,
    last: &mut Option<(usize, Key)>,
    out: &mut [Table],
) {
    if !rule.graph.accepts(k[rule.graph_col]) || rule.eqs.iter().any(|&(a, b)| k[a] != k[b]) {
        return;
    }
    let t = ranges.part(r);
    let cs = &cols[t];
    if rule.dedup {
        let mut proj = [0u64; 4];
        for (i, &c) in cs.iter().enumerate() {
            proj[i] = k[c];
        }
        if *last == Some((r, proj)) {
            return;
        }
        *last = Some((r, proj));
    }
    let tab = &mut out[t];
    for (i, &c) in cs.iter().enumerate() {
        tab.cols[i].push(Id(k[c]));
    }
    tab.len += 1;
}

/// The ranges of one pattern with their clusters and the base blocks those read.
type Plan<'a> = (Ranges<'a>, Vec<(usize, usize)>, usize);

/// The rows of every pattern for the keys (all rows when `all`), each table sorted on
/// the key (its first column), with the pattern's filter applied.
fn read_probes(
    ctx: &Ctx,
    spec: &IndexJoinSpec,
    keys: &[u64],
    all: bool,
    stats: &mut Stats,
) -> Result<Vec<Table>> {
    let mut out: Vec<Table> = spec
        .probes
        .iter()
        .map(|p| Table::new(p.scan.cols.iter().map(|c| c.1).collect()))
        .collect();
    let gallop = ctx.opt.gallop_index_join;
    let star = spec.probes.len() > 1 && !all;
    let per_pattern: Vec<Plan> = spec
        .probes
        .iter()
        .map(|p| {
            let ranges = if all {
                Ranges::All(&p.scan.prefix)
            } else {
                Ranges::Keys {
                    prefix: &p.scan.prefix,
                    keys,
                }
            };
            // the galloping reader needs no clusters, except to choose how a star is read
            let (cl, blocks) = if gallop && !star {
                (Vec::new(), 0)
            } else {
                clusters(ctx, p.scan.perm, &ranges)
            };
            (ranges, cl, blocks)
        })
        .collect();
    let cost =
        |seeks: usize, blocks: usize| seeks as f64 * SEEK_COST + blocks as f64 * BLOCK_DECODE;
    // a fused star: one walk over the subjects' runs when it reads less
    if star {
        let stars: Vec<&StarPattern> = spec
            .probes
            .iter()
            .map(|p| p.star.as_ref().expect("fused patterns are star patterns"))
            .collect();
        let mut order: Vec<usize> = (0..stars.len()).collect();
        order.sort_by_key(|&i| (stars[i].p, stars[i].o.unwrap_or(0)));
        let pats: Vec<(u64, Option<u64>)> =
            order.iter().map(|&i| (stars[i].p, stars[i].o)).collect();
        let walk = Ranges::Star { keys, pats: &pats };
        let (wcl, wblocks) = clusters(ctx, Perm::Spo, &walk);
        let per_cost: f64 = per_pattern
            .iter()
            .map(|(_, cl, b)| cost(cl.len(), *b))
            .sum();
        let take_walk = forced_walk().unwrap_or(cost(wcl.len(), wblocks) < per_cost);
        if take_walk {
            stats.mode = "subject runs";
            let (sc, oc) = (Perm::Spo.col_of(S), Perm::Spo.col_of(O));
            let cols: Vec<Vec<usize>> = order
                .iter()
                .map(|&i| {
                    if stars[i].o.is_some() {
                        vec![sc]
                    } else {
                        vec![sc, oc]
                    }
                })
                .collect();
            let rule = RowRule {
                graph: &stars[0].graph,
                graph_col: Perm::Spo.col_of(G),
                eqs: &[],
                dedup: stars[0].dedup,
            };
            // tables in the walk's pattern order, then back in the probes' order
            let mut parts: Vec<Table> = order.iter().map(|&i| out[i].clone()).collect();
            let mask = cols.iter().flatten().fold(0, |m, &c| m | (1 << c));
            prefetch(ctx, Perm::Spo, &walk, mask);
            if gallop {
                read_ranges_gallop(ctx, Perm::Spo, &walk, &cols, &rule, &mut parts, stats)?;
            } else {
                stats.blocks += wblocks;
                read_ranges(ctx, Perm::Spo, &walk, &wcl, &cols, &rule, &mut parts, stats)?;
            }
            for (t, &i) in parts.into_iter().zip(&order) {
                out[i] = t;
            }
            filter_probes(ctx, spec, &mut out)?;
            return Ok(out);
        }
        stats.mode = "per pattern";
    }
    for ((ranges, cl, blocks), (p, t)) in per_pattern
        .iter()
        .zip(spec.probes.iter().zip(out.iter_mut()))
    {
        let rule = RowRule {
            graph: &p.scan.graph,
            graph_col: p.scan.graph_col,
            eqs: &p.scan.eqs,
            dedup: p.scan.dedup,
        };
        let cols = vec![p.scan.cols.iter().map(|c| c.0).collect::<Vec<_>>()];
        let mask = cols[0].iter().fold(0, |m, &c| m | (1 << c));
        prefetch(ctx, p.scan.perm, ranges, mask);
        let out = std::slice::from_mut(t);
        if gallop {
            read_ranges_gallop(ctx, p.scan.perm, ranges, &cols, &rule, out, stats)?;
        } else {
            stats.blocks += blocks;
            read_ranges(ctx, p.scan.perm, ranges, cl, &cols, &rule, out, stats)?;
        }
    }
    filter_probes(ctx, spec, &mut out)?;
    Ok(out)
}

fn filter_probes(ctx: &Ctx, spec: &IndexJoinSpec, out: &mut [Table]) -> Result<()> {
    for (p, t) in spec.probes.iter().zip(out.iter_mut()) {
        if !p.filter.is_empty() && !t.is_empty() {
            super::exec::apply_filter(ctx, t, &p.filter)?;
        }
    }
    Ok(())
}

/// Row spans of each key of `keys` (sorted, distinct) in a column sorted on the key,
/// each searched from the end of the one before by galloping when `gallop`, else by a
/// binary search over the rest of the column.
fn spans(col: &[Id], keys: &[u64], gallop: bool) -> Vec<(u32, u32)> {
    let mut out = Vec::with_capacity(keys.len());
    let mut i = 0;
    for &k in keys {
        let e = if gallop {
            i = gallop_to(i, col.len(), |x| col[x].0 < k);
            gallop_to(i, col.len(), |x| col[x].0 == k)
        } else {
            i += col[i..].partition_point(|x| x.0 < k);
            i + col[i..].partition_point(|x| x.0 == k)
        };
        out.push((i as u32, e as u32));
        i = e;
    }
    out
}

/// Run an index join over its input table: the rows of every input row, in input
/// order, joined with its key's rows of each pattern (nested in pattern order).
pub(super) fn run(
    ctx: &Ctx,
    spec: &IndexJoinSpec,
    left: &Table,
    vars: &[VarId],
) -> Result<(Table, Stats)> {
    let mut stats = Stats::default();
    let kc = left
        .col_of(spec.key)
        .ok_or_else(|| Error::invalid("index join key missing from its input"))?;
    let col = &left.cols[kc];
    // the planner admits keys the input always binds; an unbound one (never expected)
    // is compatible with every row, so then the patterns are read whole
    let unbound = col.iter().any(|x| x.is_undef());
    let mut keys: Vec<u64> = col.iter().filter(|x| !x.is_undef()).map(|x| x.0).collect();
    if left.sorted.first() != Some(&spec.key) {
        keys.sort_unstable();
    }
    keys.dedup();
    stats.keys = keys.len();
    if unbound {
        stats.mode = "all rows";
    }
    let held = ctx.charge(super::ctx::table_bytes(keys.len(), 1))?;
    let rs = if left.is_empty() {
        spec.probes
            .iter()
            .map(|p| Table::new(p.scan.cols.iter().map(|c| c.1).collect()))
            .collect()
    } else {
        read_probes(ctx, spec, &keys, unbound, &mut stats)?
    };
    for t in &rs {
        held.add(t.mem_bytes())?;
    }
    let out = combine(ctx, spec, left, kc, &keys, unbound, &rs)?;
    Ok((out.project(vars), stats))
}

/// Join every input row with the rows of its key in each pattern's table.
fn combine(
    ctx: &Ctx,
    spec: &IndexJoinSpec,
    left: &Table,
    kc: usize,
    keys: &[u64],
    unbound: bool,
    rs: &[Table],
) -> Result<Table> {
    let m = rs.len();
    // the patterns' other variables: shared with the input (checked, filled where the
    // input leaves them unbound), shared between patterns (checked), or new
    let mut shared: Vec<(usize, usize, usize)> = Vec::new();
    let mut across: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let mut new: Vec<(usize, usize)> = Vec::new();
    for (k, t) in rs.iter().enumerate() {
        for (c, v) in t.vars.iter().enumerate().skip(1) {
            if let Some(lc) = left.col_of(*v) {
                shared.push((lc, k, c));
            } else if let Some(&(k0, c0)) = new.iter().find(|(k0, c0)| rs[*k0].vars[*c0] == *v) {
                across.push(((k0, c0), (k, c)));
            } else {
                new.push((k, c));
            }
        }
    }
    let width = left.width() + new.len() + 1;
    let gallop = ctx.opt.gallop_index_join;
    // the spans of each key in every pattern, a row of `m` per key
    let rows_of = |keys: &[u64]| -> Vec<(u32, u32)> {
        let per: Vec<Vec<(u32, u32)>> =
            rs.iter().map(|t| spans(&t.cols[0], keys, gallop)).collect();
        (0..keys.len())
            .flat_map(|x| per.iter().map(move |p| p[x]))
            .collect()
    };
    let key_spans = rows_of(keys);
    // an unbound input key ranges over every key the first pattern holds
    let all_spans = if unbound {
        let mut ks: Vec<u64> = rs[0].cols[0].iter().map(|x| x.0).collect();
        ks.dedup();
        rows_of(&ks)
    } else {
        Vec::new()
    };
    let mut li: Vec<u32> = Vec::new();
    let mut pj: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut js: Vec<u32> = vec![0; m];
    let compatible = |i: usize, js: &[u32]| {
        shared.iter().all(|&(lc, k, c)| {
            let a = left.cols[lc][i];
            a.is_undef() || a == rs[k].cols[c][js[k] as usize]
        }) && across.iter().all(|&((k0, c0), (k1, c1))| {
            rs[k0].cols[c0][js[k0] as usize] == rs[k1].cols[c1][js[k1] as usize]
        })
    };
    // the product of input row `i` with one key's spans, the first pattern outermost
    let emit = |i: usize,
                spans: &[(u32, u32)],
                js: &mut [u32],
                li: &mut Vec<u32>,
                pj: &mut [Vec<u32>]|
     -> Result<()> {
        if spans.iter().any(|(s, e)| s >= e) {
            return Ok(());
        }
        let product = spans
            .iter()
            .fold(1usize, |n, (s, e)| n.saturating_mul((e - s) as usize));
        if product > 1024 {
            ctx.check_output(li.len().saturating_add(product), width)?;
        }
        for (j, s) in js.iter_mut().zip(spans) {
            *j = s.0;
        }
        loop {
            if compatible(i, js) {
                li.push(i as u32);
                for (k, &j) in js.iter().enumerate() {
                    pj[k].push(j);
                }
            }
            // next combination: the last pattern varies fastest
            let mut k = m;
            loop {
                if k == 0 {
                    return Ok(());
                }
                k -= 1;
                js[k] += 1;
                if js[k] < spans[k].1 {
                    break;
                }
                js[k] = spans[k].0;
            }
        }
    };
    // an input whose key column is in order, flagged so or not, finds each key from the
    // one before
    let sorted_key = left.sorted.first() == Some(&spec.key)
        || (gallop && !unbound && left.cols[kc].windows(2).all(|w| w[0] <= w[1]));
    let mut x = 0usize;
    for i in 0..left.len() {
        if i % 4096 == 0 {
            ctx.check()?;
            ctx.check_output(li.len(), width)?;
        }
        let v = left.cols[kc][i];
        if v.is_undef() {
            for row in all_spans.chunks(m) {
                emit(i, row, &mut js, &mut li, &mut pj)?;
            }
            continue;
        }
        x = if sorted_key && gallop {
            gallop_to(x, keys.len(), |k| keys[k] < v.0)
        } else if sorted_key {
            x + keys[x..].partition_point(|k| *k < v.0)
        } else {
            keys.partition_point(|k| *k < v.0)
        };
        emit(i, &key_spans[x * m..(x + 1) * m], &mut js, &mut li, &mut pj)?;
    }
    ctx.check_output(li.len(), width)?;
    // the input's columns, the key and shared variables filled from the patterns where
    // the input leaves them unbound, then the new columns
    let mut vars = left.vars.clone();
    let mut cols = Vec::with_capacity(width);
    let mut filled: Vec<VarId> = Vec::new();
    for (lc, col) in left.cols.iter().enumerate() {
        let fill = if lc == kc {
            unbound.then_some((0, 0))
        } else {
            shared.iter().find(|s| s.0 == lc).map(|s| (s.1, s.2))
        };
        if fill.is_some() {
            filled.push(left.vars[lc]);
        }
        cols.push(
            li.iter()
                .enumerate()
                .map(|(n, &i)| {
                    let v = col[i as usize];
                    match fill {
                        Some((k, c)) if v.is_undef() => rs[k].cols[c][pj[k][n] as usize],
                        _ => v,
                    }
                })
                .collect(),
        );
    }
    for &(k, c) in &new {
        vars.push(rs[k].vars[c]);
        cols.push(pj[k].iter().map(|&j| rs[k].cols[c][j as usize]).collect());
    }
    let sorted = left
        .sorted
        .iter()
        .take_while(|v| !filled.contains(v))
        .copied()
        .collect();
    Ok(Table {
        vars,
        cols,
        len: li.len(),
        sorted,
    })
}
