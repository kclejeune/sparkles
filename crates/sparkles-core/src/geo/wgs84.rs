//! W3C Basic Geo (`wgs84_pos:lat` and `wgs84_pos:long` on one subject) as points of the
//! spatial index, when a dataset's `geo.json` says `"wgs84": true`.
//!
//! A subject with a `lat` and a `long` in the same graph is a point (longitude,
//! latitude in CRS84); several of either give their cross product, as in Jena. Each
//! point is a row `(s, lat, id, g)` whose object is an id of its own (the high tag
//! [`PAIR_TAG`], which no stored term has), and whose geometry the column holds under
//! that id. The row is valid in a snapshot only while both of its quads are visible
//! there, which [`Pair::live`] checks. The values are numbers (any XSD numeric type) or
//! strings holding one, within ±90 and ±180 degrees; other pairs are not points.
//!
//! The `spatial:` property functions and `GET /{ds}/geo` see these rows, the subject
//! being the feature; FILTER pushdown and query rewrite do not (there is no geometry
//! literal to bind).

use super::column::ColumnEntry;
use super::crs::{CRS84, CrsRef};
use super::geom::Geom;
use crate::id::{Id, KEY_SEP, Tag};
use crate::index::Perm;
use crate::store::{Chunk, Snapshot};
use std::sync::Arc;

pub const WGS84_POS: &str = "http://www.w3.org/2003/01/geo/wgs84_pos#";
pub const LAT: &str = "http://www.w3.org/2003/01/geo/wgs84_pos#lat";
pub const LONG: &str = "http://www.w3.org/2003/01/geo/wgs84_pos#long";
/// The predicate `GET /{ds}/geo` reports for these points.
pub const LAT_LONG: &str = "http://www.w3.org/2003/01/geo/wgs84_pos#lat_long";

/// The tag of the ids of points: no term of the store has it.
pub const PAIR_TAG: u64 = 0xF << 60;

/// Whether `o` is the id of a point (not a term).
#[inline]
pub fn is_pair(o: u64) -> bool {
    o & PAIR_TAG == PAIR_TAG
}

/// The id of the `n`-th point.
#[inline]
pub fn pair_id(n: u64) -> u64 {
    PAIR_TAG | n
}

/// The two quads of a point: their objects, the `long` predicate (the row holds `lat`),
/// and whether each quad is in the generation's base (else it was inserted since).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub lat_o: u64,
    pub long_o: u64,
    pub long_p: u64,
    pub lat_base: bool,
    pub long_base: bool,
}

impl Pair {
    /// Whether both quads of the point row `r` are visible in `snap`.
    pub(crate) fn live(&self, snap: &Snapshot, r: &super::index::Row) -> bool {
        let visible = |p: u64, o: u64, base: bool| {
            let k = [p, r.s, o, r.g];
            let i = Perm::Pso.index();
            if base {
                let del = &snap.delta.del[i];
                del.is_empty() || !del.contains(&k)
            } else {
                snap.delta.ins[i].contains(&k)
            }
        };
        visible(r.p, self.lat_o, self.lat_base) && visible(self.long_p, self.long_o, self.long_base)
    }

    /// The flags as stored (`rtree.spkg`).
    pub(crate) fn flags(&self) -> u64 {
        u64::from(self.lat_base) | u64::from(self.long_base) << 1
    }
}

/// The ids of `lat` and `long` in `snap` (`None` while one of them is unknown).
pub fn predicates(snap: &Snapshot) -> Option<(Id, Id)> {
    Some((snap.lookup_iri(LAT)?, snap.lookup_iri(LONG)?))
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// XSD types whose values are coordinates.
const NUMERIC: [&str; 17] = [
    "integer",
    "decimal",
    "double",
    "float",
    "int",
    "long",
    "short",
    "byte",
    "nonNegativeInteger",
    "positiveInteger",
    "nonPositiveInteger",
    "negativeInteger",
    "unsignedLong",
    "unsignedInt",
    "unsignedShort",
    "unsignedByte",
    "string",
];

/// The number a literal key stands for (a numeric literal, or a string holding one).
pub fn number_of_key(key: &[u8]) -> Option<f64> {
    let rest = key.strip_prefix(b"\"")?;
    let (lex, suffix) = match rest.iter().rposition(|&b| b == KEY_SEP) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &[][..]),
    };
    if let Some(dt) = suffix.strip_prefix(b"^") {
        let local = std::str::from_utf8(dt).ok()?.strip_prefix(XSD)?;
        if !NUMERIC.contains(&local) {
            return None;
        }
    } else if !suffix.is_empty() {
        // a language-tagged string
        return None;
    }
    let x: f64 = std::str::from_utf8(lex).ok()?.trim().parse().ok()?;
    x.is_finite().then_some(x)
}

/// The number term `id` of `snap` stands for.
pub fn number(snap: &Snapshot, id: Id) -> Option<f64> {
    match id.tag() {
        Tag::Int | Tag::Double | Tag::Decimal => {
            let l = crate::id::inline_to_literal(id)?;
            let x: f64 = l.value().parse().ok()?;
            x.is_finite().then_some(x)
        }
        Tag::Vocab | Tag::Delta => number_of_key(&snap.key(id)?),
        _ => None,
    }
}

/// The point of latitude `lat` and longitude `lon` (`None` out of range).
pub fn point(lat: f64, lon: f64) -> Option<Arc<ColumnEntry>> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    let g = Geom::from_geometry(
        CrsRef::Known(CRS84),
        georust::Geometry::Point(georust::Point::new(lon, lat)),
    );
    Some(Arc::new(ColumnEntry::new(
        Arc::new(g),
        [lon, lat, lon, lat],
    )))
}

/// The objects of `snap`'s quads `(s, p, ?, g)`.
pub fn objects(snap: &Snapshot, s: u64, p: u64, g: u64) -> crate::error::Result<Vec<u64>> {
    let mut out = Vec::new();
    snap.scan(Perm::Spo, &[s, p], |c| {
        let mut each = |k: [u64; 4]| {
            if k[3] == g {
                out.push(k[2]);
            }
        };
        match c {
            Chunk::Block(b, from, to) => (from..to).for_each(|i| each(b.key(i))),
            Chunk::Row(k) => each(k),
        }
        Ok(true)
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(lex: &str, suffix: &str) -> Vec<u8> {
        let mut k = vec![b'"'];
        k.extend_from_slice(lex.as_bytes());
        if !suffix.is_empty() {
            k.push(KEY_SEP);
            k.extend_from_slice(suffix.as_bytes());
        }
        k
    }

    #[test]
    fn numbers() {
        let xsd = |t: &str| format!("^http://www.w3.org/2001/XMLSchema#{t}");
        assert_eq!(number_of_key(&key("55.701", "")), Some(55.701));
        assert_eq!(number_of_key(&key(" 12.5 ", &xsd("string"))), Some(12.5));
        assert_eq!(number_of_key(&key("1.5E1", &xsd("float"))), Some(15.0));
        assert_eq!(number_of_key(&key("-3", &xsd("int"))), Some(-3.0));
        assert_eq!(number_of_key(&key("12", "@en")), None);
        assert_eq!(number_of_key(&key("x", "")), None);
        assert_eq!(number_of_key(&key("NaN", &xsd("double"))), None);
        assert_eq!(number_of_key(&key("1", &xsd("date"))), None);
        assert_eq!(number_of_key(&key("1", "^http://example.org/n")), None);
        assert_eq!(number_of_key(b"<http://example.org/x>"), None);
        assert!(point(90.0, 180.0).is_some());
        assert!(point(90.5, 0.0).is_none() && point(0.0, -181.0).is_none());
        assert_eq!(point(48.0, 2.0).unwrap().bbox84(), [2.0, 48.0, 2.0, 48.0]);
        assert!(is_pair(pair_id(7)) && !is_pair(Id::vocab(7).0) && !is_pair(Id::delta(7).0));
    }
}
