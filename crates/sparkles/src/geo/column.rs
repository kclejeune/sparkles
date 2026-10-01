//! The geometry column of an index build: literal id → parsed geometry and its CRS84
//! envelope, for the base literals of indexed predicates (parsed in parallel blocks
//! when the base is built, or read from the generation's `column.spkg`) and for the
//! literals the commit path met since (parsed once, behind a lock).
//!
//! Literals that are not indexed are remembered too, with the reason, so the commit path
//! parses each literal once and the status can count them.
//!
//! An entry read from a file keeps only its envelope and its place in the file; the
//! geometry is decoded from its record the first time it is asked for (no text is
//! parsed), and kept.

use super::GeomRef;
use super::config::GeoConfig;
use super::crs::{CRS84, CRS84_IRI, CrsRef};
use super::geom::GeomError;
use super::persist::{self, FileKind, Identity, Mapped};
use super::vocab::{GEOJSON_LITERAL, WKT_LITERAL};
use super::wgs84::{self, Pair};
use crate::id::{Id, KEY_SEP, Tag};
use crate::store::Snapshot;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// A geometry literal of the column.
pub struct ColumnEntry {
    /// the CRS84 envelope, rounded outward to `f32`
    bbox: [f32; 4],
    geom: Source,
}

/// Where an entry's geometry is.
enum Source {
    Parsed(GeomRef),
    /// entry `i` of a column file, decoded on first use
    Mapped {
        file: Arc<ColumnFile>,
        i: u32,
        cell: OnceLock<Option<GeomRef>>,
    },
}

impl ColumnEntry {
    /// An entry for `geom`, whose CRS84 envelope is `bbox84`.
    pub(crate) fn new(geom: GeomRef, bbox84: [f64; 4]) -> ColumnEntry {
        ColumnEntry {
            bbox: round_out(bbox84),
            geom: Source::Parsed(geom),
        }
    }

    /// The envelope in CRS84, rounded outward to `f32`.
    pub fn bbox84(&self) -> [f64; 4] {
        self.bbox.map(f64::from)
    }

    /// The envelope as the trees hold it.
    pub(crate) fn bbox(&self) -> [f32; 4] {
        self.bbox
    }

    /// The parsed geometry.
    pub fn geom(&self, snap: &Snapshot) -> Result<GeomRef, GeomError> {
        let _ = snap;
        match &self.geom {
            Source::Parsed(g) => Ok(g.clone()),
            Source::Mapped { file, i, cell } => cell
                .get_or_init(|| file.decode(*i as usize))
                .clone()
                .ok_or_else(|| GeomError::new("damaged spatial index record")),
        }
    }

    /// The geometry's CRS (without decoding it).
    pub(crate) fn crs(&self) -> CrsRef {
        match &self.geom {
            Source::Parsed(g) => g.crs.clone(),
            Source::Mapped { file, i, .. } => file.crs_of(*i as usize),
        }
    }

    /// Estimated memory of the entry and its geometry (an entry of a file: the entry
    /// alone; geometries decoded from files are counted by the file).
    pub(crate) fn bytes(&self) -> u64 {
        let g = match &self.geom {
            Source::Parsed(g) => g.mem_size(),
            Source::Mapped { .. } => 0,
        };
        (std::mem::size_of::<ColumnEntry>() + g) as u64
    }

    /// Vertices of the geometry (without decoding it).
    fn vertices(&self) -> u32 {
        match &self.geom {
            Source::Parsed(g) => g.vertices,
            Source::Mapped { file, i, .. } => file.entry(*i as usize).vertices,
        }
    }

    /// Bytes of the entry's record in a column file.
    fn record_len(&self) -> usize {
        match &self.geom {
            Source::Parsed(g) => persist::encoded_len(g),
            Source::Mapped { file, i, .. } => file.record(*i as usize).map_or(0, <[u8]>::len),
        }
    }
}

/// The smallest `f32` box that contains the `f64` box `b`.
pub(crate) fn round_out(b: [f64; 4]) -> [f32; 4] {
    let down = |x: f64| {
        let f = x as f32;
        if f64::from(f) > x { f.next_down() } else { f }
    };
    let up = |x: f64| {
        let f = x as f32;
        if f64::from(f) < x { f.next_up() } else { f }
    };
    [down(b[0]), down(b[1]), up(b[2]), up(b[3])]
}

/// Why a geometry literal of an indexed predicate is not indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    /// does not parse
    Malformed,
    /// a CRS this build cannot place on the globe (the IRI, for the status)
    UnknownCrs(Arc<str>),
    /// longer than `maxGeometryBytes` (functions still evaluate it)
    TooLarge,
    /// more than `maxVertices` vertices (functions refuse it too)
    TooComplex,
    /// an empty geometry
    Empty,
}

/// What the column knows of a literal.
#[derive(Clone)]
pub(crate) enum Slot {
    Geom(Arc<ColumnEntry>),
    Skipped(Skip),
    /// not a geometry literal (another datatype, an IRI, a blank node)
    Other,
}

impl Slot {
    /// A skipped literal that a `geof:` function could still find in a relation or
    /// within a distance of a constant: one too long to index, or one in a built-in CRS
    /// whose envelope has no place in longitude and latitude. Searches hand its rows
    /// out as candidates whatever the window, so that a pushed filter answers as the
    /// plain one does. The other skipped literals never pass such a test: a malformed
    /// literal or one with too many vertices is a type error for the functions too, an
    /// empty geometry intersects nothing and has no distance, and an unknown CRS has no
    /// common CRS with a constant the index can place.
    pub fn rechecked(&self) -> bool {
        match self {
            Slot::Skipped(Skip::TooLarge) => true,
            Slot::Skipped(Skip::UnknownCrs(iri)) => super::crs::lookup(iri).is_some(),
            _ => false,
        }
    }
}

/// The IRI of a CRS, for the status.
fn crs_iri(c: &CrsRef) -> Arc<str> {
    match c {
        CrsRef::Unknown(iri) => iri.clone(),
        CrsRef::Known(_) => c.iri().into(),
    }
}

/// Classify the literal with vocabulary key `key` under the limits of `cfg`.
pub(crate) fn classify(key: &[u8], cfg: &GeoConfig) -> Slot {
    classify_with(key, cfg, false)
}

/// The geometry of a [`Slot::rechecked`] literal, parsed as functions parse it (no
/// length limit); an envelope off the globe becomes the whole world, so every window
/// meets it.
pub(crate) fn recheck(key: &[u8], cfg: &GeoConfig) -> Option<Arc<ColumnEntry>> {
    match classify_with(key, cfg, true) {
        Slot::Geom(e) => Some(e),
        _ => None,
    }
}

fn classify_with(key: &[u8], cfg: &GeoConfig, recheck: bool) -> Slot {
    if key.first() != Some(&b'"') {
        return Slot::Other;
    }
    let Some(sep) = key.iter().rposition(|&b| b == KEY_SEP) else {
        return Slot::Other;
    };
    let suffix = &key[sep + 1..];
    let Some(dt) = suffix.strip_prefix(b"^") else {
        return Slot::Other;
    };
    let dt = if dt == WKT_LITERAL.as_bytes() {
        WKT_LITERAL
    } else if dt == GEOJSON_LITERAL.as_bytes() {
        GEOJSON_LITERAL
    } else {
        return Slot::Other;
    };
    let lex = &key[1..sep];
    if lex.len() > cfg.max_geometry_bytes && !recheck {
        return Slot::Skipped(Skip::TooLarge);
    }
    let Ok(lex) = std::str::from_utf8(lex) else {
        return Slot::Skipped(Skip::Malformed);
    };
    // the parser is not trusted with arbitrary input: a panic is a malformed literal
    let parsed = std::panic::catch_unwind(|| super::parse::parse(lex, dt));
    let g = match parsed {
        Ok(Ok(g)) => g,
        _ => return Slot::Skipped(Skip::Malformed),
    };
    if g.vertices > cfg.max_vertices {
        return Slot::Skipped(Skip::TooComplex);
    }
    if g.empty {
        return Slot::Skipped(Skip::Empty);
    }
    if let CrsRef::Unknown(iri) = &g.crs {
        return Slot::Skipped(Skip::UnknownCrs(iri.clone()));
    }
    match g.bbox84() {
        Some(b) if b.iter().all(|x| x.is_finite()) => {
            Slot::Geom(Arc::new(ColumnEntry::new(Arc::new(g), b)))
        }
        _ if recheck => Slot::Geom(Arc::new(ColumnEntry::new(
            Arc::new(g),
            [-180.0, -90.0, 180.0, 90.0],
        ))),
        _ => Slot::Skipped(Skip::UnknownCrs(crs_iri(&g.crs))),
    }
}

/// Literal counts of the status: indexed literals, skipped ones by reason, literals per
/// CRS (every parsed, non-empty literal, unknown CRSs included), and memory.
#[derive(Clone, Debug, Default)]
pub(crate) struct Counts {
    pub literals: u64,
    pub malformed: u64,
    pub unknown_crs: u64,
    pub too_large: u64,
    pub empty: u64,
    pub crs: FxHashMap<Arc<str>, u64>,
    pub bytes: u64,
}

impl Counts {
    pub fn add(&mut self, s: &Slot) {
        match s {
            Slot::Geom(e) => {
                self.literals += 1;
                self.bytes += e.bytes();
                self.crs_seen(&e.crs());
            }
            Slot::Skipped(Skip::Malformed) => self.malformed += 1,
            Slot::Skipped(Skip::TooLarge | Skip::TooComplex) => self.too_large += 1,
            Slot::Skipped(Skip::Empty) => self.empty += 1,
            Slot::Skipped(Skip::UnknownCrs(iri)) => {
                self.unknown_crs += 1;
                *self.crs.entry(iri.clone()).or_default() += 1;
            }
            Slot::Other => {}
        }
    }

    fn crs_seen(&mut self, c: &CrsRef) {
        if let CrsRef::Known(id) = c
            && *id == CRS84
            && let Some(n) = self.crs.get_mut(CRS84_IRI)
        {
            *n += 1;
            return;
        }
        *self.crs.entry(crs_iri(c)).or_default() += 1;
    }

    pub fn merge(&mut self, o: &Counts) {
        self.literals += o.literals;
        self.malformed += o.malformed;
        self.unknown_crs += o.unknown_crs;
        self.too_large += o.too_large;
        self.empty += o.empty;
        self.bytes += o.bytes;
        for (k, v) in &o.crs {
            *self.crs.entry(k.clone()).or_default() += v;
        }
    }
}

/// The literals the commit path classified since the build.
#[derive(Default)]
struct Extra {
    map: FxHashMap<u64, Slot>,
    counts: Counts,
    /// W3C Basic Geo points met by commits, and their ids by `[s, g, lat, long]`
    pairs: FxHashMap<u64, Pair>,
    pair_ids: FxHashMap<[u64; 4], u64>,
}

/// The geometry column of one build (one generation and configuration).
pub(crate) struct Column {
    /// the base literals of the indexed predicates (immutable after the build)
    base: FxHashMap<u64, Slot>,
    base_counts: Counts,
    /// literals met by commits since: delta literals, and base literals that were not
    /// objects of indexed predicates in the base. Ids are stable within a generation, so
    /// entries are never removed.
    extra: RwLock<Extra>,
    /// the file the base literals were read from
    file: Option<Arc<ColumnFile>>,
    /// the W3C Basic Geo points of the base rows (their geometries are in `base`)
    pairs: FxHashMap<u64, Pair>,
    /// base literals the build parsed, and those it took from the previous generation
    pub parsed: u64,
    pub reused: u64,
}

/// What a build may take from the previous generation: its snapshot (to find a literal's
/// id there by its key) and its column, built under the same configuration.
pub(crate) struct Reuse<'a> {
    pub snap: &'a Snapshot,
    pub column: &'a Column,
}

/// Distinct literals classified per parallel block of a build.
const BLOCK: usize = 4096;

impl Column {
    pub fn empty() -> Column {
        Column {
            base: FxHashMap::default(),
            base_counts: Counts::default(),
            extra: RwLock::new(Extra::default()),
            file: None,
            pairs: FxHashMap::default(),
            parsed: 0,
            reused: 0,
        }
    }

    /// Classify the distinct base-vocabulary ids `ids` (sorted) of `snap`'s generation
    /// in parallel blocks; a literal the previous generation's column holds (`reuse`)
    /// is taken from it instead of being parsed again. `step` is told the number of
    /// literals done after each block and returns an error to stop (budget,
    /// cancellation).
    pub fn build(
        snap: &Snapshot,
        ids: &[u64],
        cfg: &GeoConfig,
        reuse: Option<&Reuse<'_>>,
        step: &(dyn Fn(usize, u64) -> crate::error::Result<()> + Sync),
    ) -> crate::error::Result<Column> {
        use rayon::prelude::*;
        let vocab = &snap.generation.vocab;
        let reused = AtomicU64::new(0);
        let blocks: Vec<Vec<(u64, Slot)>> = ids
            .par_chunks(BLOCK)
            .map(|chunk| {
                let payloads: Vec<u64> = chunk.iter().map(|&o| Id(o).payload()).collect();
                let mut out = Vec::with_capacity(chunk.len());
                let mut n = 0;
                vocab.get_sorted(&payloads, |pl, key| {
                    let prev = reuse.and_then(|r| r.column.get(r.snap.lookup_key(key)?.0));
                    if prev.is_some() {
                        n += 1;
                    }
                    let slot = prev.unwrap_or_else(|| classify(key, cfg));
                    out.push((Id::vocab(pl).0, slot));
                });
                reused.fetch_add(n, Ordering::Relaxed);
                let bytes = out
                    .iter()
                    .map(|(_, s)| match s {
                        Slot::Geom(e) => e.bytes(),
                        _ => 0,
                    })
                    .sum();
                step(chunk.len(), bytes)?;
                Ok(out)
            })
            .collect::<crate::error::Result<_>>()?;
        let mut base = FxHashMap::default();
        base.reserve(ids.len());
        let mut counts = Counts::default();
        for (id, s) in blocks.into_iter().flatten() {
            counts.add(&s);
            base.insert(id, s);
        }
        let reused = reused.into_inner();
        Ok(Column {
            base,
            base_counts: counts,
            extra: RwLock::new(Extra::default()),
            file: None,
            pairs: FxHashMap::default(),
            parsed: ids.len() as u64 - reused,
            reused,
        })
    }

    /// The entry of a literal classified before (base or commit path).
    pub fn get(&self, o: u64) -> Option<Slot> {
        if let Some(s) = self.base.get(&o) {
            return Some(s.clone());
        }
        self.extra.read().map.get(&o).cloned()
    }

    /// The indexed geometry of a base literal (no lock).
    pub fn base_entry(&self, o: u64) -> Option<&Arc<ColumnEntry>> {
        match self.base.get(&o) {
            Some(Slot::Geom(e)) => Some(e),
            _ => None,
        }
    }

    /// The geometry of object `o` of `snap`, classified now (and remembered) if the
    /// column has not seen it.
    pub fn get_or_classify(&self, o: Id, snap: &Snapshot, cfg: &GeoConfig) -> Slot {
        if !matches!(o.tag(), Tag::Vocab | Tag::Delta) {
            return Slot::Other;
        }
        if let Some(s) = self.get(o.0) {
            return s;
        }
        let s = match snap.key(o) {
            Some(k) => classify(&k, cfg),
            None => Slot::Other,
        };
        let mut x = self.extra.write();
        if let Some(prev) = x.map.get(&o.0) {
            return prev.clone();
        }
        x.counts.add(&s);
        x.map.insert(o.0, s.clone());
        s
    }

    /// Add the W3C Basic Geo points of the base: their pairs, and their geometries
    /// (`None`: the column read them from its file already).
    pub fn add_base_pairs(&mut self, pairs: Vec<(u64, Pair, Option<Arc<ColumnEntry>>)>) {
        self.pairs.reserve(pairs.len());
        for (id, p, e) in pairs {
            if let Some(e) = e {
                self.base.insert(id, Slot::Geom(e));
            }
            self.pairs.insert(id, p);
        }
    }

    /// The base's W3C Basic Geo points, by id.
    pub fn base_pairs(&self) -> Vec<(u64, Pair)> {
        let mut v: Vec<(u64, Pair)> = self.pairs.iter().map(|(&k, &p)| (k, p)).collect();
        v.sort_unstable_by_key(|x| x.0);
        v
    }

    /// The pair of the point `o` (base or commit path).
    pub fn pair(&self, o: u64) -> Option<Pair> {
        if let Some(p) = self.pairs.get(&o) {
            return Some(*p);
        }
        self.extra.read().pairs.get(&o).copied()
    }

    /// The point of subject `s`, graph `g` and the objects `lat`, `long` met by a commit:
    /// its id and geometry, made by `make` the first time (`None`: not a point).
    pub fn commit_pair(
        &self,
        key: [u64; 4],
        make: impl FnOnce() -> Option<(Pair, Arc<ColumnEntry>)>,
    ) -> Option<(u64, Arc<ColumnEntry>)> {
        let entry = |x: &Extra, id: u64| match x.map.get(&id) {
            Some(Slot::Geom(e)) => Some((id, e.clone())),
            _ => None,
        };
        if let Some(&id) = self.extra.read().pair_ids.get(&key) {
            return entry(&self.extra.read(), id);
        }
        let (pair, e) = make()?;
        let mut x = self.extra.write();
        if let Some(&id) = x.pair_ids.get(&key) {
            return entry(&x, id);
        }
        let id = wgs84::pair_id((self.pairs.len() + x.pairs.len()) as u64);
        x.map.insert(id, Slot::Geom(e.clone()));
        x.pairs.insert(id, pair);
        x.pair_ids.insert(key, id);
        Some((id, e))
    }

    /// Counts over every literal classified so far.
    pub fn counts(&self) -> Counts {
        let mut c = self.base_counts.clone();
        c.merge(&self.extra.read().counts);
        c
    }

    /// Estimated memory of the column (geometries decoded from its file included).
    pub fn bytes(&self) -> u64 {
        let slots = (self.base.len() + self.extra.read().map.len()) as u64;
        let decoded = self
            .file
            .as_ref()
            .map_or(0, |f| f.decoded.load(Ordering::Relaxed));
        self.base_counts.bytes + self.extra.read().counts.bytes + slots * 32 + decoded
    }

    /// Write the base literals to `path` (`column.spkg`), for the generation and
    /// configuration `ident`.
    pub fn write(&self, path: &Path, ident: &Identity) -> crate::error::Result<()> {
        let mut ids: Vec<u64> = self.base.keys().copied().collect();
        ids.sort_unstable();
        // the CRS table, and each entry's place in the data
        let mut crs: Vec<Arc<str>> = Vec::new();
        let mut crs_ix: FxHashMap<Arc<str>, u32> = FxHashMap::default();
        let mut ix = |iri: &str| -> u32 {
            if let Some(&i) = crs_ix.get(iri) {
                return i;
            }
            let a: Arc<str> = iri.into();
            crs.push(a.clone());
            crs_ix.insert(a, crs.len() as u32 - 1);
            crs.len() as u32 - 1
        };
        let mut entries = Vec::with_capacity(ids.len() * ENTRY);
        let mut off = 0u64;
        for &o in &ids {
            let (kind, crs_i, bbox, len, vertices) = match &self.base[&o] {
                Slot::Geom(e) => (
                    KIND_GEOM,
                    ix(e.crs().iri()),
                    e.bbox,
                    e.record_len() as u32,
                    e.vertices(),
                ),
                Slot::Skipped(Skip::UnknownCrs(iri)) => (KIND_UNKNOWN_CRS, ix(iri), [0.0; 4], 0, 0),
                Slot::Skipped(s) => (skip_code(s), 0, [0.0; 4], 0, 0),
                Slot::Other => (KIND_OTHER, 0, [0.0; 4], 0, 0),
            };
            entries.extend_from_slice(&o.to_le_bytes());
            entries.extend_from_slice(&off.to_le_bytes());
            for x in bbox {
                entries.extend_from_slice(&x.to_le_bytes());
            }
            entries.extend_from_slice(&len.to_le_bytes());
            entries.extend_from_slice(&vertices.to_le_bytes());
            entries.extend_from_slice(&kind.to_le_bytes());
            entries.extend_from_slice(&crs_i.to_le_bytes());
            off += u64::from(len);
        }
        let mut index = persist::index_prefix(ident);
        index.extend_from_slice(&(ids.len() as u64).to_le_bytes());
        index.extend_from_slice(&(crs.len() as u64).to_le_bytes());
        for iri in &crs {
            index.extend_from_slice(&(iri.len() as u32).to_le_bytes());
            index.extend_from_slice(iri.as_bytes());
        }
        index.resize(persist::pad8(index.len()), 0);
        index.extend_from_slice(&entries);
        drop(entries);
        persist::write_file(
            path,
            FileKind::Column,
            ident,
            ids.len() as u64,
            &index,
            |sink| {
                let mut buf = Vec::new();
                for &o in &ids {
                    if let Slot::Geom(e) = &self.base[&o] {
                        match &e.geom {
                            Source::Parsed(g) => {
                                buf.clear();
                                persist::encode(g, &mut buf);
                                sink.put(&buf)?;
                            }
                            Source::Mapped { file, i, .. } => {
                                sink.put(file.record(*i as usize).unwrap_or_default())?
                            }
                        }
                    }
                }
                Ok(())
            },
        )
    }

    /// The column of the file `map` (`column.spkg`, checked); `Err` names what is
    /// wrong with it.
    pub fn read(map: Arc<Mapped>) -> Result<Column, String> {
        let file = Arc::new(ColumnFile::new(map)?);
        let mut base = FxHashMap::default();
        base.reserve(file.n);
        let mut counts = Counts::default();
        let mut last = None;
        let mut off = 0u64;
        for i in 0..file.n {
            let e = file.entry(i);
            if last.is_some_and(|l| l >= e.o) {
                return Err("column entries out of order".into());
            }
            last = Some(e.o);
            let slot = match e.kind {
                KIND_GEOM => {
                    // records follow each other in entry order
                    if e.off != off || file.record(i).is_none() || e.crs as usize >= file.crs.len()
                    {
                        return Err("column entry out of bounds".into());
                    }
                    off += u64::from(e.len);
                    Slot::Geom(Arc::new(ColumnEntry {
                        bbox: e.bbox,
                        geom: Source::Mapped {
                            file: file.clone(),
                            i: i as u32,
                            cell: OnceLock::new(),
                        },
                    }))
                }
                KIND_UNKNOWN_CRS => match file.crs.get(e.crs as usize) {
                    Some(c) => Slot::Skipped(Skip::UnknownCrs(match c {
                        CrsRef::Unknown(iri) => iri.clone(),
                        c => c.iri().into(),
                    })),
                    None => return Err("column entry out of bounds".into()),
                },
                KIND_OTHER => Slot::Other,
                k => Slot::Skipped(skip_of(k).ok_or("unknown column entry kind")?),
            };
            // points of W3C Basic Geo pairs are no literals
            if !wgs84::is_pair(e.o) {
                counts.add(&slot);
            }
            base.insert(e.o, slot);
        }
        if persist::pad8(off as usize) != file.map.data().len() {
            return Err("column data has the wrong length".into());
        }
        Ok(Column {
            base,
            base_counts: counts,
            extra: RwLock::new(Extra::default()),
            file: Some(file),
            pairs: FxHashMap::default(),
            parsed: 0,
            reused: 0,
        })
    }
}

/// Bytes of a column file entry.
const ENTRY: usize = 48;

const KIND_GEOM: u32 = 0;
const KIND_UNKNOWN_CRS: u32 = 2;
const KIND_OTHER: u32 = 6;

fn skip_code(s: &Skip) -> u32 {
    match s {
        Skip::Malformed => 1,
        Skip::UnknownCrs(_) => KIND_UNKNOWN_CRS,
        Skip::TooLarge => 3,
        Skip::TooComplex => 4,
        Skip::Empty => 5,
    }
}

fn skip_of(k: u32) -> Option<Skip> {
    Some(match k {
        1 => Skip::Malformed,
        3 => Skip::TooLarge,
        4 => Skip::TooComplex,
        5 => Skip::Empty,
        _ => return None,
    })
}

/// An entry of a column file.
struct RawEntry {
    o: u64,
    off: u64,
    bbox: [f32; 4],
    len: u32,
    vertices: u32,
    kind: u32,
    crs: u32,
}

/// A mapped `column.spkg`.
pub(crate) struct ColumnFile {
    map: Arc<Mapped>,
    crs: Vec<CrsRef>,
    /// where the entries start in the index section
    entries: usize,
    n: usize,
    /// memory of the geometries decoded so far
    decoded: AtomicU64,
}

impl ColumnFile {
    fn new(map: Arc<Mapped>) -> Result<ColumnFile, String> {
        let idx = map.index();
        let bad = || "damaged column index".to_string();
        let n = persist::read_u64(idx, 16).ok_or_else(bad)?;
        let ncrs = persist::read_u64(idx, 24).ok_or_else(bad)?;
        if n != map.header.rows {
            return Err(bad());
        }
        let mut at = 32;
        let mut crs = Vec::new();
        for _ in 0..ncrs {
            let len = persist::read_u32(idx, at).ok_or_else(bad)? as usize;
            let b = idx.get(at + 4..at + 4 + len).ok_or_else(bad)?;
            let iri = std::str::from_utf8(b).map_err(|_| bad())?;
            crs.push(CrsRef::from_iri(Some(iri)));
            at += 4 + len;
        }
        let entries = persist::pad8(at);
        let n = usize::try_from(n).map_err(|_| bad())?;
        if n.checked_mul(ENTRY).and_then(|b| b.checked_add(entries)) != Some(idx.len()) {
            return Err(bad());
        }
        Ok(ColumnFile {
            map,
            crs,
            entries,
            n,
            decoded: AtomicU64::new(0),
        })
    }

    fn entry(&self, i: usize) -> RawEntry {
        let b = &self.map.index()[self.entries + i * ENTRY..][..ENTRY];
        let u32_at = |k: usize| u32::from_le_bytes(b[k..k + 4].try_into().unwrap());
        let f32_at = |k: usize| f32::from_le_bytes(b[k..k + 4].try_into().unwrap());
        RawEntry {
            o: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            off: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            bbox: [f32_at(16), f32_at(20), f32_at(24), f32_at(28)],
            len: u32_at(32),
            vertices: u32_at(36),
            kind: u32_at(40),
            crs: u32_at(44),
        }
    }

    /// The record of entry `i` (`None`: out of the data).
    fn record(&self, i: usize) -> Option<&[u8]> {
        let e = self.entry(i);
        let start = usize::try_from(e.off).ok()?;
        self.map
            .data()
            .get(start..start.checked_add(e.len as usize)?)
    }

    fn crs_of(&self, i: usize) -> CrsRef {
        self.crs
            .get(self.entry(i).crs as usize)
            .cloned()
            .unwrap_or(CrsRef::Known(CRS84))
    }

    /// Decode the geometry of entry `i`.
    fn decode(&self, i: usize) -> Option<GeomRef> {
        let g = persist::decode(self.record(i)?, self.crs_of(i))?;
        self.decoded
            .fetch_add(g.mem_size() as u64, Ordering::Relaxed);
        Some(Arc::new(g))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(lex: &str, dt: &str) -> Vec<u8> {
        let mut k = vec![b'"'];
        k.extend_from_slice(lex.as_bytes());
        k.push(KEY_SEP);
        k.push(b'^');
        k.extend_from_slice(dt.as_bytes());
        k
    }

    #[test]
    fn classify_literals() {
        let cfg = GeoConfig::default();
        let Slot::Geom(e) = classify(&key("POINT(1 2)", WKT_LITERAL), &cfg) else {
            panic!("a point is indexed");
        };
        assert_eq!(e.bbox84(), [1.0, 2.0, 1.0, 2.0]);
        assert!(matches!(
            classify(&key("POINT(1)", WKT_LITERAL), &cfg),
            Slot::Skipped(Skip::Malformed)
        ));
        assert!(matches!(
            classify(&key("", WKT_LITERAL), &cfg),
            Slot::Skipped(Skip::Empty)
        ));
        assert!(matches!(
            classify(
                &key("<http://example.org/mars> POINT(1 1)", WKT_LITERAL),
                &cfg
            ),
            Slot::Skipped(Skip::UnknownCrs(_))
        ));
        assert!(matches!(
            classify(&key("POINT(1 2)", "http://example.org/dt"), &cfg),
            Slot::Other
        ));
        assert!(matches!(
            classify(b"<http://example.org/x>", &cfg),
            Slot::Other
        ));
        let small = GeoConfig {
            max_geometry_bytes: 4,
            ..GeoConfig::default()
        };
        let s = classify(&key("POINT(1 2)", WKT_LITERAL), &small);
        assert!(matches!(s, Slot::Skipped(Skip::TooLarge)));
        // still a candidate of searches, parsed without the length limit
        assert!(s.rechecked());
        let e = recheck(&key("POINT(1 2)", WKT_LITERAL), &small).unwrap();
        assert_eq!(e.bbox84(), [1.0, 2.0, 1.0, 2.0]);
        let few = GeoConfig {
            max_vertices: 3,
            ..GeoConfig::default()
        };
        let s = classify(&key("POLYGON((0 0, 1 0, 1 1, 0 0))", WKT_LITERAL), &few);
        assert!(matches!(s, Slot::Skipped(Skip::TooComplex)));
        assert!(!s.rechecked());
        assert!(recheck(&key("POLYGON((0 0, 1 0, 1 1, 0 0))", WKT_LITERAL), &few).is_none());
        // the other reasons are no candidates
        for lex in ["POINT(1)", "", "<http://example.org/mars> POINT(1 1)"] {
            assert!(!classify(&key(lex, WKT_LITERAL), &cfg).rechecked(), "{lex}");
        }
    }

    #[test]
    fn boxes_round_outward() {
        let x = 0.1f64;
        let b = round_out([x, x, x, x]);
        assert!(f64::from(b[0]) <= x && f64::from(b[1]) <= x);
        assert!(f64::from(b[2]) >= x && f64::from(b[3]) >= x);
        assert!(b[0] < b[2]);
        // exactly representable values stay
        assert_eq!(round_out([1.0, -2.5, 3.0, 4.0]), [1.0, -2.5, 3.0, 4.0]);
    }
}
