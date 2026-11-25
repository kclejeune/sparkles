//! Resumable, charged spatial cursor readers (item 829, group 4):
//! - [`SpatialScanReader`]: window search + exact refinement + residual filters
//! - [`SpatialPfReader`]: unlimited `spatial:` property-function leaf
//! - [`SpatialRelateReader`]: asserted + derived topological relation triples
//! - [`SpatialJoinReader`]: streaming/resumable spatial join over index and/or plan sides
//!
//! All readers are `Send` and never retain `Prepared` geometries across pulls; any
//! prepared geometry is built transiently per pull batch. Every retained data structure
//! (search state, literal/feature deduplication sets, build-side tables and packed trees)
//! is charged to the query's memory budget through [`OwnedCharge`] and released on
//! completion, error, or early stop.

use super::GeomRef;
use super::column::round_out;
use super::config::{DistanceModel, GeoConfig};
use super::exec::{
    Check, Counters, Scope, Tester, config, counters, pf_tests, pf_windows, radius_windows,
    scan_keys, state, transpose,
};
use super::geom::Geom;
use super::join::JoinItem;
use super::memo::{MemoKey, max_vertices, note_crs, noted, parse_value};
use super::ops::distance;
use super::ops::relate::{self, Prepared};
use super::search::{self, Hit, SearchStats, WindowIter, next_key_after};
use super::tree::PackedTree;
use super::vocab::{self, Relation};
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::expr::{Row as ExprRow, ebv};
use crate::sparql::geojoin::{JoinSide, JoinTest, SpatialJoinSpec};
use crate::sparql::geopf::{ScanShape, SpatialPfSpec, SpatialScanSpec, SpatialTest};
use crate::sparql::georewrite::{RelateEnd, SpatialRelateSpec};
use crate::sparql::plan::{GraphFilter, PathEnd, ScanSpec};
use crate::sparql::table::{Table, VarId};
use crate::store::Chunk;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_budget::sizes::{table_bytes, table_grown, vec_grown};
use std::collections::VecDeque;
use std::sync::Arc;

const CHECK_EVERY: usize = 256;
const CHECK_CANDIDATES: u64 = 4096;
const PREPARE_OVER: usize = 8;
const WORLD: [f64; 4] = [-180.0, -90.0, 180.0, 90.0];
const JOIN_WORLD: [f64; 4] = [-1e30, -1e30, 1e30, 1e30];

#[inline]
fn next_cap(len: usize, cap: usize, extra: usize) -> usize {
    if len.saturating_add(extra) <= cap {
        cap
    } else {
        cap.saturating_mul(2).max(len.saturating_add(extra)).max(8)
    }
}

fn same_frame(a: &Geom, b: &Geom) -> bool {
    a.crs == b.crs
        || matches!(
            (a.crs.known(), b.crs.known()),
            (Some(x), Some(y)) if x.is_geographic() && y.is_geographic()
        )
}

fn meets(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

fn placed(b: &[f64; 4]) -> bool {
    !b[0].is_nan()
}

fn item_box(g: &Geom) -> Option<[f64; 4]> {
    if g.empty {
        return None;
    }
    Some(match (g.bbox84(), g.crs.known()) {
        (Some(b), _) => b,
        (None, Some(_)) => [-180.0, -90.0, 180.0, 90.0],
        (None, None) => [f64::NAN; 4],
    })
}

fn join_windows(test: &JoinTest, b: [f64; 4]) -> Vec<[f64; 4]> {
    match test {
        JoinTest::Within { metres, .. } => radius_windows(b, *metres),
        _ => vec![b],
    }
}

fn table_capacity_bytes(table: &Table) -> u64 {
    let columns = table
        .cols
        .iter()
        .map(|c| c.capacity() as u64 * 8)
        .sum::<u64>();
    columns
        .max(table.len as u64 * 8 * u64::from(table.width() == 0))
        .saturating_add(table.vars.capacity() as u64 * 4)
        .saturating_add(table.sorted.capacity() as u64 * 4)
}

fn filter_table(ctx: &Ctx, table: &mut Table, filter: &[crate::sparql::expr::Expr]) -> Result<()> {
    if filter.is_empty() || table.is_empty() {
        return Ok(());
    }
    let map = table.var_map(ctx.nvars());
    let mut keep = Vec::with_capacity(table.len());
    for i in 0..table.len() {
        if i.is_multiple_of(CHECK_EVERY) {
            ctx.check()?;
        }
        let row = ExprRow {
            table,
            i,
            map: &map,
            dec: None,
            exists: None,
        };
        keep.push(filter.iter().all(|e| ebv(e, &row, ctx).unwrap_or(false)));
    }
    table.filter_rows(&keep);
    Ok(())
}

// ------------------------------------------------------------------ SpatialScan --

/// Resumable streaming reader for `SpatialScan` (item 829, group 4 / 6a).
pub(crate) struct SpatialScanReader {
    spec: SpatialScanSpec,
    vars: Vec<VarId>,
    cols: Vec<usize>,
    subj: Option<Id>,
    model: DistanceModel,
    state_name: String,
    iter: Option<WindowIter>,
    pending: VecDeque<Hit>,
    seen_lit: FxHashMap<Id, bool>,
    st: SearchStats,
    charge: OwnedCharge,
}

impl SpatialScanReader {
    pub(crate) fn open(ctx: &Arc<Ctx>, spec: &SpatialScanSpec, vars: &[VarId]) -> Result<Self> {
        let shape = ScanShape::of(&spec.scan)
            .ok_or_else(|| Error::invalid("spatial scan of an unexpected pattern"))?;
        let cfg = config(ctx);
        let state_name = state(ctx).to_string();
        let graph = shape.graph_filter(&spec.scan);
        let subj = match shape.subj {
            Some(PathEnd::Const(s)) => Some(s),
            _ => None,
        };
        let (sv, gv) = (shape.subj_var(), shape.graph_var());
        let cols: Vec<usize> = vars
            .iter()
            .map(|v| {
                if Some(*v) == sv {
                    0
                } else if *v == shape.obj {
                    1
                } else if Some(*v) == gv {
                    2
                } else {
                    usize::MAX
                }
            })
            .collect();
        let mut charge = OwnedCharge::new(ctx, 0)?;
        let iter = WindowIter::open(
            ctx,
            vec![spec.pred],
            spec.windows.clone(),
            graph,
            spec.scan.dedup,
            &mut |b| charge.resize(b),
        )?;
        Ok(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            cols,
            subj,
            model: cfg.distance,
            state_name,
            iter: Some(iter),
            pending: VecDeque::new(),
            seen_lit: FxHashMap::default(),
            st: SearchStats::default(),
            charge,
        })
    }

    pub(crate) fn counters(&self) -> Counters {
        let mut st = self.st.clone();
        if let Some(iter) = &self.iter {
            st.nodes = iter.st.nodes;
            st.rechecked = iter.st.rechecked;
            st.fallback = iter.st.fallback;
        }
        counters(&st, self.state_name.clone())
    }

    /// Fill `out` with up to `cap` matching rows. Returns `true` when exhausted.
    pub(crate) fn next(&mut self, ctx: &Ctx, out: &mut Table, cap: usize) -> Result<bool> {
        let cap = cap.max(1);
        let check = Check {
            windows: &self.spec.windows,
            envelope_test: false,
            tests: &self.spec.tests,
            rank: None,
            model: self.model,
        };
        let mut tester = Tester::new(ctx, &check);
        let mut row = vec![Id::UNDEF; self.vars.len()];
        let mut refined_since_check = 0usize;
        let mut raw = if self.spec.filter.is_empty() {
            None
        } else {
            Some(Table::new(self.vars.clone()))
        };
        let bytes_for = |iter: &Option<WindowIter>, pending_cap: usize, seen_cap: usize| -> u64 {
            let iter_bytes = iter.as_ref().map_or(0, WindowIter::retained_bytes);
            let pending_bytes = pending_cap as u64 * size_of::<Hit>() as u64;
            let seen_bytes = seen_cap as u64 * (size_of::<Id>() + 2) as u64;
            iter_bytes
                .saturating_add(pending_bytes)
                .saturating_add(seen_bytes)
        };
        let resize_charge = |iter: &Option<WindowIter>,
                             pending: &VecDeque<Hit>,
                             seen_lit: &FxHashMap<Id, bool>,
                             charge: &mut OwnedCharge|
         -> Result<()> {
            charge.resize(bytes_for(iter, pending.capacity(), seen_lit.capacity()))
        };
        loop {
            ctx.check()?;
            if self.pending.is_empty() {
                let Some(iter) = &mut self.iter else {
                    return Ok(true);
                };
                let mut buf = Vec::new();
                let chunk_want = cap.clamp(64, 4096);
                let seen_bytes = self.seen_lit.capacity() as u64 * (size_of::<Id>() + 2) as u64;
                let charge = &mut self.charge;
                let ended = iter.next_hits(ctx, &mut buf, chunk_want, &mut |b| {
                    charge.resize(b.saturating_add(seen_bytes))
                })?;
                if ended {
                    self.st.nodes = iter.st.nodes;
                    self.st.rechecked = iter.st.rechecked;
                    self.st.fallback = iter.st.fallback;
                    self.iter = None;
                }
                if !buf.is_empty() {
                    let next_p = next_cap(self.pending.len(), self.pending.capacity(), buf.len());
                    self.charge
                        .resize(bytes_for(&self.iter, next_p, self.seen_lit.capacity()))?;
                    self.pending.extend(buf);
                }
                resize_charge(&self.iter, &self.pending, &self.seen_lit, &mut self.charge)?;
            }
            let want = cap.saturating_sub(out.len());
            let target_limit = if raw.is_some() { want } else { cap };
            let target = raw.as_mut().unwrap_or(out);
            while target.len() < target_limit {
                let Some(h) = self.pending.pop_front() else {
                    break;
                };
                if self.subj.is_some_and(|s| s != h.s) {
                    continue;
                }
                self.st.candidates += 1;
                if self.st.candidates.is_multiple_of(CHECK_CANDIDATES) {
                    ctx.check()?;
                    ctx.check_rows(self.st.candidates as usize)?;
                }
                let ok = if let Some(&v) = self.seen_lit.get(&h.o) {
                    v
                } else {
                    self.st.refined += 1;
                    refined_since_check += 1;
                    if refined_since_check.is_multiple_of(CHECK_EVERY) {
                        ctx.check()?;
                    }
                    if self.seen_lit.len() == self.seen_lit.capacity() {
                        let next_s = next_cap(self.seen_lit.len(), self.seen_lit.capacity(), 1);
                        self.charge.resize(bytes_for(
                            &self.iter,
                            self.pending.capacity(),
                            next_s,
                        ))?;
                    }
                    let pass = noted(ctx, h.entry.geom(&ctx.snap))
                        .ok()
                        .and_then(|g| tester.check(&g))
                        .is_some();
                    self.seen_lit.insert(h.o, pass);
                    pass
                };
                if !ok {
                    continue;
                }
                let r = [h.s, h.o, h.g];
                for (x, &c) in row.iter_mut().zip(&self.cols) {
                    *x = r.get(c).copied().unwrap_or(Id::UNDEF);
                }
                target.push_row(&row);
            }
            if let Some(buf) = &mut raw {
                filter_table(ctx, buf, &self.spec.filter)?;
                if !buf.is_empty() {
                    let take = buf.len().min(cap.saturating_sub(out.len()));
                    let mut tmp = vec![Id::UNDEF; self.vars.len()];
                    for i in 0..take {
                        for (c, slot) in tmp.iter_mut().enumerate() {
                            *slot = buf.cols[c][i];
                        }
                        out.push_row(&tmp);
                    }
                    for col in &mut buf.cols {
                        col.clear();
                    }
                    buf.len = 0;
                }
            }
            resize_charge(&self.iter, &self.pending, &self.seen_lit, &mut self.charge)?;
            if out.len() >= cap || (self.pending.is_empty() && self.iter.is_none()) {
                self.st.matched += out.len() as u64;
                return Ok(self.pending.is_empty() && self.iter.is_none());
            }
        }
    }
}

// ------------------------------------------------------------------ SpatialPf --

struct ActiveLinkScan {
    geom_s: Id,
    h_g: Id,
    g_key: Id,
    link_at: usize,
    scan: Option<PrefixScan>,
}

/// Resumable streaming reader for unlimited `SpatialPf` without input (item 829, group 4 / 6a).
pub(crate) struct SpatialPfReader {
    spec: SpatialPfSpec,
    vars: Vec<VarId>,
    fv: Option<VarId>,
    links: Vec<Id>,
    windows: Vec<[f64; 4]>,
    tests: Vec<SpatialTest>,
    model: DistanceModel,
    how: String,
    iter: Option<WindowIter>,
    pending_hits: VecDeque<Hit>,
    pending_feats: VecDeque<(Id, Id)>,
    active_links: Option<ActiveLinkScan>,
    seen_lit: FxHashMap<Id, bool>,
    seen_geom: FxHashSet<(Id, Id)>,
    seen_feat: FxHashSet<(Id, Id)>,
    st: SearchStats,
    charge: OwnedCharge,
}

impl SpatialPfReader {
    pub(crate) fn open(ctx: &Arc<Ctx>, spec: &SpatialPfSpec, vars: &[VarId]) -> Result<Self> {
        ctx.check()?;
        let cfg = config(ctx);
        let st_str = state(ctx).to_string();
        let ids = |iris: &[String]| -> Vec<Id> {
            iris.iter().filter_map(|i| ctx.snap.lookup_iri(i)).collect()
        };
        let (mut preds, links) = (ids(&cfg.predicates), ids(&cfg.feature_links));
        let lat = search::wgs84_lat(&ctx.snap);
        preds.extend(lat);
        let windows = pf_windows(spec.func, &spec.query, spec.radius_m);
        let tests = pf_tests(spec);
        let fv = match spec.subject {
            PathEnd::Var(v) => Some(v),
            PathEnd::Const(_) => None,
        };
        let how = match spec.subject {
            PathEnd::Const(_) => "feature-links".to_string(),
            PathEnd::Var(_) => st_str,
        };
        let mut charge = OwnedCharge::new(ctx, 0)?;
        let mut st = SearchStats::default();
        let mut pending_feats = VecDeque::new();
        let mut seen_feat = FxHashSet::default();
        let iter = if windows.is_empty() {
            None
        } else if let PathEnd::Const(f) = &spec.subject {
            // A constant feature: scan its own links and W3C Basic Geo points.
            let check = Check {
                windows: &windows,
                envelope_test: tests.is_empty(),
                tests: &tests,
                rank: None,
                model: cfg.distance,
            };
            let mut tester = Tester::new(ctx, &check);
            let mut lit_memo: FxHashMap<Id, bool> = FxHashMap::default();
            let mut scope = Scope::new(&cfg);
            let mut scanned = 0usize;
            let open_bytes = |lit_cap: usize, seen_cap: usize, pend_cap: usize| -> u64 {
                (lit_cap as u64 * (size_of::<Id>() + 2) as u64)
                    .saturating_add(seen_cap as u64 * (size_of::<(Id, Id)>() + 1) as u64)
                    .saturating_add(pend_cap as u64 * size_of::<(Id, Id)>() as u64)
            };
            for l in &links {
                scan_keys(ctx, Perm::Spo, &[f.0, l.0], |k| {
                    scanned += 1;
                    if scanned.is_multiple_of(CHECK_EVERY) {
                        ctx.check()?;
                    }
                    let (geom, g1) = (k[2], k[3]);
                    if !spec.graph.accepts(g1) {
                        return Ok(());
                    }
                    for p in &preds {
                        scan_keys(ctx, Perm::Spo, &[geom, p.0], |k2| {
                            scanned += 1;
                            if scanned.is_multiple_of(CHECK_EVERY) {
                                ctx.check()?;
                            }
                            let g = k2[3];
                            if !spec.graph.accepts(g) || !scope.contains(ctx, g) {
                                return Ok(());
                            }
                            if spec.graph_var.is_some() && g1 != g {
                                return Ok(());
                            }
                            let o = Id(k2[2]);
                            st.candidates += 1;
                            if st.candidates.is_multiple_of(CHECK_CANDIDATES) {
                                ctx.check()?;
                                ctx.check_rows(st.candidates as usize)?;
                            }
                            let ok = if let Some(&v) = lit_memo.get(&o) {
                                v
                            } else {
                                if lit_memo.len() == lit_memo.capacity() {
                                    charge.resize(open_bytes(
                                        next_cap(lit_memo.len(), lit_memo.capacity(), 1),
                                        seen_feat.capacity(),
                                        pending_feats.capacity(),
                                    ))?;
                                }
                                st.refined += 1;
                                let v = (|| {
                                    let v = ctx.value(o)?;
                                    let max = max_vertices(ctx);
                                    let parsed = ctx.geo.get_or_parse_charged(
                                        ctx,
                                        MemoKey::Id(o),
                                        || parse_value(&v, max),
                                    )?;
                                    note_crs(ctx, &parsed);
                                    Some(tester.check(&parsed).is_some())
                                })()
                                .unwrap_or(false);
                                lit_memo.insert(o, v);
                                v
                            };
                            if ok {
                                st.matched += 1;
                                let g_key = if spec.graph_var.is_some() {
                                    Id(g)
                                } else {
                                    Id::UNDEF
                                };
                                if !seen_feat.contains(&(*f, g_key)) {
                                    if seen_feat.len() == seen_feat.capacity()
                                        || pending_feats.len() == pending_feats.capacity()
                                    {
                                        charge.resize(open_bytes(
                                            lit_memo.capacity(),
                                            next_cap(seen_feat.len(), seen_feat.capacity(), 1),
                                            next_cap(
                                                pending_feats.len(),
                                                pending_feats.capacity(),
                                                1,
                                            ),
                                        ))?;
                                    }
                                    seen_feat.insert((*f, g_key));
                                    pending_feats.push_back((*f, g_key));
                                }
                            }
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
            }
            if let (Some(lat_p), Some((_, long_p))) = (lat, super::wgs84::predicates(&ctx.snap)) {
                let mut scope = Scope::new(&cfg);
                scan_keys(ctx, Perm::Spo, &[f.0, lat_p.0], |k| {
                    scanned += 1;
                    if scanned.is_multiple_of(CHECK_EVERY) {
                        ctx.check()?;
                    }
                    let (lat_o, g) = (k[2], k[3]);
                    if !spec.graph.accepts(g) || !scope.contains(ctx, g) {
                        return Ok(());
                    }
                    let Some(y) = super::wgs84::number(&ctx.snap, Id(lat_o)) else {
                        return Ok(());
                    };
                    super::wgs84::visit_objects(
                        &ctx.snap,
                        [f.0, long_p.0, 0, 0],
                        [f.0, long_p.0, u64::MAX, u64::MAX],
                        g,
                        || ctx.check(),
                        |long_o| {
                            let mut point_charge = None;
                            let e = if let Some(x) = super::wgs84::number(&ctx.snap, Id(long_o))
                                .filter(|x| {
                                    (-180.0..=180.0).contains(x) && (-90.0..=90.0).contains(&y)
                                }) {
                                point_charge = Some(ctx.charge(super::wgs84::POINT_BYTES)?);
                                super::wgs84::point(y, x)
                            } else {
                                None
                            };
                            if let Some(e) = e {
                                st.candidates += 1;
                                if st.candidates.is_multiple_of(CHECK_CANDIDATES) {
                                    ctx.check()?;
                                    ctx.check_rows(st.candidates as usize)?;
                                }
                                st.refined += 1;
                                if noted(ctx, e.geom(&ctx.snap))
                                    .ok()
                                    .and_then(|geom| tester.check(&geom))
                                    .is_some()
                                {
                                    st.matched += 1;
                                    let g_key = if spec.graph_var.is_some() {
                                        Id(g)
                                    } else {
                                        Id::UNDEF
                                    };
                                    if !seen_feat.contains(&(*f, g_key)) {
                                        if seen_feat.len() == seen_feat.capacity()
                                            || pending_feats.len() == pending_feats.capacity()
                                        {
                                            charge.resize(open_bytes(
                                                lit_memo.capacity(),
                                                next_cap(seen_feat.len(), seen_feat.capacity(), 1),
                                                next_cap(
                                                    pending_feats.len(),
                                                    pending_feats.capacity(),
                                                    1,
                                                ),
                                            ))?;
                                        }
                                        seen_feat.insert((*f, g_key));
                                        pending_feats.push_back((*f, g_key));
                                    }
                                }
                            }
                            drop(point_charge);
                            Ok(true)
                        },
                    )?;
                    Ok(())
                })?;
            }
            None
        } else {
            Some(WindowIter::open(
                ctx,
                preds,
                windows.clone(),
                spec.graph.clone(),
                spec.dedup,
                &mut |b| charge.resize(b),
            )?)
        };
        let mut reader = Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            fv,
            links,
            windows,
            tests,
            model: cfg.distance,
            how,
            iter,
            pending_hits: VecDeque::new(),
            pending_feats,
            active_links: None,
            seen_lit: FxHashMap::default(),
            seen_geom: FxHashSet::default(),
            seen_feat,
            st,
            charge,
        };
        reader.update_charge()?;
        Ok(reader)
    }

    fn update_charge(&mut self) -> Result<()> {
        let iter_bytes = self.iter.as_ref().map_or(0, WindowIter::retained_bytes);
        let hits = self.pending_hits.capacity() as u64 * size_of::<Hit>() as u64;
        let feats = self.pending_feats.capacity() as u64 * size_of::<(Id, Id)>() as u64;
        let lits = self.seen_lit.capacity() as u64 * (size_of::<Id>() + 2) as u64;
        let geoms = self.seen_geom.capacity() as u64 * (size_of::<(Id, Id)>() + 1) as u64;
        let seen = self.seen_feat.capacity() as u64 * (size_of::<(Id, Id)>() + 1) as u64;
        self.charge.resize(
            iter_bytes
                .saturating_add(hits)
                .saturating_add(feats)
                .saturating_add(lits)
                .saturating_add(geoms)
                .saturating_add(seen),
        )
    }

    pub(crate) fn counters(&self) -> Counters {
        let mut st = self.st.clone();
        if let Some(iter) = &self.iter {
            st.nodes = iter.st.nodes;
            st.rechecked = iter.st.rechecked;
            st.fallback = iter.st.fallback;
        }
        counters(&st, self.how.clone())
    }

    /// Fill `out` with up to `cap` rows. Returns `true` when exhausted.
    pub(crate) fn next(&mut self, ctx: &Ctx, out: &mut Table, cap: usize) -> Result<bool> {
        let cap = cap.max(1);
        let check = Check {
            windows: &self.windows,
            envelope_test: self.tests.is_empty(),
            tests: &self.tests,
            rank: None,
            model: self.model,
        };
        let mut tester = Tester::new(ctx, &check);
        let mut row = vec![Id::UNDEF; self.vars.len()];
        let same_fg = self.fv.is_some() && self.fv == self.spec.graph_var;
        let emit_row = |f: Id, g: Id, out: &mut Table, row: &mut [Id]| {
            for (x, v) in row.iter_mut().zip(&self.vars) {
                *x = if Some(*v) == self.fv {
                    f
                } else if Some(*v) == self.spec.graph_var {
                    g
                } else {
                    Id::UNDEF
                };
            }
            out.push_row(row);
        };
        // Fallback Basic Geo hits own their point data. Indexed entries are shared.
        let generated = std::cell::Cell::new(
            self.pending_hits.len() as u64
                * u64::from(
                    self.iter.as_ref().is_some_and(|it| it.st.fallback)
                        && self
                            .pending_hits
                            .front()
                            .is_some_and(|h| super::wgs84::is_pair(h.o.0)),
                ),
        );
        let bytes_for = |iter: &Option<WindowIter>,
                         hits_cap: usize,
                         feats_cap: usize,
                         lits_cap: usize,
                         geoms_cap: usize,
                         seen_cap: usize|
         -> u64 {
            let iter_bytes = iter.as_ref().map_or(0, WindowIter::retained_bytes);
            let hits = hits_cap as u64 * size_of::<Hit>() as u64;
            let feats = feats_cap as u64 * size_of::<(Id, Id)>() as u64;
            let lits = lits_cap as u64 * (size_of::<Id>() + 2) as u64;
            let geoms = geoms_cap as u64 * (size_of::<(Id, Id)>() + 1) as u64;
            let seen = seen_cap as u64 * (size_of::<(Id, Id)>() + 1) as u64;
            iter_bytes
                .saturating_add(hits)
                .saturating_add(generated.get() * super::wgs84::POINT_BYTES)
                .saturating_add(feats)
                .saturating_add(lits)
                .saturating_add(geoms)
                .saturating_add(seen)
        };
        let resize_charge = |iter: &Option<WindowIter>,
                             pending_hits: &VecDeque<Hit>,
                             pending_feats: &VecDeque<(Id, Id)>,
                             seen_lit: &FxHashMap<Id, bool>,
                             seen_geom: &FxHashSet<(Id, Id)>,
                             seen_feat: &FxHashSet<(Id, Id)>,
                             charge: &mut OwnedCharge|
         -> Result<()> {
            charge.resize(bytes_for(
                iter,
                pending_hits.capacity(),
                pending_feats.capacity(),
                seen_lit.capacity(),
                seen_geom.capacity(),
                seen_feat.capacity(),
            ))
        };
        let mut refined_since_check = 0usize;
        loop {
            ctx.check()?;
            while out.len() < cap {
                let Some((f, g)) = self.pending_feats.pop_front() else {
                    break;
                };
                emit_row(f, g, out, &mut row);
            }
            if out.len() >= cap {
                resize_charge(
                    &self.iter,
                    &self.pending_hits,
                    &self.pending_feats,
                    &self.seen_lit,
                    &self.seen_geom,
                    &self.seen_feat,
                    &mut self.charge,
                )?;
                return Ok(false);
            }
            // Step any active reverse feature-link scan in bounded chunks.
            if let Some(act) = &mut self.active_links {
                if act.scan.is_none() {
                    if act.link_at >= self.links.len() {
                        self.active_links = None;
                        continue;
                    }
                    let l = self.links[act.link_at];
                    act.link_at += 1;
                    act.scan = Some(PrefixScan::new(Perm::Pos, &[l.0, act.geom_s.0]));
                }
                let (h_g, g_key) = (act.h_g, act.g_key);
                let mut raw = Vec::new();
                let base_now = bytes_for(
                    &self.iter,
                    self.pending_hits.capacity(),
                    self.pending_feats.capacity(),
                    self.seen_lit.capacity(),
                    self.seen_geom.capacity(),
                    self.seen_feat.capacity(),
                );
                let charge = &mut self.charge;
                let ended = act.scan.as_mut().expect("link scan").next_chunk(
                    ctx,
                    &mut raw,
                    cap.clamp(64, 4096),
                    &mut |raw_bytes| charge.resize(base_now.saturating_add(raw_bytes)),
                )?;
                if ended {
                    act.scan = None;
                }
                let raw_bytes = raw.capacity() as u64 * size_of::<[u64; 4]>() as u64;
                let mut link_steps = 0usize;
                for k in raw {
                    link_steps += 1;
                    if link_steps.is_multiple_of(CHECK_EVERY) {
                        ctx.check()?;
                    }
                    let (f, g1) = (Id(k[2]), k[3]);
                    let same = match self.spec.graph_var {
                        Some(_) => g1 == h_g.0,
                        None => self.spec.graph.accepts(g1),
                    };
                    if !same || matches!(self.spec.subject, PathEnd::Const(c) if c != f) {
                        continue;
                    }
                    if same_fg && f != g_key {
                        continue;
                    }
                    if !self.seen_feat.contains(&(f, g_key)) {
                        if self.seen_feat.len() == self.seen_feat.capacity()
                            || (out.len() >= cap
                                && self.pending_feats.len() == self.pending_feats.capacity())
                        {
                            self.charge.resize(
                                bytes_for(
                                    &self.iter,
                                    self.pending_hits.capacity(),
                                    if out.len() >= cap {
                                        next_cap(
                                            self.pending_feats.len(),
                                            self.pending_feats.capacity(),
                                            1,
                                        )
                                    } else {
                                        self.pending_feats.capacity()
                                    },
                                    self.seen_lit.capacity(),
                                    self.seen_geom.capacity(),
                                    next_cap(self.seen_feat.len(), self.seen_feat.capacity(), 1),
                                )
                                .saturating_add(raw_bytes),
                            )?;
                        }
                        self.seen_feat.insert((f, g_key));
                        if out.len() < cap {
                            emit_row(f, g_key, out, &mut row);
                        } else {
                            self.pending_feats.push_back((f, g_key));
                        }
                    }
                }
                resize_charge(
                    &self.iter,
                    &self.pending_hits,
                    &self.pending_feats,
                    &self.seen_lit,
                    &self.seen_geom,
                    &self.seen_feat,
                    &mut self.charge,
                )?;
                if out.len() >= cap {
                    return Ok(false);
                }
                continue;
            }
            if self.pending_hits.is_empty() {
                let Some(iter) = &mut self.iter else {
                    resize_charge(
                        &self.iter,
                        &self.pending_hits,
                        &self.pending_feats,
                        &self.seen_lit,
                        &self.seen_geom,
                        &self.seen_feat,
                        &mut self.charge,
                    )?;
                    return Ok(true);
                };
                let mut buf = Vec::new();
                let chunk_want = cap.clamp(64, 4096);
                let base_bytes = bytes_for(
                    &None,
                    self.pending_hits.capacity(),
                    self.pending_feats.capacity(),
                    self.seen_lit.capacity(),
                    self.seen_geom.capacity(),
                    self.seen_feat.capacity(),
                );
                let charge = &mut self.charge;
                let ended = iter.next_hits(ctx, &mut buf, chunk_want, &mut |b| {
                    charge.resize(b.saturating_add(base_bytes))
                })?;
                if iter.st.fallback && buf.first().is_some_and(|h| super::wgs84::is_pair(h.o.0)) {
                    generated.set(buf.len() as u64);
                }
                if ended {
                    self.st.nodes = iter.st.nodes;
                    self.st.rechecked = iter.st.rechecked;
                    self.st.fallback = iter.st.fallback;
                    self.iter = None;
                }
                if !buf.is_empty() {
                    let next_h = next_cap(
                        self.pending_hits.len(),
                        self.pending_hits.capacity(),
                        buf.len(),
                    );
                    let retired = if next_h > self.pending_hits.capacity() {
                        self.pending_hits.capacity()
                    } else {
                        0
                    };
                    let scratch = (buf.capacity() + retired) as u64 * size_of::<Hit>() as u64;
                    self.charge.resize(
                        bytes_for(
                            &self.iter,
                            next_h,
                            self.pending_feats.capacity(),
                            self.seen_lit.capacity(),
                            self.seen_geom.capacity(),
                            self.seen_feat.capacity(),
                        )
                        .saturating_add(scratch),
                    )?;
                    self.pending_hits.extend(buf);
                }
                resize_charge(
                    &self.iter,
                    &self.pending_hits,
                    &self.pending_feats,
                    &self.seen_lit,
                    &self.seen_geom,
                    &self.seen_feat,
                    &mut self.charge,
                )?;
                if self.pending_hits.is_empty() && self.iter.is_none() {
                    return Ok(true);
                }
            }
            while out.len() < cap && self.active_links.is_none() {
                let Some(h) = self.pending_hits.pop_front() else {
                    break;
                };
                let generated_point = generated.get() > 0;
                if generated_point {
                    generated.set(self.pending_hits.len() as u64 + 1);
                }
                self.st.candidates += 1;
                if self.st.candidates.is_multiple_of(CHECK_CANDIDATES) {
                    ctx.check()?;
                    ctx.check_rows(self.st.candidates as usize)?;
                }
                let ok = if generated_point {
                    self.st.refined += 1;
                    noted(ctx, h.entry.geom(&ctx.snap))
                        .ok()
                        .and_then(|g| tester.check(&g))
                        .is_some()
                } else if let Some(&v) = self.seen_lit.get(&h.o) {
                    v
                } else {
                    self.st.refined += 1;
                    refined_since_check += 1;
                    if refined_since_check.is_multiple_of(CHECK_EVERY) {
                        ctx.check()?;
                    }
                    if self.seen_lit.len() == self.seen_lit.capacity() {
                        self.charge.resize(bytes_for(
                            &self.iter,
                            self.pending_hits.capacity(),
                            self.pending_feats.capacity(),
                            next_cap(self.seen_lit.len(), self.seen_lit.capacity(), 1),
                            self.seen_geom.capacity(),
                            self.seen_feat.capacity(),
                        ))?;
                    }
                    let pass = noted(ctx, h.entry.geom(&ctx.snap))
                        .ok()
                        .and_then(|g| tester.check(&g))
                        .is_some();
                    self.seen_lit.insert(h.o, pass);
                    pass
                };
                if !ok {
                    continue;
                }
                self.st.matched += 1;
                let g_key = if self.spec.graph_var.is_some() {
                    h.g
                } else {
                    Id::UNDEF
                };
                if super::wgs84::is_pair(h.o.0) {
                    if matches!(self.spec.subject, PathEnd::Const(c) if c != h.s) {
                        continue;
                    }
                    if same_fg && h.s != g_key {
                        continue;
                    }
                    if !self.seen_feat.contains(&(h.s, g_key)) {
                        if self.seen_feat.len() == self.seen_feat.capacity() {
                            self.charge.resize(bytes_for(
                                &self.iter,
                                self.pending_hits.capacity(),
                                self.pending_feats.capacity(),
                                self.seen_lit.capacity(),
                                self.seen_geom.capacity(),
                                next_cap(self.seen_feat.len(), self.seen_feat.capacity(), 1),
                            ))?;
                        }
                        self.seen_feat.insert((h.s, g_key));
                        emit_row(h.s, g_key, out, &mut row);
                    }
                    continue;
                }
                if self.seen_geom.contains(&(h.s, h.g)) {
                    continue;
                }
                if self.seen_geom.len() == self.seen_geom.capacity() {
                    self.charge.resize(bytes_for(
                        &self.iter,
                        self.pending_hits.capacity(),
                        self.pending_feats.capacity(),
                        self.seen_lit.capacity(),
                        next_cap(self.seen_geom.len(), self.seen_geom.capacity(), 1),
                        self.seen_feat.capacity(),
                    ))?;
                }
                self.seen_geom.insert((h.s, h.g));
                if !self.links.is_empty() {
                    self.active_links = Some(ActiveLinkScan {
                        geom_s: h.s,
                        h_g: h.g,
                        g_key,
                        link_at: 0,
                        scan: None,
                    });
                }
            }
            generated.set(generated.get().min(self.pending_hits.len() as u64));
            resize_charge(
                &self.iter,
                &self.pending_hits,
                &self.pending_feats,
                &self.seen_lit,
                &self.seen_geom,
                &self.seen_feat,
                &mut self.charge,
            )?;
            if out.len() >= cap {
                return Ok(false);
            }
        }
    }
}

// ---------------------------------------------------------------- SpatialRelate --

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Ser {
    s: Id,
    o: Id,
    g: Id,
}

#[derive(Clone, Copy, Debug)]
struct ConstLit {
    lit: Id,
    g: Option<Id>,
}

fn const_lit_bytes(lits: &Vec<ConstLit>) -> u64 {
    lits.capacity() as u64 * size_of::<ConstLit>() as u64
}

fn hash_growth<T>(capacity: usize, len: usize) -> u64 {
    let next = table_grown(capacity, len, 1);
    if next == capacity {
        0
    } else {
        table_bytes(next, size_of::<T>()) as u64
    }
}

fn lit_pass_bytes(lit_pass: &FxHashMap<Id, bool>) -> u64 {
    table_bytes(lit_pass.capacity(), size_of::<(Id, bool)>()) as u64
}

struct RelateItem {
    lit: Id,
    geom: GeomRef,
    bbox: Option<[f64; 4]>,
}

struct ConstTest {
    q: GeomRef,
    rel: Relation,
    q_first: bool,
    prep: Option<Prepared>,
    op_vertices: u64,
}

impl ConstTest {
    fn new(q: GeomRef, rel: Relation, q_first: bool, op_vertices: u64) -> Self {
        Self {
            q,
            rel,
            q_first,
            prep: None,
            op_vertices,
        }
    }

    fn prepared(&mut self) -> &Prepared {
        let q = &self.q;
        self.prep.get_or_insert_with(|| Prepared::new(q.clone()))
    }

    fn holds(&mut self, w: &Geom) -> bool {
        if u64::from(w.vertices) + u64::from(self.q.vertices) > self.op_vertices {
            return false;
        }
        let rel = self.rel;
        if self.q_first {
            return self.prepared().relation(w, rel).unwrap_or(false);
        }
        match rel.converse() {
            Some(c) if same_frame(w, &self.q) => self.prepared().relation(w, c).unwrap_or(false),
            _ => relate::relation(w, &self.q, rel).unwrap_or(false),
        }
    }
}

fn relate_relation(a: &Geom, b: &Geom, rel: Relation, op_vertices: u64) -> bool {
    u64::from(a.vertices) + u64::from(b.vertices) <= op_vertices
        && relate::relation(a, b, rel).unwrap_or(false)
}

/// Resumable prefix scanner over one `Perm` + prefix.
struct PrefixScan {
    perm: Perm,
    lo: Option<[u64; 4]>,
    hi: [u64; 4],
    done: bool,
}

impl PrefixScan {
    fn new(perm: Perm, prefix: &[u64]) -> Self {
        Self {
            perm,
            lo: Some(crate::index::pad(prefix, 0)),
            hi: crate::index::pad(prefix, u64::MAX),
            done: false,
        }
    }

    fn next_chunk(
        &mut self,
        ctx: &Ctx,
        out: &mut Vec<[u64; 4]>,
        limit: usize,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<bool> {
        out.clear();
        if self.done {
            return Ok(true);
        }
        let Some(start) = self.lo else {
            self.done = true;
            return Ok(true);
        };
        let mut last: Option<[u64; 4]> = None;
        let mut stopped = false;
        let mut scanned = 0usize;
        let mut err: Option<Error> = None;
        ctx.snap.scan_between(self.perm, start, self.hi, |c| {
            let mut push = |k: [u64; 4]| -> Result<bool> {
                scanned += 1;
                if scanned.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let elem = size_of::<[u64; 4]>();
                let grew = out.len() == out.capacity();
                if grew {
                    let cur = out.capacity() as u64 * elem as u64;
                    let next = vec_grown(out.capacity(), out.len(), 1, elem) as u64 * elem as u64;
                    admit(cur.saturating_add(next))?;
                }
                out.push(k);
                if grew {
                    admit(out.capacity() as u64 * elem as u64)?;
                }
                last = Some(k);
                if out.len() >= limit {
                    stopped = true;
                    return Ok(false);
                }
                Ok(true)
            };
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        match push(b.key(i)) {
                            Ok(true) => {}
                            Ok(false) => return Ok(false),
                            Err(e) => {
                                err = Some(e);
                                return Ok(false);
                            }
                        }
                    }
                    Ok(true)
                }
                Chunk::Row(k) => match push(k) {
                    Ok(go) => Ok(go),
                    Err(e) => {
                        err = Some(e);
                        Ok(false)
                    }
                },
            }
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        if stopped && let Some(next) = last.and_then(next_key_after) {
            self.lo = Some(next);
            Ok(false)
        } else {
            self.lo = None;
            self.done = true;
            Ok(true)
        }
    }
}

enum OneConstCand {
    Indexed(Box<WindowIter>),
    Scan {
        pred_at: usize,
        scan: Option<PrefixScan>,
        window: Option<[f64; 4]>,
    },
    Done,
}

struct PairEmit {
    a_lit: Id,
    b_lit: Id,
    ai: usize,
    bi: usize,
    oai: usize,
    obi: usize,
    oa_len: usize,
    ob_len: usize,
    matched_counted: bool,
}

enum RelateStage {
    Asserted {
        scan: PrefixScan,
        o_filter: Option<Id>,
    },
    EnterDerived,
    OneConst {
        cid: Id,
        subject_const: bool,
        lits: Vec<ConstLit>,
        q_at: usize,
        qg: Option<GeomRef>,
        cand: OneConstCand,
        pending_sers: VecDeque<Ser>,
        active_ser: Option<(Ser, usize)>,
        lit_pass: FxHashMap<Id, bool>,
    },
    BothConst {
        sid: Id,
        oid: Id,
        left: Vec<ConstLit>,
        right: Vec<ConstLit>,
        left_at: usize,
        right_at: usize,
    },
    ConstantGraphs {
        sid: Id,
        oid: Id,
        next_graph: u64,
    },
    BothVar {
        items: Vec<RelateItem>,
        by_lit: FxHashMap<Id, Vec<Ser>>,
        by_lit_elem_bytes: u64,
        boxed_idx: Vec<usize>,
        unboxed_idx: Vec<usize>,
        tree: Option<PackedTree>,
        phase: BothVarPhase,
        pair_emit: Option<PairEmit>,
    },
    Done,
}

enum BothVarPhase {
    Boxed {
        outer: usize,
        cur_i: Option<usize>,
        stack: Vec<usize>,
        ws_f32: [f32; 4],
    },
    Unboxed {
        idx: usize,
        j: usize,
        rev_pending: bool,
    },
    Disjoint {
        i: usize,
        j: usize,
        rev_pending: bool,
    },
    Done,
}

/// Resumable streaming reader for `SpatialRelate` (item 829, group 4 / 6b).
pub(crate) struct SpatialRelateReader {
    spec: SpatialRelateSpec,
    vars: Vec<VarId>,
    sv: Option<VarId>,
    ov: Option<VarId>,
    gv: Option<VarId>,
    cfg: Arc<GeoConfig>,
    preds: Vec<Id>,
    link: Option<Id>,
    op_vertices: u64,
    state_name: String,
    scope: FxHashMap<u64, bool>,
    geoms: FxHashMap<Id, Option<GeomRef>>,
    features: FxHashMap<(Id, Id), Vec<Id>>,
    features_elem_bytes: u64,
    emitted: FxHashSet<(Id, Id, Id)>,
    pending_out: VecDeque<(Id, Id, Id)>,
    asserted_count: u64,
    pairs: u64,
    st: SearchStats,
    stage: RelateStage,
    charge: OwnedCharge,
}

impl SpatialRelateReader {
    pub(crate) fn open(ctx: &Arc<Ctx>, spec: &SpatialRelateSpec, vars: &[VarId]) -> Result<Self> {
        let cfg = config(ctx);
        let preds: Vec<Id> = cfg
            .predicates
            .iter()
            .filter_map(|p| ctx.snap.lookup_iri(p))
            .collect();
        let var = |e: &RelateEnd| match e {
            RelateEnd::Var(v) => Some(*v),
            _ => None,
        };
        let (sv, ov, gv) = (var(&spec.subject), var(&spec.object), spec.graph_var);
        let stage = if let Some(p) = spec.property {
            let id = |e: &RelateEnd| match e {
                RelateEnd::Node(id) | RelateEnd::Geometry(id, _) => Some(*id),
                _ => None,
            };
            let (s, o) = (id(&spec.subject), id(&spec.object));
            if [s, o].iter().flatten().any(|i| i.tag() == Tag::Local) {
                RelateStage::EnterDerived
            } else {
                let (perm, prefix) = match (s, o) {
                    (Some(s), _) => (Perm::Spo, vec![s.0, p.0]),
                    (None, Some(o)) => (Perm::Pos, vec![p.0, o.0]),
                    (None, None) => (Perm::Pso, vec![p.0]),
                };
                RelateStage::Asserted {
                    scan: PrefixScan::new(perm, &prefix),
                    o_filter: o,
                }
            }
        } else {
            RelateStage::EnterDerived
        };
        Ok(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            sv,
            ov,
            gv,
            cfg,
            preds,
            link: ctx.snap.lookup_iri(vocab::HAS_DEFAULT_GEOMETRY),
            op_vertices: ctx.geo.op_vertices(),
            state_name: state(ctx).to_string(),
            scope: FxHashMap::default(),
            geoms: FxHashMap::default(),
            features: FxHashMap::default(),
            features_elem_bytes: 0,
            emitted: FxHashSet::default(),
            pending_out: VecDeque::new(),
            asserted_count: 0,
            pairs: 0,
            st: SearchStats::default(),
            stage,
            charge: OwnedCharge::new(ctx, 0)?,
        })
    }

    pub(crate) fn counters(&self) -> Counters {
        let mut c = Counters::new();
        c.insert("candidates".into(), self.st.candidates.into());
        c.insert("refined".into(), self.st.refined.into());
        c.insert("matched".into(), self.st.matched.into());
        c.insert("treeNodesVisited".into(), self.st.nodes.into());
        c.insert("rechecked".into(), self.st.rechecked.into());
        c.insert("asserted".into(), self.asserted_count.into());
        c.insert("pairs".into(), self.pairs.into());
        c.insert("index".into(), self.state_name.clone().into());
        c.insert("fallback".into(), self.st.fallback.into());
        c
    }

    fn base_retained_bytes(&self) -> u64 {
        (table_bytes(self.emitted.capacity(), size_of::<(Id, Id, Id)>()) as u64)
            .saturating_add(self.pending_out.capacity() as u64 * size_of::<(Id, Id, Id)>() as u64)
            .saturating_add(
                table_bytes(self.geoms.capacity(), size_of::<(Id, Option<GeomRef>)>()) as u64,
            )
            .saturating_add(
                self.features.capacity() as u64
                    * (size_of::<(Id, Id)>() + size_of::<Vec<Id>>() + 1) as u64,
            )
            .saturating_add(self.features_elem_bytes)
            .saturating_add(table_bytes(self.scope.capacity(), size_of::<(u64, bool)>()) as u64)
    }

    fn retained_bytes(&self) -> u64 {
        let mut bytes = self.base_retained_bytes();
        match &self.stage {
            RelateStage::OneConst {
                lits,
                cand,
                pending_sers,
                lit_pass,
                ..
            } => {
                if let OneConstCand::Indexed(it) = cand {
                    bytes = bytes.saturating_add(it.retained_bytes());
                }
                bytes = bytes
                    .saturating_add(const_lit_bytes(lits))
                    .saturating_add(pending_sers.capacity() as u64 * size_of::<Ser>() as u64)
                    .saturating_add(lit_pass_bytes(lit_pass));
            }
            RelateStage::BothConst { left, right, .. } => {
                bytes = bytes
                    .saturating_add(const_lit_bytes(left))
                    .saturating_add(const_lit_bytes(right));
            }
            RelateStage::BothVar {
                items,
                by_lit,
                by_lit_elem_bytes,
                boxed_idx,
                unboxed_idx,
                tree,
                phase,
                ..
            } => {
                let stack_bytes = match phase {
                    BothVarPhase::Boxed { stack, .. } => {
                        stack.capacity() as u64 * size_of::<usize>() as u64
                    }
                    _ => 0,
                };
                bytes = bytes
                    .saturating_add(items.capacity() as u64 * size_of::<RelateItem>() as u64)
                    .saturating_add(
                        by_lit.capacity() as u64
                            * (size_of::<Id>() + size_of::<Vec<Ser>>() + 1) as u64,
                    )
                    .saturating_add(*by_lit_elem_bytes)
                    .saturating_add(
                        (boxed_idx.capacity() + unboxed_idx.capacity()) as u64
                            * size_of::<usize>() as u64,
                    )
                    .saturating_add(stack_bytes)
                    .saturating_add(tree.as_ref().map_or(0, PackedTree::bytes));
            }
            _ => {}
        }
        bytes
    }

    fn update_charge(&mut self) -> Result<()> {
        self.charge.resize(self.retained_bytes())
    }

    fn in_scope(&mut self, ctx: &Ctx, g: u64) -> bool {
        let cfg = &self.cfg;
        *self.scope.entry(g).or_insert_with(|| {
            super::exec::graph_iri(ctx, Id(g)).is_some_and(|i| cfg.graph_in_scope(&i))
        })
    }

    fn ser_graph(&mut self, ctx: &Ctx, g: u64) -> bool {
        self.spec.graph.accepts(g) && self.in_scope(ctx, g)
    }

    fn geom_charged(&mut self, ctx: &Ctx, lit: Id, extra_bytes: u64) -> Result<Option<GeomRef>> {
        if let Some(g) = self.geoms.get(&lit) {
            return Ok(g.clone());
        }
        let add = hash_growth::<(Id, Option<GeomRef>)>(self.geoms.capacity(), self.geoms.len());
        if add != 0 {
            self.charge.resize(
                self.base_retained_bytes()
                    .saturating_add(extra_bytes)
                    .saturating_add(add),
            )?;
        }
        let parsed = (|| {
            let v = ctx.value(lit)?;
            let max = max_vertices(ctx);
            let g = ctx
                .geo
                .get_or_parse_charged(ctx, MemoKey::Id(lit), || parse_value(&v, max))?;
            note_crs(ctx, &g);
            Some(g)
        })();
        self.geoms.insert(lit, parsed.clone());
        if add != 0 {
            self.charge
                .resize(self.base_retained_bytes().saturating_add(extra_bytes))?;
        }
        Ok(parsed)
    }

    fn collect_const_serializations(
        &mut self,
        ctx: &Ctx,
        subject: Id,
        graph: Option<u64>,
        out: &mut Vec<ConstLit>,
        extra: u64,
    ) -> Result<()> {
        for at in 0..self.preds.len() {
            let p = self.preds[at];
            let mut scanned = 0usize;
            scan_keys(ctx, Perm::Spo, &[subject.0, p.0], |k| {
                scanned += 1;
                if scanned.is_multiple_of(CHECK_EVERY) {
                    ctx.check()?;
                }
                if graph.is_some_and(|g| g != k[3]) || !self.spec.graph.accepts(k[3]) {
                    return Ok(());
                }
                if !self.scope.contains_key(&k[3]) {
                    let growth =
                        hash_growth::<(u64, bool)>(self.scope.capacity(), self.scope.len());
                    if growth != 0 {
                        self.charge.resize(
                            self.base_retained_bytes()
                                .saturating_add(extra)
                                .saturating_add(const_lit_bytes(out))
                                .saturating_add(growth),
                        )?;
                    }
                }
                if self.in_scope(ctx, k[3]) {
                    if out.len() == out.capacity() {
                        let elem = size_of::<ConstLit>();
                        let next = vec_grown(out.capacity(), out.len(), 1, elem);
                        self.charge.resize(
                            self.base_retained_bytes()
                                .saturating_add(extra)
                                .saturating_add(const_lit_bytes(out))
                                .saturating_add(next.saturating_mul(elem) as u64),
                        )?;
                    }
                    out.push(ConstLit {
                        lit: Id(k[2]),
                        g: Some(Id(k[3])),
                    });
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn const_lits(&mut self, ctx: &Ctx, e: &RelateEnd, extra: u64) -> Result<Vec<ConstLit>> {
        let id = match e {
            RelateEnd::Geometry(id, g) => {
                let growth = if self.geoms.contains_key(id) {
                    0
                } else {
                    hash_growth::<(Id, Option<GeomRef>)>(self.geoms.capacity(), self.geoms.len())
                };
                self.charge.resize(
                    self.base_retained_bytes()
                        .saturating_add(extra)
                        .saturating_add(growth)
                        .saturating_add(size_of::<ConstLit>() as u64),
                )?;
                self.geoms.insert(*id, Some(g.clone()));
                return Ok(vec![ConstLit { lit: *id, g: None }]);
            }
            RelateEnd::Node(id) => *id,
            RelateEnd::Var(_) | RelateEnd::Absent => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        self.collect_const_serializations(ctx, id, None, &mut out, extra)?;
        if let Some(link) = self.link {
            let mut scanned = 0usize;
            scan_keys(ctx, Perm::Spo, &[id.0, link.0], |k| {
                scanned += 1;
                if scanned.is_multiple_of(CHECK_EVERY) {
                    ctx.check()?;
                }
                if self.spec.graph.accepts(k[3]) {
                    let graph = self.spec.graph_var.is_some().then_some(k[3]);
                    self.collect_const_serializations(ctx, Id(k[2]), graph, &mut out, extra)?;
                }
                Ok(())
            })?;
        }
        out.sort_unstable_by_key(|c| (c.lit, c.g));
        out.dedup_by_key(|c| (c.lit, c.g));
        Ok(out)
    }

    fn ensure_features(&mut self, ctx: &Ctx, s: Id, g: Id, extra_bytes: u64) -> Result<usize> {
        if let Some(f) = self.features.get(&(s, g)) {
            return Ok(f.len());
        }
        let mut out: Vec<Id> = Vec::new();
        if let Some(link) = self.link {
            let same = self.spec.graph_var.is_some();
            let mut scan = PrefixScan::new(Perm::Pos, &[link.0, s.0]);
            let mut raw = Vec::new();
            loop {
                let base_now = self
                    .base_retained_bytes()
                    .saturating_add(extra_bytes)
                    .saturating_add(out.capacity() as u64 * size_of::<Id>() as u64);
                let charge = &mut self.charge;
                let ended = scan.next_chunk(ctx, &mut raw, 4096, &mut |raw_bytes| {
                    charge.resize(base_now.saturating_add(raw_bytes))
                })?;
                let raw_bytes = raw.capacity() as u64 * size_of::<[u64; 4]>() as u64;
                for &k in &raw {
                    let g1 = k[3];
                    if (same && g1 == g.0) || (!same && self.spec.graph.accepts(g1)) {
                        if out.len() == out.capacity() {
                            let cur_out = out.capacity() as u64 * size_of::<Id>() as u64;
                            let next_out = next_cap(out.len(), out.capacity(), 1) as u64
                                * size_of::<Id>() as u64;
                            self.charge.resize(
                                self.base_retained_bytes()
                                    .saturating_add(extra_bytes)
                                    .saturating_add(raw_bytes)
                                    .saturating_add(cur_out)
                                    .saturating_add(next_out),
                            )?;
                        }
                        out.push(Id(k[2]));
                    }
                }
                if ended {
                    break;
                }
            }
            drop(raw);
            out.sort_unstable();
            out.dedup();
            out.shrink_to_fit();
        }
        let map_add = if self.features.len() == self.features.capacity() {
            (next_cap(self.features.len(), self.features.capacity(), 1) - self.features.capacity())
                as u64
                * (size_of::<(Id, Id)>() + size_of::<Vec<Id>>() + 1) as u64
        } else {
            0
        };
        let elem_bytes = out.capacity() as u64 * size_of::<Id>() as u64;
        self.charge.resize(
            self.base_retained_bytes()
                .saturating_add(extra_bytes)
                .saturating_add(map_add)
                .saturating_add(elem_bytes),
        )?;
        let len = out.len();
        self.features_elem_bytes = self.features_elem_bytes.saturating_add(elem_bytes);
        self.features.insert((s, g), out);
        Ok(len)
    }

    fn object_at(&self, r: &Ser, idx: usize) -> Option<Id> {
        if idx == 0 {
            Some(r.s)
        } else {
            self.features
                .get(&(r.s, r.g))
                .and_then(|v| v.get(idx - 1).copied())
        }
    }

    fn record_emit_charged(
        &mut self,
        ctx: &Ctx,
        s: Id,
        o: Id,
        g: Id,
        extra_bytes: u64,
    ) -> Result<bool> {
        let g = if self.spec.graph_var.is_some() {
            g
        } else {
            Id::UNDEF
        };
        if self.emitted.contains(&(s, o, g)) {
            return Ok(false);
        }
        let emit_add = hash_growth::<(Id, Id, Id)>(self.emitted.capacity(), self.emitted.len());
        let pend_add = if self.pending_out.len() == self.pending_out.capacity() {
            (next_cap(self.pending_out.len(), self.pending_out.capacity(), 1)
                - self.pending_out.capacity()) as u64
                * size_of::<(Id, Id, Id)>() as u64
        } else {
            0
        };
        if emit_add > 0 || pend_add > 0 {
            self.charge.resize(
                self.base_retained_bytes()
                    .saturating_add(extra_bytes)
                    .saturating_add(emit_add)
                    .saturating_add(pend_add),
            )?;
        }
        self.emitted.insert((s, o, g));
        if self.emitted.len().is_multiple_of(4096) {
            ctx.check()?;
            ctx.check_rows(self.emitted.len())?;
        }
        self.pending_out.push_back((s, o, g));
        Ok(true)
    }

    fn push_pending_to_table(&mut self, out: &mut Table, cap: usize) {
        debug_assert!(
            self.sv.is_none_or(|v| self.vars.contains(&v))
                && self.ov.is_none_or(|v| self.vars.contains(&v))
                && self.gv.is_none_or(|v| self.vars.contains(&v)),
            "SpatialRelateReader assumes self.vars contains every variable in (sv, ov, gv)"
        );
        let mut row = vec![Id::UNDEF; self.vars.len()];
        'items: while out.len() < cap {
            let Some((s, o, g)) = self.pending_out.pop_front() else {
                break;
            };
            for (x, v) in row.iter_mut().zip(&self.vars) {
                let mut val: Option<Id> = None;
                for (w, id) in [(self.sv, s), (self.ov, o), (self.gv, g)] {
                    if w == Some(*v) {
                        match val {
                            Some(prev) if prev != id => continue 'items,
                            _ => val = Some(id),
                        }
                    }
                }
                *x = val.unwrap_or(Id::UNDEF);
            }
            out.push_row(&row);
        }
    }

    fn enter_derived(&mut self, ctx: &Ctx) -> Result<()> {
        if self.preds.is_empty() {
            self.stage = RelateStage::Done;
            return Ok(());
        }
        let spec = self.spec.clone();
        match (&spec.subject, &spec.object) {
            (RelateEnd::Var(_), RelateEnd::Var(_)) => {
                // Collect all serialization rows in scope and pack boxed geometries with
                // pre-admission before every container growth.
                if search::indexed(&ctx.snap, &self.preds).is_some() {
                    let base = self.base_retained_bytes();
                    let charge = &mut self.charge;
                    let mut iter = WindowIter::open(
                        ctx,
                        self.preds.clone(),
                        vec![WORLD],
                        self.spec.graph.clone(),
                        false,
                        &mut |b| charge.resize(base.saturating_add(b)),
                    )?;
                    let mut hits = Vec::new();
                    loop {
                        hits.clear();
                        let cur_base = self.base_retained_bytes();
                        let charge = &mut self.charge;
                        let ended = iter.next_hits(ctx, &mut hits, 4096, &mut |b| {
                            charge.resize(cur_base.saturating_add(b))
                        })?;
                        let iter_bytes = iter.retained_bytes();
                        for h in &hits {
                            if !self.geoms.contains_key(&h.o)
                                && let Ok(g) = noted(ctx, h.entry.geom(&ctx.snap))
                            {
                                if self.geoms.len() == self.geoms.capacity() {
                                    let add = (next_cap(self.geoms.len(), self.geoms.capacity(), 1)
                                        - self.geoms.capacity())
                                        as u64
                                        * (size_of::<Id>() + size_of::<Option<GeomRef>>() + 1)
                                            as u64;
                                    self.charge.resize(
                                        self.base_retained_bytes()
                                            .saturating_add(iter_bytes)
                                            .saturating_add(add),
                                    )?;
                                }
                                self.geoms.insert(h.o, Some(g));
                            }
                        }
                        if ended {
                            break;
                        }
                    }
                    self.st.nodes += iter.st.nodes;
                    self.st.rechecked += iter.st.rechecked;
                } else {
                    self.st.fallback = true;
                }
                let mut rows: Vec<Ser> = Vec::new();
                for p in self.preds.clone() {
                    let mut scan = PrefixScan::new(Perm::Pso, &[p.0]);
                    let mut raw = Vec::new();
                    loop {
                        let base_now = self
                            .base_retained_bytes()
                            .saturating_add(rows.capacity() as u64 * size_of::<Ser>() as u64);
                        let charge = &mut self.charge;
                        let ended = scan.next_chunk(ctx, &mut raw, 4096, &mut |raw_bytes| {
                            charge.resize(base_now.saturating_add(raw_bytes))
                        })?;
                        let raw_bytes = raw.capacity() as u64 * size_of::<[u64; 4]>() as u64;
                        for &k in &raw {
                            if self.ser_graph(ctx, k[3]) {
                                if rows.len() == rows.capacity() {
                                    let cur_rows = rows.capacity() as u64 * size_of::<Ser>() as u64;
                                    let next_rows = next_cap(rows.len(), rows.capacity(), 1) as u64
                                        * size_of::<Ser>() as u64;
                                    self.charge.resize(
                                        self.base_retained_bytes()
                                            .saturating_add(raw_bytes)
                                            .saturating_add(cur_rows)
                                            .saturating_add(next_rows),
                                    )?;
                                }
                                rows.push(Ser {
                                    s: Id(k[1]),
                                    o: Id(k[2]),
                                    g: Id(k[3]),
                                });
                            }
                        }
                        if ended {
                            break;
                        }
                    }
                }
                rows.sort_unstable();
                rows.dedup();
                self.st.candidates = self.st.candidates.max(rows.len() as u64);
                let rows_bytes = rows.capacity() as u64 * size_of::<Ser>() as u64;
                let mut by_lit: FxHashMap<Id, Vec<Ser>> = FxHashMap::default();
                let mut by_lit_elem_bytes = 0u64;
                for (idx, r) in rows.into_iter().enumerate() {
                    if idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    let map_growth = if by_lit.len() == by_lit.capacity()
                        && !by_lit.contains_key(&r.o)
                    {
                        (next_cap(by_lit.len(), by_lit.capacity(), 1) - by_lit.capacity()) as u64
                            * (size_of::<Id>() + size_of::<Vec<Ser>>() + 1) as u64
                    } else {
                        0
                    };
                    let vec_growth = match by_lit.get(&r.o) {
                        Some(v) if v.len() == v.capacity() => {
                            (next_cap(v.len(), v.capacity(), 1) - v.capacity()) as u64
                                * size_of::<Ser>() as u64
                        }
                        None => 4 * size_of::<Ser>() as u64,
                        _ => 0,
                    };
                    if map_growth > 0 || vec_growth > 0 {
                        let map_bytes = by_lit.capacity() as u64
                            * (size_of::<Id>() + size_of::<Vec<Ser>>() + 1) as u64;
                        self.charge.resize(
                            self.base_retained_bytes()
                                .saturating_add(rows_bytes)
                                .saturating_add(map_bytes)
                                .saturating_add(by_lit_elem_bytes)
                                .saturating_add(map_growth)
                                .saturating_add(vec_growth),
                        )?;
                    }
                    let v = by_lit.entry(r.o).or_default();
                    let old_cap = v.capacity();
                    v.push(r);
                    let new_cap = v.capacity();
                    if new_cap > old_cap {
                        by_lit_elem_bytes = by_lit_elem_bytes
                            .saturating_add((new_cap - old_cap) as u64 * size_of::<Ser>() as u64);
                    }
                }
                let by_lit_bytes = (by_lit.capacity() as u64
                    * (size_of::<Id>() + size_of::<Vec<Ser>>() + 1) as u64)
                    .saturating_add(by_lit_elem_bytes);
                let lits_bytes = by_lit.len() as u64 * size_of::<Id>() as u64;
                self.charge.resize(
                    self.base_retained_bytes()
                        .saturating_add(by_lit_bytes)
                        .saturating_add(lits_bytes),
                )?;
                let mut lits: Vec<Id> = by_lit.keys().copied().collect();
                lits.sort_unstable();
                let mut items = Vec::new();
                for (idx, lit) in lits.into_iter().enumerate() {
                    if idx.is_multiple_of(CHECK_EVERY) {
                        ctx.check()?;
                    }
                    let items_bytes = items.capacity() as u64 * size_of::<RelateItem>() as u64;
                    let extra = by_lit_bytes
                        .saturating_add(lits_bytes)
                        .saturating_add(items_bytes);
                    if let Some(geom) = self.geom_charged(ctx, lit, extra)? {
                        let bbox = geom.bbox84();
                        if items.len() == items.capacity() {
                            let next_items = next_cap(items.len(), items.capacity(), 1) as u64
                                * size_of::<RelateItem>() as u64;
                            self.charge.resize(
                                self.base_retained_bytes()
                                    .saturating_add(by_lit_bytes)
                                    .saturating_add(lits_bytes)
                                    .saturating_add(next_items),
                            )?;
                        }
                        items.push(RelateItem { lit, geom, bbox });
                    }
                }
                let items_bytes = items.capacity() as u64 * size_of::<RelateItem>() as u64;
                let (boxed_idx, unboxed_idx, tree, phase) = if self.spec.rel.index_usable() {
                    let n_boxed = items.iter().filter(|it| it.bbox.is_some()).count();
                    let n_unboxed = items
                        .iter()
                        .filter(|it| it.bbox.is_none() && !it.geom.empty)
                        .count();
                    let idx_est =
                        (n_boxed.saturating_add(n_unboxed) as u64) * size_of::<usize>() as u64;
                    let tree_est = PackedTree::estimate_bytes(n_boxed);
                    self.charge.resize(
                        self.base_retained_bytes()
                            .saturating_add(by_lit_bytes)
                            .saturating_add(items_bytes)
                            .saturating_add(idx_est)
                            .saturating_add(tree_est),
                    )?;
                    let boxed_idx: Vec<usize> = (0..items.len())
                        .filter(|&i| items[i].bbox.is_some())
                        .collect();
                    let unboxed_idx: Vec<usize> = (0..items.len())
                        .filter(|&i| items[i].bbox.is_none() && !items[i].geom.empty)
                        .collect();
                    let tree = PackedTree::pack(
                        boxed_idx
                            .iter()
                            .map(|&i| round_out(items[i].bbox.expect("boxed item"))),
                    );
                    let phase = if tree.is_some() {
                        BothVarPhase::Boxed {
                            outer: 0,
                            cur_i: None,
                            stack: Vec::new(),
                            ws_f32: [0.0; 4],
                        }
                    } else if !unboxed_idx.is_empty() {
                        BothVarPhase::Unboxed {
                            idx: 0,
                            j: 0,
                            rev_pending: false,
                        }
                    } else {
                        BothVarPhase::Done
                    };
                    (boxed_idx, unboxed_idx, tree, phase)
                } else {
                    let phase = if items.is_empty() {
                        BothVarPhase::Done
                    } else {
                        BothVarPhase::Disjoint {
                            i: 0,
                            j: 0,
                            rev_pending: false,
                        }
                    };
                    (Vec::new(), Vec::new(), None, phase)
                };
                self.stage = RelateStage::BothVar {
                    items,
                    by_lit,
                    by_lit_elem_bytes,
                    boxed_idx,
                    unboxed_idx,
                    tree,
                    phase,
                    pair_emit: None,
                };
                self.update_charge()?;
            }
            (RelateEnd::Var(_), c) | (c, RelateEnd::Var(_)) => {
                let subject_const = !matches!(spec.subject, RelateEnd::Var(_));
                let cid = match c {
                    RelateEnd::Node(id) | RelateEnd::Geometry(id, _) => *id,
                    _ => {
                        self.stage = RelateStage::Done;
                        return Ok(());
                    }
                };
                let lits = self.const_lits(ctx, c, 0)?;
                self.stage = RelateStage::OneConst {
                    cid,
                    subject_const,
                    lits,
                    q_at: 0,
                    qg: None,
                    cand: OneConstCand::Done,
                    pending_sers: VecDeque::new(),
                    active_ser: None,
                    lit_pass: FxHashMap::default(),
                };
                self.update_charge()?;
            }
            (a, b) => {
                let (sid, oid) = match (a, b) {
                    (
                        RelateEnd::Node(s) | RelateEnd::Geometry(s, _),
                        RelateEnd::Node(o) | RelateEnd::Geometry(o, _),
                    ) => (*s, *o),
                    _ => {
                        self.stage = RelateStage::Done;
                        return Ok(());
                    }
                };
                let mut left = self.const_lits(ctx, a, 0)?;
                let mut right = self.const_lits(ctx, b, const_lit_bytes(&left))?;
                let extra = const_lit_bytes(&left).saturating_add(const_lit_bytes(&right));
                self.stage = if self.spec.graph_var.is_none() {
                    let found = self.constant_match(ctx, &left, &right, extra)?;
                    drop((left, right));
                    if found {
                        self.record_emit_charged(ctx, sid, oid, Id::UNDEF, 0)?;
                    }
                    RelateStage::Done
                } else if left.first().is_some_and(|l| l.g.is_none())
                    && right.first().is_some_and(|l| l.g.is_none())
                {
                    let found = self.constant_match(ctx, &left, &right, extra)?;
                    drop((left, right));
                    if found {
                        RelateStage::ConstantGraphs {
                            sid,
                            oid,
                            next_graph: 0,
                        }
                    } else {
                        RelateStage::Done
                    }
                } else {
                    left.sort_unstable_by_key(|l| l.g);
                    right.sort_unstable_by_key(|l| l.g);
                    RelateStage::BothConst {
                        sid,
                        oid,
                        left,
                        right,
                        left_at: 0,
                        right_at: 0,
                    }
                };
                self.update_charge()?;
            }
        }
        Ok(())
    }

    fn constant_match(
        &mut self,
        ctx: &Ctx,
        left: &[ConstLit],
        right: &[ConstLit],
        extra: u64,
    ) -> Result<bool> {
        for x in left {
            ctx.check()?;
            let Some(gx) = self.geom_charged(ctx, x.lit, extra)? else {
                continue;
            };
            let mut tester = ConstTest::new(gx, self.spec.rel, true, self.op_vertices);
            for (i, y) in right.iter().enumerate() {
                if i.is_multiple_of(CHECK_EVERY) {
                    ctx.check()?;
                }
                let Some(gy) = self.geom_charged(ctx, y.lit, extra)? else {
                    continue;
                };
                self.st.refined += 1;
                if tester.holds(&gy) {
                    self.pairs += 1;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn step_both_const(&mut self, ctx: &Ctx) -> Result<()> {
        let RelateStage::BothConst {
            sid,
            oid,
            left,
            right,
            mut left_at,
            mut right_at,
        } = std::mem::replace(&mut self.stage, RelateStage::Done)
        else {
            unreachable!()
        };
        let extra = const_lit_bytes(&left).saturating_add(const_lit_bytes(&right));
        while let (Some(a), Some(b)) = (left.get(left_at), right.get(right_at)) {
            ctx.check()?;
            let a_end = left_at + left[left_at..].partition_point(|l| l.g == a.g);
            let b_end = right_at + right[right_at..].partition_point(|l| l.g == b.g);
            if let (Some(ag), Some(bg)) = (a.g, b.g) {
                match ag.cmp(&bg) {
                    std::cmp::Ordering::Less => {
                        left_at = a_end;
                        continue;
                    }
                    std::cmp::Ordering::Greater => {
                        right_at = b_end;
                        continue;
                    }
                    std::cmp::Ordering::Equal => {}
                }
            }
            let graph = a.g.or(b.g).expect("at least one constant has a graph");
            let a_start = left_at;
            let b_start = right_at;
            if a.g.is_some() {
                left_at = a_end;
            }
            if b.g.is_some() {
                right_at = b_end;
            }
            if self.constant_match(ctx, &left[a_start..a_end], &right[b_start..b_end], extra)?
                && self.record_emit_charged(ctx, sid, oid, graph, extra)?
            {
                self.stage = RelateStage::BothConst {
                    sid,
                    oid,
                    left,
                    right,
                    left_at,
                    right_at,
                };
                return Ok(());
            }
        }
        Ok(())
    }

    fn step_pair_emit(
        &mut self,
        ctx: &Ctx,
        by_lit: &FxHashMap<Id, Vec<Ser>>,
        pe: &mut PairEmit,
        cap: usize,
        stage_bytes: u64,
    ) -> Result<bool> {
        let graph_var = self.spec.graph_var.is_some();
        let a_sers = &by_lit[&pe.a_lit];
        let b_sers = &by_lit[&pe.b_lit];
        let mut steps = 0usize;
        while pe.ai < a_sers.len() {
            let ra = a_sers[pe.ai];
            if pe.bi == 0 && pe.oai == 0 && pe.obi == 0 && !pe.matched_counted {
                pe.oa_len = 1 + self.ensure_features(ctx, ra.s, ra.g, stage_bytes)?;
            }
            while pe.bi < b_sers.len() {
                steps += 1;
                if steps.is_multiple_of(CHECK_EVERY) {
                    ctx.check()?;
                }
                let rb = b_sers[pe.bi];
                if graph_var && ra.g != rb.g {
                    pe.bi += 1;
                    continue;
                }
                if !pe.matched_counted {
                    self.st.matched += 1;
                    pe.ob_len = 1 + self.ensure_features(ctx, rb.s, rb.g, stage_bytes)?;
                    pe.oai = 0;
                    pe.obi = 0;
                    pe.matched_counted = true;
                }
                while pe.oai < pe.oa_len {
                    let sa = self.object_at(&ra, pe.oai).expect("cached a object");
                    while pe.obi < pe.ob_len {
                        steps += 1;
                        if steps.is_multiple_of(CHECK_EVERY) {
                            ctx.check()?;
                        }
                        if self.pending_out.len() >= cap {
                            return Ok(false);
                        }
                        let sb = self.object_at(&rb, pe.obi).expect("cached b object");
                        pe.obi += 1;
                        self.record_emit_charged(ctx, sa, sb, ra.g, stage_bytes)?;
                    }
                    pe.oai += 1;
                    pe.obi = 0;
                }
                pe.bi += 1;
                pe.matched_counted = false;
            }
            pe.ai += 1;
            pe.bi = 0;
            pe.oai = 0;
            pe.obi = 0;
            pe.matched_counted = false;
        }
        Ok(true)
    }

    /// Fill `out` with up to `cap` rows. Returns `true` when exhausted.
    pub(crate) fn next(&mut self, ctx: &Ctx, out: &mut Table, cap: usize) -> Result<bool> {
        let cap = cap.max(1);
        loop {
            ctx.check()?;
            self.push_pending_to_table(out, cap);
            if out.len() >= cap {
                self.update_charge()?;
                return Ok(false);
            }
            let asserted_base = self.base_retained_bytes();
            match &mut self.stage {
                RelateStage::Done => {
                    self.update_charge()?;
                    return Ok(self.pending_out.is_empty());
                }
                RelateStage::Asserted { scan, o_filter } => {
                    let o_filter = *o_filter;
                    let perm = scan.perm;
                    let mut raw = Vec::new();
                    let charge = &mut self.charge;
                    let ended =
                        scan.next_chunk(ctx, &mut raw, cap.clamp(64, 4096), &mut |raw_bytes| {
                            charge.resize(asserted_base.saturating_add(raw_bytes))
                        })?;
                    if ended {
                        self.stage = RelateStage::EnterDerived;
                    }
                    let raw_bytes = raw.capacity() as u64 * size_of::<[u64; 4]>() as u64;
                    for k in raw {
                        let q = perm.to_quad(&k);
                        if self.spec.graph.accepts(q[3].0)
                            && o_filter.is_none_or(|o| o == q[2])
                            && self.record_emit_charged(ctx, q[0], q[2], q[3], raw_bytes)?
                        {
                            self.asserted_count += 1;
                        }
                    }
                    self.update_charge()?;
                }
                RelateStage::EnterDerived => {
                    self.enter_derived(ctx)?;
                }
                RelateStage::OneConst { .. } => {
                    self.step_one_const(ctx, cap)?;
                    self.update_charge()?;
                }
                RelateStage::BothConst { .. } => {
                    self.step_both_const(ctx)?;
                    self.update_charge()?;
                }
                RelateStage::ConstantGraphs {
                    sid,
                    oid,
                    next_graph,
                } => {
                    let (sid, oid, start) = (*sid, *oid, *next_graph);
                    let mut graph = None;
                    ctx.snap.scan_between(
                        Perm::Gspo,
                        [start, 0, 0, 0],
                        [u64::MAX; 4],
                        |chunk| {
                            graph = Some(match chunk {
                                Chunk::Block(b, from, _) => b.key(from)[0],
                                Chunk::Row(k) => k[0],
                            });
                            Ok(false)
                        },
                    )?;
                    self.stage = RelateStage::Done;
                    if let Some(g) = graph {
                        if let Some(next_graph) = g.checked_add(1) {
                            self.stage = RelateStage::ConstantGraphs {
                                sid,
                                oid,
                                next_graph,
                            };
                        }
                        if Id(g) != Id::DEFAULT_GRAPH && self.spec.graph.accepts(g) {
                            self.record_emit_charged(ctx, sid, oid, Id(g), 0)?;
                        }
                    }
                }
                RelateStage::BothVar { .. } => {
                    self.step_both_var(ctx, cap)?;
                    self.update_charge()?;
                }
            }
        }
    }

    fn step_one_const(&mut self, ctx: &Ctx, cap: usize) -> Result<()> {
        let mut stage = std::mem::replace(&mut self.stage, RelateStage::Done);
        let RelateStage::OneConst {
            cid,
            subject_const,
            lits,
            mut q_at,
            mut qg,
            mut cand,
            mut pending_sers,
            mut active_ser,
            mut lit_pass,
        } = stage
        else {
            unreachable!()
        };
        let graph_var = self.spec.graph_var.is_some();
        let fixed_stage_bytes = const_lit_bytes(&lits);
        let one_const_stage_bytes = |cand: &OneConstCand,
                                     pending_sers: &VecDeque<Ser>,
                                     lit_pass: &FxHashMap<Id, bool>|
         -> u64 {
            let iter_bytes = match cand {
                OneConstCand::Indexed(it) => it.retained_bytes(),
                _ => 0,
            };
            fixed_stage_bytes
                .saturating_add(iter_bytes)
                .saturating_add(pending_sers.capacity() as u64 * size_of::<Ser>() as u64)
                .saturating_add(lit_pass_bytes(lit_pass))
        };
        let res: Result<()> = (|| {
            loop {
                ctx.check()?;
                // 1. Drain any active matched serialization's objects up to `cap`.
                if let Some((r, mut obj_idx)) = active_ser.take() {
                    let stage_bytes = one_const_stage_bytes(&cand, &pending_sers, &lit_pass);
                    let total_objs = 1 + self.ensure_features(ctx, r.s, r.g, stage_bytes)?;
                    let mut steps = 0usize;
                    while obj_idx < total_objs {
                        steps += 1;
                        if steps.is_multiple_of(CHECK_EVERY) {
                            ctx.check()?;
                        }
                        if self.pending_out.len() >= cap {
                            active_ser = Some((r, obj_idx));
                            return Ok(());
                        }
                        let so = self.object_at(&r, obj_idx).expect("cached object");
                        obj_idx += 1;
                        if subject_const {
                            self.record_emit_charged(ctx, cid, so, r.g, stage_bytes)?;
                        } else {
                            self.record_emit_charged(ctx, so, cid, r.g, stage_bytes)?;
                        }
                    }
                    if self.pending_out.len() >= cap {
                        return Ok(());
                    }
                }
                if let Some(q_geom) = qg.as_ref() {
                    if pending_sers.is_empty() {
                        let mut cand_done = false;
                        match &mut cand {
                            OneConstCand::Indexed(iter) => {
                                let mut hits = Vec::new();
                                let non_iter_stage = fixed_stage_bytes
                                    .saturating_add(
                                        pending_sers.capacity() as u64 * size_of::<Ser>() as u64,
                                    )
                                    .saturating_add(lit_pass_bytes(&lit_pass));
                                let base_now =
                                    self.base_retained_bytes().saturating_add(non_iter_stage);
                                let charge = &mut self.charge;
                                let ended = iter.next_hits(
                                    ctx,
                                    &mut hits,
                                    cap.clamp(64, 4096),
                                    &mut |b| charge.resize(base_now.saturating_add(b)),
                                )?;
                                let iter_bytes = iter.retained_bytes();
                                let hits_bytes = hits.capacity() as u64 * size_of::<Hit>() as u64;
                                for h in hits {
                                    if !self.geoms.contains_key(&h.o)
                                        && let Ok(g) = noted(ctx, h.entry.geom(&ctx.snap))
                                    {
                                        let add = hash_growth::<(Id, Option<GeomRef>)>(
                                            self.geoms.capacity(),
                                            self.geoms.len(),
                                        );
                                        if add != 0 {
                                            let cur_stage = fixed_stage_bytes
                                                .saturating_add(iter_bytes)
                                                .saturating_add(hits_bytes)
                                                .saturating_add(
                                                    pending_sers.capacity() as u64
                                                        * size_of::<Ser>() as u64,
                                                )
                                                .saturating_add(lit_pass_bytes(&lit_pass));
                                            self.charge.resize(
                                                self.base_retained_bytes()
                                                    .saturating_add(cur_stage)
                                                    .saturating_add(add),
                                            )?;
                                        }
                                        self.geoms.insert(h.o, Some(g));
                                        if add != 0 {
                                            let cur_stage = fixed_stage_bytes
                                                .saturating_add(iter_bytes)
                                                .saturating_add(hits_bytes)
                                                .saturating_add(
                                                    pending_sers.capacity() as u64
                                                        * size_of::<Ser>() as u64,
                                                )
                                                .saturating_add(lit_pass_bytes(&lit_pass));
                                            self.charge.resize(
                                                self.base_retained_bytes()
                                                    .saturating_add(cur_stage),
                                            )?;
                                        }
                                    }
                                    if pending_sers.len() == pending_sers.capacity() {
                                        let cur_ps = pending_sers.capacity() as u64
                                            * size_of::<Ser>() as u64;
                                        let next_ps = next_cap(
                                            pending_sers.len(),
                                            pending_sers.capacity(),
                                            1,
                                        )
                                            as u64
                                            * size_of::<Ser>() as u64;
                                        let cur_stage = fixed_stage_bytes
                                            .saturating_add(iter_bytes)
                                            .saturating_add(hits_bytes)
                                            .saturating_add(cur_ps)
                                            .saturating_add(next_ps)
                                            .saturating_add(lit_pass_bytes(&lit_pass));
                                        self.charge.resize(
                                            self.base_retained_bytes().saturating_add(cur_stage),
                                        )?;
                                    }
                                    pending_sers.push_back(Ser {
                                        s: h.s,
                                        o: h.o,
                                        g: h.g,
                                    });
                                }
                                if ended {
                                    self.st.candidates += iter.st.candidates;
                                    self.st.nodes += iter.st.nodes;
                                    self.st.rechecked += iter.st.rechecked;
                                    self.st.fallback |= iter.st.fallback;
                                    cand_done = true;
                                }
                            }
                            OneConstCand::Scan {
                                pred_at,
                                scan,
                                window,
                            } => {
                                if scan.is_none() {
                                    if *pred_at >= self.preds.len() {
                                        cand_done = true;
                                    } else {
                                        let p = self.preds[*pred_at];
                                        *pred_at += 1;
                                        *scan = Some(PrefixScan::new(Perm::Pso, &[p.0]));
                                    }
                                }
                                if !cand_done && let Some(sc) = scan {
                                    let mut raw = Vec::new();
                                    let base_now = self
                                        .base_retained_bytes()
                                        .saturating_add(fixed_stage_bytes)
                                        .saturating_add(
                                            pending_sers.capacity() as u64
                                                * size_of::<Ser>() as u64,
                                        )
                                        .saturating_add(lit_pass_bytes(&lit_pass));
                                    let charge = &mut self.charge;
                                    let ended = sc.next_chunk(
                                        ctx,
                                        &mut raw,
                                        cap.clamp(64, 4096),
                                        &mut |raw_bytes| {
                                            charge.resize(base_now.saturating_add(raw_bytes))
                                        },
                                    )?;
                                    if ended {
                                        *scan = None;
                                    }
                                    let raw_bytes =
                                        raw.capacity() as u64 * size_of::<[u64; 4]>() as u64;
                                    let win = *window;
                                    for k in raw {
                                        if !self.ser_graph(ctx, k[3]) {
                                            continue;
                                        }
                                        let ser = Ser {
                                            s: Id(k[1]),
                                            o: Id(k[2]),
                                            g: Id(k[3]),
                                        };
                                        let cur_stage = fixed_stage_bytes
                                            .saturating_add(
                                                pending_sers.capacity() as u64
                                                    * size_of::<Ser>() as u64,
                                            )
                                            .saturating_add(lit_pass_bytes(&lit_pass));
                                        if let Some(w) = win
                                            && let Some(g) = self.geom_charged(
                                                ctx,
                                                ser.o,
                                                cur_stage.saturating_add(raw_bytes),
                                            )?
                                            && g.bbox84().is_some_and(|b| !meets(b, w))
                                        {
                                            continue;
                                        }
                                        self.st.candidates += 1;
                                        if pending_sers.len() == pending_sers.capacity() {
                                            let cur_ps = pending_sers.capacity() as u64
                                                * size_of::<Ser>() as u64;
                                            let next_ps = next_cap(
                                                pending_sers.len(),
                                                pending_sers.capacity(),
                                                1,
                                            )
                                                as u64
                                                * size_of::<Ser>() as u64;
                                            let projected = self
                                                .base_retained_bytes()
                                                .saturating_add(fixed_stage_bytes)
                                                .saturating_add(raw_bytes)
                                                .saturating_add(cur_ps)
                                                .saturating_add(next_ps)
                                                .saturating_add(lit_pass_bytes(&lit_pass));
                                            self.charge.resize(projected)?;
                                        }
                                        pending_sers.push_back(ser);
                                    }
                                    let settled = self
                                        .base_retained_bytes()
                                        .saturating_add(fixed_stage_bytes)
                                        .saturating_add(
                                            pending_sers.capacity() as u64
                                                * size_of::<Ser>() as u64,
                                        )
                                        .saturating_add(lit_pass_bytes(&lit_pass));
                                    self.charge.resize(settled)?;
                                }
                            }
                            OneConstCand::Done => {}
                        }
                        if cand_done {
                            cand = OneConstCand::Done;
                        }
                    }
                    if !pending_sers.is_empty() {
                        let q_id = lits[q_at - 1].lit;
                        let start = lits[..q_at].partition_point(|l| l.lit < q_id);
                        let graphs = &lits[start..q_at];
                        let mut tester = ConstTest::new(
                            q_geom.clone(),
                            self.spec.rel,
                            subject_const,
                            self.op_vertices,
                        );
                        let mut refined_since = 0usize;
                        while self.pending_out.len() < cap {
                            let Some(r) = pending_sers.pop_front() else {
                                break;
                            };
                            let stage_bytes =
                                one_const_stage_bytes(&cand, &pending_sers, &lit_pass);
                            let pass = if let Some(&v) = lit_pass.get(&r.o) {
                                v
                            } else {
                                let geom = self.geom_charged(ctx, r.o, stage_bytes)?;
                                let add =
                                    hash_growth::<(Id, bool)>(lit_pass.capacity(), lit_pass.len());
                                if add != 0 {
                                    self.charge.resize(
                                        self.base_retained_bytes()
                                            .saturating_add(stage_bytes)
                                            .saturating_add(add),
                                    )?;
                                }
                                let ok = if let Some(w) = geom {
                                    self.st.refined += 1;
                                    refined_since += 1;
                                    if refined_since.is_multiple_of(CHECK_EVERY) {
                                        ctx.check()?;
                                    }
                                    let ok = tester.holds(&w);
                                    if ok {
                                        self.pairs += 1;
                                    }
                                    ok
                                } else {
                                    false
                                };
                                lit_pass.insert(r.o, ok);
                                if add != 0 {
                                    let updated_stage =
                                        one_const_stage_bytes(&cand, &pending_sers, &lit_pass);
                                    self.charge.resize(
                                        self.base_retained_bytes().saturating_add(updated_stage),
                                    )?;
                                }
                                ok
                            };
                            if !pass {
                                continue;
                            }
                            if graph_var
                                && graphs[0].g.is_some()
                                && graphs.binary_search_by_key(&Some(r.g), |l| l.g).is_err()
                            {
                                continue;
                            }
                            self.st.matched += 1;
                            let stage_bytes =
                                one_const_stage_bytes(&cand, &pending_sers, &lit_pass);
                            let total_objs =
                                1 + self.ensure_features(ctx, r.s, r.g, stage_bytes)?;
                            for obj_idx in 0..total_objs {
                                if obj_idx.is_multiple_of(CHECK_EVERY) {
                                    ctx.check()?;
                                }
                                if self.pending_out.len() >= cap {
                                    active_ser = Some((r, obj_idx));
                                    return Ok(());
                                }
                                let so = self.object_at(&r, obj_idx).expect("cached object");
                                if subject_const {
                                    self.record_emit_charged(ctx, cid, so, r.g, stage_bytes)?;
                                } else {
                                    self.record_emit_charged(ctx, so, cid, r.g, stage_bytes)?;
                                }
                            }
                        }
                        if !self.pending_out.is_empty() {
                            return Ok(());
                        }
                        continue;
                    }
                    if matches!(cand, OneConstCand::Done) {
                        qg = None;
                        lit_pass.clear();
                    } else {
                        // Candidate source is not yet exhausted (e.g., an empty filtered
                        // scan chunk or predicate boundary); keep stepping it for this q.
                        continue;
                    }
                }
                if q_at >= lits.len() {
                    return Ok(());
                }
                let q = lits[q_at].lit;
                q_at += lits[q_at..].partition_point(|l| l.lit == q);
                lit_pass.clear();
                let stage_bytes = one_const_stage_bytes(&cand, &pending_sers, &lit_pass);
                let Some(q_geom) = self.geom_charged(ctx, q, stage_bytes)? else {
                    continue;
                };
                let window = q_geom.bbox84().filter(|_| self.spec.rel.index_usable());
                cand = match window {
                    Some(w) if search::indexed(&ctx.snap, &self.preds).is_some() => {
                        let base_now = self
                            .base_retained_bytes()
                            .saturating_add(fixed_stage_bytes)
                            .saturating_add(
                                pending_sers.capacity() as u64 * size_of::<Ser>() as u64,
                            )
                            .saturating_add(lit_pass_bytes(&lit_pass));
                        let charge = &mut self.charge;
                        OneConstCand::Indexed(Box::new(WindowIter::open(
                            ctx,
                            self.preds.clone(),
                            vec![w],
                            self.spec.graph.clone(),
                            false,
                            &mut |b| charge.resize(base_now.saturating_add(b)),
                        )?))
                    }
                    None if self.spec.rel.index_usable() && q_geom.empty => OneConstCand::Done,
                    w => {
                        if search::indexed(&ctx.snap, &self.preds).is_none() {
                            self.st.fallback = true;
                        }
                        OneConstCand::Scan {
                            pred_at: 0,
                            scan: None,
                            window: w,
                        }
                    }
                };
                qg = Some(q_geom);
            }
        })();
        stage = if q_at >= lits.len()
            && qg.is_none()
            && active_ser.is_none()
            && pending_sers.is_empty()
            && matches!(cand, OneConstCand::Done)
        {
            RelateStage::Done
        } else {
            RelateStage::OneConst {
                cid,
                subject_const,
                lits,
                q_at,
                qg,
                cand,
                pending_sers,
                active_ser,
                lit_pass,
            }
        };
        self.stage = stage;
        res
    }

    fn step_both_var(&mut self, ctx: &Ctx, cap: usize) -> Result<()> {
        let mut stage = std::mem::replace(&mut self.stage, RelateStage::Done);
        let RelateStage::BothVar {
            items,
            by_lit,
            by_lit_elem_bytes,
            boxed_idx,
            unboxed_idx,
            tree,
            mut phase,
            mut pair_emit,
        } = stage
        else {
            unreachable!()
        };
        let rel = self.spec.rel;
        let opv = self.op_vertices;
        let fixed_stage_bytes = (items.capacity() as u64 * size_of::<RelateItem>() as u64)
            .saturating_add(
                by_lit.capacity() as u64 * (size_of::<Id>() + size_of::<Vec<Ser>>() + 1) as u64,
            )
            .saturating_add(by_lit_elem_bytes)
            .saturating_add(
                (boxed_idx.capacity() + unboxed_idx.capacity()) as u64 * size_of::<usize>() as u64,
            )
            .saturating_add(tree.as_ref().map_or(0, PackedTree::bytes));
        let res: Result<()> = (|| {
            let mut cached_prep: Option<(usize, Prepared)> = None;
            let mut prep_hits = 0usize;
            let mut prep_for_i: Option<usize> = None;
            let mut scanned = 0usize;
            while self.pending_out.len() < cap {
                ctx.check()?;
                let stack_bytes = match &phase {
                    BothVarPhase::Boxed { stack, .. } => {
                        stack.capacity() as u64 * size_of::<usize>() as u64
                    }
                    _ => 0,
                };
                let stage_bytes = fixed_stage_bytes.saturating_add(stack_bytes);
                if let Some(pe) = &mut pair_emit {
                    if self.step_pair_emit(ctx, &by_lit, pe, cap, stage_bytes)? {
                        pair_emit = None;
                    } else {
                        break;
                    }
                }
                match &mut phase {
                    BothVarPhase::Boxed {
                        outer,
                        cur_i,
                        stack,
                        ws_f32,
                    } => {
                        let Some(t) = &tree else {
                            phase = if !unboxed_idx.is_empty() {
                                BothVarPhase::Unboxed {
                                    idx: 0,
                                    j: 0,
                                    rev_pending: false,
                                }
                            } else {
                                BothVarPhase::Done
                            };
                            continue;
                        };
                        if cur_i.is_none() {
                            if *outer >= boxed_idx.len() {
                                phase = if !unboxed_idx.is_empty() {
                                    BothVarPhase::Unboxed {
                                        idx: 0,
                                        j: 0,
                                        rev_pending: false,
                                    }
                                } else {
                                    BothVarPhase::Done
                                };
                                continue;
                            }
                            let i = boxed_idx[*outer];
                            *outer += 1;
                            let w = items[i].bbox.expect("boxed item");
                            *ws_f32 = super::index::window_f32(&w);
                            stack.clear();
                            stack.push(t.tree().root());
                            *cur_i = Some(i);
                            cached_prep = None;
                            prep_hits = 0;
                            prep_for_i = Some(i);
                        }
                        let i = cur_i.expect("active outer boxed item");
                        if prep_for_i != Some(i) {
                            cached_prep = None;
                            prep_hits = 0;
                            prep_for_i = Some(i);
                        }
                        let a = &items[i];
                        let w = *ws_f32;
                        let hits_f32 = |b: [f32; 4]| {
                            b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1]
                        };
                        let tr = t.tree();
                        while let Some(n) = stack.pop() {
                            scanned += 1;
                            if scanned.is_multiple_of(CHECK_EVERY) {
                                ctx.check()?;
                            }
                            if tr.is_item(n) {
                                if !hits_f32(tr.bbox(n)) {
                                    continue;
                                }
                                let c = tr.item(n);
                                self.st.candidates += 1;
                                if self.st.candidates.is_multiple_of(CHECK_CANDIDATES) {
                                    ctx.check_rows(self.st.candidates as usize)?;
                                }
                                let j = boxed_idx[c];
                                let b = &items[j];
                                self.st.refined += 1;
                                prep_hits += 1;
                                if prep_hits == PREPARE_OVER + 1 && cached_prep.is_none() {
                                    cached_prep = Some((i, Prepared::new(a.geom.clone())));
                                }
                                let ok = match &cached_prep {
                                    Some((pi, p))
                                        if *pi == i
                                            && u64::from(a.geom.vertices)
                                                + u64::from(b.geom.vertices)
                                                <= opv =>
                                    {
                                        p.relation(&b.geom, rel).unwrap_or(false)
                                    }
                                    Some((pi, _)) if *pi == i => false,
                                    _ => relate_relation(&a.geom, &b.geom, rel, opv),
                                };
                                if ok {
                                    self.pairs += 1;
                                    pair_emit = Some(PairEmit {
                                        a_lit: a.lit,
                                        b_lit: b.lit,
                                        ai: 0,
                                        bi: 0,
                                        oai: 0,
                                        obi: 0,
                                        oa_len: 0,
                                        ob_len: 0,
                                        matched_counted: false,
                                    });
                                    break;
                                }
                            } else {
                                self.st.nodes += 1;
                                for ch in tr.children(n) {
                                    if hits_f32(tr.bbox(ch)) {
                                        stack.push(ch);
                                    }
                                }
                            }
                        }
                        if stack.is_empty() && pair_emit.is_none() {
                            *cur_i = None;
                        }
                    }
                    BothVarPhase::Unboxed {
                        idx,
                        j,
                        rev_pending,
                    } => {
                        if *idx >= unboxed_idx.len() {
                            phase = BothVarPhase::Done;
                            continue;
                        }
                        let i = unboxed_idx[*idx];
                        let a = &items[i];
                        if *rev_pending {
                            *rev_pending = false;
                            let cur_j = *j;
                            *j += 1;
                            let b = &items[cur_j];
                            self.st.refined += 1;
                            if relate_relation(&b.geom, &a.geom, rel, opv) {
                                self.pairs += 1;
                                pair_emit = Some(PairEmit {
                                    a_lit: b.lit,
                                    b_lit: a.lit,
                                    ai: 0,
                                    bi: 0,
                                    oai: 0,
                                    obi: 0,
                                    oa_len: 0,
                                    ob_len: 0,
                                    matched_counted: false,
                                });
                            }
                            continue;
                        }
                        if *j >= items.len() {
                            *idx += 1;
                            *j = 0;
                            continue;
                        }
                        while *j < items.len() {
                            scanned += 1;
                            if scanned.is_multiple_of(CHECK_EVERY) {
                                ctx.check()?;
                            }
                            let cur_j = *j;
                            let b = &items[cur_j];
                            if b.bbox.is_none() && (b.geom.empty || cur_j < i) {
                                *j += 1;
                                continue;
                            }
                            self.st.refined += 1;
                            let fwd = relate_relation(&a.geom, &b.geom, rel, opv);
                            if i != cur_j {
                                *rev_pending = true;
                            } else {
                                *j += 1;
                            }
                            if fwd {
                                self.pairs += 1;
                                pair_emit = Some(PairEmit {
                                    a_lit: a.lit,
                                    b_lit: b.lit,
                                    ai: 0,
                                    bi: 0,
                                    oai: 0,
                                    obi: 0,
                                    oa_len: 0,
                                    ob_len: 0,
                                    matched_counted: false,
                                });
                                break;
                            }
                            if *rev_pending {
                                break;
                            }
                        }
                    }
                    BothVarPhase::Disjoint { i, j, rev_pending } => {
                        if *i >= items.len() {
                            phase = BothVarPhase::Done;
                            continue;
                        }
                        let cur_i = *i;
                        let a = &items[cur_i];
                        if *rev_pending {
                            *rev_pending = false;
                            let cur_j = *j;
                            *j += 1;
                            let b = &items[cur_j];
                            self.st.refined += 1;
                            if relate_relation(&b.geom, &a.geom, rel, opv) {
                                self.pairs += 1;
                                pair_emit = Some(PairEmit {
                                    a_lit: b.lit,
                                    b_lit: a.lit,
                                    ai: 0,
                                    bi: 0,
                                    oai: 0,
                                    obi: 0,
                                    oa_len: 0,
                                    ob_len: 0,
                                    matched_counted: false,
                                });
                            }
                            continue;
                        }
                        if *j >= items.len() {
                            *i += 1;
                            *j = *i;
                            cached_prep = None;
                            continue;
                        }
                        if items.len() - cur_i > PREPARE_OVER
                            && !matches!(cached_prep, Some((pi, _)) if pi == cur_i)
                        {
                            cached_prep = Some((cur_i, Prepared::new(a.geom.clone())));
                        }
                        while *j < items.len() {
                            scanned += 1;
                            if scanned.is_multiple_of(CHECK_EVERY) {
                                ctx.check()?;
                            }
                            let cur_j = *j;
                            let b = &items[cur_j];
                            self.st.refined += 1;
                            let fwd = match &cached_prep {
                                Some((pi, p))
                                    if *pi == cur_i
                                        && u64::from(a.geom.vertices)
                                            + u64::from(b.geom.vertices)
                                            <= opv =>
                                {
                                    p.relation(&b.geom, rel).unwrap_or(false)
                                }
                                Some((pi, _)) if *pi == cur_i => false,
                                _ => relate_relation(&a.geom, &b.geom, rel, opv),
                            };
                            if cur_i != cur_j {
                                *rev_pending = true;
                            } else {
                                *j += 1;
                            }
                            if fwd {
                                self.pairs += 1;
                                pair_emit = Some(PairEmit {
                                    a_lit: a.lit,
                                    b_lit: b.lit,
                                    ai: 0,
                                    bi: 0,
                                    oai: 0,
                                    obi: 0,
                                    oa_len: 0,
                                    ob_len: 0,
                                    matched_counted: false,
                                });
                                break;
                            }
                            if *rev_pending {
                                break;
                            }
                        }
                    }
                    BothVarPhase::Done => break,
                }
            }
            Ok(())
        })();
        stage = if matches!(phase, BothVarPhase::Done) && pair_emit.is_none() {
            RelateStage::Done
        } else {
            RelateStage::BothVar {
                items,
                by_lit,
                by_lit_elem_bytes,
                boxed_idx,
                unboxed_idx,
                tree,
                phase,
                pair_emit,
            }
        };
        self.stage = stage;
        res
    }
}

// ----------------------------------------------------------------- SpatialJoin --

struct OwnedIndexSide {
    scan: ScanSpec,
    pred: Id,
    subj: Option<Id>,
    graph: GraphFilter,
    vars: Vec<VarId>,
    cols: Vec<u8>,
}

impl OwnedIndexSide {
    fn of(s: &JoinSide) -> Result<Self> {
        let JoinSide::Index {
            scan,
            pred,
            geom_var,
            subj_var,
            graph_var,
        } = s
        else {
            unreachable!("an index side")
        };
        let shape = ScanShape::of(scan)
            .ok_or_else(|| Error::invalid("spatial join of an unexpected pattern"))?;
        let mut vars = Vec::new();
        let mut cols = Vec::new();
        for (v, c) in [(*subj_var, 0u8), (Some(*geom_var), 1), (*graph_var, 2)] {
            if let Some(v) = v
                && !vars.contains(&v)
            {
                vars.push(v);
                cols.push(c);
            }
        }
        Ok(Self {
            scan: scan.clone(),
            pred: *pred,
            subj: match shape.subj {
                Some(PathEnd::Const(s)) => Some(s),
                _ => None,
            },
            graph: shape.graph_filter(scan),
            vars,
            cols,
        })
    }

    fn rows(&self, ctx: &Ctx) -> u64 {
        ctx.snap
            .count(self.scan.perm, &self.scan.prefix)
            .unwrap_or(u64::MAX)
    }

    fn hit_row(&self, h: &Hit, out: &mut [Id]) {
        for (dst, c) in out.iter_mut().zip(&self.cols) {
            *dst = match c {
                0 => h.s,
                1 => h.o,
                _ => h.g,
            };
        }
    }
}

struct ExactTest {
    test: JoinTest,
    transposed: Option<String>,
    model: DistanceModel,
    op_vertices: u64,
}

impl ExactTest {
    fn new(ctx: &Ctx, test: &JoinTest) -> Self {
        Self {
            test: test.clone(),
            transposed: match test {
                JoinTest::Relate(p) => Some(transpose(p)),
                _ => None,
            },
            model: config(ctx).distance,
            op_vertices: ctx.geo.op_vertices(),
        }
    }

    fn sized(&self, a: &Geom, b: &Geom) -> bool {
        u64::from(a.vertices) + u64::from(b.vertices) <= self.op_vertices
    }

    fn holds(&self, a: &Geom, b: &Geom) -> bool {
        if !self.sized(a, b) {
            return false;
        }
        match &self.test {
            JoinTest::Relation(r) => relate::relation(a, b, *r),
            JoinTest::Relate(p) => relate::relate(a, b, p),
            JoinTest::Within { metres, inclusive } => {
                return distance::distance_m(a, b, self.model)
                    .is_ok_and(|d| d < *metres || *inclusive && d == *metres);
            }
        }
        .unwrap_or(false)
    }

    fn holds_left(&self, a: &Prepared, b: &Geom) -> bool {
        if !self.sized(a.geom(), b) {
            return false;
        }
        match &self.test {
            JoinTest::Relation(r) => a.relation(b, *r).unwrap_or(false),
            JoinTest::Relate(p) => a.relate(b, p).unwrap_or(false),
            JoinTest::Within { .. } => self.holds(a.geom(), b),
        }
    }

    fn holds_right(&self, a: &Geom, b: &Prepared) -> bool {
        if !self.sized(a, b.geom()) {
            return false;
        }
        if !same_frame(a, b.geom()) {
            return self.holds(a, b.geom());
        }
        match (&self.test, &self.transposed) {
            (JoinTest::Relation(r), _) => match r.converse() {
                Some(c) => b.relation(a, c).unwrap_or(false),
                None => self.holds(a, b.geom()),
            },
            (JoinTest::Relate(_), Some(tp)) => b.relate(a, tp).unwrap_or(false),
            _ => self.holds(a, b.geom()),
        }
    }

    fn prepares(&self) -> bool {
        !matches!(self.test, JoinTest::Within { .. })
    }
}

/// A build-side collection of rows grouped by distinct geometry, with an optional packed
/// R-tree over its placed items.
pub(crate) struct JoinSideData {
    table: Table,
    items: Vec<JoinItem>,
    rows: Vec<Vec<u32>>,
    by_id: FxHashMap<Id, u32>,
    boxed_idx: Vec<usize>,
    unplaced_idx: Vec<usize>,
    tree: Option<PackedTree>,
}

impl JoinSideData {
    fn new(vars: Vec<VarId>) -> Self {
        Self {
            table: Table::new(vars),
            items: Vec::new(),
            rows: Vec::new(),
            by_id: FxHashMap::default(),
            boxed_idx: Vec::new(),
            unplaced_idx: Vec::new(),
            tree: None,
        }
    }

    fn retained_bytes(&self) -> u64 {
        let rows_vec: u64 = self
            .rows
            .iter()
            .map(|r| r.capacity() as u64 * size_of::<u32>() as u64)
            .sum();
        table_capacity_bytes(&self.table)
            .saturating_add(self.items.capacity() as u64 * size_of::<JoinItem>() as u64)
            .saturating_add(self.rows.capacity() as u64 * size_of::<Vec<u32>>() as u64)
            .saturating_add(rows_vec)
            .saturating_add(self.by_id.capacity() as u64 * (size_of::<Id>() + 5) as u64)
            .saturating_add(
                (self.boxed_idx.capacity() + self.unplaced_idx.capacity()) as u64
                    * size_of::<usize>() as u64,
            )
            .saturating_add(self.tree.as_ref().map_or(0, PackedTree::bytes))
    }

    fn add_table_batch_charged(
        &mut self,
        ctx: &Ctx,
        batch: &Table,
        geom_var: VarId,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let Some(c) = batch.col_of(geom_var) else {
            return Ok(());
        };
        let mut row_buf = vec![Id::UNDEF; self.table.width()];
        let col_map: Vec<Option<usize>> =
            self.table.vars.iter().map(|v| batch.col_of(*v)).collect();
        for i in 0..batch.len() {
            if i.is_multiple_of(CHECK_CANDIDATES as usize) {
                ctx.check()?;
            }
            let id = batch.cols[c][i];
            if id.is_undef() {
                continue;
            }
            let item = match self.by_id.get(&id) {
                Some(&idx) => idx,
                None => {
                    let g = super::memo::by_id(ctx, id, None).ok();
                    let by_id_add = if self.by_id.len() == self.by_id.capacity() {
                        (next_cap(self.by_id.len(), self.by_id.capacity(), 1)
                            - self.by_id.capacity()) as u64
                            * (size_of::<Id>() + 5) as u64
                    } else {
                        0
                    };
                    let idx = match g.as_ref().and_then(|g| item_box(g)) {
                        Some(b) => {
                            let items_add = if self.items.len() == self.items.capacity() {
                                (next_cap(self.items.len(), self.items.capacity(), 1)
                                    - self.items.capacity()) as u64
                                    * size_of::<JoinItem>() as u64
                            } else {
                                0
                            };
                            let rows_add = if self.rows.len() == self.rows.capacity() {
                                (next_cap(self.rows.len(), self.rows.capacity(), 1)
                                    - self.rows.capacity()) as u64
                                    * size_of::<Vec<u32>>() as u64
                            } else {
                                0
                            };
                            if by_id_add > 0 || items_add > 0 || rows_add > 0 {
                                admit(
                                    self.retained_bytes()
                                        .saturating_add(by_id_add)
                                        .saturating_add(items_add)
                                        .saturating_add(rows_add),
                                )?;
                            }
                            let idx = self.items.len() as u32;
                            self.items.push(JoinItem {
                                row: idx,
                                id,
                                bbox84: b,
                                geom: g.unwrap(),
                            });
                            self.rows.push(Vec::new());
                            idx
                        }
                        None => {
                            if by_id_add > 0 {
                                admit(self.retained_bytes().saturating_add(by_id_add))?;
                            }
                            u32::MAX
                        }
                    };
                    self.by_id.insert(id, idx);
                    idx
                }
            };
            if item != u32::MAX {
                let tbl_add: u64 = self
                    .table
                    .cols
                    .iter()
                    .map(|col| {
                        if col.len() == col.capacity() {
                            (next_cap(col.len(), col.capacity(), 1) - col.capacity()) as u64 * 8
                        } else {
                            0
                        }
                    })
                    .sum();
                let rvec = &self.rows[item as usize];
                let rvec_add = if rvec.len() == rvec.capacity() {
                    (next_cap(rvec.len(), rvec.capacity(), 1) - rvec.capacity()) as u64
                        * size_of::<u32>() as u64
                } else {
                    0
                };
                if tbl_add > 0 || rvec_add > 0 {
                    admit(
                        self.retained_bytes()
                            .saturating_add(tbl_add)
                            .saturating_add(rvec_add),
                    )?;
                }
                let row_idx = self.table.len() as u32;
                for (dst, src) in row_buf.iter_mut().zip(&col_map) {
                    *dst = src.map_or(Id::UNDEF, |sc| batch.cols[sc][i]);
                }
                self.table.push_row(&row_buf);
                self.rows[item as usize].push(row_idx);
            }
        }
        Ok(())
    }

    fn add_hit_charged(
        &mut self,
        ctx: &Ctx,
        ix: &OwnedIndexSide,
        h: &Hit,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let item = match self.by_id.get(&h.o) {
            Some(&idx) => idx,
            None => {
                let by_id_add = if self.by_id.len() == self.by_id.capacity() {
                    (next_cap(self.by_id.len(), self.by_id.capacity(), 1) - self.by_id.capacity())
                        as u64
                        * (size_of::<Id>() + 5) as u64
                } else {
                    0
                };
                let idx = match noted(ctx, h.entry.geom(&ctx.snap))
                    .ok()
                    .and_then(|g| item_box(&g).map(|b| (g, b)))
                {
                    Some((g, b)) => {
                        let items_add = if self.items.len() == self.items.capacity() {
                            (next_cap(self.items.len(), self.items.capacity(), 1)
                                - self.items.capacity()) as u64
                                * size_of::<JoinItem>() as u64
                        } else {
                            0
                        };
                        let rows_add = if self.rows.len() == self.rows.capacity() {
                            (next_cap(self.rows.len(), self.rows.capacity(), 1)
                                - self.rows.capacity()) as u64
                                * size_of::<Vec<u32>>() as u64
                        } else {
                            0
                        };
                        if by_id_add > 0 || items_add > 0 || rows_add > 0 {
                            admit(
                                self.retained_bytes()
                                    .saturating_add(by_id_add)
                                    .saturating_add(items_add)
                                    .saturating_add(rows_add),
                            )?;
                        }
                        let idx = self.items.len() as u32;
                        self.items.push(JoinItem {
                            row: idx,
                            id: h.o,
                            bbox84: b,
                            geom: g,
                        });
                        self.rows.push(Vec::new());
                        idx
                    }
                    None => {
                        if by_id_add > 0 {
                            admit(self.retained_bytes().saturating_add(by_id_add))?;
                        }
                        u32::MAX
                    }
                };
                self.by_id.insert(h.o, idx);
                idx
            }
        };
        if item != u32::MAX {
            let tbl_add: u64 = self
                .table
                .cols
                .iter()
                .map(|col| {
                    if col.len() == col.capacity() {
                        (next_cap(col.len(), col.capacity(), 1) - col.capacity()) as u64 * 8
                    } else {
                        0
                    }
                })
                .sum();
            let rvec = &self.rows[item as usize];
            let rvec_add = if rvec.len() == rvec.capacity() {
                (next_cap(rvec.len(), rvec.capacity(), 1) - rvec.capacity()) as u64
                    * size_of::<u32>() as u64
            } else {
                0
            };
            if tbl_add > 0 || rvec_add > 0 {
                admit(
                    self.retained_bytes()
                        .saturating_add(tbl_add)
                        .saturating_add(rvec_add),
                )?;
            }
            let row_idx = self.table.len() as u32;
            let mut row_buf = vec![Id::UNDEF; ix.cols.len()];
            ix.hit_row(h, &mut row_buf);
            self.table.push_row(&row_buf);
            self.rows[item as usize].push(row_idx);
        }
        Ok(())
    }

    fn pack_charged(&mut self, admit: &mut dyn FnMut(u64) -> Result<()>) -> Result<()> {
        let n_boxed = self.items.iter().filter(|it| placed(&it.bbox84)).count();
        let n_unplaced = self.items.len().saturating_sub(n_boxed);
        let idx_bytes =
            (n_boxed.saturating_add(n_unplaced) as u64).saturating_mul(size_of::<usize>() as u64);
        let tree_est = PackedTree::estimate_bytes(n_boxed);
        admit(
            self.retained_bytes()
                .saturating_add(idx_bytes)
                .saturating_add(tree_est),
        )?;
        self.boxed_idx = (0..self.items.len())
            .filter(|&i| placed(&self.items[i].bbox84))
            .collect();
        self.unplaced_idx = (0..self.items.len())
            .filter(|&i| !placed(&self.items[i].bbox84))
            .collect();
        self.tree = PackedTree::pack(
            self.boxed_idx
                .iter()
                .map(|&i| round_out(self.items[i].bbox84)),
        );
        admit(self.retained_bytes())
    }

    fn read_index(
        ctx: &Ctx,
        ix: &OwnedIndexSide,
        st: &mut SearchStats,
        charge: &mut OwnedCharge,
    ) -> Result<Self> {
        let mut side = Self::new(ix.vars.clone());
        let mut iter = WindowIter::open(
            ctx,
            vec![ix.pred],
            vec![JOIN_WORLD],
            ix.graph.clone(),
            ix.scan.dedup,
            &mut |b| charge.resize(b),
        )?;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let base = side.retained_bytes();
            let ended = iter.next_hits(ctx, &mut buf, 4096, &mut |b| {
                charge.resize(base.saturating_add(b))
            })?;
            let iter_bytes = iter.retained_bytes();
            for h in &buf {
                if ix.subj.is_none_or(|s| s == h.s) {
                    side.add_hit_charged(ctx, ix, h, &mut |b| {
                        charge.resize(b.saturating_add(iter_bytes))
                    })?;
                }
            }
            ctx.check_output(side.table.len(), side.table.width())?;
            charge.resize(side.retained_bytes().saturating_add(iter_bytes))?;
            if ended {
                break;
            }
        }
        st.nodes += iter.st.nodes;
        st.rechecked += iter.st.rechecked;
        st.fallback |= iter.st.fallback;
        drop(iter);
        side.pack_charged(&mut |b| charge.resize(b))?;
        Ok(side)
    }
}

/// Column mapping from `(left_row, right_row)` into the join's output `vars`.
struct OutputMap {
    /// For each output column: `(from_left, col_index)`.
    cols: Vec<(bool, Option<usize>)>,
}

impl OutputMap {
    fn new(vars: &[VarId], left_vars: &[VarId], right_vars: &[VarId]) -> Self {
        let cols = vars
            .iter()
            .map(|v| match left_vars.iter().position(|x| x == v) {
                Some(c) => (true, Some(c)),
                None => (false, right_vars.iter().position(|x| x == v)),
            })
            .collect();
        Self { cols }
    }

    fn write_row(&self, lrow: &[Id], rrow: &[Id], dst: &mut [Id]) {
        for (x, &(is_left, col)) in dst.iter_mut().zip(&self.cols) {
            *x = match (is_left, col) {
                (true, Some(c)) => lrow[c],
                (false, Some(c)) => rrow[c],
                _ => Id::UNDEF,
            };
        }
    }
}

/// Streaming probe cursor for one outer geometry against an indexed `OwnedIndexSide`.
struct ActiveProbe {
    item_idx: usize,
    iter: WindowIter,
    hits: VecDeque<Hit>,
    lit_pass: FxHashMap<Id, bool>,
    /// Currently matched inner hit row and next index into `outer.rows[item_idx]`.
    pending_match: Option<(Vec<Id>, usize)>,
}

impl ActiveProbe {
    fn retained_bytes(&self) -> u64 {
        let pm = self
            .pending_match
            .as_ref()
            .map_or(0, |(r, _)| r.capacity() as u64 * size_of::<Id>() as u64);
        self.iter
            .retained_bytes()
            .saturating_add(self.hits.capacity() as u64 * size_of::<Hit>() as u64)
            .saturating_add(lit_pass_bytes(&self.lit_pass))
            .saturating_add(pm)
    }
}

/// Resumable probe cursor for one probe geometry against an in-memory `JoinSideData`.
struct SideProbeState {
    probe_geom: GeomRef,
    probe_row: Vec<Id>,
    probe_is_left: bool,
    placed: bool,
    ws_f32: Vec<[f32; 4]>,
    tree_stack: Vec<usize>,
    unplaced_at: usize,
    /// Currently matched build item index and next index into `side.rows[bi]`.
    active_match: Option<(usize, usize)>,
}

impl SideProbeState {
    fn open(
        side: &JoinSideData,
        test: &JoinTest,
        probe_geom: GeomRef,
        probe_box: [f64; 4],
        probe_row: Vec<Id>,
        probe_is_left: bool,
    ) -> Self {
        let is_placed = placed(&probe_box);
        let (ws_f32, tree_stack) = if is_placed {
            if let Some(tree) = &side.tree {
                let ws: Vec<[f32; 4]> = join_windows(test, probe_box)
                    .iter()
                    .map(super::index::window_f32)
                    .collect();
                (ws, vec![tree.tree().root()])
            } else {
                (Vec::new(), Vec::new())
            }
        } else {
            (Vec::new(), Vec::new())
        };
        Self {
            probe_geom,
            probe_row,
            probe_is_left,
            placed: is_placed,
            ws_f32,
            tree_stack,
            unplaced_at: 0,
            active_match: None,
        }
    }

    fn retained_bytes(&self) -> u64 {
        (self.probe_row.capacity() as u64 * size_of::<Id>() as u64)
            .saturating_add(self.ws_f32.capacity() as u64 * size_of::<[f32; 4]>() as u64)
            .saturating_add(self.tree_stack.capacity() as u64 * size_of::<usize>() as u64)
    }
}

/// Resumable spatial join state machine supporting:
/// - two index sides (`0` plan children)
/// - one plan child + one index side (`1` plan child)
/// - two plan children (`2` plan children)
pub(crate) struct SpatialJoinEngine {
    spec: SpatialJoinSpec,
    vars: Vec<VarId>,
    exact: ExactTest,
    state_name: String,
    mode: JoinMode,
    pending_out: VecDeque<Vec<Id>>,
    st: SearchStats,
    npairs: u64,
    probes: bool,
    charge: OwnedCharge,
}

type BatchSink<'a> = dyn FnMut(&Table) -> Result<()> + 'a;

struct ProbeSink<'a> {
    ctx: &'a Ctx,
    exact: &'a ExactTest,
    st: &'a mut SearchStats,
    npairs: &'a mut u64,
    map: &'a OutputMap,
    vars_len: usize,
    pending_out: &'a mut VecDeque<Vec<Id>>,
    charge: &'a mut OwnedCharge,
    /// Live bytes held by the engine excluding `pending_out` and the active probe.
    other_bytes: u64,
}

impl ProbeSink<'_> {
    fn pending_out_bytes(&self) -> u64 {
        self.pending_out.capacity() as u64
            * (size_of::<Vec<Id>>() + self.vars_len * size_of::<Id>()) as u64
    }

    fn push_pending_row(&mut self, row: Vec<Id>, active_bytes: u64) -> Result<()> {
        if self.pending_out.len() == self.pending_out.capacity() {
            let next_p = next_cap(self.pending_out.len(), self.pending_out.capacity(), 1) as u64
                * (size_of::<Vec<Id>>() + self.vars_len * size_of::<Id>()) as u64;
            self.charge.resize(
                self.other_bytes
                    .saturating_add(active_bytes)
                    .saturating_add(next_p),
            )?;
        }
        self.pending_out.push_back(row);
        Ok(())
    }
}

enum JoinMode {
    /// Two index sides, streaming outer via `WindowIter` and probing inner via `WindowIter`.
    IndexIndexProbe {
        outer_ix: OwnedIndexSide,
        inner_ix: OwnedIndexSide,
        outer_is_left: bool,
        map: OutputMap,
        outer_iter: Option<Box<WindowIter>>,
        wave: Option<JoinSideData>,
        active: Option<Box<ActiveProbe>>,
        next_item: usize,
    },
    /// Two index sides where inner is not indexed: inner built into `JoinSideData`,
    /// outer streamed via `WindowIter`.
    IndexIndexBuild {
        outer_ix: OwnedIndexSide,
        inner: JoinSideData,
        outer_is_left: bool,
        map: OutputMap,
        outer_iter: Option<Box<WindowIter>>,
        outer_hits: VecDeque<Hit>,
        active_probe: Option<Box<SideProbeState>>,
    },
    /// One plan child streamed batch-by-batch, probing a usable spatial index.
    PlanIndexProbe {
        child_idx: usize,
        geom_var: VarId,
        plan_is_left: bool,
        ix: OwnedIndexSide,
        map: OutputMap,
        batch: Option<JoinSideData>,
        active: Option<Box<ActiveProbe>>,
        next_item: usize,
        child_done: bool,
    },
    /// One plan child streamed batch-by-batch against a built (unindexed) `IndexSide`.
    PlanIndexBuild {
        child_idx: usize,
        geom_var: VarId,
        plan_is_left: bool,
        inner: JoinSideData,
        map: OutputMap,
        batch: Option<(Table, usize)>,
        active_probe: Option<Box<SideProbeState>>,
        child_done: bool,
    },
    /// Two plan children: build side read whole into `JoinSideData`, probe child streamed.
    PlanPlan {
        probe_child: usize,
        probe_geom_var: VarId,
        build_is_left: bool,
        build: JoinSideData,
        map: OutputMap,
        batch: Option<(Table, usize)>,
        active_probe: Option<Box<SideProbeState>>,
        probe_done: bool,
    },
    Done,
}

impl SpatialJoinEngine {
    pub(crate) fn open_no_children(
        ctx: &Arc<Ctx>,
        spec: &SpatialJoinSpec,
        vars: &[VarId],
    ) -> Result<Self> {
        let exact = ExactTest::new(ctx, &spec.test);
        let state_name = state(ctx).to_string();
        let l = OwnedIndexSide::of(&spec.left)?;
        let r = OwnedIndexSide::of(&spec.right)?;
        let outer_is_left = l.rows(ctx) <= r.rows(ctx);
        let (outer_ix, inner_ix) = if outer_is_left { (l, r) } else { (r, l) };
        let map = if outer_is_left {
            OutputMap::new(vars, &outer_ix.vars, &inner_ix.vars)
        } else {
            OutputMap::new(vars, &inner_ix.vars, &outer_ix.vars)
        };
        let mut charge = OwnedCharge::new(ctx, 0)?;
        let mut st = SearchStats::default();
        let inner_usable = search::indexed(&ctx.snap, &[inner_ix.pred]).is_some();
        let (mode, probes) = if inner_usable {
            let outer_iter = WindowIter::open(
                ctx,
                vec![outer_ix.pred],
                vec![JOIN_WORLD],
                outer_ix.graph.clone(),
                outer_ix.scan.dedup,
                &mut |b| charge.resize(b),
            )?;
            (
                JoinMode::IndexIndexProbe {
                    outer_ix,
                    inner_ix,
                    outer_is_left,
                    map,
                    outer_iter: Some(Box::new(outer_iter)),
                    wave: None,
                    active: None,
                    next_item: 0,
                },
                true,
            )
        } else {
            let inner = JoinSideData::read_index(ctx, &inner_ix, &mut st, &mut charge)?;
            let base = inner.retained_bytes();
            let outer_iter = WindowIter::open(
                ctx,
                vec![outer_ix.pred],
                vec![JOIN_WORLD],
                outer_ix.graph.clone(),
                outer_ix.scan.dedup,
                &mut |b| charge.resize(base.saturating_add(b)),
            )?;
            (
                JoinMode::IndexIndexBuild {
                    outer_ix,
                    inner,
                    outer_is_left,
                    map,
                    outer_iter: Some(Box::new(outer_iter)),
                    outer_hits: VecDeque::new(),
                    active_probe: None,
                },
                false,
            )
        };
        Ok(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            exact,
            state_name,
            mode,
            pending_out: VecDeque::new(),
            st,
            npairs: 0,
            probes,
            charge,
        })
    }

    pub(crate) fn open_one_child(
        ctx: &Arc<Ctx>,
        spec: &SpatialJoinSpec,
        vars: &[VarId],
        child_vars: &[VarId],
    ) -> Result<Self> {
        let exact = ExactTest::new(ctx, &spec.test);
        let state_name = state(ctx).to_string();
        let (child_idx, geom_var, plan_is_left, ix) = match (&spec.left, &spec.right) {
            (JoinSide::Plan { child, geom_var }, JoinSide::Index { .. }) => {
                (*child, *geom_var, true, OwnedIndexSide::of(&spec.right)?)
            }
            (JoinSide::Index { .. }, JoinSide::Plan { child, geom_var }) => {
                (*child, *geom_var, false, OwnedIndexSide::of(&spec.left)?)
            }
            _ => unreachable!("one-child spatial join has one plan side and one index side"),
        };
        let map = if plan_is_left {
            OutputMap::new(vars, child_vars, &ix.vars)
        } else {
            OutputMap::new(vars, &ix.vars, child_vars)
        };
        let mut charge = OwnedCharge::new(ctx, 0)?;
        let mut st = SearchStats::default();
        let usable = search::indexed(&ctx.snap, &[ix.pred]).is_some();
        let (mode, probes) = if usable {
            (
                JoinMode::PlanIndexProbe {
                    child_idx,
                    geom_var,
                    plan_is_left,
                    ix,
                    map,
                    batch: None,
                    active: None,
                    next_item: 0,
                    child_done: false,
                },
                true,
            )
        } else {
            let inner = JoinSideData::read_index(ctx, &ix, &mut st, &mut charge)?;
            (
                JoinMode::PlanIndexBuild {
                    child_idx,
                    geom_var,
                    plan_is_left,
                    inner,
                    map,
                    batch: None,
                    active_probe: None,
                    child_done: false,
                },
                false,
            )
        };
        Ok(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            exact,
            state_name,
            mode,
            pending_out: VecDeque::new(),
            st,
            npairs: 0,
            probes,
            charge,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_two_children(
        ctx: &Arc<Ctx>,
        spec: &SpatialJoinSpec,
        vars: &[VarId],
        left_vars: &[VarId],
        right_vars: &[VarId],
        build_is_left: bool,
        build_table_fn: &mut dyn FnMut(usize, &mut BatchSink<'_>) -> Result<()>,
    ) -> Result<Self> {
        let exact = ExactTest::new(ctx, &spec.test);
        let state_name = state(ctx).to_string();
        let (
            JoinSide::Plan {
                child: lc,
                geom_var: lg,
            },
            JoinSide::Plan {
                child: rc,
                geom_var: rg,
            },
        ) = (&spec.left, &spec.right)
        else {
            unreachable!("two-child spatial join has two plan sides")
        };
        let (build_child, build_geom, build_vars, probe_child, probe_geom) = if build_is_left {
            (*lc, *lg, left_vars, *rc, *rg)
        } else {
            (*rc, *rg, right_vars, *lc, *lg)
        };
        let mut charge = OwnedCharge::new(ctx, 0)?;
        let mut build = JoinSideData::new(build_vars.to_vec());
        build_table_fn(build_child, &mut |batch| {
            build.add_table_batch_charged(ctx, batch, build_geom, &mut |b| charge.resize(b))?;
            charge.resize(build.retained_bytes())
        })?;
        build.pack_charged(&mut |b| charge.resize(b))?;
        let map = OutputMap::new(vars, left_vars, right_vars);
        Ok(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            exact,
            state_name,
            mode: JoinMode::PlanPlan {
                probe_child,
                probe_geom_var: probe_geom,
                build_is_left,
                build,
                map,
                batch: None,
                active_probe: None,
                probe_done: false,
            },
            pending_out: VecDeque::new(),
            st: SearchStats::default(),
            npairs: 0,
            probes: false,
            charge,
        })
    }

    pub(crate) fn counters(&self) -> Counters {
        let mut c = Counters::new();
        c.insert("candidates".into(), self.st.candidates.into());
        c.insert("refined".into(), self.st.refined.into());
        c.insert("matched".into(), self.st.matched.into());
        c.insert("pairs".into(), self.npairs.into());
        c.insert("treeNodesVisited".into(), self.st.nodes.into());
        c.insert("rechecked".into(), self.st.rechecked.into());
        c.insert("indexProbes".into(), self.probes.into());
        c.insert("index".into(), self.state_name.clone().into());
        c.insert("fallback".into(), self.st.fallback.into());
        c
    }

    fn pending_bytes(&self) -> u64 {
        self.pending_out.capacity() as u64
            * (size_of::<Vec<Id>>() + self.vars.len() * size_of::<Id>()) as u64
    }

    fn retained_bytes(&self) -> u64 {
        let pending = self.pending_bytes();
        let mode_bytes = match &self.mode {
            JoinMode::IndexIndexProbe {
                outer_iter,
                wave,
                active,
                ..
            } => outer_iter
                .as_deref()
                .map_or(0, WindowIter::retained_bytes)
                .saturating_add(wave.as_ref().map_or(0, JoinSideData::retained_bytes))
                .saturating_add(active.as_deref().map_or(0, ActiveProbe::retained_bytes)),
            JoinMode::IndexIndexBuild {
                inner,
                outer_iter,
                outer_hits,
                active_probe,
                ..
            } => inner
                .retained_bytes()
                .saturating_add(outer_iter.as_deref().map_or(0, WindowIter::retained_bytes))
                .saturating_add(outer_hits.capacity() as u64 * size_of::<Hit>() as u64)
                .saturating_add(
                    active_probe
                        .as_deref()
                        .map_or(0, SideProbeState::retained_bytes),
                ),
            JoinMode::PlanIndexProbe { batch, active, .. } => batch
                .as_ref()
                .map_or(0, JoinSideData::retained_bytes)
                .saturating_add(active.as_deref().map_or(0, ActiveProbe::retained_bytes)),
            JoinMode::PlanIndexBuild {
                inner,
                batch,
                active_probe,
                ..
            } => inner
                .retained_bytes()
                .saturating_add(batch.as_ref().map_or(0, |(t, _)| table_capacity_bytes(t)))
                .saturating_add(
                    active_probe
                        .as_deref()
                        .map_or(0, SideProbeState::retained_bytes),
                ),
            JoinMode::PlanPlan {
                build,
                batch,
                active_probe,
                ..
            } => build
                .retained_bytes()
                .saturating_add(batch.as_ref().map_or(0, |(t, _)| table_capacity_bytes(t)))
                .saturating_add(
                    active_probe
                        .as_deref()
                        .map_or(0, SideProbeState::retained_bytes),
                ),
            JoinMode::Done => 0,
        };
        pending.saturating_add(mode_bytes)
    }

    fn update_charge(&mut self) -> Result<()> {
        self.charge.resize(self.retained_bytes())
    }

    /// If this is a two-child join whose build side produced zero geometries, returns
    /// the probe child index so the caller can mark it skipped.
    pub(crate) fn empty_build_probe_child(&self) -> Option<usize> {
        match &self.mode {
            JoinMode::PlanPlan {
                probe_child, build, ..
            } if build.items.is_empty() => Some(*probe_child),
            _ => None,
        }
    }

    fn flush_pending(&mut self, ctx: &Ctx, out: &mut Table, cap: usize) -> Result<()> {
        if self.pending_out.is_empty() {
            return Ok(());
        }
        if self.spec.filter.is_empty() {
            while out.len() < cap {
                let Some(row) = self.pending_out.pop_front() else {
                    break;
                };
                out.push_row(&row);
            }
            return Ok(());
        }
        let mut tmp = Table::new(self.vars.clone());
        let mut buf = vec![Id::UNDEF; self.vars.len()];
        while out.len() < cap && !self.pending_out.is_empty() {
            let take = cap.saturating_sub(out.len());
            for _ in 0..take {
                let Some(row) = self.pending_out.pop_front() else {
                    break;
                };
                tmp.push_row(&row);
            }
            filter_table(ctx, &mut tmp, &self.spec.filter)?;
            for i in 0..tmp.len() {
                for (c, slot) in buf.iter_mut().enumerate() {
                    *slot = tmp.cols[c][i];
                }
                out.push_row(&buf);
            }
            for col in &mut tmp.cols {
                col.clear();
            }
            tmp.len = 0;
        }
        Ok(())
    }

    /// Advance one resumable probe against an in-memory `JoinSideData`, yielding as soon
    /// as `sink.pending_out.len() >= cap`. Returns `true` when the probe is exhausted.
    fn step_side_probe(
        sink: &mut ProbeSink<'_>,
        side: &JoinSideData,
        probe: &mut SideProbeState,
        cap: usize,
    ) -> Result<bool> {
        let mut build_row = vec![Id::UNDEF; side.table.width()];
        let mut out_row = vec![Id::UNDEF; sink.vars_len];
        let mut prep: Option<Prepared> = None;
        let mut tested = 0usize;
        let mut ticks = 0usize;
        loop {
            // 1. Drain any currently matched build item's rows up to `cap`.
            if let Some((bi, mut row_at)) = probe.active_match.take() {
                let rows = &side.rows[bi];
                while row_at < rows.len() {
                    ticks += 1;
                    if ticks.is_multiple_of(CHECK_EVERY) {
                        sink.ctx.check()?;
                    }
                    if sink.pending_out.len() >= cap {
                        probe.active_match = Some((bi, row_at));
                        return Ok(false);
                    }
                    let br = rows[row_at];
                    row_at += 1;
                    for (col, dst) in build_row.iter_mut().enumerate() {
                        *dst = side.table.cols[col][br as usize];
                    }
                    if probe.probe_is_left {
                        sink.map
                            .write_row(&probe.probe_row, &build_row, &mut out_row);
                    } else {
                        sink.map
                            .write_row(&build_row, &probe.probe_row, &mut out_row);
                    }
                    let active_bytes = probe.retained_bytes();
                    sink.push_pending_row(out_row.clone(), active_bytes)?;
                }
                if sink.pending_out.len() >= cap {
                    return Ok(false);
                }
            }
            // 2. Find the next matching build item.
            if probe.placed {
                let Some(tree) = &side.tree else {
                    return Ok(true);
                };
                let tr = tree.tree();
                let ws_f32 = &probe.ws_f32;
                let hits_f32 = |b: [f32; 4]| {
                    ws_f32
                        .iter()
                        .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1])
                };
                let mut matched_bi = None;
                while let Some(n) = probe.tree_stack.pop() {
                    ticks += 1;
                    if ticks.is_multiple_of(CHECK_EVERY) {
                        sink.ctx.check()?;
                    }
                    if tr.is_item(n) {
                        if !hits_f32(tr.bbox(n)) {
                            continue;
                        }
                        sink.st.candidates = sink.st.candidates.saturating_add(1);
                        if sink.st.candidates > sink.ctx.max_rows as u64 {
                            return Err(Error::BudgetExceeded(Budget {
                                kind: BudgetKind::Rows,
                                limit: sink.ctx.max_rows as u64,
                                requested: sink.st.candidates,
                            }));
                        }
                        let bi = side.boxed_idx[tr.item(n)];
                        let other = &side.items[bi];
                        sink.st.refined += 1;
                        tested += 1;
                        if sink.exact.prepares() && tested == PREPARE_OVER + 1 && prep.is_none() {
                            prep = Some(Prepared::new(probe.probe_geom.clone()));
                        }
                        let ok = match (&prep, probe.probe_is_left) {
                            (Some(p), true) => sink.exact.holds_left(p, &other.geom),
                            (Some(p), false) => sink.exact.holds_right(&other.geom, p),
                            (None, true) => sink.exact.holds(&probe.probe_geom, &other.geom),
                            (None, false) => sink.exact.holds(&other.geom, &probe.probe_geom),
                        };
                        if ok {
                            *sink.npairs += 1;
                            matched_bi = Some(bi);
                            break;
                        }
                    } else {
                        sink.st.nodes += 1;
                        for c in tr.children(n) {
                            if hits_f32(tr.bbox(c)) {
                                if probe.tree_stack.len() == probe.tree_stack.capacity() {
                                    let add = (next_cap(
                                        probe.tree_stack.len(),
                                        probe.tree_stack.capacity(),
                                        1,
                                    ) - probe.tree_stack.capacity())
                                        as u64
                                        * size_of::<usize>() as u64;
                                    let projected = sink
                                        .other_bytes
                                        .saturating_add(sink.pending_out_bytes())
                                        .saturating_add(probe.retained_bytes())
                                        .saturating_add(add);
                                    sink.charge.resize(projected)?;
                                }
                                probe.tree_stack.push(c);
                            }
                        }
                    }
                }
                match matched_bi {
                    Some(bi) => probe.active_match = Some((bi, 0)),
                    None => return Ok(true),
                }
            } else {
                let mut matched_bi = None;
                while probe.unplaced_at < side.unplaced_idx.len() {
                    ticks += 1;
                    if ticks.is_multiple_of(CHECK_EVERY) {
                        sink.ctx.check()?;
                    }
                    let bi = side.unplaced_idx[probe.unplaced_at];
                    probe.unplaced_at += 1;
                    let other = &side.items[bi];
                    if probe.probe_geom.crs != other.geom.crs {
                        continue;
                    }
                    sink.st.candidates = sink.st.candidates.saturating_add(1);
                    if sink.st.candidates > sink.ctx.max_rows as u64 {
                        return Err(Error::BudgetExceeded(Budget {
                            kind: BudgetKind::Rows,
                            limit: sink.ctx.max_rows as u64,
                            requested: sink.st.candidates,
                        }));
                    }
                    sink.st.refined += 1;
                    let ok = if probe.probe_is_left {
                        sink.exact.holds(&probe.probe_geom, &other.geom)
                    } else {
                        sink.exact.holds(&other.geom, &probe.probe_geom)
                    };
                    if ok {
                        *sink.npairs += 1;
                        matched_bi = Some(bi);
                        break;
                    }
                }
                match matched_bi {
                    Some(bi) => probe.active_match = Some((bi, 0)),
                    None => return Ok(true),
                }
            }
        }
    }

    fn step_active_probe(
        sink: &mut ProbeSink<'_>,
        outer: &JoinSideData,
        ix: &OwnedIndexSide,
        outer_is_left: bool,
        active: &mut ActiveProbe,
        cap: usize,
    ) -> Result<bool> {
        let it = &outer.items[active.item_idx];
        let prep = sink
            .exact
            .prepares()
            .then(|| Prepared::new(it.geom.clone()));
        let mut outer_row = vec![Id::UNDEF; outer.table.width()];
        let mut inner_row = vec![Id::UNDEF; ix.cols.len()];
        let mut out_row = vec![Id::UNDEF; sink.vars_len];
        let mut ticks = 0usize;
        loop {
            sink.ctx.check()?;
            // 1. Drain any active matched inner hit across outer rows up to `cap`.
            if let Some((in_row, mut row_at)) = active.pending_match.take() {
                let orows = &outer.rows[active.item_idx];
                while row_at < orows.len() {
                    ticks += 1;
                    if ticks.is_multiple_of(CHECK_EVERY) {
                        sink.ctx.check()?;
                    }
                    if sink.pending_out.len() >= cap {
                        active.pending_match = Some((in_row, row_at));
                        return Ok(false);
                    }
                    let orow = orows[row_at];
                    row_at += 1;
                    for (col, dst) in outer_row.iter_mut().enumerate() {
                        *dst = outer.table.cols[col][orow as usize];
                    }
                    if outer_is_left {
                        sink.map.write_row(&outer_row, &in_row, &mut out_row);
                    } else {
                        sink.map.write_row(&in_row, &outer_row, &mut out_row);
                    }
                    let active_bytes = active
                        .retained_bytes()
                        .saturating_add(in_row.capacity() as u64 * size_of::<Id>() as u64);
                    sink.push_pending_row(out_row.clone(), active_bytes)?;
                }
                if sink.pending_out.len() >= cap {
                    return Ok(false);
                }
            }
            if active.hits.is_empty() {
                let mut buf = Vec::new();
                let non_iter = sink
                    .other_bytes
                    .saturating_add(sink.pending_out_bytes())
                    .saturating_add(active.hits.capacity() as u64 * size_of::<Hit>() as u64)
                    .saturating_add(lit_pass_bytes(&active.lit_pass));
                let charge = &mut *sink.charge;
                let ended =
                    active
                        .iter
                        .next_hits(sink.ctx, &mut buf, cap.clamp(64, 4096), &mut |b| {
                            charge.resize(non_iter.saturating_add(b))
                        })?;
                let iter_bytes = active.iter.retained_bytes();
                let buf_bytes = buf.capacity() as u64 * size_of::<Hit>() as u64;
                for h in buf {
                    if ix.subj.is_none_or(|s| s == h.s) {
                        if active.hits.len() == active.hits.capacity() {
                            let cur_h = active.hits.capacity() as u64 * size_of::<Hit>() as u64;
                            let next_h = next_cap(active.hits.len(), active.hits.capacity(), 1)
                                as u64
                                * size_of::<Hit>() as u64;
                            let projected = sink
                                .other_bytes
                                .saturating_add(sink.pending_out_bytes())
                                .saturating_add(iter_bytes)
                                .saturating_add(buf_bytes)
                                .saturating_add(cur_h)
                                .saturating_add(next_h)
                                .saturating_add(lit_pass_bytes(&active.lit_pass));
                            sink.charge.resize(projected)?;
                        }
                        active.hits.push_back(h);
                    }
                }
                if ended && active.hits.is_empty() {
                    sink.st.nodes += active.iter.st.nodes;
                    sink.st.rechecked += active.iter.st.rechecked;
                    sink.st.fallback |= active.iter.st.fallback;
                    return Ok(true);
                }
            }
            while sink.pending_out.len() < cap {
                let Some(h) = active.hits.pop_front() else {
                    break;
                };
                let ok = if let Some(&v) = active.lit_pass.get(&h.o) {
                    v
                } else {
                    sink.st.candidates += 1;
                    if sink.st.candidates > sink.ctx.max_rows as u64 {
                        return Err(Error::BudgetExceeded(Budget {
                            kind: BudgetKind::Rows,
                            limit: sink.ctx.max_rows as u64,
                            requested: sink.st.candidates,
                        }));
                    }
                    sink.st.refined += 1;
                    if sink.st.refined.is_multiple_of(CHECK_EVERY as u64) {
                        sink.ctx.check()?;
                    }
                    let add = hash_growth::<(Id, bool)>(
                        active.lit_pass.capacity(),
                        active.lit_pass.len(),
                    );
                    if add != 0 {
                        let projected = sink
                            .other_bytes
                            .saturating_add(sink.pending_out_bytes())
                            .saturating_add(active.retained_bytes())
                            .saturating_add(add);
                        sink.charge.resize(projected)?;
                    }
                    let pass = match noted(sink.ctx, h.entry.geom(&sink.ctx.snap)) {
                        Ok(geom) => match (&prep, outer_is_left) {
                            (Some(p), true) => sink.exact.holds_left(p, &geom),
                            (Some(p), false) => sink.exact.holds_right(&geom, p),
                            (None, true) => sink.exact.holds(&it.geom, &geom),
                            (None, false) => sink.exact.holds(&geom, &it.geom),
                        },
                        Err(_) => false,
                    };
                    active.lit_pass.insert(h.o, pass);
                    if add != 0 {
                        let settled = sink
                            .other_bytes
                            .saturating_add(sink.pending_out_bytes())
                            .saturating_add(active.retained_bytes());
                        sink.charge.resize(settled)?;
                    }
                    pass
                };
                if !ok {
                    continue;
                }
                *sink.npairs += 1;
                ix.hit_row(&h, &mut inner_row);
                let orows = &outer.rows[active.item_idx];
                for (row_at, &orow) in orows.iter().enumerate() {
                    ticks += 1;
                    if ticks.is_multiple_of(CHECK_EVERY) {
                        sink.ctx.check()?;
                    }
                    if sink.pending_out.len() >= cap {
                        active.pending_match = Some((inner_row.clone(), row_at));
                        return Ok(false);
                    }
                    for (col, dst) in outer_row.iter_mut().enumerate() {
                        *dst = outer.table.cols[col][orow as usize];
                    }
                    if outer_is_left {
                        sink.map.write_row(&outer_row, &inner_row, &mut out_row);
                    } else {
                        sink.map.write_row(&inner_row, &outer_row, &mut out_row);
                    }
                    let active_bytes = active.retained_bytes();
                    sink.push_pending_row(out_row.clone(), active_bytes)?;
                }
            }
            if sink.pending_out.len() >= cap {
                return Ok(false);
            }
        }
    }

    /// Fill `out` with up to `cap` rows, pulling child batches on demand through `pull_child`.
    /// Returns `true` when the join is exhausted.
    pub(crate) fn next(
        &mut self,
        ctx: &Ctx,
        out: &mut Table,
        cap: usize,
        pull_child: &mut dyn FnMut(usize) -> Result<Option<Table>>,
    ) -> Result<bool> {
        let cap = cap.max(1);
        let vars_len = self.vars.len();
        loop {
            ctx.check()?;
            self.flush_pending(ctx, out, cap)?;
            if out.len() >= cap {
                self.st.matched += out.len() as u64;
                self.update_charge()?;
                return Ok(false);
            }
            let pending_bytes = self.pending_bytes();
            match &mut self.mode {
                JoinMode::Done => {
                    self.st.matched += out.len() as u64;
                    self.update_charge()?;
                    return Ok(self.pending_out.is_empty());
                }
                JoinMode::IndexIndexProbe {
                    outer_ix,
                    inner_ix,
                    outer_is_left,
                    map,
                    outer_iter,
                    wave,
                    active,
                    next_item,
                } => {
                    if let (Some(w), Some(act)) = (wave.as_ref(), active.as_deref_mut()) {
                        let other_bytes = outer_iter
                            .as_deref()
                            .map_or(0, WindowIter::retained_bytes)
                            .saturating_add(w.retained_bytes());
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                            charge: &mut self.charge,
                            other_bytes,
                        };
                        let done = Self::step_active_probe(
                            &mut sink,
                            w,
                            inner_ix,
                            *outer_is_left,
                            act,
                            cap,
                        )?;
                        if done {
                            *active = None;
                        }
                        self.update_charge()?;
                        continue;
                    }
                    if let Some(w) = wave.as_ref() {
                        let mut opened = false;
                        let base_open = pending_bytes
                            .saturating_add(
                                outer_iter.as_deref().map_or(0, WindowIter::retained_bytes),
                            )
                            .saturating_add(w.retained_bytes());
                        while *next_item < w.items.len() {
                            let idx = *next_item;
                            *next_item += 1;
                            if !placed(&w.items[idx].bbox84) {
                                continue;
                            }
                            let ws = join_windows(&self.exact.test, w.items[idx].bbox84);
                            let charge = &mut self.charge;
                            let iter = WindowIter::open(
                                ctx,
                                vec![inner_ix.pred],
                                ws,
                                inner_ix.graph.clone(),
                                inner_ix.scan.dedup,
                                &mut |b| charge.resize(base_open.saturating_add(b)),
                            )?;
                            *active = Some(Box::new(ActiveProbe {
                                item_idx: idx,
                                iter,
                                hits: VecDeque::new(),
                                lit_pass: FxHashMap::default(),
                                pending_match: None,
                            }));
                            opened = true;
                            break;
                        }
                        if opened {
                            self.update_charge()?;
                            continue;
                        }
                        *wave = None;
                        *next_item = 0;
                    }
                    let Some(iter) = outer_iter.as_mut() else {
                        self.mode = JoinMode::Done;
                        continue;
                    };
                    let mut buf = Vec::new();
                    let charge = &mut self.charge;
                    let ended = iter.next_hits(ctx, &mut buf, 256, &mut |b| {
                        charge.resize(pending_bytes.saturating_add(b))
                    })?;
                    if ended {
                        self.st.nodes += iter.st.nodes;
                        self.st.rechecked += iter.st.rechecked;
                        self.st.fallback |= iter.st.fallback;
                        *outer_iter = None;
                    }
                    if !buf.is_empty() {
                        let outer_bytes = pending_bytes.saturating_add(
                            outer_iter.as_deref().map_or(0, WindowIter::retained_bytes),
                        );
                        let mut w = JoinSideData::new(outer_ix.vars.clone());
                        let charge = &mut self.charge;
                        for h in &buf {
                            if outer_ix.subj.is_none_or(|s| s == h.s) {
                                w.add_hit_charged(ctx, outer_ix, h, &mut |b| {
                                    charge.resize(outer_bytes.saturating_add(b))
                                })?;
                            }
                        }
                        if !w.items.is_empty() {
                            *wave = Some(w);
                            *next_item = 0;
                        }
                    }
                    self.update_charge()?;
                }
                JoinMode::IndexIndexBuild {
                    outer_ix,
                    inner,
                    outer_is_left,
                    map,
                    outer_iter,
                    outer_hits,
                    active_probe,
                } => {
                    if let Some(probe) = active_probe.as_deref_mut() {
                        let other_bytes = inner
                            .retained_bytes()
                            .saturating_add(
                                outer_iter.as_deref().map_or(0, WindowIter::retained_bytes),
                            )
                            .saturating_add(outer_hits.capacity() as u64 * size_of::<Hit>() as u64);
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                            charge: &mut self.charge,
                            other_bytes,
                        };
                        if Self::step_side_probe(&mut sink, inner, probe, cap)? {
                            *active_probe = None;
                        }
                        self.update_charge()?;
                        continue;
                    }
                    if outer_hits.is_empty() {
                        let Some(iter) = outer_iter.as_mut() else {
                            self.mode = JoinMode::Done;
                            continue;
                        };
                        let mut buf = Vec::new();
                        let base_outer = pending_bytes.saturating_add(inner.retained_bytes());
                        let charge = &mut self.charge;
                        let ended =
                            iter.next_hits(ctx, &mut buf, cap.clamp(64, 4096), &mut |b| {
                                charge.resize(base_outer.saturating_add(b))
                            })?;
                        if ended {
                            self.st.nodes += iter.st.nodes;
                            self.st.rechecked += iter.st.rechecked;
                            self.st.fallback |= iter.st.fallback;
                            *outer_iter = None;
                        }
                        if !buf.is_empty() {
                            let next_oh =
                                next_cap(outer_hits.len(), outer_hits.capacity(), buf.len()) as u64
                                    * size_of::<Hit>() as u64;
                            let iter_b =
                                outer_iter.as_deref().map_or(0, WindowIter::retained_bytes);
                            self.charge.resize(
                                base_outer.saturating_add(iter_b).saturating_add(next_oh),
                            )?;
                            outer_hits.extend(buf);
                        }
                        if outer_hits.is_empty() && outer_iter.is_none() {
                            self.mode = JoinMode::Done;
                            continue;
                        }
                    }
                    while self.pending_out.len() < cap && active_probe.is_none() {
                        let Some(h) = outer_hits.pop_front() else {
                            break;
                        };
                        if outer_ix.subj.is_some_and(|s| s != h.s) {
                            continue;
                        }
                        let Ok(g) = noted(ctx, h.entry.geom(&ctx.snap)) else {
                            continue;
                        };
                        let Some(b) = item_box(&g) else {
                            continue;
                        };
                        let mut outer_row = vec![Id::UNDEF; outer_ix.cols.len()];
                        outer_ix.hit_row(&h, &mut outer_row);
                        *active_probe = Some(Box::new(SideProbeState::open(
                            inner,
                            &self.exact.test,
                            g,
                            b,
                            outer_row,
                            *outer_is_left,
                        )));
                    }
                    self.update_charge()?;
                }
                JoinMode::PlanIndexProbe {
                    child_idx,
                    geom_var,
                    plan_is_left,
                    ix,
                    map,
                    batch,
                    active,
                    next_item,
                    child_done,
                } => {
                    if let (Some(b), Some(act)) = (batch.as_ref(), active.as_deref_mut()) {
                        let other_bytes = b.retained_bytes();
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                            charge: &mut self.charge,
                            other_bytes,
                        };
                        let done =
                            Self::step_active_probe(&mut sink, b, ix, *plan_is_left, act, cap)?;
                        if done {
                            *active = None;
                        }
                        self.update_charge()?;
                        continue;
                    }
                    if let Some(b) = batch.as_ref() {
                        let mut opened = false;
                        let base_open = pending_bytes.saturating_add(b.retained_bytes());
                        while *next_item < b.items.len() {
                            let idx = *next_item;
                            *next_item += 1;
                            if !placed(&b.items[idx].bbox84) {
                                continue;
                            }
                            let ws = join_windows(&self.exact.test, b.items[idx].bbox84);
                            let charge = &mut self.charge;
                            let iter = WindowIter::open(
                                ctx,
                                vec![ix.pred],
                                ws,
                                ix.graph.clone(),
                                ix.scan.dedup,
                                &mut |bytes| charge.resize(base_open.saturating_add(bytes)),
                            )?;
                            *active = Some(Box::new(ActiveProbe {
                                item_idx: idx,
                                iter,
                                hits: VecDeque::new(),
                                lit_pass: FxHashMap::default(),
                                pending_match: None,
                            }));
                            opened = true;
                            break;
                        }
                        if opened {
                            self.update_charge()?;
                            continue;
                        }
                        *batch = None;
                        *next_item = 0;
                    }
                    if *child_done {
                        self.mode = JoinMode::Done;
                        continue;
                    }
                    match pull_child(*child_idx)? {
                        Some(t) => {
                            let mut side = JoinSideData::new(t.vars.clone());
                            let charge = &mut self.charge;
                            side.add_table_batch_charged(ctx, &t, *geom_var, &mut |b| {
                                charge.resize(pending_bytes.saturating_add(b))
                            })?;
                            if !side.items.is_empty() {
                                *batch = Some(side);
                                *next_item = 0;
                            }
                        }
                        None => {
                            *child_done = true;
                            self.mode = JoinMode::Done;
                        }
                    }
                    self.update_charge()?;
                }
                JoinMode::PlanIndexBuild {
                    child_idx,
                    geom_var,
                    plan_is_left,
                    inner,
                    map,
                    batch,
                    active_probe,
                    child_done,
                } => {
                    if let Some(probe) = active_probe.as_deref_mut() {
                        let other_bytes = inner.retained_bytes().saturating_add(
                            batch.as_ref().map_or(0, |(t, _)| table_capacity_bytes(t)),
                        );
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                            charge: &mut self.charge,
                            other_bytes,
                        };
                        if Self::step_side_probe(&mut sink, inner, probe, cap)? {
                            *active_probe = None;
                        }
                        self.update_charge()?;
                        continue;
                    }
                    if batch.is_none() {
                        if *child_done {
                            self.mode = JoinMode::Done;
                            continue;
                        }
                        match pull_child(*child_idx)? {
                            Some(t) => *batch = Some((t, 0)),
                            None => {
                                *child_done = true;
                                self.mode = JoinMode::Done;
                                continue;
                            }
                        }
                    }
                    let (t, at) = batch.as_mut().expect("active batch");
                    let Some(gcol) = t.col_of(*geom_var) else {
                        *batch = None;
                        continue;
                    };
                    while *at < t.len() && self.pending_out.len() < cap && active_probe.is_none() {
                        let i = *at;
                        *at += 1;
                        let id = t.cols[gcol][i];
                        if id.is_undef() {
                            continue;
                        }
                        let Ok(g) = super::memo::by_id(ctx, id, None) else {
                            continue;
                        };
                        let Some(b) = item_box(&g) else {
                            continue;
                        };
                        let mut probe_row = vec![Id::UNDEF; t.width()];
                        for (c, dst) in probe_row.iter_mut().enumerate() {
                            *dst = t.cols[c][i];
                        }
                        *active_probe = Some(Box::new(SideProbeState::open(
                            inner,
                            &self.exact.test,
                            g,
                            b,
                            probe_row,
                            *plan_is_left,
                        )));
                    }
                    if *at >= t.len() && active_probe.is_none() {
                        *batch = None;
                    }
                    self.update_charge()?;
                }
                JoinMode::PlanPlan {
                    probe_child,
                    probe_geom_var,
                    build_is_left,
                    build,
                    map,
                    batch,
                    active_probe,
                    probe_done,
                } => {
                    if build.items.is_empty() {
                        // Drain/skip handled cleanly: no build geometries means no matches.
                        self.mode = JoinMode::Done;
                        continue;
                    }
                    if let Some(probe) = active_probe.as_deref_mut() {
                        let other_bytes = build.retained_bytes().saturating_add(
                            batch.as_ref().map_or(0, |(t, _)| table_capacity_bytes(t)),
                        );
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                            charge: &mut self.charge,
                            other_bytes,
                        };
                        if Self::step_side_probe(&mut sink, build, probe, cap)? {
                            *active_probe = None;
                        }
                        self.update_charge()?;
                        continue;
                    }
                    if batch.is_none() {
                        if *probe_done {
                            self.mode = JoinMode::Done;
                            continue;
                        }
                        match pull_child(*probe_child)? {
                            Some(t) => *batch = Some((t, 0)),
                            None => {
                                *probe_done = true;
                                self.mode = JoinMode::Done;
                                continue;
                            }
                        }
                    }
                    let (t, at) = batch.as_mut().expect("active probe batch");
                    let Some(gcol) = t.col_of(*probe_geom_var) else {
                        *batch = None;
                        continue;
                    };
                    while *at < t.len() && self.pending_out.len() < cap && active_probe.is_none() {
                        let i = *at;
                        *at += 1;
                        let id = t.cols[gcol][i];
                        if id.is_undef() {
                            continue;
                        }
                        let Ok(g) = super::memo::by_id(ctx, id, None) else {
                            continue;
                        };
                        let Some(b) = item_box(&g) else {
                            continue;
                        };
                        let mut probe_row = vec![Id::UNDEF; t.width()];
                        for (c, dst) in probe_row.iter_mut().enumerate() {
                            *dst = t.cols[c][i];
                        }
                        *active_probe = Some(Box::new(SideProbeState::open(
                            build,
                            &self.exact.test,
                            g,
                            b,
                            probe_row,
                            !*build_is_left,
                        )));
                    }
                    if *at >= t.len() && active_probe.is_none() {
                        *batch = None;
                    }
                    self.update_charge()?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};
    use std::sync::atomic::Ordering;

    #[test]
    fn r2_reverse_links_poll_inside_a_resumed_prefix() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;

        let store = Store::in_memory(StoreOptions::default());
        let data = (0..4200)
            .map(|i| {
                format!(
                    "<urn:f:{i}> <{}> <urn:geometry> .\n",
                    vocab::HAS_DEFAULT_GEOMETRY
                )
            })
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let ctx = Ctx::new(store.snapshot());
        let link = ctx.snap.lookup_iri(vocab::HAS_DEFAULT_GEOMETRY).unwrap();
        let geom = ctx.snap.lookup_iri("urn:geometry").unwrap();
        let mut scan = PrefixScan::new(Perm::Pos, &[link.0, geom.0]);
        let mut raw = Vec::new();
        assert!(
            !scan
                .next_chunk(&ctx, &mut raw, 64, &mut |_| Ok(()))
                .unwrap()
        );
        assert_eq!(raw.len(), 64);
        ctx.cancel.store(true, Ordering::Relaxed);
        let result = scan.next_chunk(&ctx, &mut raw, 4096, &mut |_| Ok(()));
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(raw.len(), 4095);
    }

    #[test]
    fn r2_join_probe_resumes_one_row_and_polls_unplaced_rejections() {
        let geoms = |crs| {
            Arc::new(Geom::from_geometry(
                super::super::crs::CrsRef::Unknown(Arc::from(crs)),
                georust::Geometry::Point(georust::Point::new(0.0, 0.0)),
            ))
        };
        let mut side = JoinSideData::new(vec![0]);
        side.items.push(JoinItem {
            row: 0,
            id: Id(1),
            bbox84: [f64::NAN; 4],
            geom: geoms("urn:build-frame"),
        });
        let count = 4096;
        side.table.cols[0] = vec![Id(1); count];
        side.table.len = count;
        side.rows.push((0..count as u32).collect());
        side.unplaced_idx = vec![0; 2 * CHECK_EVERY];
        let side_bytes = side.retained_bytes();
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = side_bytes + 1024;
        let ctx = Arc::new(ctx);
        let mut charge = OwnedCharge::new(&ctx, side_bytes).unwrap();
        let relation = JoinTest::Relation(Relation::SfIntersects);
        let exact = ExactTest::new(&ctx, &relation);
        let mut sink = ProbeSink {
            ctx: &ctx,
            exact: &exact,
            st: &mut SearchStats::default(),
            npairs: &mut 0,
            map: &OutputMap::new(&[0], &[], &[0]),
            vars_len: 1,
            pending_out: &mut VecDeque::new(),
            charge: &mut charge,
            other_bytes: side_bytes,
        };
        let mut probe = SideProbeState::open(
            &side,
            &relation,
            geoms("urn:probe-frame"),
            [f64::NAN; 4],
            Vec::new(),
            true,
        );
        probe.active_match = Some((0, 0));
        assert!(!SpatialJoinEngine::step_side_probe(&mut sink, &side, &mut probe, 1).unwrap());
        assert_eq!(sink.pending_out.pop_front(), Some(vec![Id(1)]));
        assert_eq!(probe.active_match, Some((0, 1)));

        probe.active_match = None;
        ctx.cancel.store(true, Ordering::Relaxed);
        let res = SpatialJoinEngine::step_side_probe(&mut sink, &side, &mut probe, 1);
        assert!(matches!(res, Err(Error::Cancelled)));
        assert_eq!(probe.unplaced_at, CHECK_EVERY - 1);
        assert!(sink.pending_out.is_empty());
    }

    #[test]
    fn r2_pair_emitter_polls_when_every_graph_is_rejected() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Arc::new(Ctx::new(store.snapshot()));
        let spec = SpatialRelateSpec {
            rel: Relation::SfIntersects,
            subject: RelateEnd::Var(0),
            object: RelateEnd::Var(1),
            property: None,
            graph: GraphFilter::Named,
            graph_var: Some(2),
            dedup: false,
            key: 0,
        };
        let mut reader = SpatialRelateReader::open(&ctx, &spec, &[0, 1, 2]).unwrap();
        let (a, b) = (Id(1), Id(2));
        let by_lit = FxHashMap::from_iter([
            (
                a,
                vec![Ser {
                    s: a,
                    o: a,
                    g: Id(3),
                }],
            ),
            (
                b,
                vec![
                    Ser {
                        s: b,
                        o: b,
                        g: Id(4)
                    };
                    2 * CHECK_EVERY
                ],
            ),
        ]);
        let mut pe = PairEmit {
            a_lit: a,
            b_lit: b,
            ai: 0,
            bi: 0,
            oai: 0,
            obi: 0,
            oa_len: 0,
            ob_len: 0,
            matched_counted: false,
        };
        ctx.cancel.store(true, Ordering::Relaxed);
        let res = reader.step_pair_emit(&ctx, &by_lit, &mut pe, 1, 0);
        assert!(matches!(res, Err(Error::Cancelled)));
        assert_eq!(pe.bi, CHECK_EVERY - 1);
        assert!(reader.pending_out.is_empty());
    }

    #[test]
    fn r3_one_const_lit_pass_uses_physical_table_growth_after_geom_lookup() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;

        let store = Store::in_memory(StoreOptions::default());
        // L-shaped query polygon whose bbox [0, 0, 10, 10] contains every candidate point,
        // while every stored point at (5.xxxx, 5) is disjoint from the polygon and emits no rows.
        let mut data = String::from("@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n");
        for i in 0..4000 {
            data.push_str(&format!(
                "<urn:p:{i}> geo:asWKT \"POINT(5.{i:04} 5)\"^^geo:wktLiteral .\n"
            ));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let poly_term: oxrdf::Term = oxrdf::Literal::new_typed_literal(
            "POLYGON((0 0, 10 0, 10 1, 1 1, 1 10, 0 10, 0 0))",
            oxrdf::NamedNode::new_unchecked(vocab::WKT_LITERAL),
        )
        .into();
        let make_spec = |q_lit: Id, q_geom: GeomRef| SpatialRelateSpec {
            rel: Relation::SfIntersects,
            subject: RelateEnd::Geometry(q_lit, q_geom),
            object: RelateEnd::Var(0),
            property: None,
            graph: GraphFilter::Default,
            graph_var: None,
            dedup: false,
            key: 0,
        };

        // Part 1: simultaneous `self.geoms` + `lit_pass` growth at capacity 7 -> 14.
        // Pre-warm `ctx.geo` so the step exercises only `self.geoms` and `lit_pass` growth.
        let mut ctx = Ctx::new(snap.clone());
        let q_lit = ctx.intern_term(&poly_term);
        let mut reader_geoms = FxHashMap::default();
        let mut lit_pass = FxHashMap::default();
        let point_lits: Vec<Id> = (0..8)
            .map(|i| {
                let lit = oxrdf::Literal::new_typed_literal(
                    format!("POINT(5.{i:04} 5)"),
                    oxrdf::NamedNode::new_unchecked(vocab::WKT_LITERAL),
                );
                snap.lookup_term(&lit.into()).unwrap()
            })
            .collect();
        let q_geom = {
            let v = ctx.value(q_lit).unwrap();
            ctx.geo
                .get_or_parse_charged(&ctx, MemoKey::Id(q_lit), || {
                    parse_value(&v, max_vertices(&ctx))
                })
                .unwrap()
        };
        let spec = make_spec(q_lit, q_geom.clone());
        let extra_lit = point_lits[7];
        {
            let v = ctx.value(extra_lit).unwrap();
            let _ = ctx
                .geo
                .get_or_parse_charged(&ctx, MemoKey::Id(extra_lit), || {
                    parse_value(&v, max_vertices(&ctx))
                })
                .unwrap();
        }
        let warm_geo_bytes = ctx.live_bytes();
        for &id in &point_lits[..7] {
            reader_geoms.insert(id, None);
            lit_pass.insert(id, false);
        }
        assert_eq!(reader_geoms.capacity(), 7);
        assert_eq!(lit_pass.capacity(), 7);
        let pending_sers = VecDeque::from([Ser {
            s: Id(1),
            o: extra_lit,
            g: Id::DEFAULT_GRAPH,
        }]);
        let old_geoms_bytes = table_bytes(7, size_of::<(Id, Option<GeomRef>)>()) as u64;
        let new_geoms_bytes = table_bytes(14, size_of::<(Id, Option<GeomRef>)>()) as u64;
        let old_lit_pass_bytes = table_bytes(7, size_of::<(Id, bool)>()) as u64;
        let new_lit_pass_bytes = table_bytes(14, size_of::<(Id, bool)>()) as u64;
        assert_eq!(
            table_bytes(7168, size_of::<(Id, bool)>()),
            139_280,
            "physical table sizing for FxHashMap<Id, bool> at usable capacity 7,168"
        );
        let lits = vec![ConstLit {
            lit: q_lit,
            g: None,
        }];
        let fixed_stage = const_lit_bytes(&lits)
            + pending_sers.capacity() as u64 * size_of::<Ser>() as u64
            + old_lit_pass_bytes;
        let geoms_peak = warm_geo_bytes + old_geoms_bytes + fixed_stage + new_geoms_bytes;
        let lit_pass_peak = warm_geo_bytes + new_geoms_bytes + fixed_stage + new_lit_pass_bytes;
        assert!(lit_pass_peak > geoms_peak);
        ctx.mem_limit = lit_pass_peak - 1;
        let ctx = Arc::new(ctx);
        let mut reader = SpatialRelateReader::open(&ctx, &spec, &[0]).unwrap();
        reader.geoms = reader_geoms;
        reader.stage = RelateStage::OneConst {
            cid: q_lit,
            subject_const: true,
            lits,
            q_at: 1,
            qg: Some(q_geom.clone()),
            cand: OneConstCand::Done,
            pending_sers,
            active_ser: None,
            lit_pass,
        };
        reader.update_charge().unwrap();
        let err = reader.step_one_const(&ctx, 64).unwrap_err();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.requested == lit_pass_peak
        ));
        // `self.geoms` grew, while `lit_pass` refused BEFORE growing past capacity 7.
        assert_eq!(reader.geoms.capacity(), 14);
        let RelateStage::OneConst { lit_pass, .. } = &reader.stage else {
            panic!("expected OneConst stage");
        };
        assert_eq!(lit_pass.capacity(), 7);
        drop(reader);
        assert_eq!(ctx.live_bytes(), warm_geo_bytes);

        // Part 2: full one-constant scan over 4,000 distinct non-matching literals refuses
        // before `lit_pass` grows from 3,584 to 7,168 under a budget below `new_geoms + old_lit_pass + new_lit_pass`.
        let full_ctx_unbounded = Arc::new(Ctx::new(snap.clone()));
        let full_spec_unbounded =
            make_spec(full_ctx_unbounded.intern_term(&poly_term), q_geom.clone());
        let mut out = Table::new(vec![0]);
        let mut probe_reader =
            SpatialRelateReader::open(&full_ctx_unbounded, &full_spec_unbounded, &[0]).unwrap();
        assert!(
            probe_reader
                .next(&full_ctx_unbounded, &mut out, 64)
                .unwrap()
        );
        assert!(out.is_empty());
        let held = probe_reader.charge.bytes();
        let before_drop = full_ctx_unbounded.live_bytes();
        drop(probe_reader);
        assert_eq!(full_ctx_unbounded.live_bytes(), before_drop - held);

        // At 4,000 distinct literals, both `(Id, Option<GeomRef>)` and `(Id, bool)` are 16 bytes,
        // so `new_geoms (139,280) + old_lit_pass (69,648) + new_lit_pass (139,280) = 348,208` bytes
        // even before cache reservations; a 340 KiB cap refuses before `lit_pass` can grow to 7,168.
        let mut full_ctx = Ctx::new(snap.clone());
        full_ctx.mem_limit = 340 * 1024;
        let full_ctx = Arc::new(full_ctx);
        let full_spec = make_spec(full_ctx.intern_term(&poly_term), q_geom);
        let mut tight_reader = SpatialRelateReader::open(&full_ctx, &full_spec, &[0]).unwrap();
        let mut out = Table::new(vec![0]);
        let err = tight_reader.next(&full_ctx, &mut out, 64).unwrap_err();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory
        ));
        if let RelateStage::OneConst { lit_pass, .. } = &tight_reader.stage {
            assert!(
                lit_pass.capacity() < 7168,
                "lit_pass grew to {} past tight budget",
                lit_pass.capacity()
            );
        }
        let held = tight_reader.charge.bytes();
        let before_drop = full_ctx.live_bytes();
        drop(tight_reader);
        assert_eq!(full_ctx.live_bytes(), before_drop - held);
    }

    #[test]
    fn r4_one_const_fallback_scan_admits_key_chunk_before_push() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;

        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n");
        for i in 0..4096 {
            data.push_str(&format!(
                "<urn:p:{i}> geo:asWKT \"POINT(5.{i:04} 5)\"^^geo:wktLiteral .\n"
            ));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let pred = snap.lookup_iri(vocab::AS_WKT).unwrap();

        // Direct `PrefixScan::next_chunk` check: refusing the initial 0 -> 4 allocation leaves `raw` empty.
        let direct_ctx = Ctx::new(snap.clone());
        let mut scan = PrefixScan::new(Perm::Pso, &[pred.0]);
        let mut raw = Vec::new();
        let err = scan
            .next_chunk(&direct_ctx, &mut raw, 4096, &mut |bytes| {
                assert_eq!(bytes, 4 * size_of::<[u64; 4]>() as u64);
                Err(Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    limit: bytes - 1,
                    requested: bytes,
                }))
            })
            .unwrap_err();
        assert!(matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory));
        assert_eq!(raw.capacity(), 0);
        assert!(raw.is_empty());

        // Fallback `OneConstCand::Scan` with a 4,096-row pull (128 KiB key chunk + doubling overlap)
        // refuses inside `PrefixScan::next_chunk` before reaching any candidate geometry lookup.
        let poly_term: oxrdf::Term = oxrdf::Literal::new_typed_literal(
            "POLYGON((0 0, 10 0, 10 1, 1 1, 1 10, 0 10, 0 0))",
            oxrdf::NamedNode::new_unchecked(vocab::WKT_LITERAL),
        )
        .into();
        let mut ctx = Ctx::new(snap);
        let q_lit = ctx.intern_term(&poly_term);
        let q_geom = {
            let v = ctx.value(q_lit).unwrap();
            ctx.geo
                .get_or_parse_charged(&ctx, MemoKey::Id(q_lit), || {
                    parse_value(&v, max_vertices(&ctx))
                })
                .unwrap()
        };
        let warm_bytes = ctx.live_bytes();
        // Fits setup and a 1,024-key chunk (32 KiB) but cannot fit the 4,096-key chunk (128 KiB + 64 KiB overlap).
        ctx.mem_limit = warm_bytes + 64 * 1024;
        let ctx = Arc::new(ctx);
        let spec = SpatialRelateSpec {
            rel: Relation::SfIntersects,
            subject: RelateEnd::Geometry(q_lit, q_geom),
            object: RelateEnd::Var(0),
            property: None,
            graph: GraphFilter::Default,
            graph_var: None,
            dedup: false,
            key: 0,
        };
        let mut reader = SpatialRelateReader::open(&ctx, &spec, &[0]).unwrap();
        let mut out = Table::new(vec![0]);
        let err = reader.next(&ctx, &mut out, 4096).unwrap_err();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory
        ));
        assert_eq!(
            reader.st.candidates, 0,
            "must refuse inside PrefixScan::next_chunk before consuming the key chunk"
        );
        let held = reader.charge.bytes();
        let before_drop = ctx.live_bytes();
        drop(reader);
        assert_eq!(ctx.live_bytes(), before_drop - held);
    }
}
