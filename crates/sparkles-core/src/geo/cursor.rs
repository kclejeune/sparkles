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
use std::collections::VecDeque;
use std::sync::Arc;

const CHECK_EVERY: usize = 256;
const CHECK_CANDIDATES: u64 = 4096;
const PREPARE_OVER: usize = 8;
const WORLD: [f64; 4] = [-180.0, -90.0, 180.0, 90.0];
const JOIN_WORLD: [f64; 4] = [-1e30, -1e30, 1e30, 1e30];

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
            st.candidates = st.candidates.max(iter.st.candidates);
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
        let resize_charge = |iter: &Option<WindowIter>,
                             pending: &VecDeque<Hit>,
                             seen_lit: &FxHashMap<Id, bool>,
                             charge: &mut OwnedCharge|
         -> Result<()> {
            let iter_bytes = iter.as_ref().map_or(0, WindowIter::retained_bytes);
            let pending_bytes = pending.capacity() as u64 * size_of::<Hit>() as u64;
            let seen_bytes = seen_lit.capacity() as u64 * (size_of::<Id>() + 2) as u64;
            charge.resize(
                iter_bytes
                    .saturating_add(pending_bytes)
                    .saturating_add(seen_bytes),
            )
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
                    self.st.candidates = self.st.candidates.max(iter.st.candidates);
                    self.st.nodes = iter.st.nodes;
                    self.st.rechecked = iter.st.rechecked;
                    self.st.fallback = iter.st.fallback;
                    self.iter = None;
                }
                self.pending.extend(buf);
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
    seen_lit: FxHashMap<Id, bool>,
    seen_geom: FxHashSet<(Id, Id)>,
    seen_feat: FxHashSet<(Id, Id)>,
    st: SearchStats,
    charge: OwnedCharge,
}

impl SpatialPfReader {
    pub(crate) fn open(ctx: &Arc<Ctx>, spec: &SpatialPfSpec, vars: &[VarId]) -> Result<Self> {
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
            for l in &links {
                scan_keys(ctx, Perm::Spo, &[f.0, l.0], |k| {
                    let (geom, g1) = (k[2], k[3]);
                    if !spec.graph.accepts(g1) {
                        return Ok(());
                    }
                    for p in &preds {
                        scan_keys(ctx, Perm::Spo, &[geom, p.0], |k2| {
                            let g = k2[3];
                            if !spec.graph.accepts(g) || !scope.contains(ctx, g) {
                                return Ok(());
                            }
                            if spec.graph_var.is_some() && g1 != g {
                                return Ok(());
                            }
                            let o = Id(k2[2]);
                            st.candidates += 1;
                            let ok = *lit_memo.entry(o).or_insert_with(|| {
                                st.refined += 1;
                                let Some(v) = ctx.value(o) else {
                                    return false;
                                };
                                let max = max_vertices(ctx);
                                let Some(parsed) =
                                    ctx.geo.get_or_parse_charged(ctx, MemoKey::Id(o), || {
                                        parse_value(&v, max)
                                    })
                                else {
                                    return false;
                                };
                                note_crs(ctx, &parsed);
                                tester.check(&parsed).is_some()
                            });
                            if ok {
                                st.matched += 1;
                                let g_key = if spec.graph_var.is_some() {
                                    Id(g)
                                } else {
                                    Id::UNDEF
                                };
                                if seen_feat.insert((*f, g_key)) {
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
                    let (lat_o, g) = (k[2], k[3]);
                    if !spec.graph.accepts(g) || !scope.contains(ctx, g) {
                        return Ok(());
                    }
                    let Some(y) = super::wgs84::number(&ctx.snap, Id(lat_o)) else {
                        return Ok(());
                    };
                    for long_o in super::wgs84::objects(&ctx.snap, f.0, long_p.0, g)? {
                        let e = super::wgs84::number(&ctx.snap, Id(long_o))
                            .and_then(|x| super::wgs84::point(y, x));
                        if let Some(e) = e {
                            st.candidates += 1;
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
                                if seen_feat.insert((*f, g_key)) {
                                    pending_feats.push_back((*f, g_key));
                                }
                            }
                        }
                    }
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
            st.candidates = st.candidates.max(iter.st.candidates);
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
        let resize_charge = |iter: &Option<WindowIter>,
                             pending_hits: &VecDeque<Hit>,
                             pending_feats: &VecDeque<(Id, Id)>,
                             seen_lit: &FxHashMap<Id, bool>,
                             seen_geom: &FxHashSet<(Id, Id)>,
                             seen_feat: &FxHashSet<(Id, Id)>,
                             charge: &mut OwnedCharge|
         -> Result<()> {
            let iter_bytes = iter.as_ref().map_or(0, WindowIter::retained_bytes);
            let hits = pending_hits.capacity() as u64 * size_of::<Hit>() as u64;
            let feats = pending_feats.capacity() as u64 * size_of::<(Id, Id)>() as u64;
            let lits = seen_lit.capacity() as u64 * (size_of::<Id>() + 2) as u64;
            let geoms = seen_geom.capacity() as u64 * (size_of::<(Id, Id)>() + 1) as u64;
            let seen = seen_feat.capacity() as u64 * (size_of::<(Id, Id)>() + 1) as u64;
            charge.resize(
                iter_bytes
                    .saturating_add(hits)
                    .saturating_add(feats)
                    .saturating_add(lits)
                    .saturating_add(geoms)
                    .saturating_add(seen),
            )
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
                let base_bytes = self.seen_lit.capacity() as u64 * (size_of::<Id>() + 2) as u64
                    + self.seen_geom.capacity() as u64 * (size_of::<(Id, Id)>() + 1) as u64
                    + self.seen_feat.capacity() as u64 * (size_of::<(Id, Id)>() + 1) as u64;
                let charge = &mut self.charge;
                let ended = iter.next_hits(ctx, &mut buf, chunk_want, &mut |b| {
                    charge.resize(b.saturating_add(base_bytes))
                })?;
                if ended {
                    self.st.candidates = self.st.candidates.max(iter.st.candidates);
                    self.st.nodes = iter.st.nodes;
                    self.st.rechecked = iter.st.rechecked;
                    self.st.fallback = iter.st.fallback;
                    self.iter = None;
                }
                self.pending_hits.extend(buf);
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
            while out.len() < cap {
                let Some(h) = self.pending_hits.pop_front() else {
                    break;
                };
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
                    if self.seen_feat.insert((h.s, g_key)) {
                        emit_row(h.s, g_key, out, &mut row);
                    }
                    continue;
                }
                if !self.seen_geom.insert((h.s, h.g)) {
                    continue;
                }
                for &l in &self.links {
                    scan_keys(ctx, Perm::Pos, &[l.0, h.s.0], |k| {
                        let (f, g1) = (Id(k[2]), k[3]);
                        let same = match self.spec.graph_var {
                            Some(_) => g1 == h.g.0,
                            None => self.spec.graph.accepts(g1),
                        };
                        if !same || matches!(self.spec.subject, PathEnd::Const(c) if c != f) {
                            return Ok(());
                        }
                        if same_fg && f != g_key {
                            return Ok(());
                        }
                        if self.seen_feat.insert((f, g_key)) {
                            if out.len() < cap {
                                emit_row(f, g_key, out, &mut row);
                            } else {
                                self.pending_feats.push_back((f, g_key));
                            }
                        }
                        Ok(())
                    })?;
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

    fn next_chunk(&mut self, ctx: &Ctx, out: &mut Vec<[u64; 4]>, limit: usize) -> Result<bool> {
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
                last = Some(k);
                scanned += 1;
                if scanned.is_multiple_of(4096) {
                    ctx.check()?;
                }
                out.push(k);
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

enum RelateStage {
    Asserted {
        scan: PrefixScan,
        o_filter: Option<Id>,
    },
    EnterDerived,
    OneConst {
        cid: Id,
        subject_const: bool,
        qs: Vec<Id>,
        by_lit: FxHashMap<Id, Vec<Option<Id>>>,
        q_at: usize,
        qg: Option<GeomRef>,
        cand: OneConstCand,
        pending_sers: VecDeque<Ser>,
        lit_pass: FxHashMap<Id, bool>,
    },
    BothVar {
        items: Vec<RelateItem>,
        by_lit: FxHashMap<Id, Vec<Ser>>,
        boxed_idx: Vec<usize>,
        unboxed_idx: Vec<usize>,
        tree: Option<PackedTree>,
        phase: BothVarPhase,
    },
    Done,
}

enum BothVarPhase {
    Boxed { outer: usize },
    Unboxed { idx: usize },
    Disjoint { i: usize, j: usize },
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

    fn retained_bytes(&self) -> u64 {
        let mut bytes = (self.emitted.capacity() as u64 * (size_of::<(Id, Id, Id)>() + 1) as u64)
            .saturating_add(self.pending_out.capacity() as u64 * size_of::<(Id, Id, Id)>() as u64)
            .saturating_add(
                self.geoms.capacity() as u64
                    * (size_of::<Id>() + size_of::<Option<GeomRef>>() + 1) as u64,
            )
            .saturating_add(
                self.features.capacity() as u64
                    * (size_of::<(Id, Id)>() + size_of::<Vec<Id>>() + 1) as u64,
            );
        match &self.stage {
            RelateStage::OneConst {
                cand,
                pending_sers,
                lit_pass,
                ..
            } => {
                if let OneConstCand::Indexed(it) = cand {
                    bytes = bytes.saturating_add(it.retained_bytes());
                }
                bytes = bytes
                    .saturating_add(pending_sers.capacity() as u64 * size_of::<Ser>() as u64)
                    .saturating_add(lit_pass.capacity() as u64 * (size_of::<Id>() + 2) as u64);
            }
            RelateStage::BothVar {
                items,
                by_lit,
                boxed_idx,
                unboxed_idx,
                tree,
                ..
            } => {
                bytes = bytes
                    .saturating_add(items.capacity() as u64 * size_of::<RelateItem>() as u64)
                    .saturating_add(
                        by_lit.capacity() as u64
                            * (size_of::<Id>() + size_of::<Vec<Ser>>() + 1) as u64,
                    )
                    .saturating_add(
                        (boxed_idx.capacity() + unboxed_idx.capacity()) as u64
                            * size_of::<usize>() as u64,
                    )
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

    fn geom(&mut self, ctx: &Ctx, lit: Id) -> Option<GeomRef> {
        self.geoms
            .entry(lit)
            .or_insert_with(|| {
                let v = ctx.value(lit)?;
                let max = max_vertices(ctx);
                let g = ctx
                    .geo
                    .get_or_parse_charged(ctx, MemoKey::Id(lit), || parse_value(&v, max))?;
                note_crs(ctx, &g);
                Some(g)
            })
            .clone()
    }

    fn sers_of(&mut self, ctx: &Ctx, s: Id) -> Result<Vec<Ser>> {
        let mut out = Vec::new();
        for p in self.preds.clone() {
            let mut keys = Vec::new();
            scan_keys(ctx, Perm::Spo, &[s.0, p.0], |k| {
                keys.push(k);
                Ok(())
            })?;
            for k in keys {
                if self.ser_graph(ctx, k[3]) {
                    out.push(Ser {
                        s,
                        o: Id(k[2]),
                        g: Id(k[3]),
                    });
                }
            }
        }
        Ok(out)
    }

    fn const_lits(&mut self, ctx: &Ctx, e: &RelateEnd) -> Result<Vec<ConstLit>> {
        let id = match e {
            RelateEnd::Geometry(id, g) => {
                self.geoms.insert(*id, Some(g.clone()));
                return Ok(vec![ConstLit { lit: *id, g: None }]);
            }
            RelateEnd::Node(id) => *id,
            RelateEnd::Var(_) | RelateEnd::Absent => return Ok(Vec::new()),
        };
        let mut out: Vec<ConstLit> = self
            .sers_of(ctx, id)?
            .into_iter()
            .map(|r| ConstLit {
                lit: r.o,
                g: Some(r.g),
            })
            .collect();
        if let Some(link) = self.link {
            let mut geoms = Vec::new();
            scan_keys(ctx, Perm::Spo, &[id.0, link.0], |k| {
                if self.spec.graph.accepts(k[3]) {
                    geoms.push((Id(k[2]), k[3]));
                }
                Ok(())
            })?;
            for (geom, g1) in geoms {
                for r in self.sers_of(ctx, geom)? {
                    if self.spec.graph_var.is_none() || r.g.0 == g1 {
                        out.push(ConstLit {
                            lit: r.o,
                            g: Some(r.g),
                        });
                    }
                }
            }
        }
        out.sort_unstable_by_key(|c| (c.lit, c.g));
        out.dedup_by_key(|c| (c.lit, c.g));
        Ok(out)
    }

    fn features_of(&mut self, ctx: &Ctx, s: Id, g: Id) -> Result<Vec<Id>> {
        if let Some(f) = self.features.get(&(s, g)) {
            return Ok(f.clone());
        }
        let mut out = Vec::new();
        if let Some(link) = self.link {
            let same = self.spec.graph_var.is_some();
            let graph = &self.spec.graph;
            scan_keys(ctx, Perm::Pos, &[link.0, s.0], |k| {
                let g1 = k[3];
                if (same && g1 == g.0) || (!same && graph.accepts(g1)) {
                    out.push(Id(k[2]));
                }
                Ok(())
            })?;
            out.sort_unstable();
            out.dedup();
        }
        self.features.insert((s, g), out.clone());
        Ok(out)
    }

    fn objects(&mut self, ctx: &Ctx, r: &Ser) -> Result<Vec<Id>> {
        let mut v = vec![r.s];
        v.extend(self.features_of(ctx, r.s, r.g)?);
        Ok(v)
    }

    fn record_emit(&mut self, ctx: &Ctx, s: Id, o: Id, g: Id) -> Result<bool> {
        let g = if self.spec.graph_var.is_some() {
            g
        } else {
            Id::UNDEF
        };
        if !self.emitted.insert((s, o, g)) {
            return Ok(false);
        }
        if self.emitted.len().is_multiple_of(4096) {
            ctx.check()?;
            ctx.check_rows(self.emitted.len())?;
        }
        self.pending_out.push_back((s, o, g));
        Ok(true)
    }

    fn push_pending_to_table(&mut self, out: &mut Table, cap: usize) {
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
                // Collect all serialization rows in scope and pack boxed geometries.
                if search::indexed(&ctx.snap, &self.preds).is_some() {
                    let mut st = SearchStats::default();
                    let mut known: Vec<(Id, GeomRef)> = Vec::new();
                    search::window_with(
                        ctx,
                        &self.preds,
                        &[WORLD],
                        &self.spec.graph,
                        false,
                        &mut st,
                        &mut |hits| {
                            for h in hits {
                                if let Ok(g) = noted(ctx, h.entry.geom(&ctx.snap)) {
                                    known.push((h.o, g));
                                }
                            }
                            Ok(())
                        },
                    )?;
                    self.st.nodes += st.nodes;
                    self.st.rechecked += st.rechecked;
                    for (o, g) in known {
                        self.geoms.entry(o).or_insert(Some(g));
                    }
                } else {
                    self.st.fallback = true;
                }
                let mut rows: Vec<Ser> = Vec::new();
                for p in self.preds.clone() {
                    let mut keys = Vec::new();
                    scan_keys(ctx, Perm::Pso, &[p.0], |k| {
                        keys.push(k);
                        if keys.len().is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        Ok(())
                    })?;
                    for k in keys {
                        if self.ser_graph(ctx, k[3]) {
                            rows.push(Ser {
                                s: Id(k[1]),
                                o: Id(k[2]),
                                g: Id(k[3]),
                            });
                        }
                    }
                }
                rows.sort_unstable();
                rows.dedup();
                self.st.candidates = self.st.candidates.max(rows.len() as u64);
                let mut by_lit: FxHashMap<Id, Vec<Ser>> = FxHashMap::default();
                for r in rows {
                    by_lit.entry(r.o).or_default().push(r);
                }
                let mut lits: Vec<Id> = by_lit.keys().copied().collect();
                lits.sort_unstable();
                let mut items = Vec::new();
                for lit in lits {
                    if let Some(geom) = self.geom(ctx, lit) {
                        let bbox = geom.bbox84();
                        items.push(RelateItem { lit, geom, bbox });
                    }
                }
                let (boxed_idx, unboxed_idx, tree, phase) = if self.spec.rel.index_usable() {
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
                        BothVarPhase::Boxed { outer: 0 }
                    } else if !unboxed_idx.is_empty() {
                        BothVarPhase::Unboxed { idx: 0 }
                    } else {
                        BothVarPhase::Done
                    };
                    (boxed_idx, unboxed_idx, tree, phase)
                } else {
                    let phase = if items.is_empty() {
                        BothVarPhase::Done
                    } else {
                        BothVarPhase::Disjoint { i: 0, j: 0 }
                    };
                    (Vec::new(), Vec::new(), None, phase)
                };
                self.stage = RelateStage::BothVar {
                    items,
                    by_lit,
                    boxed_idx,
                    unboxed_idx,
                    tree,
                    phase,
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
                let lits = self.const_lits(ctx, c)?;
                let mut by_lit: FxHashMap<Id, Vec<Option<Id>>> = FxHashMap::default();
                for l in lits {
                    by_lit.entry(l.lit).or_default().push(l.g);
                }
                let mut qs: Vec<Id> = by_lit.keys().copied().collect();
                qs.sort_unstable();
                self.stage = RelateStage::OneConst {
                    cid,
                    subject_const,
                    qs,
                    by_lit,
                    q_at: 0,
                    qg: None,
                    cand: OneConstCand::Done,
                    pending_sers: VecDeque::new(),
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
                let la = self.const_lits(ctx, a)?;
                let lb = self.const_lits(ctx, b)?;
                let graph_var = self.spec.graph_var.is_some();
                let mut graphs = Vec::new();
                let mut any = false;
                for x in &la {
                    let Some(gx) = self.geom(ctx, x.lit) else {
                        continue;
                    };
                    let mut tester = ConstTest::new(gx, self.spec.rel, true, self.op_vertices);
                    for y in &lb {
                        let g = match (x.g, y.g) {
                            (Some(p), Some(q)) if graph_var && p != q => continue,
                            (Some(p), _) | (None, Some(p)) => Some(p),
                            (None, None) => None,
                        };
                        let Some(gy) = self.geom(ctx, y.lit) else {
                            continue;
                        };
                        self.st.refined += 1;
                        if tester.holds(&gy) {
                            self.pairs += 1;
                            match g {
                                Some(g) => graphs.push(g),
                                None => any = true,
                            }
                        }
                    }
                }
                if any {
                    if graph_var {
                        let ids = ctx.snap.graph_ids()?;
                        graphs.extend(ids.into_iter().filter(|g| self.spec.graph.accepts(g.0)));
                    } else {
                        graphs.push(Id::UNDEF);
                    }
                }
                graphs.sort_unstable();
                graphs.dedup();
                for g in graphs {
                    self.record_emit(ctx, sid, oid, g)?;
                }
                self.stage = RelateStage::Done;
                self.update_charge()?;
            }
        }
        Ok(())
    }

    fn emit_pair_objects(&mut self, ctx: &Ctx, a_sers: &[Ser], b_sers: &[Ser]) -> Result<()> {
        let graph_var = self.spec.graph_var.is_some();
        for ra in a_sers {
            let oa = self.objects(ctx, ra)?;
            for rb in b_sers {
                if graph_var && ra.g != rb.g {
                    continue;
                }
                self.st.matched += 1;
                let ob = self.objects(ctx, rb)?;
                for &sa in &oa {
                    for &sb in &ob {
                        self.record_emit(ctx, sa, sb, ra.g)?;
                    }
                }
            }
        }
        Ok(())
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
            match &mut self.stage {
                RelateStage::Done => {
                    self.update_charge()?;
                    return Ok(self.pending_out.is_empty());
                }
                RelateStage::Asserted { scan, o_filter } => {
                    let o_filter = *o_filter;
                    let perm = scan.perm;
                    let mut raw = Vec::new();
                    let ended = scan.next_chunk(ctx, &mut raw, cap.clamp(64, 4096))?;
                    if ended {
                        self.stage = RelateStage::EnterDerived;
                    }
                    for k in raw {
                        let q = perm.to_quad(&k);
                        if self.spec.graph.accepts(q[3].0)
                            && o_filter.is_none_or(|o| o == q[2])
                            && self.record_emit(ctx, q[0], q[2], q[3])?
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
            qs,
            by_lit,
            mut q_at,
            mut qg,
            mut cand,
            mut pending_sers,
            mut lit_pass,
        } = stage
        else {
            unreachable!()
        };
        let graph_var = self.spec.graph_var.is_some();
        let base_retained = self.retained_bytes();
        let res: Result<()> = (|| {
            loop {
                ctx.check()?;
                if let Some(q_geom) = qg.as_ref() {
                    if pending_sers.is_empty() {
                        let mut cand_done = false;
                        match &mut cand {
                            OneConstCand::Indexed(iter) => {
                                let mut hits = Vec::new();
                                let charge = &mut self.charge;
                                let ended = iter.next_hits(
                                    ctx,
                                    &mut hits,
                                    cap.clamp(64, 4096),
                                    &mut |b| charge.resize(base_retained.saturating_add(b)),
                                )?;
                                for h in hits {
                                    if let Ok(g) = noted(ctx, h.entry.geom(&ctx.snap)) {
                                        self.geoms.entry(h.o).or_insert(Some(g));
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
                                    let ended =
                                        sc.next_chunk(ctx, &mut raw, cap.clamp(64, 4096))?;
                                    if ended {
                                        *scan = None;
                                    }
                                    for k in raw {
                                        if !self.ser_graph(ctx, k[3]) {
                                            continue;
                                        }
                                        let ser = Ser {
                                            s: Id(k[1]),
                                            o: Id(k[2]),
                                            g: Id(k[3]),
                                        };
                                        if let Some(w) = *window
                                            && let Some(g) = self.geom(ctx, ser.o)
                                            && g.bbox84().is_some_and(|b| !meets(b, w))
                                        {
                                            continue;
                                        }
                                        self.st.candidates += 1;
                                        pending_sers.push_back(ser);
                                    }
                                }
                            }
                            OneConstCand::Done => {}
                        }
                        if cand_done {
                            cand = OneConstCand::Done;
                        }
                    }
                    if !pending_sers.is_empty() {
                        let q_id = qs[q_at - 1];
                        let graphs = by_lit[&q_id].clone();
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
                            let pass = if let Some(&v) = lit_pass.get(&r.o) {
                                v
                            } else if let Some(w) = self.geom(ctx, r.o) {
                                self.st.refined += 1;
                                refined_since += 1;
                                if refined_since.is_multiple_of(CHECK_EVERY) {
                                    ctx.check()?;
                                }
                                let ok = tester.holds(&w);
                                if ok {
                                    self.pairs += 1;
                                }
                                lit_pass.insert(r.o, ok);
                                ok
                            } else {
                                lit_pass.insert(r.o, false);
                                false
                            };
                            if !pass {
                                continue;
                            }
                            if graph_var && !graphs.iter().any(|g| g.is_none_or(|g| g == r.g)) {
                                continue;
                            }
                            self.st.matched += 1;
                            for so in self.objects(ctx, &r)? {
                                if subject_const {
                                    self.record_emit(ctx, cid, so, r.g)?;
                                } else {
                                    self.record_emit(ctx, so, cid, r.g)?;
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
                    }
                }
                if q_at >= qs.len() {
                    return Ok(());
                }
                let q = qs[q_at];
                q_at += 1;
                let Some(q_geom) = self.geom(ctx, q) else {
                    continue;
                };
                let window = q_geom.bbox84().filter(|_| self.spec.rel.index_usable());
                cand = match window {
                    Some(w) if search::indexed(&ctx.snap, &self.preds).is_some() => {
                        let charge = &mut self.charge;
                        OneConstCand::Indexed(Box::new(WindowIter::open(
                            ctx,
                            self.preds.clone(),
                            vec![w],
                            self.spec.graph.clone(),
                            false,
                            &mut |b| charge.resize(base_retained.saturating_add(b)),
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
        stage = if q_at >= qs.len()
            && qg.is_none()
            && pending_sers.is_empty()
            && matches!(cand, OneConstCand::Done)
        {
            RelateStage::Done
        } else {
            RelateStage::OneConst {
                cid,
                subject_const,
                qs,
                by_lit,
                q_at,
                qg,
                cand,
                pending_sers,
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
            boxed_idx,
            unboxed_idx,
            tree,
            mut phase,
        } = stage
        else {
            unreachable!()
        };
        let rel = self.spec.rel;
        let opv = self.op_vertices;
        let res: Result<()> = (|| {
            while self.pending_out.len() < cap {
                ctx.check()?;
                match &mut phase {
                    BothVarPhase::Boxed { outer } => {
                        let Some(t) = &tree else {
                            phase = if !unboxed_idx.is_empty() {
                                BothVarPhase::Unboxed { idx: 0 }
                            } else {
                                BothVarPhase::Done
                            };
                            continue;
                        };
                        if *outer >= boxed_idx.len() {
                            phase = if !unboxed_idx.is_empty() {
                                BothVarPhase::Unboxed { idx: 0 }
                            } else {
                                BothVarPhase::Done
                            };
                            continue;
                        }
                        let i = boxed_idx[*outer];
                        *outer += 1;
                        let a = &items[i];
                        let w = a.bbox.expect("boxed item");
                        let found = t.search(&[w], &mut self.st.nodes);
                        if found.is_empty() {
                            continue;
                        }
                        self.st.candidates += found.len() as u64;
                        ctx.check_rows(self.st.candidates as usize)?;
                        let prep =
                            (found.len() > PREPARE_OVER).then(|| Prepared::new(a.geom.clone()));
                        let a_sers = by_lit[&a.lit].clone();
                        for c in found {
                            let j = boxed_idx[c as usize];
                            let b = &items[j];
                            self.st.refined += 1;
                            if self.st.refined.is_multiple_of(CHECK_EVERY as u64) {
                                ctx.check()?;
                            }
                            let ok = match &prep {
                                Some(p)
                                    if u64::from(a.geom.vertices) + u64::from(b.geom.vertices)
                                        <= opv =>
                                {
                                    p.relation(&b.geom, rel).unwrap_or(false)
                                }
                                Some(_) => false,
                                None => relate_relation(&a.geom, &b.geom, rel, opv),
                            };
                            if ok {
                                self.pairs += 1;
                                let b_sers = by_lit[&b.lit].clone();
                                self.emit_pair_objects(ctx, &a_sers, &b_sers)?;
                            }
                        }
                    }
                    BothVarPhase::Unboxed { idx } => {
                        if *idx >= unboxed_idx.len() {
                            phase = BothVarPhase::Done;
                            continue;
                        }
                        let i = unboxed_idx[*idx];
                        *idx += 1;
                        let a = &items[i];
                        let a_sers = by_lit[&a.lit].clone();
                        for (j, b) in items.iter().enumerate() {
                            if b.bbox.is_none() && (b.geom.empty || j < i) {
                                continue;
                            }
                            self.st.refined += 1;
                            if relate_relation(&a.geom, &b.geom, rel, opv) {
                                self.pairs += 1;
                                let b_sers = by_lit[&b.lit].clone();
                                self.emit_pair_objects(ctx, &a_sers, &b_sers)?;
                            }
                            if i != j {
                                self.st.refined += 1;
                                if relate_relation(&b.geom, &a.geom, rel, opv) {
                                    self.pairs += 1;
                                    let b_sers = by_lit[&b.lit].clone();
                                    self.emit_pair_objects(ctx, &b_sers, &a_sers)?;
                                }
                            }
                        }
                    }
                    BothVarPhase::Disjoint { i, j } => {
                        if *i >= items.len() {
                            phase = BothVarPhase::Done;
                            continue;
                        }
                        let cur_i = *i;
                        let a = &items[cur_i];
                        let a_sers = by_lit[&a.lit].clone();
                        let prep = (items.len() - cur_i > PREPARE_OVER)
                            .then(|| Prepared::new(a.geom.clone()));
                        while *j < items.len() && self.pending_out.len() < cap {
                            let cur_j = *j;
                            *j += 1;
                            let b = &items[cur_j];
                            self.st.refined += 1;
                            if self.st.refined.is_multiple_of(CHECK_EVERY as u64) {
                                ctx.check()?;
                            }
                            let fwd = match &prep {
                                Some(p)
                                    if u64::from(a.geom.vertices) + u64::from(b.geom.vertices)
                                        <= opv =>
                                {
                                    p.relation(&b.geom, rel).unwrap_or(false)
                                }
                                Some(_) => false,
                                None => relate_relation(&a.geom, &b.geom, rel, opv),
                            };
                            if fwd {
                                self.pairs += 1;
                                let b_sers = by_lit[&b.lit].clone();
                                self.emit_pair_objects(ctx, &a_sers, &b_sers)?;
                            }
                            if cur_i != cur_j {
                                self.st.refined += 1;
                                if relate_relation(&b.geom, &a.geom, rel, opv) {
                                    self.pairs += 1;
                                    let b_sers = by_lit[&b.lit].clone();
                                    self.emit_pair_objects(ctx, &b_sers, &a_sers)?;
                                }
                            }
                        }
                        if *j >= items.len() {
                            *i += 1;
                            *j = *i;
                        }
                    }
                    BothVarPhase::Done => break,
                }
            }
            Ok(())
        })();
        stage = if matches!(phase, BothVarPhase::Done) {
            RelateStage::Done
        } else {
            RelateStage::BothVar {
                items,
                by_lit,
                boxed_idx,
                unboxed_idx,
                tree,
                phase,
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

    fn add_table_batch(&mut self, ctx: &Ctx, batch: &Table, geom_var: VarId) -> Result<()> {
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
                    let idx = match g.as_ref().and_then(|g| item_box(g)) {
                        Some(b) => {
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
                        None => u32::MAX,
                    };
                    self.by_id.insert(id, idx);
                    idx
                }
            };
            if item != u32::MAX {
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

    fn add_hit(&mut self, ctx: &Ctx, ix: &OwnedIndexSide, h: &Hit) {
        let item = match self.by_id.get(&h.o) {
            Some(&idx) => idx,
            None => {
                let idx = match noted(ctx, h.entry.geom(&ctx.snap))
                    .ok()
                    .and_then(|g| item_box(&g).map(|b| (g, b)))
                {
                    Some((g, b)) => {
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
                    None => u32::MAX,
                };
                self.by_id.insert(h.o, idx);
                idx
            }
        };
        if item != u32::MAX {
            let row_idx = self.table.len() as u32;
            let mut row_buf = vec![Id::UNDEF; ix.cols.len()];
            ix.hit_row(h, &mut row_buf);
            self.table.push_row(&row_buf);
            self.rows[item as usize].push(row_idx);
        }
    }

    fn pack(&mut self) {
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
            for h in &buf {
                if ix.subj.is_none_or(|s| s == h.s) {
                    side.add_hit(ctx, ix, h);
                }
            }
            ctx.check_output(side.table.len(), side.table.width())?;
            charge.resize(side.retained_bytes().saturating_add(iter.retained_bytes()))?;
            if ended {
                break;
            }
        }
        st.nodes += iter.st.nodes;
        st.rechecked += iter.st.rechecked;
        st.fallback |= iter.st.fallback;
        side.pack();
        charge.resize(side.retained_bytes())?;
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
            build.add_table_batch(ctx, batch, build_geom)?;
            charge.resize(build.retained_bytes())
        })?;
        build.pack();
        charge.resize(build.retained_bytes())?;
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

    fn retained_bytes(&self) -> u64 {
        let pending = self.pending_out.capacity() as u64
            * (size_of::<Vec<Id>>() + self.vars.len() * size_of::<Id>()) as u64;
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
                .saturating_add(active.as_deref().map_or(0, |a| {
                    a.iter
                        .retained_bytes()
                        .saturating_add(a.hits.capacity() as u64 * size_of::<Hit>() as u64)
                        .saturating_add(a.lit_pass.capacity() as u64 * (size_of::<Id>() + 2) as u64)
                })),
            JoinMode::IndexIndexBuild {
                inner,
                outer_iter,
                outer_hits,
                ..
            } => inner
                .retained_bytes()
                .saturating_add(outer_iter.as_deref().map_or(0, WindowIter::retained_bytes))
                .saturating_add(outer_hits.capacity() as u64 * size_of::<Hit>() as u64),
            JoinMode::PlanIndexProbe { batch, active, .. } => batch
                .as_ref()
                .map_or(0, JoinSideData::retained_bytes)
                .saturating_add(active.as_deref().map_or(0, |a| {
                    a.iter
                        .retained_bytes()
                        .saturating_add(a.hits.capacity() as u64 * size_of::<Hit>() as u64)
                        .saturating_add(a.lit_pass.capacity() as u64 * (size_of::<Id>() + 2) as u64)
                })),
            JoinMode::PlanIndexBuild { inner, batch, .. } => inner
                .retained_bytes()
                .saturating_add(batch.as_ref().map_or(0, |(t, _)| table_capacity_bytes(t))),
            JoinMode::PlanPlan { build, batch, .. } => build
                .retained_bytes()
                .saturating_add(batch.as_ref().map_or(0, |(t, _)| table_capacity_bytes(t))),
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

    fn probe_item_against_side(
        sink: &mut ProbeSink<'_>,
        side: &JoinSideData,
        probe_geom: &GeomRef,
        probe_box: [f64; 4],
        probe_row: &[Id],
        probe_is_left: bool,
    ) -> Result<()> {
        let mut build_row = vec![Id::UNDEF; side.table.width()];
        let mut out_row = vec![Id::UNDEF; sink.vars_len];
        if placed(&probe_box) {
            if let Some(tree) = &side.tree {
                let found = tree.search(
                    &join_windows(&sink.exact.test, probe_box),
                    &mut sink.st.nodes,
                );
                if !found.is_empty() {
                    sink.st.candidates = sink.st.candidates.saturating_add(found.len() as u64);
                    if sink.st.candidates > sink.ctx.max_rows as u64 {
                        return Err(Error::BudgetExceeded(Budget {
                            kind: BudgetKind::Rows,
                            limit: sink.ctx.max_rows as u64,
                            requested: sink.st.candidates,
                        }));
                    }
                    let prep = (sink.exact.prepares() && found.len() > PREPARE_OVER)
                        .then(|| Prepared::new(probe_geom.clone()));
                    for c in found {
                        let bi = side.boxed_idx[c as usize];
                        let other = &side.items[bi];
                        sink.st.refined += 1;
                        if sink.st.refined.is_multiple_of(CHECK_EVERY as u64) {
                            sink.ctx.check()?;
                        }
                        let ok = match (&prep, probe_is_left) {
                            (Some(p), true) => sink.exact.holds_left(p, &other.geom),
                            (Some(p), false) => sink.exact.holds_right(&other.geom, p),
                            (None, true) => sink.exact.holds(probe_geom, &other.geom),
                            (None, false) => sink.exact.holds(&other.geom, probe_geom),
                        };
                        if ok {
                            *sink.npairs += 1;
                            for &br in &side.rows[bi] {
                                for (col, dst) in build_row.iter_mut().enumerate() {
                                    *dst = side.table.cols[col][br as usize];
                                }
                                if probe_is_left {
                                    sink.map.write_row(probe_row, &build_row, &mut out_row);
                                } else {
                                    sink.map.write_row(&build_row, probe_row, &mut out_row);
                                }
                                sink.pending_out.push_back(out_row.clone());
                            }
                        }
                    }
                }
            }
        } else {
            for &bi in &side.unplaced_idx {
                let other = &side.items[bi];
                if probe_geom.crs != other.geom.crs {
                    continue;
                }
                sink.st.candidates += 1;
                if sink.st.candidates > sink.ctx.max_rows as u64 {
                    return Err(Error::BudgetExceeded(Budget {
                        kind: BudgetKind::Rows,
                        limit: sink.ctx.max_rows as u64,
                        requested: sink.st.candidates,
                    }));
                }
                sink.st.refined += 1;
                let ok = if probe_is_left {
                    sink.exact.holds(probe_geom, &other.geom)
                } else {
                    sink.exact.holds(&other.geom, probe_geom)
                };
                if ok {
                    *sink.npairs += 1;
                    for &br in &side.rows[bi] {
                        for (col, dst) in build_row.iter_mut().enumerate() {
                            *dst = side.table.cols[col][br as usize];
                        }
                        if probe_is_left {
                            sink.map.write_row(probe_row, &build_row, &mut out_row);
                        } else {
                            sink.map.write_row(&build_row, probe_row, &mut out_row);
                        }
                        sink.pending_out.push_back(out_row.clone());
                    }
                }
            }
        }
        Ok(())
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
        loop {
            sink.ctx.check()?;
            if active.hits.is_empty() {
                let mut buf = Vec::new();
                let ended =
                    active
                        .iter
                        .next_hits(sink.ctx, &mut buf, cap.clamp(64, 4096), &mut |_| Ok(()))?;
                for h in buf {
                    if ix.subj.is_none_or(|s| s == h.s) {
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
                    pass
                };
                if !ok {
                    continue;
                }
                *sink.npairs += 1;
                ix.hit_row(&h, &mut inner_row);
                for &orow in &outer.rows[active.item_idx] {
                    for (col, dst) in outer_row.iter_mut().enumerate() {
                        *dst = outer.table.cols[col][orow as usize];
                    }
                    if outer_is_left {
                        sink.map.write_row(&outer_row, &inner_row, &mut out_row);
                    } else {
                        sink.map.write_row(&inner_row, &outer_row, &mut out_row);
                    }
                    sink.pending_out.push_back(out_row.clone());
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
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
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
                        while *next_item < w.items.len() {
                            let idx = *next_item;
                            *next_item += 1;
                            if !placed(&w.items[idx].bbox84) {
                                continue;
                            }
                            let ws = join_windows(&self.exact.test, w.items[idx].bbox84);
                            let iter = WindowIter::open(
                                ctx,
                                vec![inner_ix.pred],
                                ws,
                                inner_ix.graph.clone(),
                                inner_ix.scan.dedup,
                                &mut |_| Ok(()),
                            )?;
                            *active = Some(Box::new(ActiveProbe {
                                item_idx: idx,
                                iter,
                                hits: VecDeque::new(),
                                lit_pass: FxHashMap::default(),
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
                    let ended = iter.next_hits(ctx, &mut buf, 256, &mut |_| Ok(()))?;
                    if ended {
                        self.st.nodes += iter.st.nodes;
                        self.st.rechecked += iter.st.rechecked;
                        self.st.fallback |= iter.st.fallback;
                        *outer_iter = None;
                    }
                    if !buf.is_empty() {
                        let mut w = JoinSideData::new(outer_ix.vars.clone());
                        for h in &buf {
                            if outer_ix.subj.is_none_or(|s| s == h.s) {
                                w.add_hit(ctx, outer_ix, h);
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
                } => {
                    if outer_hits.is_empty() {
                        let Some(iter) = outer_iter.as_mut() else {
                            self.mode = JoinMode::Done;
                            continue;
                        };
                        let mut buf = Vec::new();
                        let ended =
                            iter.next_hits(ctx, &mut buf, cap.clamp(64, 4096), &mut |_| Ok(()))?;
                        if ended {
                            self.st.nodes += iter.st.nodes;
                            self.st.rechecked += iter.st.rechecked;
                            self.st.fallback |= iter.st.fallback;
                            *outer_iter = None;
                        }
                        outer_hits.extend(buf);
                        if outer_hits.is_empty() && outer_iter.is_none() {
                            self.mode = JoinMode::Done;
                            continue;
                        }
                    }
                    let mut outer_row = vec![Id::UNDEF; outer_ix.cols.len()];
                    while self.pending_out.len() < cap {
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
                        outer_ix.hit_row(&h, &mut outer_row);
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                        };
                        Self::probe_item_against_side(
                            &mut sink,
                            inner,
                            &g,
                            b,
                            &outer_row,
                            *outer_is_left,
                        )?;
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
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
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
                        while *next_item < b.items.len() {
                            let idx = *next_item;
                            *next_item += 1;
                            if !placed(&b.items[idx].bbox84) {
                                continue;
                            }
                            let ws = join_windows(&self.exact.test, b.items[idx].bbox84);
                            let iter = WindowIter::open(
                                ctx,
                                vec![ix.pred],
                                ws,
                                ix.graph.clone(),
                                ix.scan.dedup,
                                &mut |_| Ok(()),
                            )?;
                            *active = Some(Box::new(ActiveProbe {
                                item_idx: idx,
                                iter,
                                hits: VecDeque::new(),
                                lit_pass: FxHashMap::default(),
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
                            side.add_table_batch(ctx, &t, *geom_var)?;
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
                    child_done,
                } => {
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
                    let mut probe_row = vec![Id::UNDEF; t.width()];
                    while *at < t.len() && self.pending_out.len() < cap {
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
                        for (c, dst) in probe_row.iter_mut().enumerate() {
                            *dst = t.cols[c][i];
                        }
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                        };
                        Self::probe_item_against_side(
                            &mut sink,
                            inner,
                            &g,
                            b,
                            &probe_row,
                            *plan_is_left,
                        )?;
                    }
                    if *at >= t.len() {
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
                    probe_done,
                    ..
                } => {
                    if build.items.is_empty() {
                        // Drain/skip handled cleanly: no build geometries means no matches.
                        self.mode = JoinMode::Done;
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
                    let mut probe_row = vec![Id::UNDEF; t.width()];
                    while *at < t.len() && self.pending_out.len() < cap {
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
                        for (c, dst) in probe_row.iter_mut().enumerate() {
                            *dst = t.cols[c][i];
                        }
                        let mut sink = ProbeSink {
                            ctx,
                            exact: &self.exact,
                            st: &mut self.st,
                            npairs: &mut self.npairs,
                            map,
                            vars_len,
                            pending_out: &mut self.pending_out,
                        };
                        Self::probe_item_against_side(
                            &mut sink,
                            build,
                            &g,
                            b,
                            &probe_row,
                            !*build_is_left,
                        )?;
                    }
                    if *at >= t.len() {
                        *batch = None;
                    }
                    self.update_charge()?;
                }
            }
        }
    }
}
