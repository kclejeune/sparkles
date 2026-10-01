//! The geometry column of an index build: literal id → parsed geometry and its CRS84
//! envelope, for the base literals of indexed predicates (parsed in parallel blocks
//! when the base is built) and for the literals the commit path met since (parsed once,
//! behind a lock).
//!
//! Literals that are not indexed are remembered too, with the reason, so the commit path
//! parses each literal once and the status can count them.

use super::GeomRef;
use super::config::GeoConfig;
use super::crs::{CRS84, CRS84_IRI, CrsId, CrsRef};
use super::geom::{Geom, GeomError};
use super::vocab::{GEOJSON_LITERAL, WKT_LITERAL};
use crate::id::{Id, KEY_SEP, Tag};
use crate::store::Snapshot;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// A geometry literal of the column.
pub struct ColumnEntry {
    /// the CRS84 envelope, rounded outward to `f32`
    bbox: [f32; 4],
    geom: GeomRef,
}

impl ColumnEntry {
    /// An entry for `geom`, whose CRS84 envelope is `bbox84`.
    pub(crate) fn new(geom: GeomRef, bbox84: [f64; 4]) -> ColumnEntry {
        ColumnEntry {
            bbox: round_out(bbox84),
            geom,
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
        // the parsed geometry is kept in memory: nothing to decode
        let _ = snap;
        Ok(self.geom.clone())
    }

    /// Estimated memory of the entry and its geometry.
    pub(crate) fn bytes(&self) -> u64 {
        (std::mem::size_of::<ColumnEntry>() + std::mem::size_of::<Geom>() + 48) as u64
            + 16 * u64::from(self.geom.vertices)
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
    /// longer than `maxGeometryBytes` or with more than `maxVertices` vertices
    TooLarge,
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

/// The IRI of a CRS, for the status.
fn crs_iri(c: &CrsRef) -> Arc<str> {
    match c {
        CrsRef::Known(id) => known_crs_iri(*id).into(),
        CrsRef::Unknown(iri) => iri.clone(),
    }
}

fn known_crs_iri(id: CrsId) -> String {
    if id == CRS84 {
        CRS84_IRI.to_string()
    } else {
        format!("urn:x-sparkles:crs:{}", id.0)
    }
}

/// Parse a geometry literal: [`super::parse::parse`], with a stand-in parser while
/// tests run against a build whose parser is not written yet.
fn parse_literal(lex: &str, dt: &str) -> Result<Geom, GeomError> {
    #[cfg(test)]
    if let Some(p) = tests::PARSER.get() {
        return p(lex, dt);
    }
    super::parse::parse(lex, dt)
}

/// Classify the literal with vocabulary key `key` under the limits of `cfg`.
pub(crate) fn classify(key: &[u8], cfg: &GeoConfig) -> Slot {
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
    if lex.len() > cfg.max_geometry_bytes {
        return Slot::Skipped(Skip::TooLarge);
    }
    let Ok(lex) = std::str::from_utf8(lex) else {
        return Slot::Skipped(Skip::Malformed);
    };
    // the parser is not trusted with arbitrary input: a panic is a malformed literal
    let parsed = std::panic::catch_unwind(|| parse_literal(lex, dt));
    let g = match parsed {
        Ok(Ok(g)) => g,
        _ => return Slot::Skipped(Skip::Malformed),
    };
    if g.vertices > cfg.max_vertices {
        return Slot::Skipped(Skip::TooLarge);
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
                self.crs_seen(&e.geom.crs);
            }
            Slot::Skipped(Skip::Malformed) => self.malformed += 1,
            Slot::Skipped(Skip::TooLarge) => self.too_large += 1,
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
}

/// Distinct literals classified per parallel block of a build.
const BLOCK: usize = 4096;

impl Column {
    pub fn empty() -> Column {
        Column {
            base: FxHashMap::default(),
            base_counts: Counts::default(),
            extra: RwLock::new(Extra::default()),
        }
    }

    /// Classify the distinct base-vocabulary ids `ids` (sorted) of `snap`'s generation
    /// in parallel blocks. `step` is told the number of literals done after each block
    /// and returns an error to stop (budget, cancellation).
    pub fn build(
        snap: &Snapshot,
        ids: &[u64],
        cfg: &GeoConfig,
        step: &(dyn Fn(usize, u64) -> crate::error::Result<()> + Sync),
    ) -> crate::error::Result<Column> {
        use rayon::prelude::*;
        let vocab = &snap.generation.vocab;
        let blocks: Vec<Vec<(u64, Slot)>> = ids
            .par_chunks(BLOCK)
            .map(|chunk| {
                let payloads: Vec<u64> = chunk.iter().map(|&o| Id(o).payload()).collect();
                let mut out = Vec::with_capacity(chunk.len());
                vocab.get_sorted(&payloads, |pl, key| {
                    out.push((Id::vocab(pl).0, classify(key, cfg)));
                });
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
        Ok(Column {
            base,
            base_counts: counts,
            extra: RwLock::new(Extra::default()),
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

    /// Counts over every literal classified so far.
    pub fn counts(&self) -> Counts {
        let mut c = self.base_counts.clone();
        c.merge(&self.extra.read().counts);
        c
    }

    /// Estimated memory of the column.
    pub fn bytes(&self) -> u64 {
        let slots = (self.base.len() + self.extra.read().map.len()) as u64;
        self.base_counts.bytes + self.extra.read().counts.bytes + slots * 32
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::geo::crs::CrsRef;
    use georust::{Coord, Geometry, LineString, Point, Polygon};

    type Parser = fn(&str, &str) -> Result<Geom, GeomError>;

    /// The stand-in parser of tests (see [`parse_literal`]).
    pub(crate) static PARSER: std::sync::OnceLock<Parser> = std::sync::OnceLock::new();

    fn err(msg: &str) -> GeomError {
        GeomError {
            offset: None,
            msg: msg.into(),
        }
    }

    fn nums(s: &str) -> Result<Vec<Coord<f64>>, GeomError> {
        s.split(',')
            .map(|p| {
                let v: Vec<f64> = p
                    .split_whitespace()
                    .map(|x| x.parse::<f64>().map_err(|_| err("number")))
                    .collect::<Result<_, _>>()?;
                match v[..] {
                    [x, y] if x.is_finite() && y.is_finite() => Ok(Coord { x, y }),
                    _ => Err(err("a position needs two numbers")),
                }
            })
            .collect()
    }

    /// A small parser for the shapes tests use: `[<crs>] POINT(x y)`,
    /// `POLYGON((…))`, `LINESTRING(…)`, the empty literal, and GeoJSON points. EPSG:4326
    /// is read as latitude, longitude and placed in CRS84.
    pub(crate) fn test_parse(lex: &str, dt: &str) -> Result<Geom, GeomError> {
        let mut s = lex.trim();
        let mut crs = CrsRef::Known(CRS84);
        let mut swap = false;
        if dt == GEOJSON_LITERAL {
            let v: serde_json::Value = serde_json::from_str(s).map_err(|_| err("json"))?;
            let c = v["coordinates"]
                .as_array()
                .ok_or_else(|| err("coordinates"))?;
            let (x, y) = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
            return Ok(Geom::from_geometry(crs, Geometry::Point(Point::new(x, y))));
        }
        if let Some(rest) = s.strip_prefix('<') {
            let (iri, rest) = rest.split_once('>').ok_or_else(|| err("crs"))?;
            crs = match iri {
                CRS84_IRI => CrsRef::Known(CRS84),
                "http://www.opengis.net/def/crs/EPSG/0/4326" => {
                    swap = true;
                    CrsRef::Known(CRS84)
                }
                other => CrsRef::Unknown(other.into()),
            };
            s = rest.trim();
        }
        if s.is_empty() {
            let mut g = Geom::from_geometry(
                crs,
                Geometry::GeometryCollection(georust::GeometryCollection(vec![])),
            );
            g.empty = true;
            return Ok(g);
        }
        let open = s.find('(').ok_or_else(|| err("("))?;
        let kind = s[..open].trim().to_ascii_uppercase();
        let body = s[open..].trim();
        let inner = |b: &str| -> Result<Vec<Coord<f64>>, GeomError> {
            let b = b.trim();
            let b = b
                .strip_prefix('(')
                .and_then(|b| b.strip_suffix(')'))
                .ok_or_else(|| err("parentheses"))?;
            let mut c = nums(b)?;
            if swap {
                for p in &mut c {
                    std::mem::swap(&mut p.x, &mut p.y);
                }
            }
            Ok(c)
        };
        let g = match kind.as_str() {
            "POINT" => {
                let c = inner(body)?;
                if c.len() != 1 {
                    return Err(err("a point has one position"));
                }
                Geometry::Point(Point(c[0]))
            }
            "LINESTRING" => Geometry::LineString(LineString(inner(body)?)),
            "POLYGON" => {
                let b = body
                    .strip_prefix('(')
                    .and_then(|b| b.strip_suffix(')'))
                    .ok_or_else(|| err("parentheses"))?;
                Geometry::Polygon(Polygon::new(LineString(inner(b)?), vec![]))
            }
            _ => return Err(err("unsupported in tests")),
        };
        Ok(Geom::from_geometry(crs, g))
    }

    /// Use [`test_parse`] when the real parser cannot read literals yet.
    pub(crate) fn install_test_parser() {
        if super::super::parse::parse("POINT(1 2)", WKT_LITERAL).is_err() {
            let _ = PARSER.set(test_parse);
        }
    }

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
        install_test_parser();
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
        assert!(matches!(
            classify(&key("POINT(1 2)", WKT_LITERAL), &small),
            Slot::Skipped(Skip::TooLarge)
        ));
        let few = GeoConfig {
            max_vertices: 3,
            ..GeoConfig::default()
        };
        assert!(matches!(
            classify(&key("POLYGON((0 0, 1 0, 1 1, 0 0))", WKT_LITERAL), &few),
            Slot::Skipped(Skip::TooLarge)
        ));
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
