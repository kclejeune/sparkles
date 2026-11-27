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
use super::column::{Slot, classify_controlled};
use super::exec::{Counters, config, state};
use super::geom::Geom;
use super::ops::in_crs;
use super::search::{self, Rechecked, SearchStats};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{G, O, S};
use crate::sparql::ctx::Ctx;
use crate::sparql::exec::{PlanInfo, execute_materialized};
use crate::sparql::expr::{Row, eval};
use crate::sparql::geojoin::SpatialKnnSpec;
use crate::sparql::geopf::{ScanShape, number};
use crate::sparql::plan::{Kind, Node, PathEnd};
use crate::sparql::table::Materialized;
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
struct Runner<'ctx, 'plan> {
    ctx: &'ctx Ctx,
    spec: &'plan SpatialKnnSpec,
    template: &'plan Node,
    vars: &'plan [VarId],
    out: Materialized<'ctx>,
    runs: Vec<PlanInfo>,
    batches: u64,
    empty_batches: u64,
    /// rows whose key was evaluated
    keyed: u64,
}

impl Runner<'_, '_> {
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
        let (input, info) = execute_materialized(self.ctx, &t)?;
        let res = input.as_table();
        self.batches += 1;
        self.empty_batches += u64::from(res.is_empty());
        if self.runs.len() < RUNS_SHOWN {
            self.runs.push(info);
        }
        let map = res.var_map(self.ctx.nvars());
        let mut keys = Vec::with_capacity(res.len());
        for i in 0..res.len() {
            if i.is_multiple_of(1024) {
                self.ctx.check()?;
            }
            let row = Row {
                table: res,
                i,
                map: &map,
                dec: None,
                exists: None,
            };
            let k = match eval(&spec.order, &row, self.ctx) {
                Ok(v) => match v.value(self.ctx) {
                    Ok(val) => number(&val),
                    Err(_) => {
                        if self.ctx.has_memory_failure() {
                            self.ctx.check()?;
                        }
                        None
                    }
                },
                Err(_) => {
                    if self.ctx.has_memory_failure() {
                        self.ctx.check()?;
                    }
                    None
                }
            };
            keys.push(k);
        }
        self.ctx.check()?;
        self.keyed += keys.len() as u64;
        let projected = input.project(self.ctx, self.vars)?;
        self.out.append_copy(self.ctx, &projected)?;
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
    // Explicitly raw low-level application API. The engine uses the protected
    // materialized entry and transfers its actual output credit into its result.
    let (output, counters, plans) = spatial_knn_materialized(ctx, spec, template, vars)?;
    Ok((output.into_application_table(), counters, plans))
}

pub(crate) fn spatial_knn_materialized<'ctx>(
    ctx: &'ctx Ctx,
    spec: &SpatialKnnSpec,
    template: &Node,
    vars: &[VarId],
) -> Result<(Materialized<'ctx>, Counters, Vec<PlanInfo>)> {
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
        out: Materialized::empty_fast(ctx, vars, &[])?,
        runs: Vec::new(),
        batches: 0,
        empty_batches: 0,
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
        (rows, taken) = rows_without_distance(
            ctx,
            spec,
            subj,
            #[cfg(test)]
            |_| {},
        )?;
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
    c.insert("matched".into(), (run.out.as_table().len() as u64).into());
    c.insert("treeNodesVisited".into(), st.nodes.into());
    c.insert("rechecked".into(), st.rechecked.into());
    c.insert("batches".into(), run.batches.into());
    c.insert("emptyBatches".into(), run.empty_batches.into());
    c.insert("errorRows".into(), (error_rows as u64).into());
    c.insert("index".into(), state(ctx).to_string().into());
    c.insert("fallback".into(), st.fallback.into());
    Ok((run.out, c, run.runs))
}

/// The scan's rows `(s, o, g)` whose literal has no distance to the constant, in scan
/// order (what the nearest-first search never hands out, and what it hands out but
/// cannot be measured), and those literals.
pub(crate) fn rows_without_distance(
    ctx: &Ctx,
    spec: &SpatialKnnSpec,
    subj: Option<Id>,
    #[cfg(test)] observe: impl Fn(&Geom),
) -> Result<(Vec<[Id; 3]>, FxHashSet<Id>)> {
    let mut held = ctx.retained_charge(0)?;
    rows_without_distance_inner(
        ctx,
        spec,
        subj,
        &mut |bytes| {
            if let Some(h) = &mut held {
                h.resize(bytes)?;
            }
            Ok(())
        },
        #[cfg(test)]
        observe,
    )
}

/// [`rows_without_distance`] with pre-admission of `memo`, `rechecked`, output rows, and
/// the returned exclusion set before capacity growth.
pub(crate) fn rows_without_distance_charged(
    ctx: &Ctx,
    spec: &SpatialKnnSpec,
    subj: Option<Id>,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<(Vec<[Id; 3]>, FxHashSet<Id>)> {
    rows_without_distance_inner(
        ctx,
        spec,
        subj,
        admit,
        #[cfg(test)]
        |_| {},
    )
}

fn rows_without_distance_inner(
    ctx: &Ctx,
    spec: &SpatialKnnSpec,
    subj: Option<Id>,
    admit: &mut dyn FnMut(u64) -> Result<()>,
    #[cfg(test)] observe: impl Fn(&Geom),
) -> Result<(Vec<[Id; 3]>, FxHashSet<Id>)> {
    fn next_cap(len: usize, cap: usize, extra: usize) -> usize {
        if len.saturating_add(extra) <= cap {
            cap
        } else {
            cap.saturating_mul(2).max(len.saturating_add(extra)).max(8)
        }
    }
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
    let mut rechecked = Rechecked::new(ctx)?;
    // literal → whether its distance is an error
    let memo_charge = ctx.charge(0)?;
    let mut memo: FxHashMap<u64, bool> = FxHashMap::default();
    let mut out = Vec::new();
    let mut bad_lits = 0usize;
    let mut last: Option<[u64; 3]> = None;
    let mut n = 0usize;
    let mut err = None;
    let bytes_for = |out_cap: usize, memo_cap: usize, re_bytes: u64, taken_cap: usize| -> u64 {
        (out_cap as u64 * size_of::<[Id; 3]>() as u64)
            .saturating_add(memo_cap as u64 * (size_of::<u64>() + size_of::<bool>() + 1) as u64)
            .saturating_add(re_bytes)
            .saturating_add(taken_cap as u64 * (size_of::<Id>() + 1) as u64)
    };
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
        let bad = if let Some(&cached) = memo.get(&o.0) {
            cached
        } else {
            if memo.len() == memo.capacity() {
                admit(bytes_for(
                    out.capacity(),
                    next_cap(memo.len(), memo.capacity(), 1),
                    rechecked.retained_bytes(),
                    0,
                ))?;
            }
            let is_bad = if !matches!(o.tag(), Tag::Vocab | Tag::Delta) {
                true
            } else {
                let parse_charge = ctx.charge(0)?;
                let slot = match base.as_ref().and_then(|b| b.column.get(o.0)) {
                    Some(slot) => slot,
                    None => classify_controlled(ctx, o, &cfg, false, &parse_charge)?,
                };
                let geom = match &slot {
                    Slot::Geom(e) => e.geom(snap).ok(),
                    s if s.rechecked() => {
                        if rechecked.needs_growth(o.0) {
                            admit(bytes_for(
                                out.capacity(),
                                memo.capacity(),
                                rechecked.projected_bytes_after_insert(),
                                0,
                            ))?;
                        }
                        rechecked.entry(o.0)?.and_then(|e| e.geom(snap).ok())
                    }
                    _ => None,
                };
                let bad = geom.is_none_or(|g| {
                    #[cfg(test)]
                    observe(&g);
                    !test.measurable(&g)
                });
                drop(parse_charge);
                bad
            };
            memo_charge.add(32)?;
            memo.insert(o.0, is_bad);
            if is_bad {
                bad_lits += 1;
            }
            is_bad
        };
        if bad {
            if out.len() == out.capacity() {
                admit(bytes_for(
                    next_cap(out.len(), out.capacity(), 1),
                    memo.capacity(),
                    rechecked.retained_bytes(),
                    0,
                ))?;
            }
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
    if let Some(e) = err {
        return Err(e);
    }
    let taken_cap = if bad_lits == 0 {
        0
    } else {
        bad_lits.next_power_of_two().max(8)
    };
    admit(bytes_for(
        out.capacity(),
        memo.capacity(),
        rechecked.retained_bytes(),
        taken_cap,
    ))?;
    let mut taken: FxHashSet<Id> = FxHashSet::default();
    if bad_lits > 0 {
        taken.reserve(bad_lits);
        for (o, bad) in memo {
            if bad {
                taken.insert(Id(o));
            }
        }
    }
    Ok((out, taken))
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

#[cfg(test)]
#[test]
fn fallback_knn_keeps_geometry_credit_during_measurement() {
    use crate::index::Perm;
    use crate::io::{RdfFormat, Source};
    use crate::sparql::expr::Expr;
    use crate::sparql::plan::{GraphFilter, ScanSpec};
    use crate::store::{Store, StoreOptions};
    use oxrdf::{NamedNode, Term};

    let store = Store::in_memory(StoreOptions::default());
    let line = format!(
        "<http://www.opengis.net/def/crs/EPSG/0/3857> LINESTRING({}1 1)",
        "0 0,".repeat(255)
    );
    let data = format!(
        "<urn:feature> <urn:geometry> \"{line}\"^^<{}> .",
        super::WKT_LITERAL
    );
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let ctx = Ctx::new(store.snapshot());
    assert!(
        ctx.snap
            .geo
            .as_ref()
            .and_then(|v| v.base.as_ref())
            .is_none()
    );
    let pred = ctx
        .snap
        .lookup_term(&Term::NamedNode(NamedNode::new_unchecked("urn:geometry")))
        .unwrap();
    let order = Perm::Pos.order();
    let col = |c| order.iter().position(|&x| x == c).unwrap();
    let spec = SpatialKnnSpec {
        scan: ScanSpec {
            perm: Perm::Pos,
            prefix: vec![pred.0],
            cols: vec![(col(O), 0), (col(S), 1)],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: col(G),
            dedup: false,
            src: None,
        },
        pred,
        geom_var: 0,
        subj_var: Some(1),
        graph_var: None,
        q: std::sync::Arc::new(super::parse("POINT(0 0)", super::WKT_LITERAL).unwrap()),
        k: 1,
        metres_per_unit: 1.0,
        errors: true,
        placeholder: Vec::new(),
        key: 0,
        order: Expr::Var(0),
        w_first: false,
    };
    let observations = std::cell::Cell::new(0);
    let (rows, skipped) = rows_without_distance(&ctx, &spec, None, |g| {
        assert_eq!(g.vertices, 256);
        assert!(ctx.live_bytes_for_tests() >= g.mem_size() as u64);
        observations.set(observations.get() + 1);
    })
    .unwrap();
    assert_eq!(observations.get(), 1);
    assert!(rows.is_empty() && skipped.is_empty());
    assert_eq!(ctx.live_bytes_for_tests(), 0);
}
