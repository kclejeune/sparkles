//! Query Rewrite: the asserted and derived triples of a topological property.
//!
//! A spatial object resolves to geometry literals: a feature through
//! `geo:hasDefaultGeometry` and the configured serialization predicates, a geometry
//! through the serialization predicates, a literal to itself. `(so1, geo:R, so2)` is
//! derived when some pair of their literals passes `geof:R`, computed as the function
//! computes it; the answer is the set union with the asserted triples. Variables bind
//! features and geometries (never literals: a literal is an end only when written in
//! the query). Under `GRAPH ?g` both ends' literals, and the feature links, are in the
//! graph `?g` binds; otherwise every graph of the active graph counts.
//!
//! The literals are those the spatial index covers: rows of the configured
//! serialization predicates in graphs of its scope, the same whether the index is
//! ready or not. One constant end searches the index around each of its literals (or
//! reads every literal when the index cannot serve: building, a disjoint relation, a
//! literal with no place in longitude and latitude); two variable ends pair every
//! literal with the ones whose envelope meets it (every pair for the disjoint
//! relations, within the row budget); two constants test their literals.

use super::GeomRef;
use super::config::GeoConfig;
use super::exec::{Counters, config, graph_iri, state};
use super::geom::Geom;
use super::join::JoinItem;
use super::memo::{MemoKey, max_vertices, parse_value};
use super::ops::relate::{self, Prepared};
use super::search::{self, SearchStats};
use super::vocab::{self, Relation};
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::sparql::ctx::Ctx;
use crate::sparql::geojoin::JoinTest;
use crate::sparql::georewrite::{RelateEnd, SpatialRelateSpec};
use crate::sparql::table::{Table, VarId};
use crate::store::Chunk;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// Exact tests between two cancellation checks.
const CHECK_EVERY: usize = 256;
/// Prepare a geometry tested against more candidates than this.
const PREPARE_OVER: usize = 8;
const WORLD: [f64; 4] = [-180.0, -90.0, 180.0, 90.0];

/// A serialization row: geometry `s` has literal `o` in graph `g`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Ser {
    s: Id,
    o: Id,
    g: Id,
}

/// A literal of a constant end: its id and, unless the end is a literal written in the
/// query (which holds in every graph), the graph of its serialization.
#[derive(Clone, Copy, Debug)]
struct ConstLit {
    lit: Id,
    g: Option<Id>,
}

/// A distinct literal with its geometry.
struct Item {
    lit: Id,
    geom: GeomRef,
    bbox: Option<[f64; 4]>,
}

/// Execute a [`SpatialRelateSpec`].
pub fn spatial_relate(
    ctx: &Ctx,
    spec: &SpatialRelateSpec,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    let mut r = Relate::new(ctx, spec);
    if let Some(p) = spec.property {
        r.asserted(p)?;
    }
    let asserted = r.out.len();
    r.derived()?;
    let st = &r.st;
    let mut c = Counters::new();
    c.insert("candidates".into(), st.candidates.into());
    c.insert("refined".into(), st.refined.into());
    c.insert("matched".into(), st.matched.into());
    c.insert("treeNodesVisited".into(), st.nodes.into());
    c.insert("rechecked".into(), st.rechecked.into());
    c.insert("asserted".into(), (asserted as u64).into());
    c.insert("pairs".into(), r.pairs.into());
    c.insert("index".into(), state(ctx).to_string().into());
    c.insert("fallback".into(), st.fallback.into());
    let t = r.table(vars)?;
    Ok((t, c))
}

/// The state of one execution.
struct Relate<'a> {
    ctx: &'a Ctx,
    spec: &'a SpatialRelateSpec,
    cfg: Arc<GeoConfig>,
    preds: Vec<Id>,
    /// `geo:hasDefaultGeometry`, when the data holds it
    link: Option<Id>,
    op_vertices: u64,
    /// graph id → in the index's scope
    scope: FxHashMap<u64, bool>,
    /// literal → its geometry (`None`: no geometry a function accepts)
    geoms: FxHashMap<Id, Option<GeomRef>>,
    /// (geometry, graph of its serialization) → its features
    features: FxHashMap<(Id, Id), Vec<Id>>,
    /// every serialization row, once read
    all: Option<Vec<Ser>>,
    /// `(subject, object, graph)` (the graph `UNDEF` without a graph variable)
    out: FxHashSet<(Id, Id, Id)>,
    st: SearchStats,
    /// literal pairs that passed
    pairs: u64,
}

impl<'a> Relate<'a> {
    fn new(ctx: &'a Ctx, spec: &'a SpatialRelateSpec) -> Relate<'a> {
        let cfg = config(ctx);
        let preds = cfg
            .predicates
            .iter()
            .filter_map(|p| ctx.snap.lookup_iri(p))
            .collect();
        Relate {
            ctx,
            spec,
            preds,
            link: ctx.snap.lookup_iri(vocab::HAS_DEFAULT_GEOMETRY),
            op_vertices: ctx.geo.op_vertices(),
            cfg,
            scope: FxHashMap::default(),
            geoms: FxHashMap::default(),
            features: FxHashMap::default(),
            all: None,
            out: FxHashSet::default(),
            st: SearchStats::default(),
            pairs: 0,
        }
    }

    // ------------------------------------------------------------------ output --

    fn emit(&mut self, s: Id, o: Id, g: Id) -> Result<()> {
        let g = if self.spec.graph_var.is_some() {
            g
        } else {
            Id::UNDEF
        };
        if self.out.insert((s, o, g)) && self.out.len().is_multiple_of(4096) {
            self.ctx.check()?;
            self.ctx.check_rows(self.out.len())?;
        }
        Ok(())
    }

    /// The solutions over `vars`, sorted (repeated variables must agree).
    fn table(self, vars: &[VarId]) -> Result<Table> {
        let spec = self.spec;
        let var = |e: &RelateEnd| match e {
            RelateEnd::Var(v) => Some(*v),
            _ => None,
        };
        let (sv, ov, gv) = (var(&spec.subject), var(&spec.object), spec.graph_var);
        let mut rows: Vec<Vec<Id>> = Vec::with_capacity(self.out.len());
        'rows: for (s, o, g) in self.out {
            let mut row = vec![Id::UNDEF; vars.len()];
            for (x, v) in row.iter_mut().zip(vars) {
                let mut val: Option<Id> = None;
                for (w, id) in [(sv, s), (ov, o), (gv, g)] {
                    if w == Some(*v) {
                        match val {
                            Some(prev) if prev != id => continue 'rows,
                            _ => val = Some(id),
                        }
                    }
                }
                *x = val.unwrap_or(Id::UNDEF);
            }
            rows.push(row);
        }
        rows.sort_unstable();
        rows.dedup();
        self.ctx.check_output(rows.len(), vars.len())?;
        let mut t = Table::new(vars.to_vec());
        for r in &rows {
            t.push_row(r);
        }
        t.sorted = vars.to_vec();
        Ok(t)
    }

    // ---------------------------------------------------------------- asserted --

    /// The asserted triples of the property.
    fn asserted(&mut self, p: Id) -> Result<()> {
        let id = |e: &RelateEnd| match e {
            RelateEnd::Node(id) | RelateEnd::Geometry(id, _) => Some(*id),
            _ => None,
        };
        let (s, o) = (id(&self.spec.subject), id(&self.spec.object));
        if [s, o].iter().flatten().any(|i| i.tag() == Tag::Local) {
            return Ok(());
        }
        let (perm, prefix) = match (s, o) {
            (Some(s), _) => (Perm::Spo, vec![s.0, p.0]),
            (None, Some(o)) => (Perm::Pos, vec![p.0, o.0]),
            (None, None) => (Perm::Pso, vec![p.0]),
        };
        let mut quads = Vec::new();
        scan_keys(self.ctx, perm, &prefix, |k| {
            let q = perm.to_quad(&k);
            if self.spec.graph.accepts(q[3].0) && o.is_none_or(|o| o == q[2]) {
                quads.push(q);
            }
            Ok(())
        })?;
        for q in quads {
            self.emit(q[0], q[2], q[3])?;
        }
        Ok(())
    }

    // ----------------------------------------------------------------- derived --

    fn derived(&mut self) -> Result<()> {
        let spec = self.spec;
        if self.preds.is_empty() {
            return Ok(());
        }
        match (&spec.subject, &spec.object) {
            (RelateEnd::Var(_), RelateEnd::Var(_)) => self.both_variable(),
            (RelateEnd::Var(_), c) => {
                let lits = self.const_lits(c)?;
                self.one_constant(&lits, c, false)
            }
            (c, RelateEnd::Var(_)) => {
                let lits = self.const_lits(c)?;
                self.one_constant(&lits, c, true)
            }
            (a, b) => self.both_constant(a, b),
        }
    }

    /// Whether serialization quads of graph `g` are in the index's scope.
    fn in_scope(&mut self, g: u64) -> bool {
        let (ctx, cfg) = (self.ctx, &self.cfg);
        *self
            .scope
            .entry(g)
            .or_insert_with(|| graph_iri(ctx, Id(g)).is_some_and(|i| cfg.graph_in_scope(&i)))
    }

    /// Whether a serialization quad of graph `g` counts.
    fn ser_graph(&mut self, g: u64) -> bool {
        self.spec.graph.accepts(g) && self.in_scope(g)
    }

    /// The geometry of a literal, parsed as the functions parse it.
    fn geom(&mut self, lit: Id) -> Option<GeomRef> {
        let ctx = self.ctx;
        self.geoms
            .entry(lit)
            .or_insert_with(|| {
                let v = ctx.value(lit)?;
                let max = max_vertices(ctx);
                let g = ctx
                    .geo
                    .get_or_parse(MemoKey::Id(lit), || parse_value(&v, max))?;
                super::memo::note_crs(ctx, &g);
                Some(g)
            })
            .clone()
    }

    /// The serialization rows of geometry `s`.
    fn sers_of(&mut self, s: Id) -> Result<Vec<Ser>> {
        let mut out = Vec::new();
        for p in self.preds.clone() {
            let mut keys = Vec::new();
            scan_keys(self.ctx, Perm::Spo, &[s.0, p.0], |k| {
                keys.push(k);
                Ok(())
            })?;
            for k in keys {
                if self.ser_graph(k[3]) {
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

    /// The literals of a constant end (a feature's, a geometry's, or itself).
    fn const_lits(&mut self, e: &RelateEnd) -> Result<Vec<ConstLit>> {
        let id = match e {
            RelateEnd::Geometry(id, g) => {
                self.geoms.insert(*id, Some(g.clone()));
                return Ok(vec![ConstLit { lit: *id, g: None }]);
            }
            RelateEnd::Node(id) => *id,
            RelateEnd::Var(_) | RelateEnd::Absent => return Ok(Vec::new()),
        };
        let mut out: Vec<ConstLit> = self
            .sers_of(id)?
            .into_iter()
            .map(|r| ConstLit {
                lit: r.o,
                g: Some(r.g),
            })
            .collect();
        if let Some(link) = self.link {
            let mut geoms = Vec::new();
            scan_keys(self.ctx, Perm::Spo, &[id.0, link.0], |k| {
                if self.spec.graph.accepts(k[3]) {
                    geoms.push((Id(k[2]), k[3]));
                }
                Ok(())
            })?;
            for (geom, g1) in geoms {
                for r in self.sers_of(geom)? {
                    // under GRAPH ?g the link is in the serialization's graph
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

    /// The features of geometry `s` whose serialization is in graph `g`.
    fn features_of(&mut self, s: Id, g: Id) -> Result<Vec<Id>> {
        if let Some(f) = self.features.get(&(s, g)) {
            return Ok(f.clone());
        }
        let mut out = Vec::new();
        if let Some(link) = self.link {
            let same = self.spec.graph_var.is_some();
            let graph = &self.spec.graph;
            scan_keys(self.ctx, Perm::Pos, &[link.0, s.0], |k| {
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

    /// The spatial objects of a serialization row: its geometry and its features.
    fn objects(&mut self, r: &Ser) -> Result<Vec<Id>> {
        let mut v = vec![r.s];
        v.extend(self.features_of(r.s, r.g)?);
        Ok(v)
    }

    /// Every serialization row of the index's scope in the active graph, with the
    /// geometries the index already holds.
    fn all_rows(&mut self) -> Result<Vec<Ser>> {
        if let Some(a) = &self.all {
            return Ok(a.clone());
        }
        let ctx = self.ctx;
        if search::indexed(&ctx.snap, &self.preds).is_some() {
            // the index's geometries, so that only the literals it leaves out are parsed
            let mut known: Vec<(Id, GeomRef)> = Vec::new();
            let mut st = SearchStats::default();
            search::window_with(
                ctx,
                &self.preds,
                &[WORLD],
                &self.spec.graph,
                false,
                &mut st,
                &mut |hits| {
                    for h in hits {
                        if let Ok(g) = super::memo::noted(ctx, h.entry.geom(&ctx.snap)) {
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
        let mut keys = Vec::new();
        for p in self.preds.clone() {
            scan_keys(ctx, Perm::Pso, &[p.0], |k| {
                keys.push(k);
                if keys.len().is_multiple_of(4096) {
                    ctx.check()?;
                }
                Ok(())
            })?;
        }
        let mut rows = Vec::with_capacity(keys.len());
        for k in keys {
            if self.ser_graph(k[3]) {
                rows.push(Ser {
                    s: Id(k[1]),
                    o: Id(k[2]),
                    g: Id(k[3]),
                });
            }
        }
        rows.sort_unstable();
        rows.dedup();
        self.st.candidates = self.st.candidates.max(rows.len() as u64);
        self.all = Some(rows.clone());
        Ok(rows)
    }

    /// The rows whose literal may relate to `q` (its envelope meets `q`'s, when the
    /// relation needs the geometries to meet and the index can search).
    fn candidates(&mut self, q: &Geom) -> Result<Vec<Ser>> {
        let window = q.bbox84().filter(|_| self.spec.rel.index_usable());
        let ctx = self.ctx;
        match window {
            Some(w) if search::indexed(&ctx.snap, &self.preds).is_some() => {
                let mut rows = Vec::new();
                let mut found: Vec<(Id, GeomRef)> = Vec::new();
                let mut st = SearchStats::default();
                search::window_with(
                    ctx,
                    &self.preds,
                    &[w],
                    &self.spec.graph,
                    false,
                    &mut st,
                    &mut |hits| {
                        for h in hits {
                            rows.push(Ser {
                                s: h.s,
                                o: h.o,
                                g: h.g,
                            });
                            if let Ok(g) = super::memo::noted(ctx, h.entry.geom(&ctx.snap)) {
                                found.push((h.o, g));
                            }
                        }
                        Ok(())
                    },
                )?;
                self.st.candidates += st.candidates;
                self.st.nodes += st.nodes;
                self.st.rechecked += st.rechecked;
                for (o, g) in found {
                    self.geoms.entry(o).or_insert(Some(g));
                }
                Ok(rows)
            }
            // an empty geometry meets nothing
            None if self.spec.rel.index_usable() && q.empty => Ok(Vec::new()),
            Some(w) => {
                let mut rows = self.all_rows()?;
                // envelopes that miss the window cannot meet `q`
                rows.retain(|r| {
                    let g = self.geoms.get(&r.o).cloned().flatten();
                    g.is_none_or(|g| g.bbox84().is_none_or(|b| meets(b, w)))
                });
                Ok(rows)
            }
            None => self.all_rows(),
        }
    }

    /// One constant end (the subject when `subject_const`): the variable end's
    /// objects whose literals relate to one of the constant's.
    fn one_constant(
        &mut self,
        lits: &[ConstLit],
        c: &RelateEnd,
        subject_const: bool,
    ) -> Result<()> {
        let cid = match c {
            RelateEnd::Node(id) | RelateEnd::Geometry(id, _) => *id,
            _ => return Ok(()),
        };
        let graph_var = self.spec.graph_var.is_some();
        let mut by_lit: FxHashMap<Id, Vec<Option<Id>>> = FxHashMap::default();
        for l in lits {
            by_lit.entry(l.lit).or_default().push(l.g);
        }
        let mut qs: Vec<Id> = by_lit.keys().copied().collect();
        qs.sort_unstable();
        for q in qs {
            let Some(qg) = self.geom(q) else {
                continue;
            };
            let rows = self.candidates(&qg)?;
            let mut distinct: Vec<Id> = rows.iter().map(|r| r.o).collect();
            distinct.sort_unstable();
            distinct.dedup();
            let items: Vec<(Id, GeomRef)> = distinct
                .into_iter()
                .filter_map(|o| self.geom(o).map(|g| (o, g)))
                .collect();
            let pass = self.refine_against(&qg, subject_const, &items)?;
            let graphs = &by_lit[&q];
            for r in rows.iter().filter(|r| pass.contains(&r.o)) {
                // under GRAPH ?g the constant's literal is in the row's graph
                if graph_var && !graphs.iter().any(|g| g.is_none_or(|g| g == r.g)) {
                    continue;
                }
                self.st.matched += 1;
                for so in self.objects(r)? {
                    if subject_const {
                        self.emit(cid, so, r.g)?;
                    } else {
                        self.emit(so, cid, r.g)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The literals of `items` that relate to `q`: `R(q, w)` when `q_first`, else
    /// `R(w, q)`.
    fn refine_against(
        &mut self,
        q: &GeomRef,
        q_first: bool,
        items: &[(Id, GeomRef)],
    ) -> Result<FxHashSet<Id>> {
        let (ctx, rel, opv) = (self.ctx, self.spec.rel, self.op_vertices);
        let run = |part: &[(Id, GeomRef)]| -> Result<Vec<Id>> {
            let mut t = ConstTest::new(q.clone(), rel, q_first, opv);
            let mut out = Vec::new();
            for (i, (id, w)) in part.iter().enumerate() {
                if i % CHECK_EVERY == CHECK_EVERY - 1 {
                    ctx.check()?;
                }
                if t.holds(w) {
                    out.push(*id);
                }
            }
            Ok(out)
        };
        self.st.refined += items.len() as u64;
        let pass: Vec<Id> = if items.len() <= 2 * CHECK_EVERY {
            run(items)?
        } else {
            let size = (items.len() / (rayon::current_num_threads() * 4)).max(CHECK_EVERY);
            let parts: Vec<Vec<Id>> = items.par_chunks(size).map(run).collect::<Result<_>>()?;
            parts.into_iter().flatten().collect()
        };
        self.pairs += pass.len() as u64;
        Ok(pass.into_iter().collect())
    }

    /// Two variable ends: every pair of literals that relate, joined back to their
    /// spatial objects.
    fn both_variable(&mut self) -> Result<()> {
        let rows = self.all_rows()?;
        let mut by_lit: FxHashMap<Id, Vec<Ser>> = FxHashMap::default();
        for r in &rows {
            by_lit.entry(r.o).or_default().push(*r);
        }
        let mut lits: Vec<Id> = by_lit.keys().copied().collect();
        lits.sort_unstable();
        let items: Vec<Item> = lits
            .into_iter()
            .filter_map(|lit| {
                let geom = self.geom(lit)?;
                Some(Item {
                    lit,
                    bbox: geom.bbox84(),
                    geom,
                })
            })
            .collect();
        // a sweep over the envelopes, in parallel; the spatial join kernel
        // (`super::join::pairs`) can take its place for the geometries with an envelope
        let pairs = literal_pairs(
            self.ctx,
            self.spec.rel,
            &items,
            self.op_vertices,
            &mut self.st,
        )?;
        self.pairs += pairs.len() as u64;
        let graph_var = self.spec.graph_var.is_some();
        for (i, j) in pairs {
            let (a, b) = (
                &by_lit[&items[i as usize].lit],
                &by_lit[&items[j as usize].lit],
            );
            let (a, b) = (a.clone(), b.clone());
            for ra in &a {
                let oa = self.objects(ra)?;
                for rb in &b {
                    if graph_var && ra.g != rb.g {
                        continue;
                    }
                    self.st.matched += 1;
                    let ob = self.objects(rb)?;
                    for &sa in &oa {
                        for &sb in &ob {
                            self.emit(sa, sb, ra.g)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Two constant ends: whether some pair of their literals relates.
    fn both_constant(&mut self, a: &RelateEnd, b: &RelateEnd) -> Result<()> {
        let (sid, oid) = match (a, b) {
            (
                RelateEnd::Node(s) | RelateEnd::Geometry(s, _),
                RelateEnd::Node(o) | RelateEnd::Geometry(o, _),
            ) => (*s, *o),
            _ => return Ok(()),
        };
        let la = self.const_lits(a)?;
        let lb = self.const_lits(b)?;
        let graph_var = self.spec.graph_var.is_some();
        let mut graphs: Vec<Id> = Vec::new();
        let mut any = false;
        for x in &la {
            let Some(gx) = self.geom(x.lit) else {
                continue;
            };
            let mut tester = ConstTest::new(gx, self.spec.rel, true, self.op_vertices);
            for y in &lb {
                let g = match (x.g, y.g) {
                    (Some(p), Some(q)) if graph_var && p != q => continue,
                    (Some(p), _) | (None, Some(p)) => Some(p),
                    (None, None) => None,
                };
                let Some(gy) = self.geom(y.lit) else {
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
                // two literals relate in every graph of the scope
                let ids = self.ctx.snap.graph_ids()?;
                graphs.extend(ids.into_iter().filter(|g| self.spec.graph.accepts(g.0)));
            } else {
                graphs.push(Id::UNDEF);
            }
        }
        graphs.sort_unstable();
        graphs.dedup();
        for g in graphs {
            self.emit(sid, oid, g)?;
        }
        Ok(())
    }
}

/// Whether two boxes meet (closed).
fn meets(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

/// Whether `a` and `b` have the same internal coordinates: the same CRS, or both
/// geographic (longitude, latitude), where a transform changes nothing.
fn same_frame(a: &Geom, b: &Geom) -> bool {
    a.crs == b.crs
        || matches!((a.crs.known(), b.crs.known()),
            (Some(x), Some(y)) if x.is_geographic() && y.is_geographic())
}

/// `R(a, b)` as the `geof:` function computes it (`b` transformed into `a`'s CRS); an
/// error, or more vertices than one operation may take, is false.
fn relation(a: &Geom, b: &Geom, rel: Relation, op_vertices: u64) -> bool {
    u64::from(a.vertices) + u64::from(b.vertices) <= op_vertices
        && relate::relation(a, b, rel).unwrap_or(false)
}

/// Tests of many geometries against one constant geometry, prepared once.
struct ConstTest {
    q: GeomRef,
    rel: Relation,
    /// `R(q, w)` (else `R(w, q)`)
    q_first: bool,
    prep: Option<Prepared>,
    op_vertices: u64,
}

impl ConstTest {
    fn new(q: GeomRef, rel: Relation, q_first: bool, op_vertices: u64) -> ConstTest {
        ConstTest {
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
            // the prepared geometry is the first argument, as in the function
            return self.prepared().relation(w, rel).unwrap_or(false);
        }
        // `R(w, q)` is `converse(R)(q, w)`, the same computation in a common frame
        match rel.converse() {
            Some(c) if same_frame(w, &self.q) => self.prepared().relation(w, c).unwrap_or(false),
            _ => relate::relation(w, &self.q, rel).unwrap_or(false),
        }
    }
}

/// The pairs `(i, j)` of `items` with `R(items[i], items[j])`. A relation that needs
/// the geometries to meet tests the pairs of geometries with an envelope through the
/// spatial join's kernel ([`super::join::pairs`]: those whose envelopes meet), and every
/// pair with a geometry that has no envelope in longitude and latitude; a disjoint
/// relation tests every pair.
fn literal_pairs(
    ctx: &Ctx,
    rel: Relation,
    items: &[Item],
    op_vertices: u64,
    st: &mut SearchStats,
) -> Result<Vec<(u32, u32)>> {
    let n = items.len();
    let mut pairs = Vec::new();
    let mut tests = 0u64;
    let others: Vec<usize> = if rel.index_usable() {
        // both orders of every pair, and each geometry with itself
        let boxed: Vec<JoinItem> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                Some(JoinItem {
                    row: i as u32,
                    id: it.lit,
                    bbox84: it.bbox?,
                    geom: it.geom.clone(),
                })
            })
            .collect();
        super::join::pairs(
            ctx,
            &boxed,
            &boxed,
            &JoinTest::Relation(rel),
            st,
            &mut |p| {
                pairs.extend_from_slice(p);
                ctx.check_rows(pairs.len())
            },
        )?;
        // an empty geometry meets nothing
        (0..n)
            .filter(|&i| items[i].bbox.is_none() && !items[i].geom.empty)
            .collect()
    } else {
        // every pair `(i, j)` with `i <= j`, both orders
        let run = |ks: std::ops::Range<usize>| -> Result<(Vec<(u32, u32)>, u64)> {
            let mut out = Vec::new();
            let mut tests = 0u64;
            for i in ks {
                ctx.check()?;
                let a = &items[i];
                let prep = (n - i > PREPARE_OVER).then(|| Prepared::new(a.geom.clone()));
                for (j, b) in items.iter().enumerate().skip(i) {
                    tests += 1;
                    let fwd = match &prep {
                        Some(p)
                            if u64::from(a.geom.vertices) + u64::from(b.geom.vertices)
                                <= op_vertices =>
                        {
                            p.relation(&b.geom, rel).unwrap_or(false)
                        }
                        Some(_) => false,
                        None => relation(&a.geom, &b.geom, rel, op_vertices),
                    };
                    if fwd {
                        out.push((i as u32, j as u32));
                    }
                    if i != j {
                        tests += 1;
                        if relation(&b.geom, &a.geom, rel, op_vertices) {
                            out.push((j as u32, i as u32));
                        }
                    }
                }
            }
            Ok((out, tests))
        };
        let chunk = 64usize;
        let ranges: Vec<std::ops::Range<usize>> = (0..n)
            .step_by(chunk)
            .map(|s| s..(s + chunk).min(n))
            .collect();
        // the disjoint relations hold for most pairs: stop at the row budget
        for batch in ranges.chunks(rayon::current_num_threads().max(1) * 4) {
            let parts: Vec<(Vec<(u32, u32)>, u64)> =
                batch.par_iter().cloned().map(run).collect::<Result<_>>()?;
            for (p, t) in parts {
                pairs.extend(p);
                tests += t;
            }
            ctx.check_rows(pairs.len())?;
        }
        Vec::new()
    };
    // geometries without an envelope: against every geometry, both orders
    for (n_done, &i) in others.iter().enumerate() {
        if n_done % CHECK_EVERY == 0 {
            ctx.check()?;
        }
        let a = &items[i];
        for (j, b) in items.iter().enumerate() {
            // an empty geometry meets nothing; two geometries without an envelope were
            // tested from the first of them
            if b.bbox.is_none() && (b.geom.empty || j < i) {
                continue;
            }
            tests += 1;
            if relation(&a.geom, &b.geom, rel, op_vertices) {
                pairs.push((i as u32, j as u32));
            }
            if i != j {
                tests += 1;
                if relation(&b.geom, &a.geom, rel, op_vertices) {
                    pairs.push((j as u32, i as u32));
                }
            }
        }
        ctx.check_rows(pairs.len())?;
    }
    st.refined += tests;
    Ok(pairs)
}

/// Visit the keys of a prefix scan.
fn scan_keys(
    ctx: &Ctx,
    perm: Perm,
    prefix: &[u64],
    mut f: impl FnMut([u64; 4]) -> Result<()>,
) -> Result<()> {
    let mut err = None;
    ctx.snap.scan(perm, prefix, |c| {
        let r = match c {
            Chunk::Block(b, s, e) => (s..e).try_for_each(|i| f(b.key(i))),
            Chunk::Row(k) => f(k),
        };
        match r {
            Ok(()) => Ok(true),
            Err(e) => {
                err = Some(e);
                Ok(false)
            }
        }
    })?;
    err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use crate::geo::GeoConfig;
    use crate::io::{RdfFormat, Source};
    use crate::sparql::{PlanInfo, QueryOptions, query};
    use crate::store::{Store, StoreOptions};

    const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:B geo:hasDefaultGeometry ex:gB . ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
ex:p1 geo:hasDefaultGeometry ex:g1 .  ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2 .  ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:nil geo:hasDefaultGeometry ex:gE . ex:gE geo:asWKT ""^^geo:wktLiteral .
ex:mars geo:hasDefaultGeometry ex:gM . ex:gM geo:asWKT "<http://example.org/crs/mars> POINT(1 1)"^^geo:wktLiteral .
ex:G1 { ex:p4 geo:hasDefaultGeometry ex:g4 . ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral . }
ex:A geo:sfTouches ex:p2 .
"#;

    const QUERIES: [&str; 8] = [
        "SELECT ?x { ?x geo:sfContains ex:g1 }",
        "SELECT ?x ?y { ?x geo:sfIntersects ?y }",
        "SELECT ?x ?y { ?x geo:sfDisjoint ?y }",
        "SELECT ?x ?g { GRAPH ?g { ?x geo:sfWithin ?y } }",
        "SELECT ?y { ex:A geo:sfTouches ?y }",
        "SELECT ?y { ex:gM geo:sfEquals ?y }",
        "SELECT ?x { ?x geo:ehInside \"POLYGON((1 1, 3 1, 3 3, 1 3, 1 1))\"^^geo:wktLiteral }",
        "SELECT ?x { ?x <http://jena.apache.org/spatial#equals> ex:p1 }",
    ];

    fn index(p: &PlanInfo) -> Option<String> {
        if p.operator == "SpatialRelate" {
            return Some(p.counters.as_ref()?["index"].to_string());
        }
        p.children.iter().find_map(index)
    }

    /// Each query's sorted solutions and the index state its operator reports.
    fn answers(s: &Store) -> Vec<(Vec<String>, String)> {
        QUERIES
            .iter()
            .map(|q| {
                let q = format!(
                    "PREFIX ex: <http://example.org/> \
                     PREFIX geo: <http://www.opengis.net/ont/geosparql#> {q}"
                );
                let o = QueryOptions {
                    no_cache: true,
                    ..Default::default()
                };
                let r = query(s.snapshot(), &q, &o).unwrap();
                let mut rows: Vec<String> =
                    r.rows().into_iter().map(|r| format!("{r:?}")).collect();
                rows.sort();
                (rows, index(&r.plan).unwrap())
            })
            .collect()
    }

    /// While the index builds, the operator reads the predicates instead, with the same
    /// answers.
    #[test]
    fn a_building_index_gives_the_same_answers() {
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            FIXTURE.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        s.pause_geo_build(true);
        s.enable_geo(GeoConfig {
            query_rewrite: true,
            ..GeoConfig::default()
        })
        .unwrap();
        let building = answers(&s);
        s.pause_geo_build(false);
        s.wait_geo();
        let ready = answers(&s);
        for ((b, bi), (r, ri)) in building.iter().zip(&ready) {
            assert_eq!(b, r);
            assert!(bi.contains("building"), "{bi}");
            assert_eq!(ri, "\"ready\"");
        }
        // A, gA, g1, p1
        assert_eq!(ready[0].0.len(), 4, "{:?}", ready[0]);
        assert!(ready.iter().all(|(r, _)| !r.is_empty()), "{ready:?}");
    }
}
