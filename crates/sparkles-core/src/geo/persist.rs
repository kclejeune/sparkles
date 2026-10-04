//! The spatial index's files: a generation's base rows, tree and geometry column under
//! `gen-NNNN/geo/`, so that opening a store maps them instead of parsing every literal
//! again.
//!
//! Each file is a 64-byte header, an index section, data sections, and a 32-byte footer
//! (little-endian throughout, every section starting at a multiple of 8 bytes):
//!
//! ```text
//! header  0  magic  b"SPKGEO\0\x01"
//!         8  u32    file format version (FILE_VERSION)
//!        12  u32    kind (FileKind)
//!        16  u64    rows: tree items (rtree.spkg) or column entries (column.spkg)
//!        24  u64    GeoConfig::index_hash of the configuration it was built for
//!        32  u64    the generation's base commit (`seq`)
//!        40  u64    the generation's quad count
//!        48  u64    byte length of the index section (directory of the data)
//!        56  u32    CRC-32 of bytes 0..56
//!        60  u32    reserved (0)
//! footer  0  u64    rows (again)
//!         8  u32    CRC-32 of the index section
//!        12  u32    CRC-32 of the data sections
//!        16  u64    file length
//!        24  magic  b"SPKGEOF\x01"
//! ```
//!
//! Both index sections start with the hash of the build that wrote them ([`engine_hash`]:
//! this program's version and its CRS table, which decide how literals are read) and the
//! generation's term count.
//!
//! * `rtree.spkg`: index = engine, terms, then the counts of base rows, skipped rows,
//!   predicate slots, tree bytes and Basic Geo pairs; data = the base rows (`[s, p, o, g]`,
//!   `u64` each, PSO order, the tree's items), the skipped rows, the rows per predicate
//!   slot (`u64`), the flatbush buffer of the packed tree, the Basic Geo pairs (`[id,
//!   lat, long]` plus the two quads' base flags).
//! * `column.spkg`: index = engine, terms, entry count, the CRS table (IRIs), then one
//!   48-byte entry per literal sorted by id (`o: u64`, record offset `u64`, the `f32`
//!   CRS84 envelope, record length `u32`, vertices `u32`, kind `u32`, CRS `u32`); data =
//!   the geometry records ([`encode`]), decoded on demand.
//!
//! A file is used only when magic, version, kind, configuration hash, base commit, quad
//! count, rows, the build hash, the term count and all three checksums match (the data
//! checksum is verified when a base is loaded, off the open path, and by `sparkles
//! check --full`); anything else is deleted and the base is built again. Files are
//! written to `*.tmp`, synced and renamed, never changed in place: a reader's mapping
//! stays valid whatever later builds or compactions do. A layout change bumps
//! [`FILE_VERSION`]; files of other versions are rebuilt, never migrated.

use super::crs::CrsRef;
use super::geom::{Geom, GeomType, Layout};
use crate::error::Result;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The directory of a generation holding the index files.
pub const DIR: &str = "geo";
/// The base rows and their packed tree.
pub const RTREE_FILE: &str = "rtree.spkg";
/// The geometry column of the base literals.
pub const COLUMN_FILE: &str = "column.spkg";

pub const MAGIC: [u8; 8] = *b"SPKGEO\0\x01";
pub const FOOTER_MAGIC: [u8; 8] = *b"SPKGEOF\x01";
pub const FILE_VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 64;
pub const FOOTER_BYTES: usize = 32;

/// What a file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum FileKind {
    Column = 1,
    Rtree = 2,
}

impl FileKind {
    pub fn file_name(self) -> &'static str {
        match self {
            FileKind::Column => COLUMN_FILE,
            FileKind::Rtree => RTREE_FILE,
        }
    }
}

/// The identity of a file: what it must match to be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub kind: u32,
    pub rows: u64,
    pub config_hash: u64,
    pub base_seq: u64,
    pub quads: u64,
    pub index_bytes: u64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut b = [0u8; HEADER_BYTES];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..12].copy_from_slice(&self.version.to_le_bytes());
        b[12..16].copy_from_slice(&self.kind.to_le_bytes());
        b[16..24].copy_from_slice(&self.rows.to_le_bytes());
        b[24..32].copy_from_slice(&self.config_hash.to_le_bytes());
        b[32..40].copy_from_slice(&self.base_seq.to_le_bytes());
        b[40..48].copy_from_slice(&self.quads.to_le_bytes());
        b[48..56].copy_from_slice(&self.index_bytes.to_le_bytes());
        let crc = crc32(&[&b[0..56]]);
        b[56..60].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// The header of `b` (`None`: not a header of this format, or damaged).
    pub fn decode(b: &[u8]) -> Option<Header> {
        let b = b.get(..HEADER_BYTES)?;
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if b[0..8] != MAGIC || u32_at(56) != crc32(&[&b[0..56]]) || u32_at(60) != 0 {
            return None;
        }
        Some(Header {
            version: u32_at(8),
            kind: u32_at(12),
            rows: u64_at(16),
            config_hash: u64_at(24),
            base_seq: u64_at(32),
            quads: u64_at(40),
            index_bytes: u64_at(48),
        })
    }
}

/// The end of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Footer {
    rows: u64,
    index_crc: u32,
    data_crc: u32,
    len: u64,
}

impl Footer {
    fn encode(&self) -> [u8; FOOTER_BYTES] {
        let mut b = [0u8; FOOTER_BYTES];
        b[0..8].copy_from_slice(&self.rows.to_le_bytes());
        b[8..12].copy_from_slice(&self.index_crc.to_le_bytes());
        b[12..16].copy_from_slice(&self.data_crc.to_le_bytes());
        b[16..24].copy_from_slice(&self.len.to_le_bytes());
        b[24..32].copy_from_slice(&FOOTER_MAGIC);
        b
    }

    fn decode(b: &[u8]) -> Option<Footer> {
        let b = b.get(..FOOTER_BYTES)?;
        if b[24..32] != FOOTER_MAGIC {
            return None;
        }
        Some(Footer {
            rows: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            index_crc: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            data_crc: u32::from_le_bytes(b[12..16].try_into().unwrap()),
            len: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        })
    }
}

/// CRC-32 (IEEE) of the concatenated parts.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = flate2::Crc::new();
    for p in parts {
        c.update(p);
    }
    c.sum()
}

/// The hash of how this build reads literals into the index (its version, CRS table and
/// the CRSs registered in this process): files written by another build, or with other
/// registered CRSs, are rebuilt, since it may classify literals differently.
pub fn engine_hash() -> u64 {
    static HASH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let registered = super::crs::registry_fingerprint();
    registered.rotate_left(17)
        ^ *HASH.get_or_init(|| {
            let mut h = super::Fnv::new();
            h.field(env!("CARGO_PKG_VERSION").as_bytes());
            h.field(&FILE_VERSION.to_le_bytes());
            for id in super::crs::CrsId::all() {
                h.field(id.iri().as_bytes());
            }
            // the EPSG table resolves more CRSs
            h.field(&[u8::from(cfg!(feature = "geo-epsg"))]);
            h.finish()
        })
}

/// What the files of a generation must match: the configuration's [`index_hash`], the
/// generation's base commit, quad and term counts.
///
/// [`index_hash`]: super::GeoConfig::index_hash
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub config_hash: u64,
    pub base_seq: u64,
    pub quads: u64,
    pub terms: u64,
}

impl Identity {
    /// The identity of the generation in `dir` (with its quad and term counts) under the
    /// configuration hash `config_hash`. The base commit comes from `commit.json` (0
    /// when there is none).
    pub fn of(dir: &Path, quads: u64, terms: u64, config_hash: u64) -> Identity {
        let base_seq = crate::commit::read_gen_commit(dir)
            .ok()
            .flatten()
            .map_or(0, |(_, c, _)| c.seq);
        Identity {
            config_hash,
            base_seq,
            quads,
            terms,
        }
    }
}

/// The directory of the index files of the generation in `gen_dir`.
pub fn dir_of(gen_dir: &Path) -> PathBuf {
    gen_dir.join(DIR)
}

/// Why a file cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// there is no file
    Missing,
    /// the file is damaged or belongs to something else (the reason)
    Unusable(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Missing => f.write_str("missing"),
            Problem::Unusable(m) => f.write_str(m),
        }
    }
}

/// The common start of both index sections.
pub(crate) const INDEX_PREFIX: usize = 16;

/// A file mapped read-only and checked: its header, index section and data.
pub(crate) struct Mapped {
    map: memmap2::Mmap,
    pub header: Header,
    index: std::ops::Range<usize>,
    data: std::ops::Range<usize>,
}

impl std::fmt::Debug for Mapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mapped")
            .field("header", &self.header)
            .finish()
    }
}

impl Mapped {
    /// Map and check `path`. With `ident`, the file must belong to that generation and
    /// configuration (and to this build); with `verify_data`, the data checksum is
    /// verified too (reading the whole file).
    pub fn open(
        path: &Path,
        kind: FileKind,
        ident: Option<&Identity>,
        verify_data: bool,
    ) -> std::result::Result<Mapped, Problem> {
        let bad = |m: String| Problem::Unusable(m);
        let f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Problem::Missing),
            Err(e) => return Err(bad(format!("cannot be read: {e}"))),
        };
        let len = f
            .metadata()
            .map_err(|e| bad(format!("cannot be read: {e}")))?
            .len();
        if len < (HEADER_BYTES + FOOTER_BYTES) as u64 {
            return Err(bad(format!("truncated ({len} bytes)")));
        }
        // SAFETY: the files are written once (to a temporary name, then renamed) and never
        // changed in place, so the mapped bytes do not change under the mapping.
        let map =
            unsafe { memmap2::Mmap::map(&f) }.map_err(|e| bad(format!("cannot be mapped: {e}")))?;
        let header = Header::decode(&map).ok_or_else(|| bad("damaged header".into()))?;
        if header.version != FILE_VERSION {
            return Err(bad(format!(
                "format version {} (this build reads {FILE_VERSION})",
                header.version
            )));
        }
        if header.kind != kind as u32 {
            return Err(bad(format!(
                "holds kind {}, not {}",
                header.kind, kind as u32
            )));
        }
        let n = map.len();
        let footer = Footer::decode(&map[n - FOOTER_BYTES..])
            .ok_or_else(|| bad("damaged or missing footer (truncated?)".into()))?;
        if footer.len != n as u64 {
            return Err(bad(format!(
                "truncated or extended: {n} bytes, written as {}",
                footer.len
            )));
        }
        if footer.rows != header.rows {
            return Err(bad("header and footer disagree".into()));
        }
        let idx_end = usize::try_from(header.index_bytes)
            .ok()
            .and_then(|b| b.checked_add(HEADER_BYTES))
            .filter(|&e| e <= n - FOOTER_BYTES && header.index_bytes % 8 == 0)
            .ok_or_else(|| bad("index section out of bounds".into()))?;
        let index = HEADER_BYTES..idx_end;
        let data = idx_end..n - FOOTER_BYTES;
        if crc32(&[&map[index.clone()]]) != footer.index_crc {
            return Err(bad("index section checksum mismatch".into()));
        }
        if verify_data && crc32(&[&map[data.clone()]]) != footer.data_crc {
            return Err(bad("data checksum mismatch".into()));
        }
        let m = Mapped {
            map,
            header,
            index,
            data,
        };
        if m.index().len() < INDEX_PREFIX {
            return Err(bad("index section too short".into()));
        }
        if m.u64_at(0) != engine_hash() {
            return Err(bad("written by another version of sparkles".into()));
        }
        if let Some(id) = ident {
            if header.config_hash != id.config_hash {
                return Err(bad("built for another configuration".into()));
            }
            if header.base_seq != id.base_seq || header.quads != id.quads || m.u64_at(8) != id.terms
            {
                return Err(bad("built for another generation".into()));
            }
        }
        Ok(m)
    }

    /// The index section.
    pub fn index(&self) -> &[u8] {
        &self.map[self.index.clone()]
    }

    /// The data sections.
    pub fn data(&self) -> &[u8] {
        &self.map[self.data.clone()]
    }

    /// The `u64` at byte `at` of the index section.
    pub fn u64_at(&self, at: usize) -> u64 {
        read_u64(self.index(), at).unwrap_or(0)
    }

    /// Bytes mapped.
    pub fn len(&self) -> u64 {
        self.map.len() as u64
    }
}

/// The little-endian `u64` at `at` of `b`.
pub(crate) fn read_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The little-endian `u32` at `at` of `b`.
pub(crate) fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// `n` rounded up to a multiple of 8.
pub(crate) fn pad8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// Where the data sections of a file being written go: counted and checksummed.
pub(crate) struct Sink {
    w: std::io::BufWriter<std::fs::File>,
    crc: flate2::Crc,
    n: u64,
}

impl Sink {
    pub fn put(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.crc.update(b);
        self.n += b.len() as u64;
        self.w.write_all(b)
    }

    /// Pad with zeros to a multiple of 8 bytes.
    pub fn align(&mut self) -> std::io::Result<()> {
        let pad = pad8(self.n as usize) - self.n as usize;
        self.put(&[0u8; 8][..pad])
    }
}

/// Write the file `path` durably (a synced temporary file renamed over it; the caller
/// syncs the directory): header, the index section `index` (padded to 8 bytes; it must
/// start with the [`INDEX_PREFIX`]), the data `data` writes, footer.
pub(crate) fn write_file(
    path: &Path,
    kind: FileKind,
    ident: &Identity,
    rows: u64,
    index: &[u8],
    data: impl FnOnce(&mut Sink) -> std::io::Result<()>,
) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let r = (|| -> Result<()> {
        let mut index = index.to_vec();
        index.resize(pad8(index.len()), 0);
        let header = Header {
            version: FILE_VERSION,
            kind: kind as u32,
            rows,
            config_hash: ident.config_hash,
            base_seq: ident.base_seq,
            quads: ident.quads,
            index_bytes: index.len() as u64,
        };
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        w.write_all(&header.encode())?;
        w.write_all(&index)?;
        let mut sink = Sink {
            w,
            crc: flate2::Crc::new(),
            n: 0,
        };
        data(&mut sink)?;
        sink.align()?;
        let footer = Footer {
            rows,
            index_crc: crc32(&[&index]),
            data_crc: sink.crc.sum(),
            len: (HEADER_BYTES + index.len() + FOOTER_BYTES) as u64 + sink.n,
        };
        let mut w = sink.w;
        w.write_all(&footer.encode())?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// The start of an index section: the build hash and the generation's term count.
pub(crate) fn index_prefix(ident: &Identity) -> Vec<u8> {
    let mut v = Vec::with_capacity(256);
    v.extend_from_slice(&engine_hash().to_le_bytes());
    v.extend_from_slice(&ident.terms.to_le_bytes());
    v
}

/// Remove the index files of a generation (`dir`: its `geo/` directory).
pub(crate) fn remove(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(target: "sparkles::geo::persist", "cannot remove {}: {e}", dir.display())
        }
    }
}

/// Whether this platform reads the files in place (they are little-endian).
pub(crate) const SUPPORTED: bool = cfg!(target_endian = "little");

// ------------------------------------------------------------- geometry records ------

/// The flags of a record.
const EMPTY: u8 = 1;
const HAS_Z: u8 = 2;

/// Nesting allowed in records (collections in collections), as in literals.
const MAX_DEPTH: usize = 64;

fn type_code(t: GeomType) -> u8 {
    match t {
        GeomType::Point => 0,
        GeomType::LineString => 1,
        GeomType::Polygon => 2,
        GeomType::MultiPoint => 3,
        GeomType::MultiLineString => 4,
        GeomType::MultiPolygon => 5,
        GeomType::GeometryCollection => 6,
        GeomType::LinearRing => 7,
        GeomType::Triangle => 8,
        GeomType::Tin => 9,
        GeomType::PolyhedralSurface => 10,
    }
}

fn type_of(c: u8) -> Option<GeomType> {
    Some(match c {
        0 => GeomType::Point,
        1 => GeomType::LineString,
        2 => GeomType::Polygon,
        3 => GeomType::MultiPoint,
        4 => GeomType::MultiLineString,
        5 => GeomType::MultiPolygon,
        6 => GeomType::GeometryCollection,
        7 => GeomType::LinearRing,
        8 => GeomType::Triangle,
        9 => GeomType::Tin,
        10 => GeomType::PolyhedralSurface,
        _ => return None,
    })
}

fn layout_code(l: Layout) -> u8 {
    match l {
        Layout::Xy => 0,
        Layout::Xyz => 1,
        Layout::Xym => 2,
        Layout::Xyzm => 3,
    }
}

fn layout_of(c: u8) -> Option<Layout> {
    Some(match c {
        0 => Layout::Xy,
        1 => Layout::Xyz,
        2 => Layout::Xym,
        3 => Layout::Xyzm,
        _ => return None,
    })
}

/// Append the record of `g` (everything but its CRS, which the column entry holds) to
/// `out`: type, layout, flags, vertices, the Z range, then the geometry's structure
/// with its coordinates as `f64` pairs.
pub(crate) fn encode(g: &Geom, out: &mut Vec<u8>) {
    let flags = if g.empty { EMPTY } else { 0 } | if g.z.is_some() { HAS_Z } else { 0 };
    out.extend_from_slice(&[type_code(g.declared), layout_code(g.layout), flags, 0]);
    out.extend_from_slice(&g.vertices.to_le_bytes());
    if let Some((lo, hi)) = g.z {
        out.extend_from_slice(&lo.to_le_bytes());
        out.extend_from_slice(&hi.to_le_bytes());
    }
    encode_geometry(&g.g, out);
}

/// The length of the record [`encode`] writes for `g`.
pub(crate) fn encoded_len(g: &Geom) -> usize {
    fn line(l: &georust::LineString<f64>) -> usize {
        4 + 16 * l.0.len()
    }
    fn polygon(p: &georust::Polygon<f64>) -> usize {
        4 + line(p.exterior()) + p.interiors().iter().map(line).sum::<usize>()
    }
    fn geometry(g: &georust::Geometry<f64>) -> usize {
        use georust::Geometry as G;
        1 + match g {
            G::Point(_) => 16,
            G::Line(_) | G::Rect(_) => 32,
            G::Triangle(_) => 48,
            G::LineString(l) => line(l),
            G::Polygon(p) => polygon(p),
            G::MultiPoint(m) => 4 + 16 * m.0.len(),
            G::MultiLineString(m) => 4 + m.0.iter().map(line).sum::<usize>(),
            G::MultiPolygon(m) => 4 + m.0.iter().map(polygon).sum::<usize>(),
            G::GeometryCollection(c) => 4 + c.0.iter().map(geometry).sum::<usize>(),
        }
    }
    8 + if g.z.is_some() { 16 } else { 0 } + geometry(&g.g)
}

fn coord(c: georust::Coord<f64>, out: &mut Vec<u8>) {
    out.extend_from_slice(&c.x.to_le_bytes());
    out.extend_from_slice(&c.y.to_le_bytes());
}

fn count(n: usize, out: &mut Vec<u8>) {
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
}

fn line(l: &georust::LineString<f64>, out: &mut Vec<u8>) {
    count(l.0.len(), out);
    for &c in &l.0 {
        coord(c, out);
    }
}

fn polygon(p: &georust::Polygon<f64>, out: &mut Vec<u8>) {
    count(1 + p.interiors().len(), out);
    line(p.exterior(), out);
    for r in p.interiors() {
        line(r, out);
    }
}

fn encode_geometry(g: &georust::Geometry<f64>, out: &mut Vec<u8>) {
    use georust::Geometry as G;
    match g {
        G::Point(p) => {
            out.push(0);
            coord(p.0, out);
        }
        G::Line(l) => {
            out.push(1);
            coord(l.start, out);
            coord(l.end, out);
        }
        G::LineString(l) => {
            out.push(2);
            line(l, out);
        }
        G::Polygon(p) => {
            out.push(3);
            polygon(p, out);
        }
        G::MultiPoint(m) => {
            out.push(4);
            count(m.0.len(), out);
            for p in &m.0 {
                coord(p.0, out);
            }
        }
        G::MultiLineString(m) => {
            out.push(5);
            count(m.0.len(), out);
            for l in &m.0 {
                line(l, out);
            }
        }
        G::MultiPolygon(m) => {
            out.push(6);
            count(m.0.len(), out);
            for p in &m.0 {
                polygon(p, out);
            }
        }
        G::GeometryCollection(c) => {
            out.push(7);
            count(c.0.len(), out);
            for g in &c.0 {
                encode_geometry(g, out);
            }
        }
        G::Rect(r) => {
            out.push(8);
            coord(r.min(), out);
            coord(r.max(), out);
        }
        G::Triangle(t) => {
            out.push(9);
            coord(t.v1(), out);
            coord(t.v2(), out);
            coord(t.v3(), out);
        }
    }
}

/// Reads a record; every read is checked, so damaged bytes give `None`, never a panic
/// or an allocation larger than the record.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }
    fn coord(&mut self) -> Option<georust::Coord<f64>> {
        Some(georust::Coord {
            x: self.f64()?,
            y: self.f64()?,
        })
    }
    /// A count of items of at least `min` bytes each.
    fn count(&mut self, min: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.checked_mul(min)? <= self.b.len() - self.at).then_some(n)
    }
    fn line(&mut self) -> Option<georust::LineString<f64>> {
        let n = self.count(16)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.coord()?);
        }
        Some(georust::LineString(v))
    }
    fn polygon(&mut self) -> Option<georust::Polygon<f64>> {
        let n = self.count(4)?;
        if n == 0 {
            return None;
        }
        let ext = self.line()?;
        let mut holes = Vec::with_capacity(n - 1);
        for _ in 1..n {
            holes.push(self.line()?);
        }
        Some(georust::Polygon::new(ext, holes))
    }
    fn geometry(&mut self, depth: usize) -> Option<georust::Geometry<f64>> {
        use georust::Geometry as G;
        if depth > MAX_DEPTH {
            return None;
        }
        Some(match self.u8()? {
            0 => G::Point(georust::Point(self.coord()?)),
            1 => G::Line(georust::Line::new(self.coord()?, self.coord()?)),
            2 => G::LineString(self.line()?),
            3 => G::Polygon(self.polygon()?),
            4 => {
                let n = self.count(16)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(georust::Point(self.coord()?));
                }
                G::MultiPoint(georust::MultiPoint(v))
            }
            5 => {
                let n = self.count(4)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.line()?);
                }
                G::MultiLineString(georust::MultiLineString(v))
            }
            6 => {
                let n = self.count(8)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.polygon()?);
                }
                G::MultiPolygon(georust::MultiPolygon(v))
            }
            7 => {
                let n = self.count(1)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.geometry(depth + 1)?);
                }
                G::GeometryCollection(georust::GeometryCollection(v))
            }
            8 => G::Rect(georust::Rect::new(self.coord()?, self.coord()?)),
            9 => G::Triangle(georust::Triangle::new(
                self.coord()?,
                self.coord()?,
                self.coord()?,
            )),
            _ => return None,
        })
    }
}

/// The geometry of record `b` in CRS `crs` (`None`: not a whole, valid record).
pub(crate) fn decode(b: &[u8], crs: CrsRef) -> Option<Geom> {
    let mut r = Reader { b, at: 0 };
    let declared = type_of(r.u8()?)?;
    let layout = layout_of(r.u8()?)?;
    let flags = r.u8()?;
    r.u8()?;
    let vertices = r.u32()?;
    let z = if flags & HAS_Z != 0 {
        Some((r.f64()?, r.f64()?))
    } else {
        None
    };
    let g = r.geometry(0)?;
    (r.at == b.len()).then_some(Geom {
        crs,
        declared,
        layout,
        g,
        z,
        empty: flags & EMPTY != 0,
        vertices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::vocab::{GEOJSON_LITERAL, WKT_LITERAL};

    #[test]
    fn header_round_trip() {
        let h = Header {
            version: FILE_VERSION,
            kind: FileKind::Rtree as u32,
            rows: 7,
            config_hash: 0xfeed,
            base_seq: 3,
            quads: 42,
            index_bytes: 4096,
        };
        let mut b = h.encode();
        assert_eq!(Header::decode(&b), Some(h));
        b[20] ^= 1;
        assert_eq!(Header::decode(&b), None);
    }

    /// Every geometry the parser makes comes back from its record exactly.
    #[test]
    fn records_round_trip() {
        let lits = [
            ("POINT(1 2)", WKT_LITERAL),
            ("POINT Z(1 2 3)", WKT_LITERAL),
            ("POINT M(1 2 3)", WKT_LITERAL),
            ("POINT ZM(1 2 3 4)", WKT_LITERAL),
            ("POINT EMPTY", WKT_LITERAL),
            ("LINESTRING(0 0, 1 1, 2 0.5)", WKT_LITERAL),
            ("LINEARRING(0 0, 1 0, 1 1, 0 0)", WKT_LITERAL),
            (
                "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0), (2 2, 3 2, 3 3, 2 2))",
                WKT_LITERAL,
            ),
            ("POLYGON EMPTY", WKT_LITERAL),
            ("TRIANGLE((0 0, 1 0, 0 1, 0 0))", WKT_LITERAL),
            ("MULTIPOINT((1 1), (30 30))", WKT_LITERAL),
            ("MULTILINESTRING((0 0, 1 1), (2 2, 3 3, 4 2))", WKT_LITERAL),
            (
                "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)), ((5 5, 6 5, 6 6, 5 5), (5.2 5.1, 5.5 5.1, 5.5 5.3, 5.2 5.1)))",
                WKT_LITERAL,
            ),
            (
                "GEOMETRYCOLLECTION(POINT(1 1), GEOMETRYCOLLECTION(LINESTRING(0 0, 1 1)), POLYGON EMPTY)",
                WKT_LITERAL,
            ),
            ("GEOMETRYCOLLECTION EMPTY", WKT_LITERAL),
            (
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8606 2.3376)",
                WKT_LITERAL,
            ),
            (
                "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(222638.98 222684.21)",
                WKT_LITERAL,
            ),
            ("<http://example.org/crs/mars> POINT(1 1)", WKT_LITERAL),
            (
                r#"{"type":"LineString","coordinates":[[2.25,48.84],[2.30,48.86],[2.36,48.85]]}"#,
                GEOJSON_LITERAL,
            ),
            (
                r#"{"type":"Point","coordinates":[0.1,-0.30000000000000004]}"#,
                GEOJSON_LITERAL,
            ),
        ];
        for (lex, dt) in lits {
            let g = crate::geo::parse(lex, dt).unwrap();
            let mut b = Vec::new();
            encode(&g, &mut b);
            assert_eq!(b.len(), encoded_len(&g), "{lex}");
            let back = decode(&b, g.crs.clone()).unwrap_or_else(|| panic!("{lex}"));
            assert_eq!(format!("{g:?}"), format!("{back:?}"), "{lex}");
            // every truncation is refused
            for n in 0..b.len() {
                assert!(decode(&b[..n], g.crs.clone()).is_none(), "{lex} at {n}");
            }
            // and garbage never panics
            for i in 0..b.len() {
                let mut c = b.clone();
                c[i] ^= 0xa5;
                let _ = decode(&c, g.crs.clone());
            }
        }
    }

    #[test]
    fn files_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.spkg");
        let ident = Identity {
            config_hash: 1,
            base_seq: 2,
            quads: 3,
            terms: 4,
        };
        let mut index = index_prefix(&ident);
        index.extend_from_slice(&9u64.to_le_bytes());
        write_file(&path, FileKind::Column, &ident, 5, &index, |s| {
            s.put(b"hello")
        })
        .unwrap();
        assert!(!path.with_extension("tmp").exists());
        let m = Mapped::open(&path, FileKind::Column, Some(&ident), true).unwrap();
        assert_eq!(m.header.rows, 5);
        assert_eq!(m.u64_at(16), 9);
        assert_eq!(&m.data()[..5], b"hello");
        assert_eq!(m.data().len(), 8);
        let open = |p: &Path, id: &Identity| Mapped::open(p, FileKind::Column, Some(id), true);
        let other = Identity {
            base_seq: 7,
            ..ident
        };
        assert!(
            matches!(open(&path, &other), Err(Problem::Unusable(m)) if m.contains("generation"))
        );
        let other = Identity { terms: 5, ..ident };
        assert!(open(&path, &other).is_err());
        let other = Identity {
            config_hash: 2,
            ..ident
        };
        assert!(
            matches!(open(&path, &other), Err(Problem::Unusable(m)) if m.contains("configuration"))
        );
        assert!(matches!(
            Mapped::open(&path, FileKind::Rtree, None, true),
            Err(Problem::Unusable(_))
        ));
        assert_eq!(
            open(&dir.path().join("none"), &ident).unwrap_err(),
            Problem::Missing
        );
        // every truncation and every flipped byte is noticed
        let bytes = std::fs::read(&path).unwrap();
        let p2 = dir.path().join("y.spkg");
        for n in 0..bytes.len() {
            std::fs::write(&p2, &bytes[..n]).unwrap();
            assert!(open(&p2, &ident).is_err(), "truncated to {n}");
        }
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x10;
            std::fs::write(&p2, &b).unwrap();
            assert!(open(&p2, &ident).is_err(), "byte {i} flipped");
        }
    }
}
