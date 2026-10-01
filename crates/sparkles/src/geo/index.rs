//! The spatial index: a packed R-tree over a generation's base rows, an overlay of rows
//! inserted by transactions since, and the view a snapshot holds of both.
//!
//! * The **base** ([`GeoBase`]) is built from the generation's PSO index for the
//!   configured predicates: one row per base quad whose object is an indexable geometry,
//!   and a Hilbert-packed R-tree (`f32` boxes rounded outward, 16-entry nodes) over them.
//! * The **overlay** holds rows inserted by commits since, in a packed tree of its own,
//!   and the **tail** the rows inserted since that tree was built (a persistent vector, so
//!   each snapshot keeps its own cheaply).
//! * A row is valid for a snapshot S iff it is a base row whose quad is not in
//!   `S.delta.del`, or an overlay or tail row whose quad is in `S.delta.ins`: since the
//!   delta keeps `ins` disjoint from the base and `del` within it, the rows of S are
//!   exactly `(base − del) ∪ (overlay ∪ tail) ∩ ins`.

use super::column::{Column, ColumnEntry, Counts, Reuse, Slot};
use super::config::{
    FORMAT_VERSION, GeoBuild, GeoConfig, GeoFiles, GeoMemory, GeoRows, GeoSkipped, GeoStatus,
    IndexState,
};
use super::persist::{self, FileKind, Identity, Mapped, Problem};
use super::tree::PackedTree;
use super::wgs84::{self, Pair};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Key, Perm};
use crate::store::Snapshot;
use crate::text::PredicateSet;
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// An indexed quad (raw ids). The layout is that of the rows in `rtree.spkg` (four
/// little-endian `u64`), which are read in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(C)]
pub(crate) struct Row {
    pub s: u64,
    pub p: u64,
    pub o: u64,
    pub g: u64,
}

impl Row {
    pub fn of(q: &[Id; 4]) -> Row {
        Row {
            s: q[0].0,
            p: q[1].0,
            o: q[2].0,
            g: q[3].0,
        }
    }

    /// The row's key in the PSO permutation.
    #[inline]
    pub fn pso(&self) -> Key {
        [self.p, self.s, self.o, self.g]
    }
}

/// A row inserted by a commit, with its geometry.
#[derive(Clone)]
pub(crate) struct TailRow {
    pub row: Row,
    pub entry: Arc<ColumnEntry>,
}

/// Rows held in memory, or read in place from a mapped `rtree.spkg`.
pub(crate) enum Rows {
    Owned(Vec<Row>),
    Mapped {
        file: Arc<Mapped>,
        /// byte offset in the file's data (a multiple of 8)
        at: usize,
        n: usize,
    },
}

impl Rows {
    /// The `n` rows at `at` of `file`'s data (`None`: out of bounds or misaligned).
    fn mapped(file: Arc<Mapped>, at: usize, n: usize) -> Option<Rows> {
        let len = n.checked_mul(std::mem::size_of::<Row>())?;
        let b = file.data().get(at..at.checked_add(len)?)?;
        if !persist::SUPPORTED || !(b.as_ptr() as usize).is_multiple_of(std::mem::align_of::<Row>())
        {
            return None;
        }
        Some(Rows::Mapped { file, at, n })
    }

    /// Bytes held in memory.
    fn heap_bytes(&self) -> u64 {
        match self {
            Rows::Owned(v) => (v.len() * std::mem::size_of::<Row>()) as u64,
            Rows::Mapped { .. } => 0,
        }
    }
}

impl std::ops::Deref for Rows {
    type Target = [Row];
    fn deref(&self) -> &[Row] {
        match self {
            Rows::Owned(v) => v,
            Rows::Mapped { file, at, n } => {
                let b = &file.data()[*at..*at + n * std::mem::size_of::<Row>()];
                // SAFETY: `Rows::mapped` checked that the bytes are in the mapping and
                // aligned for `Row`, a `repr(C)` struct of four `u64` for which every bit
                // pattern is valid; the files are little-endian, as this platform
                // (`persist::SUPPORTED`), and the mapping lives as long as `file`.
                unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<Row>(), *n) }
            }
        }
    }
}

fn tree_bytes(t: &Option<PackedTree>) -> u64 {
    t.as_ref()
        .filter(|t| !t.is_mapped())
        .map_or(0, PackedTree::bytes)
}

/// Whether the `f32` box `b` intersects the CRS84 window `w`.
#[inline]
pub(crate) fn intersects(b: &[f32; 4], w: &[f64; 4]) -> bool {
    f64::from(b[0]) <= w[2]
        && f64::from(b[2]) >= w[0]
        && f64::from(b[1]) <= w[3]
        && f64::from(b[3]) >= w[1]
}

/// The window as tree coordinates, rounded outward (so no box touching it is missed).
pub(crate) fn window_f32(w: &[f64; 4]) -> [f32; 4] {
    super::column::round_out(*w)
}

/// Predicate and graph decisions of one generation and configuration, cached by id: the
/// commit path pays one lookup per logged quad.
pub(crate) struct Lookup {
    /// predicate id → slot (index in the configuration's predicates), or not indexed
    preds: RwLock<FxHashMap<u64, Option<u16>>>,
    /// graph id → in scope (empty when every graph is)
    graphs: RwLock<FxHashMap<u64, bool>>,
    every_graph: bool,
}

impl Lookup {
    /// The lookup for `snap`'s generation, with the configured predicates that `snap`
    /// knows resolved.
    pub fn new(snap: &Snapshot, cfg: &GeoConfig) -> Lookup {
        let l = Lookup::empty(cfg);
        l.resolve(snap, cfg);
        l
    }

    /// Learn the ids `snap` has for the configured predicates (a predicate first used
    /// after the lookup was made has a delta id it does not know yet).
    pub fn resolve(&self, snap: &Snapshot, cfg: &GeoConfig) {
        let mut preds = self.preds.write();
        for (i, iri) in cfg.predicates.iter().enumerate() {
            if let Some(id) = snap.lookup_iri(iri) {
                preds.insert(id.0, Some(i as u16));
            }
        }
    }

    /// A lookup that knows no ids yet.
    pub fn empty(cfg: &GeoConfig) -> Lookup {
        Lookup {
            preds: RwLock::new(FxHashMap::default()),
            graphs: RwLock::new(FxHashMap::default()),
            every_graph: matches!(cfg.graphs.include, PredicateSet::All)
                && cfg.graphs.exclude.is_empty(),
        }
    }

    /// The slot of predicate `p` if it is indexed; `snap` decodes unseen ids.
    #[inline]
    pub fn slot(&self, p: Id, snap: &Snapshot, cfg: &GeoConfig) -> Option<u16> {
        if let Some(&s) = self.preds.read().get(&p.0) {
            return s;
        }
        let s = match snap.key(p) {
            Some(k) if k.first() == Some(&b'<') => cfg
                .predicates
                .iter()
                .position(|x| x.as_bytes() == &k[1..])
                .map(|i| i as u16),
            _ => None,
        };
        self.preds.write().insert(p.0, s);
        s
    }

    /// The slot of `p`, from what is known already.
    pub fn known_slot(&self, p: Id) -> Option<u16> {
        self.preds.read().get(&p.0).copied().flatten()
    }

    /// The ids known for the configured predicates.
    pub fn indexed(&self) -> Vec<(u64, u16)> {
        let mut v: Vec<(u64, u16)> = self
            .preds
            .read()
            .iter()
            .filter_map(|(&p, s)| s.map(|s| (p, s)))
            .collect();
        v.sort_unstable();
        v
    }

    /// Whether quads of graph `g` are indexed.
    #[inline]
    pub fn graph(&self, g: Id, snap: &Snapshot, cfg: &GeoConfig) -> bool {
        if self.every_graph {
            return true;
        }
        if let Some(&b) = self.graphs.read().get(&g.0) {
            return b;
        }
        let b = graph_name(snap, g).is_some_and(|n| cfg.graph_in_scope(&n));
        self.graphs.write().insert(g.0, b);
        b
    }
}

fn graph_name(snap: &Snapshot, g: Id) -> Option<String> {
    if g == Id::DEFAULT_GRAPH {
        return Some(crate::text::DEFAULT_GRAPH_IRI.to_string());
    }
    match snap.term(g)? {
        oxrdf::Term::NamedNode(n) => Some(n.into_string()),
        oxrdf::Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        _ => None,
    }
}

/// The base of one generation under one configuration: rows, tree and column.
pub(crate) struct GeoBase {
    /// `Generation::uid`
    pub generation: u64,
    pub generation_name: String,
    /// base rows in PSO order (the tree's items)
    pub rows: Rows,
    /// base rows whose literal was skipped but may still match (see
    /// [`Slot::rechecked`]): candidates of every search
    pub skipped: Vec<Row>,
    pub tree: Option<PackedTree>,
    pub column: Column,
    /// base rows per predicate slot
    pub slot_rows: Vec<u64>,
    pub built_ms: f64,
    /// base rows that are W3C Basic Geo points
    pub pair_rows: u64,
    /// read from the generation's index files (not built in this process)
    pub opened: bool,
    /// the index files the base is read from (bytes)
    pub files_bytes: u64,
}

impl GeoBase {
    /// An empty base (no rows, empty column).
    pub fn empty(snap: &Snapshot, cfg: &GeoConfig) -> GeoBase {
        GeoBase {
            generation: snap.generation.uid,
            generation_name: snap.generation.name.clone(),
            rows: Rows::Owned(Vec::new()),
            skipped: Vec::new(),
            tree: None,
            column: Column::empty(),
            slot_rows: vec![0; cfg.predicates.len()],
            built_ms: 0.0,
            pair_rows: 0,
            opened: false,
            files_bytes: 0,
        }
    }

    /// Memory of rows, tree and column (what is read from files in place excluded).
    pub fn bytes(&self) -> u64 {
        self.tree_bytes() + self.column.bytes()
    }

    fn tree_bytes(&self) -> u64 {
        self.rows.heap_bytes()
            + (self.skipped.len() * std::mem::size_of::<Row>()) as u64
            + tree_bytes(&self.tree)
    }

    /// Write the base's files (`rtree.spkg`, `column.spkg`) into `dir` (a generation's
    /// `geo/` directory) for `ident`, durably.
    pub(crate) fn write_files(&self, dir: &Path, ident: &Identity) -> Result<()> {
        std::fs::create_dir(dir).or_else(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => Ok(()),
            _ => Err(e),
        })?;
        self.column.write(&dir.join(persist::COLUMN_FILE), ident)?;
        let tree = self.tree.as_ref().map_or(&[][..], PackedTree::data);
        let pairs = self.column.base_pairs();
        let mut index = persist::index_prefix(ident);
        for n in [
            self.rows.len(),
            self.skipped.len(),
            self.slot_rows.len(),
            tree.len(),
            pairs.len(),
        ] {
            index.extend_from_slice(&(n as u64).to_le_bytes());
        }
        let row = |r: &Row| {
            let mut b = [0u8; 32];
            for (i, x) in [r.s, r.p, r.o, r.g].into_iter().enumerate() {
                b[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
            }
            b
        };
        persist::write_file(
            &dir.join(persist::RTREE_FILE),
            FileKind::Rtree,
            ident,
            self.rows.len() as u64,
            &index,
            |sink| {
                let mut buf = Vec::with_capacity(32 * 4096);
                for chunk in self.rows.chunks(4096).chain(self.skipped.chunks(4096)) {
                    buf.clear();
                    for r in chunk {
                        buf.extend_from_slice(&row(r));
                    }
                    sink.put(&buf)?;
                }
                for n in &self.slot_rows {
                    sink.put(&n.to_le_bytes())?;
                }
                sink.put(tree)?;
                sink.align()?;
                for (id, p) in &pairs {
                    for x in [*id, p.lat_o, p.long_o, p.long_p, p.flags()] {
                        sink.put(&x.to_le_bytes())?;
                    }
                }
                Ok(())
            },
        )?;
        crate::store::sync_dir(dir)?;
        if let Some(parent) = dir.parent() {
            crate::store::sync_dir(parent)?;
        }
        Ok(())
    }

    /// The base of `snap`'s generation read from the files in `dir` (built for `ident`);
    /// with `verify`, every data checksum is checked too.
    pub(crate) fn read_files(
        dir: &Path,
        ident: &Identity,
        snap: &Snapshot,
        cfg: &GeoConfig,
        verify: bool,
    ) -> std::result::Result<GeoBase, (&'static str, Problem)> {
        let t0 = std::time::Instant::now();
        let open = |k: FileKind| {
            Mapped::open(&dir.join(k.file_name()), k, Some(ident), verify)
                .map(Arc::new)
                .map_err(|p| (k.file_name(), p))
        };
        let rt = open(FileKind::Rtree)?;
        let col = open(FileKind::Column)?;
        let bad = |m: &str| (persist::RTREE_FILE, Problem::Unusable(m.to_string()));
        let count = |i: usize| usize::try_from(rt.u64_at(persist::INDEX_PREFIX + 8 * i)).ok();
        let (Some(n), Some(m), Some(k), Some(t), Some(w)) =
            (count(0), count(1), count(2), count(3), count(4))
        else {
            return Err(bad("damaged index section"));
        };
        if rt.index().len() != persist::pad8(persist::INDEX_PREFIX + 40)
            || n as u64 != rt.header.rows
            || k != cfg.predicates.len()
        {
            return Err(bad("damaged index section"));
        }
        let sections = (|| {
            let skipped_at = n.checked_mul(32)?;
            let slots_at = skipped_at.checked_add(m.checked_mul(32)?)?;
            let tree_at = slots_at.checked_add(k.checked_mul(8)?)?;
            let pairs_at = persist::pad8(tree_at.checked_add(t)?);
            let end = pairs_at.checked_add(w.checked_mul(PAIR_BYTES)?)?;
            (end == rt.data().len()).then_some((skipped_at, slots_at, tree_at, pairs_at))
        })();
        let Some((skipped_at, slots_at, tree_at, pairs_at)) = sections else {
            return Err(bad("data sections have the wrong length"));
        };
        let rows = Rows::mapped(rt.clone(), 0, n).ok_or_else(|| bad("misaligned rows"))?;
        let skipped: Vec<Row> = rt.data()[skipped_at..slots_at]
            .as_chunks::<32>()
            .0
            .iter()
            .map(|b| {
                let x = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
                Row {
                    s: x(0),
                    p: x(1),
                    o: x(2),
                    g: x(3),
                }
            })
            .collect();
        let slot_rows: Vec<u64> = rt.data()[slots_at..tree_at]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| u64::from_le_bytes(*b))
            .collect();
        let tree = match n {
            0 if t == 0 => None,
            0 => return Err(bad("a tree without rows")),
            _ => Some(
                PackedTree::mapped(rt.clone(), tree_at, t, n as u64)
                    .ok_or_else(|| bad("damaged tree"))?,
            ),
        };
        let mut pairs = Vec::with_capacity(w);
        for b in rt.data()[pairs_at..].as_chunks::<PAIR_BYTES>().0 {
            let x = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
            if !wgs84::is_pair(x(0)) || x(4) > 3 {
                return Err(bad("damaged W3C Basic Geo pairs"));
            }
            let pair = Pair {
                lat_o: x(1),
                long_o: x(2),
                long_p: x(3),
                lat_base: x(4) & 1 != 0,
                long_base: x(4) & 2 != 0,
            };
            pairs.push((x(0), pair, None));
        }
        let files_bytes = rt.len() + col.len();
        let mut column =
            Column::read(col).map_err(|m| (persist::COLUMN_FILE, Problem::Unusable(m)))?;
        if pairs
            .iter()
            .any(|(id, _, _)| column.base_entry(*id).is_none())
        {
            return Err(bad("W3C Basic Geo pairs without their points"));
        }
        column.add_base_pairs(pairs);
        Ok(GeoBase {
            generation: snap.generation.uid,
            generation_name: snap.generation.name.clone(),
            rows,
            skipped,
            tree,
            column,
            slot_rows,
            built_ms: t0.elapsed().as_secs_f64() * 1000.0,
            pair_rows: w as u64,
            opened: true,
            files_bytes,
        })
    }
}

/// How a build is told its budget, reports progress and learns it should stop.
pub(crate) struct BuildCtl<'a> {
    pub budget: u64,
    /// progress 0–1 as `f32` bits
    pub progress: &'a AtomicU32,
    /// true: give up (the store closes, or the build was superseded)
    pub cancel: &'a (dyn Fn() -> bool + Sync),
}

fn set_progress(p: &AtomicU32, x: f32) {
    p.store(x.to_bits(), Ordering::Relaxed);
}

fn over_budget(limit: u64, requested: u64) -> Error {
    Error::BudgetExceeded(crate::Budget {
        kind: crate::BudgetKind::Memory,
        limit,
        requested,
    })
}

/// Build the base of `snap`'s generation: rows of the configured predicates in the
/// generation's own index (not the delta), their literals parsed in parallel blocks
/// (or taken from the previous generation's column, `reuse`), and the packed tree.
/// Fails with [`Error::BudgetExceeded`] past the budget and [`Error::Cancelled`] when
/// told to stop.
pub(crate) fn build_base(
    snap: &Snapshot,
    cfg: &GeoConfig,
    lookup: &Lookup,
    ctl: &BuildCtl<'_>,
    reuse: Option<&Reuse<'_>>,
) -> Result<GeoBase> {
    let t0 = std::time::Instant::now();
    set_progress(ctl.progress, 0.0);
    let mut base = GeoBase::empty(snap, cfg);
    let perm = snap.perm(Perm::Pso);
    // the base vocabulary ids of the configured predicates
    let mut preds: Vec<(u64, u16)> = Vec::new();
    for (i, iri) in cfg.predicates.iter().enumerate() {
        if let Ok(v) = snap.generation.vocab.find(&crate::id::iri_key(iri)) {
            preds.push((Id::vocab(v).0, i as u16));
        }
    }
    let mut n = 0u64;
    for &(p, _) in &preds {
        n += perm.count(&snap.cache, &[p])?;
    }
    // rows alone must fit (the parsed geometries are checked as they come)
    let row_bytes = std::mem::size_of::<Row>() as u64;
    if n * row_bytes > ctl.budget {
        return Err(over_budget(ctl.budget, n * row_bytes));
    }
    let mut rows: Vec<Row> = Vec::with_capacity(n as usize);
    for &(p, _) in &preds {
        perm.for_each_range(&snap.cache, &[p], |b, s, e| {
            if (ctl.cancel)() {
                return Err(Error::Cancelled);
            }
            for i in s..e {
                let k = b.key(i);
                if lookup.graph(Id(k[3]), snap, cfg) {
                    rows.push(Row {
                        s: k[1],
                        p: k[0],
                        o: k[2],
                        g: k[3],
                    });
                }
            }
            Ok(())
        })?;
    }
    set_progress(ctl.progress, 0.1);
    let mut objs: Vec<u64> = rows
        .iter()
        .map(|r| r.o)
        .filter(|&o| Id(o).tag() == Tag::Vocab)
        .collect();
    objs.sort_unstable();
    objs.dedup();
    let total = objs.len().max(1) as f32;
    let done = AtomicU64::new(0);
    let used = AtomicU64::new(rows.len() as u64 * row_bytes);
    let mut column = Column::build(snap, &objs, cfg, reuse, &|k, bytes| {
        if (ctl.cancel)() {
            return Err(Error::Cancelled);
        }
        let u = used.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if u > ctl.budget {
            return Err(over_budget(ctl.budget, u));
        }
        let d = done.fetch_add(k as u64, Ordering::Relaxed) + k as u64;
        set_progress(ctl.progress, 0.1 + 0.8 * (d as f32 / total));
        Ok(())
    })?;
    if cfg.wgs84 {
        let (pair_rows, pairs) = base_pairs(snap, cfg, lookup, ctl)?;
        base.pair_rows = pair_rows.len() as u64;
        column.add_base_pairs(pairs);
        rows.extend(pair_rows);
    }
    let mut boxes: Vec<[f32; 4]> = Vec::with_capacity(rows.len());
    let mut skipped = Vec::new();
    rows.retain(|r| match column.base_entry(r.o) {
        Some(e) => {
            boxes.push(e.bbox());
            true
        }
        None => {
            if column.get(r.o).is_some_and(|s| s.rechecked()) {
                skipped.push(*r);
            }
            false
        }
    });
    for r in &rows {
        if let Some(&(_, slot)) = preds.iter().find(|(p, _)| *p == r.p) {
            base.slot_rows[slot as usize] += 1;
        }
    }
    if (ctl.cancel)() {
        return Err(Error::Cancelled);
    }
    base.tree = PackedTree::pack(boxes.into_iter());
    rows.shrink_to_fit();
    base.rows = Rows::Owned(rows);
    base.skipped = skipped;
    base.column = column;
    let need = base.bytes();
    if need > ctl.budget {
        return Err(over_budget(ctl.budget, need));
    }
    base.built_ms = t0.elapsed().as_secs_f64() * 1000.0;
    set_progress(ctl.progress, 1.0);
    Ok(base)
}

/// Bytes of a W3C Basic Geo pair in `rtree.spkg`: id, `lat` and `long` objects, the
/// `long` predicate, flags.
const PAIR_BYTES: usize = 40;

/// The W3C Basic Geo points of `snap`'s generation base: their rows (`lat` the
/// predicate, the point's id the object) and pairs, numbered in row order.
#[allow(clippy::type_complexity)]
fn base_pairs(
    snap: &Snapshot,
    cfg: &GeoConfig,
    lookup: &Lookup,
    ctl: &BuildCtl<'_>,
) -> Result<(Vec<Row>, Vec<(u64, Pair, Option<Arc<ColumnEntry>>)>)> {
    let vocab = &snap.generation.vocab;
    let (Ok(lat), Ok(long)) = (
        vocab.find(&crate::id::iri_key(wgs84::LAT)),
        vocab.find(&crate::id::iri_key(wgs84::LONG)),
    ) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (lat_p, long_p) = (Id::vocab(lat).0, Id::vocab(long).0);
    let perm = snap.perm(Perm::Pso);
    // `[s, g, o]` of a predicate's quads in scope, sorted
    let read = |p: u64| -> Result<Vec<[u64; 3]>> {
        let mut v = Vec::new();
        perm.for_each_range(&snap.cache, &[p], |b, s, e| {
            if (ctl.cancel)() {
                return Err(Error::Cancelled);
            }
            for i in s..e {
                let k = b.key(i);
                if lookup.graph(Id(k[3]), snap, cfg) {
                    v.push([k[1], k[3], k[2]]);
                }
            }
            Ok(())
        })?;
        v.sort_unstable();
        Ok(v)
    };
    let (lats, longs) = (read(lat_p)?, read(long_p)?);
    // the values of the objects
    let mut objs: Vec<u64> = lats.iter().chain(&longs).map(|x| x[2]).collect();
    objs.sort_unstable();
    objs.dedup();
    let mut values: FxHashMap<u64, f64> = FxHashMap::default();
    let payloads: Vec<u64> = objs
        .iter()
        .filter(|&&o| Id(o).tag() == Tag::Vocab)
        .map(|&o| Id(o).payload())
        .collect();
    vocab.get_sorted(&payloads, |pl, key| {
        if let Some(x) = wgs84::number_of_key(key) {
            values.insert(Id::vocab(pl).0, x);
        }
    });
    for &o in objs.iter().filter(|&&o| Id(o).tag() != Tag::Vocab) {
        if let Some(x) = wgs84::number(snap, Id(o)) {
            values.insert(o, x);
        }
    }
    // the cross product of each subject's lats and longs in one graph
    let mut rows = Vec::new();
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < lats.len() && j < longs.len() {
        let (a, b) = ((lats[i][0], lats[i][1]), (longs[j][0], longs[j][1]));
        if a != b {
            if a < b {
                i += 1;
            } else {
                j += 1;
            }
            continue;
        }
        let ie = i + lats[i..].iter().take_while(|x| (x[0], x[1]) == a).count();
        let je = j + longs[j..].iter().take_while(|x| (x[0], x[1]) == a).count();
        for la in &lats[i..ie] {
            for lo in &longs[j..je] {
                let (Some(&y), Some(&x)) = (values.get(&la[2]), values.get(&lo[2])) else {
                    continue;
                };
                let Some(e) = wgs84::point(y, x) else {
                    continue;
                };
                let id = wgs84::pair_id(pairs.len() as u64);
                rows.push(Row {
                    s: a.0,
                    p: lat_p,
                    o: id,
                    g: a.1,
                });
                let pair = Pair {
                    lat_o: la[2],
                    long_o: lo[2],
                    long_p,
                    lat_base: true,
                    long_base: true,
                };
                pairs.push((id, pair, Some(e)));
            }
        }
        (i, j) = (ie, je);
    }
    Ok((rows, pairs))
}

/// The point rows the quad `q` (`[s, p, o, g]`, a `lat` or `long` quad inserted since
/// the base, visible in `snap`) makes with the other half of each pair visible there.
pub(crate) fn pair_rows(
    snap: &Snapshot,
    column: &Column,
    q: [u64; 4],
    lat: u64,
    long: u64,
) -> Result<Vec<TailRow>> {
    let is_lat = q[1] == lat;
    let ins = &snap.delta.ins[Perm::Pso.index()];
    let in_base = |p: u64, o: u64| !ins.contains(&[p, q[0], o, q[3]]);
    let mut out = Vec::new();
    for other in wgs84::objects(snap, q[0], if is_lat { long } else { lat }, q[3])? {
        let (lat_o, long_o) = if is_lat { (q[2], other) } else { (other, q[2]) };
        let made = column.commit_pair([q[0], q[3], lat_o, long_o], || {
            let e = wgs84::point(
                wgs84::number(snap, Id(lat_o))?,
                wgs84::number(snap, Id(long_o))?,
            )?;
            let pair = Pair {
                lat_o,
                long_o,
                long_p: long,
                lat_base: in_base(lat, lat_o),
                long_base: in_base(long, long_o),
            };
            Some((pair, e))
        });
        if let Some((id, entry)) = made {
            out.push(TailRow {
                row: Row {
                    s: q[0],
                    p: lat,
                    o: id,
                    g: q[3],
                },
                entry,
            });
        }
    }
    Ok(out)
}

/// Whether the overlay or tail row `r` is in `snap`: its quad inserted (a point: both
/// of its quads visible).
pub(crate) fn delta_row_live(snap: &Snapshot, column: &Column, r: &Row) -> bool {
    if wgs84::is_pair(r.o) {
        column.pair(r.o).is_some_and(|p| p.live(snap, r))
    } else {
        snap.delta.ins[Perm::Pso.index()].contains(&r.pso())
    }
}

/// Rows inserted by commits, in a packed tree.
pub(crate) struct Overlay {
    pub rows: Vec<TailRow>,
    pub tree: Option<PackedTree>,
    /// inserted rows whose literal was skipped but may still match
    /// ([`Slot::rechecked`])
    pub skipped: Vec<Row>,
}

impl Overlay {
    pub fn empty() -> Overlay {
        Overlay {
            rows: Vec::new(),
            tree: None,
            skipped: Vec::new(),
        }
    }

    /// The overlay of `rows` and the skipped rows `skipped` (each sorted, one per quad).
    pub fn build(rows: Vec<TailRow>, mut skipped: Vec<Row>) -> Overlay {
        skipped.sort_unstable();
        skipped.dedup();
        Overlay {
            skipped,
            ..Overlay::tree(rows)
        }
    }

    fn tree(mut rows: Vec<TailRow>) -> Overlay {
        rows.sort_unstable_by_key(|r| r.row);
        rows.dedup_by_key(|r| r.row);
        let tree = PackedTree::pack(
            rows.iter()
                .map(|r| r.entry.bbox())
                .collect::<Vec<_>>()
                .into_iter(),
        );
        Overlay {
            rows,
            tree,
            skipped: Vec::new(),
        }
    }

    pub fn bytes(&self) -> u64 {
        (self.rows.len() * std::mem::size_of::<TailRow>()
            + self.skipped.len() * std::mem::size_of::<Row>()) as u64
            + tree_bytes(&self.tree)
    }
}

/// The overlay of every indexable inserted quad of `snap` (a scan of `delta.ins` for
/// the configured predicates), as at open and after a build.
pub(crate) fn overlay_of(
    snap: &Snapshot,
    cfg: &GeoConfig,
    base: &GeoBase,
    lookup: &Lookup,
) -> Overlay {
    lookup.resolve(snap, cfg);
    let ins = &snap.delta.ins[Perm::Pso.index()];
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    if ins.is_empty() {
        return Overlay::empty();
    }
    for (p, _) in lookup.indexed() {
        let lo = [p, 0, 0, 0];
        let hi = [p, u64::MAX, u64::MAX, u64::MAX];
        for k in ins.range(lo..=hi) {
            if !lookup.graph(Id(k[3]), snap, cfg) {
                continue;
            }
            let row = Row {
                s: k[1],
                p: k[0],
                o: k[2],
                g: k[3],
            };
            match base.column.get_or_classify(Id(k[2]), snap, cfg) {
                Slot::Geom(entry) => rows.push(TailRow { row, entry }),
                s if s.rechecked() => skipped.push(row),
                _ => {}
            }
        }
    }
    // W3C Basic Geo points with an inserted quad
    if cfg.wgs84
        && let Some((lat, long)) = wgs84::predicates(snap)
    {
        for p in [lat.0, long.0] {
            for k in ins.range([p, 0, 0, 0]..=[p, u64::MAX, u64::MAX, u64::MAX]) {
                if lookup.graph(Id(k[3]), snap, cfg)
                    && let Ok(r) =
                        pair_rows(snap, &base.column, [k[1], k[0], k[2], k[3]], lat.0, long.0)
                {
                    rows.extend(r);
                }
            }
        }
    }
    Overlay::build(rows, skipped)
}

/// The tail length past which the overlay tree is rebuilt.
pub(crate) fn tail_limit(overlay_rows: usize) -> usize {
    (overlay_rows / 8).max(4096)
}

/// What a view allows.
#[derive(Clone)]
pub(crate) enum ViewState {
    Ready,
    /// the base is being built: progress as `f32` bits
    Building(Arc<AtomicU32>),
    Failed,
    OverBudget,
    Txn,
    Historical,
}

/// The spatial index as one snapshot sees it (immutable).
#[derive(Clone)]
pub struct GeoView {
    pub config: Arc<GeoConfig>,
    /// +1 per enable, reconfiguration or rebuild (part of result-cache keys)
    pub epoch: u64,
    /// the `uid` of the generation the base belongs to
    pub generation: u64,
    /// the commit of the snapshot
    pub commit: u64,
    pub(crate) state: ViewState,
    pub(crate) lookup: Arc<Lookup>,
    /// set when ready
    pub(crate) base: Option<Arc<GeoBase>>,
    pub(crate) overlay: Arc<Overlay>,
    pub(crate) tail: imbl::Vector<TailRow>,
    /// skipped rows inserted since the overlay was built (as the tail)
    pub(crate) skipped: imbl::Vector<Row>,
}

impl GeoView {
    /// A view without a usable base (building, failed, over budget, …).
    pub(crate) fn pending(
        config: Arc<GeoConfig>,
        epoch: u64,
        snap: &Snapshot,
        lookup: Arc<Lookup>,
        state: ViewState,
    ) -> GeoView {
        GeoView {
            config,
            epoch,
            generation: snap.generation.uid,
            commit: snap.commit,
            state,
            lookup,
            base: None,
            overlay: Arc::new(Overlay::empty()),
            tail: imbl::Vector::new(),
            skipped: imbl::Vector::new(),
        }
    }

    /// A ready view of `base` and `overlay`.
    pub(crate) fn ready(
        config: Arc<GeoConfig>,
        epoch: u64,
        commit: u64,
        lookup: Arc<Lookup>,
        base: Arc<GeoBase>,
        overlay: Overlay,
    ) -> GeoView {
        GeoView {
            config,
            epoch,
            generation: base.generation,
            commit,
            state: ViewState::Ready,
            lookup,
            base: Some(base),
            overlay: Arc::new(overlay),
            tail: imbl::Vector::new(),
            skipped: imbl::Vector::new(),
        }
    }

    /// The same view in state `s`, without its rows.
    pub(crate) fn without_rows(&self, s: ViewState) -> GeoView {
        GeoView {
            state: s,
            base: None,
            overlay: Arc::new(Overlay::empty()),
            tail: imbl::Vector::new(),
            skipped: imbl::Vector::new(),
            ..self.clone()
        }
    }

    /// Whether (and why not) queries on this view can use the index.
    pub fn state(&self) -> IndexState {
        match &self.state {
            ViewState::Ready => IndexState::Ready,
            ViewState::Building(p) => {
                IndexState::Building(f32::from_bits(p.load(Ordering::Relaxed)))
            }
            ViewState::Failed => IndexState::Failed,
            ViewState::OverBudget => IndexState::OverBudget,
            ViewState::Txn => IndexState::Txn,
            ViewState::Historical => IndexState::Historical,
        }
    }

    /// The base and overlay when queries may use them.
    pub(crate) fn usable(&self) -> Option<&Arc<GeoBase>> {
        match self.state {
            ViewState::Ready => self.base.as_ref(),
            _ => None,
        }
    }

    /// The slot of an indexed predicate (`None`: the predicate is not indexed).
    pub fn predicate_slot(&self, p: Id) -> Option<u16> {
        self.lookup.known_slot(p)
    }

    /// Estimated rows of the predicates `preds` whose envelope intersects one of the
    /// CRS84 `windows`.
    pub fn estimate(&self, preds: &[u16], windows: &[[f64; 4]]) -> f64 {
        let Some(base) = self.usable() else {
            return 0.0;
        };
        let all: u64 = base.slot_rows.iter().sum();
        let frac = if preds.is_empty() || all == 0 {
            1.0
        } else {
            let mine: u64 = preds
                .iter()
                .filter_map(|&s| base.slot_rows.get(s as usize))
                .sum();
            mine as f64 / all as f64
        };
        let mut est = base.tree.as_ref().map_or(0.0, |t| t.estimate(windows)) * frac;
        if let Some(t) = &self.overlay.tree {
            est += t.estimate(windows) * frac;
        }
        est + self
            .tail
            .iter()
            .filter(|r| windows.iter().any(|w| intersects(&r.entry.bbox(), w)))
            .count() as f64
    }

    /// Height of the base tree (for costs).
    pub fn levels(&self) -> u32 {
        self.usable()
            .and_then(|b| b.tree.as_ref())
            .map_or(0, |t| t.num_levels() as u32)
    }

    /// The view of a transaction's uncommitted changes, which the index does not cover.
    pub fn for_txn(&self) -> GeoView {
        GeoView {
            state: ViewState::Txn,
            ..self.clone()
        }
    }

    /// Memory of the rows this view adds to its base (overlay and tail).
    pub(crate) fn overlay_bytes(&self) -> u64 {
        self.overlay.bytes()
            + (self.tail.len() * std::mem::size_of::<TailRow>()
                + self.skipped.len() * std::mem::size_of::<Row>()) as u64
    }

    /// Memory of the index as this view sees it.
    pub(crate) fn bytes(&self) -> u64 {
        self.base.as_ref().map_or(0, |b| b.bytes()) + self.overlay_bytes()
    }
}

/// The last build and its outcome, for the status.
#[derive(Default)]
pub(crate) struct BuildInfo {
    pub message: Option<String>,
    pub last_build: Option<GeoBuild>,
    /// the latest epoch whose build finished (published or not)
    pub finished: u64,
}

/// A dataset's spatial index (the store holds one while the index is enabled).
pub struct GeoIndex {
    pub(crate) config: Arc<GeoConfig>,
    pub(crate) epoch: AtomicU64,
    pub(crate) budget: u64,
    /// progress of the running build (`f32` bits)
    pub(crate) progress: Arc<AtomicU32>,
    pub(crate) info: Mutex<BuildInfo>,
    done: Condvar,
    /// replaced or disabled: running builds publish nothing
    pub(crate) retired: AtomicBool,
    /// test hook: background builds wait before they publish
    pub(crate) paused: AtomicBool,
    /// test hook: the next commit-path update fails
    #[cfg(any(test, feature = "failpoints"))]
    pub(crate) fail_next: AtomicBool,
}

impl GeoIndex {
    pub(crate) fn new(config: GeoConfig, epoch: u64, budget: u64) -> GeoIndex {
        GeoIndex {
            config: Arc::new(config),
            epoch: AtomicU64::new(epoch),
            budget,
            progress: Arc::new(AtomicU32::new(0)),
            info: Mutex::new(BuildInfo::default()),
            done: Condvar::new(),
            retired: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            #[cfg(any(test, feature = "failpoints"))]
            fail_next: AtomicBool::new(false),
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Record that the build of `epoch` is over (`message`: why it was not published).
    pub(crate) fn finish(&self, epoch: u64, message: Option<String>, last: Option<GeoBuild>) {
        let mut i = self.info.lock();
        if epoch >= i.finished {
            i.finished = epoch;
            i.message = message;
            if last.is_some() {
                i.last_build = last;
            }
        }
        self.done.notify_all();
    }

    /// Set the status message (failures of the commit path).
    pub(crate) fn note(&self, message: String) {
        self.info.lock().message = Some(message);
    }

    /// Wait until the build of `epoch` (or a later one) is over.
    pub(crate) fn wait(&self, epoch: u64) {
        let mut i = self.info.lock();
        while i.finished < epoch && !self.retired.load(Ordering::SeqCst) {
            self.done
                .wait_for(&mut i, std::time::Duration::from_millis(50));
        }
    }

    /// Wake waiters (the index was retired).
    pub(crate) fn wake(&self) {
        let _i = self.info.lock();
        self.done.notify_all();
    }

    /// The status as of `snap`.
    pub(crate) fn status(&self, snap: &Snapshot) -> GeoStatus {
        let view = snap.geo.as_deref();
        let state = view.map_or(IndexState::Building(0.0), GeoView::state);
        let info = self.info.lock();
        let base = view.and_then(|v| v.base.as_ref());
        let counts = base.map(|b| b.column.counts()).unwrap_or_default();
        let crs = counts
            .crs
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let Counts {
            literals,
            malformed,
            unknown_crs,
            too_large,
            empty,
            ..
        } = counts;
        GeoStatus {
            enabled: true,
            state: match state {
                // a snapshot's own states never reach the live status
                IndexState::Txn | IndexState::Historical | IndexState::Off => {
                    IndexState::Failed.as_str().into()
                }
                s => s.as_str().into(),
            },
            progress: match state {
                IndexState::Building(p) => Some(p),
                _ => None,
            },
            message: info.message.clone(),
            generation: base
                .map(|b| b.generation_name.clone())
                .unwrap_or_else(|| snap.generation.name.clone()),
            commit: snap.commit,
            rows: GeoRows {
                base: base.map_or(0, |b| b.rows.len() as u64),
                overlay: view.map_or(0, |v| v.overlay.rows.len() as u64),
                tail: view.map_or(0, |v| v.tail.len() as u64),
                wgs84: base.map_or(0, |b| b.pair_rows)
                    + view.map_or(0, |v| {
                        v.overlay
                            .rows
                            .iter()
                            .chain(v.tail.iter())
                            .filter(|r| wgs84::is_pair(r.row.o))
                            .count() as u64
                    }),
            },
            literals,
            skipped: GeoSkipped {
                malformed,
                unknown_crs,
                too_large,
                empty,
            },
            crs,
            memory: GeoMemory {
                tree_bytes: base.map_or(0, |b| b.tree_bytes())
                    + view.map_or(0, |v| tree_bytes(&v.overlay.tree)),
                geometry_bytes: base.map_or(0, |b| b.column.bytes()),
                overlay_bytes: view.map_or(0, |v| v.overlay_bytes()),
                budget_bytes: self.budget,
                mapped_bytes: base.map_or(0, |b| b.files_bytes),
            },
            config: (*self.config).clone(),
            format_version: FORMAT_VERSION,
            last_build: info.last_build.clone(),
            files: base.filter(|b| b.files_bytes > 0).map(|b| GeoFiles {
                bytes: b.files_bytes,
                opened: b.opened,
            }),
        }
    }
}

/// The spatial data of one generation. The base, its column and the overlay live in the
/// views of the generation's snapshots (a reconfiguration replaces them for the same
/// generation); what is kept here guards the generation's index files.
#[derive(Default)]
pub struct GenerationGeo {
    /// held while the files are written or removed; true once the generation is
    /// replaced (its directory may be removed at any time then, so nothing writes
    /// there any more)
    files: Mutex<bool>,
}

impl GenerationGeo {
    /// The generation is being replaced: wait for a running write of its files, and
    /// let none start.
    pub(crate) fn retire(&self) {
        *self.files.lock() = true;
    }

    /// Run `f` (a write or removal of the generation's files) unless the generation was
    /// replaced; no replacement completes meanwhile.
    pub(crate) fn with_files<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        let retired = self.files.lock();
        (!*retired).then(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_bound_the_true_count() {
        // a 100 × 100 grid of unit boxes
        let mut boxes = Vec::new();
        for i in 0..100 {
            for j in 0..100 {
                let (x, y) = (i as f32, j as f32);
                boxes.push([x, y, x + 0.5, y + 0.5]);
            }
        }
        let tree = PackedTree::pack(boxes.clone().into_iter()).unwrap();
        let w = [10.0, 10.0, 19.9, 19.9];
        let exact = boxes.iter().filter(|b| intersects(b, &w)).count() as f64;
        assert_eq!(exact, 100.0);
        let est = tree.estimate(&[w]);
        assert!(est >= exact && est <= 10_000.0 / 4.0, "{est}");
        assert_eq!(tree.estimate(&[[500.0, 500.0, 600.0, 600.0]]), 0.0);
        let world = tree.estimate(&[[-1000.0, -1000.0, 1000.0, 1000.0]]);
        assert_eq!(world, 10_000.0);
        assert!(
            PackedTree::pack(
                std::iter::empty::<[f32; 4]>()
                    .collect::<Vec<_>>()
                    .into_iter()
            )
            .is_none()
        );
    }
}
