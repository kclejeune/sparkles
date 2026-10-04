//! k-nearest-neighbour ordering: an indexed scan's rows in increasing lower bound of
//! their distance to a constant, in batches, each joined with the rest of the group
//! (the template plan), until `k` rows whose exact distance is below the next bound
//! exist. Rows whose distance is an error come first, as SPARQL orders them.
//!
//! The operator hands the top-k above it a superset of its first `k` rows: every row
//! whose key is an error (when the template keeps them), and every row of the batches
//! run. A row it did not produce has a distance of at least the bound the search stopped
//! at, which is above the `k`-th distance found, so the top-k's answer is the one it
//! would compute over the whole group (up to ties at the `k`-th distance).
//!
//! The exact distance of a row is the ordering key itself, evaluated on the template's
//! rows, so it is the value the top-k sorts by. The rows whose key is an error are those
//! whose literal has no distance to the constant: not a geometry, malformed, empty, in a
//! CRS without a transform to or from the constant's, or over the operation limit. They
//! are found by reading the scan's literals (without parsing what the index holds); a
//! `FILTER(BOUND(?d))` or a bound on the distance removes them, and the scan read with
//! them.

use super::GeomRef;
use super::column::{Slot, classify};
use super::exec::{Counters, config, state};
use super::geom::Geom;
use super::ops::in_crs;
use super::search::{self, Rechecked, SearchStats};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{G, O, S};
use crate::sparql::ctx::Ctx;
use crate::sparql::exec::{PlanInfo, execute};
use crate::sparql::expr::{Row, eval};
use crate::sparql::geojoin::SpatialKnnSpec;
use crate::sparql::geopf::{ScanShape, number};
use crate::sparql::plan::{Kind, Node, PathEnd};
use crate::sparql::table::{Table, VarId};
use crate::store::Chunk;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BinaryHeap;

/// Template runs kept for EXPLAIN.
const RUNS_SHOWN: usize = 4;
/// Rows whose distance is an error handed to the template at once.
const ERROR_BATCH: usize = 4096;

/// A distance ordered for the heap of the nearest keys (`total_cmp`).
#[derive(Clone, Copy, PartialEq)]
struct Key(f64);

impl Eq for Key {}
impl PartialOrd for Key {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Key {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

/// Runs of the template over batches of the scan's rows.
struct Runner<'a> {
    ctx: &'a Ctx,
    spec: &'a SpatialKnnSpec,
    template: &'a Node,
    vars: &'a [VarId],
    out: Table,
    runs: Vec<PlanInfo>,
    batches: u64,
    /// rows whose key was evaluated
    keyed: u64,
}

impl Runner<'_> {
    /// Run the template over the scan's rows `(s, o, g)`; returns the ordering key of
    /// each of its rows (`None`: an error).
    fn run(&mut self, rows: &[[Id; 3]]) -> Result<Vec<Option<f64>>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut t = self.template.clone();
        let mut leaf = &mut t;
        for &i in &self.spec.placeholder {
            leaf = leaf
                .children
                .get_mut(i)
                .ok_or_else(|| Error::invalid("nearest-neighbour template without its input"))?;
        }
        let spec = self.spec;
        let cols: Vec<usize> = leaf
            .vars
            .iter()
            .map(|v| {
                if Some(*v) == spec.subj_var {
                    0
                } else if *v == spec.geom_var {
                    1
                } else {
                    2
                }
            })
            .collect();
        let mut batch = Table::new(leaf.vars.clone());
        let mut row = vec![Id::UNDEF; cols.len()];
        for r in rows {
            for (x, &c) in row.iter_mut().zip(&cols) {
                *x = r[c];
            }
            batch.push_row(&row);
        }
        leaf.est = rows.len() as f64;
        leaf.kind = Kind::Values(batch);
        let (res, info) = execute(self.ctx, &t)?;
        self.batches += 1;
        if self.runs.len() < RUNS_SHOWN {
            self.runs.push(info);
        }
        let map = res.var_map(self.ctx.nvars());
        let keys = (0..res.len())
            .map(|i| {
                let row = Row {
                    table: &res,
                    i,
                    map: &map,
                    dec: None,
                };
                eval(&spec.order, &row, self.ctx)
                    .ok()
                    .and_then(|v| v.value(self.ctx).ok())
                    .as_ref()
                    .and_then(number)
            })
            .collect::<Vec<_>>();
        self.keyed += keys.len() as u64;
        self.out.append(res.project(self.vars));
        self.ctx.check_output(self.out.len(), self.out.width())?;
        Ok(keys)
    }
}

/// Execute a [`SpatialKnnSpec`] with its template plan; returns the rows (a superset of
/// the first `k` in the ordering), the counters and the template's runs for EXPLAIN.
pub fn spatial_knn(
    ctx: &Ctx,
    spec: &SpatialKnnSpec,
    template: &Node,
    vars: &[VarId],
) -> Result<(Table, Counters, Vec<PlanInfo>)> {
    let shape = ScanShape::of(&spec.scan)
        .ok_or_else(|| Error::invalid("nearest-neighbour search of an unexpected pattern"))?;
    let subj = match shape.subj {
        Some(PathEnd::Const(s)) => Some(s),
        _ => None,
    };
    let mut run = Runner {
        ctx,
        spec,
        template,
        vars,
        out: Table::new(vars.to_vec()),
        runs: Vec::new(),
        batches: 0,
        keyed: 0,
    };
    let mut st = SearchStats::default();
    // the rows whose key is an error come first
    let mut errors = 0usize;
    let mut error_rows = 0usize;
    // their literals (the search hands out some of them: they are not run twice)
    let mut taken: FxHashSet<Id> = FxHashSet::default();
    if spec.errors {
        let rows;
        (rows, taken) = rows_without_distance(ctx, spec, subj)?;
        error_rows = rows.len();
        for chunk in rows.chunks(ERROR_BATCH) {
            errors += run.run(chunk)?.iter().filter(|k| k.is_none()).count();
        }
    }
    let need = spec.k.saturating_sub(errors);
    if need > 0 {
        let batch = spec.k.saturating_mul(2).max(64);
        let mut pending: Vec<[Id; 3]> = Vec::new();
        // the `need` smallest keys so far (the largest on top)
        let mut best: BinaryHeap<Key> = BinaryHeap::new();
        let take = |keys: Vec<Option<f64>>, best: &mut BinaryHeap<Key>| {
            for k in keys.into_iter().flatten() {
                if best.len() < need {
                    best.push(Key(k));
                } else if best.peek().is_some_and(|m| k < m.0) {
                    best.pop();
                    best.push(Key(k));
                }
            }
        };
        let graph = shape.graph_filter(&spec.scan);
        search::nearest_with(
            ctx,
            &[spec.pred],
            &spec.q,
            &graph,
            spec.scan.dedup,
            &mut st,
            &mut |hits, bound| {
                pending.extend(
                    hits.iter()
                        .filter(|h| subj.is_none_or(|s| s == h.s) && !taken.contains(&h.o))
                        .map(|h| [h.s, h.o, h.g]),
                );
                if pending.len() < batch && bound.is_finite() {
                    return Ok(true);
                }
                let keys = run.run(&pending)?;
                pending.clear();
                take(keys, &mut best);
                // no row after this one is nearer than `bound`
                let proven = best.len() >= need
                    && best
                        .peek()
                        .is_some_and(|m| m.0 < bound / spec.metres_per_unit);
                Ok(!proven)
            },
        )?;
        if !pending.is_empty() {
            let keys = run.run(&pending)?;
            take(keys, &mut best);
        }
    }
    let mut c = Counters::new();
    c.insert("candidates".into(), st.candidates.into());
    c.insert("refined".into(), run.keyed.into());
    c.insert("matched".into(), (run.out.len() as u64).into());
    c.insert("treeNodesVisited".into(), st.nodes.into());
    c.insert("rechecked".into(), st.rechecked.into());
    c.insert("batches".into(), run.batches.into());
    c.insert("errorRows".into(), (error_rows as u64).into());
    c.insert("index".into(), state(ctx).to_string().into());
    c.insert("fallback".into(), st.fallback.into());
    Ok((run.out, c, run.runs))
}

/// The scan's rows `(s, o, g)` whose literal has no distance to the constant, in scan
/// order (what the nearest-first search never hands out, and what it hands out but
/// cannot be measured), and those literals.
fn rows_without_distance(
    ctx: &Ctx,
    spec: &SpatialKnnSpec,
    subj: Option<Id>,
) -> Result<(Vec<[Id; 3]>, FxHashSet<Id>)> {
    let snap = &*ctx.snap;
    let base = snap.geo.as_deref().and_then(|v| v.base.clone());
    let cfg = config(ctx);
    let scan = &spec.scan;
    let order = scan.perm.order();
    let col = |c: usize| order.iter().position(|&x| x == c).unwrap_or(usize::MAX);
    let (sc, oc, gc) = (col(S), col(O), col(G));
    let mut test = Measurable {
        q: &spec.q,
        w_first: spec.w_first,
        op_vertices: ctx.geo.op_vertices(),
        crs: FxHashMap::default(),
    };
    let mut rechecked = Rechecked::new(snap);
    // literal → whether its distance is an error
    let mut memo: FxHashMap<u64, bool> = FxHashMap::default();
    let mut out = Vec::new();
    let mut last: Option<[u64; 3]> = None;
    let mut n = 0usize;
    let mut err = None;
    let mut row = |k: [u64; 4]| -> Result<()> {
        n += 1;
        if n.is_multiple_of(4096) {
            ctx.check()?;
        }
        if !scan.graph.accepts(k[scan.graph_col]) {
            return Ok(());
        }
        // a merged default graph: one row per triple, as the scan keeps
        if scan.dedup {
            let t = [k[0], k[1], k[2]];
            if last == Some(t) {
                return Ok(());
            }
            last = Some(t);
        }
        let get = |c: usize| k.get(c).copied().map(Id);
        let (s, o, g) = (get(sc), get(oc), get(gc));
        let Some(o) = o else {
            return Ok(());
        };
        let s = s.unwrap_or(Id::UNDEF);
        if subj.is_some_and(|x| x != s) {
            return Ok(());
        }
        let bad = *memo.entry(o.0).or_insert_with(|| {
            if !matches!(o.tag(), Tag::Vocab | Tag::Delta) {
                return true;
            }
            let slot = base
                .as_ref()
                .and_then(|b| b.column.get(o.0))
                .unwrap_or_else(|| match snap.key(o) {
                    Some(key) => classify(&key, &cfg),
                    None => Slot::Other,
                });
            let geom = match slot {
                Slot::Geom(e) => e.geom(snap).ok(),
                s if s.rechecked() => rechecked.entry(o.0).and_then(|e| e.geom(snap).ok()),
                _ => None,
            };
            geom.is_none_or(|g| !test.measurable(&g))
        });
        if bad {
            out.push([s, o, g.unwrap_or(Id::DEFAULT_GRAPH)]);
        }
        Ok(())
    };
    snap.scan(scan.perm, &scan.prefix, |c| {
        let r = match c {
            Chunk::Block(b, s, e) => (s..e).try_for_each(|i| row(b.key(i))),
            Chunk::Row(k) => row(k),
        };
        match r {
            Ok(()) => Ok(true),
            Err(e) => {
                err = Some(e);
                Ok(false)
            }
        }
    })?;
    match err {
        Some(e) => Err(e),
        None => Ok((
            out,
            memo.into_iter()
                .filter(|(_, bad)| *bad)
                .map(|(o, _)| Id(o))
                .collect(),
        )),
    }
}

/// Whether the distance between a geometry and the constant can be computed, as the
/// function computes it: within the operation limit, and with a transform between the
/// two CRSs in the direction of the call (the first argument's CRS is the frame).
struct Measurable<'a> {
    q: &'a GeomRef,
    w_first: bool,
    op_vertices: u64,
    /// CRS IRI → whether the constant has a transform into it
    crs: FxHashMap<String, bool>,
}

impl Measurable<'_> {
    fn measurable(&mut self, g: &Geom) -> bool {
        if g.empty
            || g.crs.known().is_none()
            || u64::from(g.vertices) + u64::from(self.q.vertices) > self.op_vertices
        {
            return false;
        }
        let geographic = g.crs.known().is_some_and(|c| c.is_geographic());
        if self.w_first {
            // the constant into the row's CRS (nothing to do between geographic CRSs)
            geographic || {
                let q = self.q;
                *self
                    .crs
                    .entry(g.crs.iri().to_string())
                    .or_insert_with(|| in_crs(q, &g.crs).is_ok())
            }
        } else {
            // the row into the constant's (geographic) CRS
            geographic || in_crs(g, &self.q.crs).is_ok()
        }
    }
}
