//! Coordinate reference systems: the built-in table (CRS84, EPSG:4326 and their 3D
//! forms, Web Mercator, the 120 UTM zones on WGS 84), IRI aliases and transforms between
//! the built-in CRSs. Pure data, compiled without the `geo` feature too.
//!
//! With the `geo-proj4` feature, an operator registers projected CRSs by proj4 strings
//! ([`register`], from a `crs.json` file), and they transform through `proj4rs`. With
//! `geo-epsg`, an EPSG code that is neither built in nor registered is looked up in the
//! EPSG-derived proj4 table of `crs-definitions` and registered on first use.
//!
//! Geometries are held in internal (east, north) order: (longitude, latitude) in degrees
//! for the geographic CRSs, (easting, northing) in metres for the projected ones. A
//! literal in a latitude-first CRS (EPSG:4326, EPSG:4979) is swapped on parse and
//! swapped back when written, so transforms between the geographic CRSs (all on WGS 84)
//! leave internal coordinates unchanged.

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

/// The default CRS of WKT literals without a CRS IRI (longitude, latitude on WGS 84).
pub const CRS84_IRI: &str = "http://www.opengis.net/def/crs/OGC/1.3/CRS84";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrsKind {
    Geographic2D,
    Geographic3D,
    Projected,
}

/// A built-in CRS: an index into the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CrsId(pub u8);

pub const CRS84: CrsId = CrsId(0);
/// CRS84 with ellipsoidal height
pub const CRS84H: CrsId = CrsId(1);
/// EPSG:4326, latitude first
pub const EPSG_4326: CrsId = CrsId(2);
/// EPSG:4979, latitude first, with ellipsoidal height
pub const EPSG_4979: CrsId = CrsId(3);
/// `http://www.opengis.net/def/crs/EPSG/4326` (no version): GeoSPARQL 1.0 examples use
/// it with longitude first, and Jena reads it as CRS84
pub const EPSG_4326_LEGACY: CrsId = CrsId(4);
/// EPSG:3857, spherical Web Mercator
pub const WEB_MERCATOR: CrsId = CrsId(5);

/// The first UTM zone: ids `UTM_FIRST..UTM_FIRST + 60` are the northern zones 1 to 60
/// (EPSG:32601 to 32660), the next 60 the southern ones (EPSG:32701 to 32760).
const UTM_FIRST: u8 = BASE.len() as u8;
/// UTM zones per hemisphere.
const UTM_ZONES: u8 = 60;

/// How a CRS maps to longitude/latitude on WGS 84.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Projection {
    /// geographic: coordinates are longitude/latitude
    None,
    /// spherical Mercator on the WGS 84 semi-major axis
    WebMercator,
    /// Universal Transverse Mercator on WGS 84: zone 1 to 60, northern or southern
    Utm { zone: u8, south: bool },
    /// a registered proj4 definition: its index in [`REGISTERED`]
    Registered(u8),
}

struct CrsDef {
    iri: Cow<'static, str>,
    kind: CrsKind,
    /// the literal's first axis is northing (latitude)
    lat_first: bool,
    projection: Projection,
}

/// The CRSs with entries of their own; the UTM zones follow them in [`TABLE`].
const BASE: [CrsDef; 6] = [
    CrsDef {
        iri: Cow::Borrowed(CRS84_IRI),
        kind: CrsKind::Geographic2D,
        lat_first: false,
        projection: Projection::None,
    },
    CrsDef {
        iri: Cow::Borrowed("http://www.opengis.net/def/crs/OGC/0/CRS84h"),
        kind: CrsKind::Geographic3D,
        lat_first: false,
        projection: Projection::None,
    },
    CrsDef {
        iri: Cow::Borrowed("http://www.opengis.net/def/crs/EPSG/0/4326"),
        kind: CrsKind::Geographic2D,
        lat_first: true,
        projection: Projection::None,
    },
    CrsDef {
        iri: Cow::Borrowed("http://www.opengis.net/def/crs/EPSG/0/4979"),
        kind: CrsKind::Geographic3D,
        lat_first: true,
        projection: Projection::None,
    },
    CrsDef {
        iri: Cow::Borrowed("http://www.opengis.net/def/crs/EPSG/4326"),
        kind: CrsKind::Geographic2D,
        lat_first: false,
        projection: Projection::None,
    },
    CrsDef {
        iri: Cow::Borrowed("http://www.opengis.net/def/crs/EPSG/0/3857"),
        kind: CrsKind::Projected,
        lat_first: false,
        projection: Projection::WebMercator,
    },
];

/// Every built-in CRS: [`BASE`], then the northern and the southern UTM zones.
static TABLE: LazyLock<Vec<CrsDef>> = LazyLock::new(|| {
    let mut t: Vec<CrsDef> = BASE.into_iter().collect();
    for south in [false, true] {
        for zone in 1..=UTM_ZONES {
            t.push(CrsDef {
                iri: Cow::Owned(format!("{EPSG_PREFIX}{}", utm_code(zone, south))),
                kind: CrsKind::Projected,
                lat_first: false,
                projection: Projection::Utm { zone, south },
            });
        }
    }
    t
});

const EPSG_PREFIX: &str = "http://www.opengis.net/def/crs/EPSG/0/";

/// The EPSG code of a UTM zone on WGS 84 (32601 to 32660 north, 32701 to 32760 south).
fn utm_code(zone: u8, south: bool) -> u32 {
    (if south { 32700 } else { 32600 }) + u32::from(zone)
}

/// Semi-major axis of WGS 84, the sphere radius of Web Mercator.
const WGS84_A: f64 = 6_378_137.0;

impl CrsId {
    fn def(self) -> &'static CrsDef {
        let i = usize::from(self.0);
        match TABLE.get(i) {
            Some(d) => d,
            None => {
                &REGISTERED[i - TABLE.len()]
                    .get()
                    .expect("an id is handed out after its CRS is registered")
                    .def
            }
        }
    }

    /// The UTM zone `zone` (1 to 60) of the northern or southern hemisphere on WGS 84
    /// (EPSG:326NN / 327NN).
    pub fn utm(zone: u8, south: bool) -> Option<CrsId> {
        (1..=UTM_ZONES)
            .contains(&zone)
            .then(|| CrsId(UTM_FIRST + zone - 1 + if south { UTM_ZONES } else { 0 }))
    }

    /// The canonical IRI.
    pub fn iri(self) -> &'static str {
        &self.def().iri
    }

    pub fn kind(self) -> CrsKind {
        self.def().kind
    }

    /// Longitude/latitude (degrees), as opposed to projected coordinates (metres).
    pub fn is_geographic(self) -> bool {
        self.kind() != CrsKind::Projected
    }

    /// The literal writes northing (latitude) first; internal coordinates are swapped.
    pub fn lat_first(self) -> bool {
        self.def().lat_first
    }

    /// Every built-in CRS.
    pub fn all() -> impl Iterator<Item = CrsId> {
        (0..BUILTIN).map(CrsId)
    }

    /// The index of a registered CRS among the registered ones (`None`: built in).
    pub fn registered_index(self) -> Option<usize> {
        usize::from(self.0).checked_sub(usize::from(BUILTIN))
    }
}

/// How many CRSs are built in: [`BASE`] and the UTM zones. Registered ids follow.
const BUILTIN: u8 = UTM_FIRST + 2 * UTM_ZONES;

/// The CRS of a geometry: a built-in one, or an IRI this build does not know (valid,
/// but only planar operations between geometries of that same CRS work).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CrsRef {
    Known(CrsId),
    Unknown(Arc<str>),
}

impl CrsRef {
    /// The IRI: the canonical one of a built-in CRS, else as written.
    pub fn iri(&self) -> &str {
        match self {
            CrsRef::Known(id) => id.iri(),
            CrsRef::Unknown(iri) => iri,
        }
    }

    pub fn known(&self) -> Option<CrsId> {
        match self {
            CrsRef::Known(id) => Some(*id),
            CrsRef::Unknown(_) => None,
        }
    }

    /// The CRS a literal names (`None`: no IRI, CRS84).
    pub fn from_iri(iri: Option<&str>) -> CrsRef {
        match iri {
            None => CrsRef::Known(CRS84),
            Some(iri) => match lookup(iri) {
                Some(id) => CrsRef::Known(id),
                None => CrsRef::Unknown(iri.into()),
            },
        }
    }
}

/// The built-in or registered CRS an IRI (or one of its aliases) names.
pub fn lookup(iri: &str) -> Option<CrsId> {
    let n = normalize(iri);
    if let Some(id) = builtin(&n) {
        return Some(id);
    }
    if let Some(id) = registry().read().ok()?.ids.get(&*n) {
        return Some(*id);
    }
    #[cfg(feature = "geo-epsg")]
    if let Some(id) = epsg::resolve(&n) {
        return Some(id);
    }
    None
}

/// The built-in CRS of a normalized IRI.
fn builtin(n: &str) -> Option<CrsId> {
    if let Some(i) = BASE.iter().position(|d| d.iri == n) {
        return Some(CrsId(i as u8));
    }
    // EPSG:326NN / 327NN, the UTM zones
    let code: u32 = n.strip_prefix(EPSG_PREFIX)?.parse().ok()?;
    match code {
        32601..=32660 => CrsId::utm((code - 32600) as u8, false),
        32701..=32760 => CrsId::utm((code - 32700) as u8, true),
        _ => None,
    }
}

// --------------------------------------------------------------- registered CRSs --

/// Most CRSs registered in one process (their ids follow the built-in ones in a `u8`).
pub const MAX_REGISTERED: usize = 128;

/// A CRS registered from a proj4 definition.
struct Registered {
    def: CrsDef,
    /// the definition as given, for [`registry_fingerprint`] and conflicts
    proj4: String,
    /// registered on first use from the build's EPSG table (`geo-epsg`), so the same in
    /// every process of this build
    auto: bool,
    /// how the definition's datum relates to WGS 84
    accuracy: Accuracy,
    /// how messages name the CRS: `EPSG:27700 (OSGB 1936 / British National Grid)` for one
    /// from the EPSG table, the IRI in angle brackets otherwise
    label: String,
    #[cfg(feature = "geo-proj4")]
    proj: proj4rs::Proj,
}

/// The registered CRSs by index. Slots are set once and never change, so ids handed out
/// stay valid and reads take no lock.
static REGISTERED: [std::sync::OnceLock<Registered>; MAX_REGISTERED] =
    [const { std::sync::OnceLock::new() }; MAX_REGISTERED];

#[derive(Default)]
struct Registry {
    /// normalized IRI → id
    ids: std::collections::HashMap<String, CrsId>,
    n: usize,
}

fn registry() -> &'static std::sync::RwLock<Registry> {
    static R: LazyLock<std::sync::RwLock<Registry>> = LazyLock::new(Default::default);
    &R
}

/// Why a CRS definition was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterError(pub String);

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegisterError {}

/// Register the projected CRS `iri` with the proj4 definition `proj4`, whose literals
/// write northing first when `lat_first`. Registering the same definition again returns
/// the same id. These are refused: a built-in CRS, a different definition for a
/// registered IRI, a geographic definition (`+proj=longlat`, since geographic CRSs other
/// than the built-in WGS 84 ones are not supported), a definition whose datum shift needs
/// grid files (`+nadgrids`, or `+datum=NAD27`), and one `proj4rs` cannot read. A
/// definition with a Helmert shift (`+towgs84`) is accepted, and [`approximation`]
/// describes it.
pub fn register(iri: &str, proj4: &str, lat_first: bool) -> Result<CrsId, RegisterError> {
    let label = format!("<{}>", iri.trim());
    register_as(iri, proj4, lat_first, None, label)
        .map_err(|m| RegisterError(format!("CRS <{iri}>: {m}")))
}

/// [`register`], with the reason of a refusal on its own. `auto` is the EPSG code of a
/// definition from the build's table.
fn register_as(
    iri: &str,
    proj4: &str,
    lat_first: bool,
    auto: Option<u16>,
    label: String,
) -> Result<CrsId, String> {
    if oxiri::Iri::parse(iri.trim()).is_err() && !iri.contains(':') {
        return Err("not an IRI".into());
    }
    let n = normalize(iri).into_owned();
    if builtin(&n).is_some() {
        return Err("is built in".into());
    }
    let proj4 = proj4.trim();
    let mut reg = registry().write().map_err(|_| "registry poisoned")?;
    if let Some(&id) = reg.ids.get(&n) {
        let r = &REGISTERED[id.registered_index().expect("registered")]
            .get()
            .expect("registered");
        return if r.proj4 == proj4 && r.def.lat_first == lat_first {
            Ok(id)
        } else {
            Err("is registered with another definition".into())
        };
    }
    if reg.n >= MAX_REGISTERED {
        return Err(format!("more than {MAX_REGISTERED} CRSs are registered"));
    }
    let accuracy = datum_accuracy(proj4)?;
    let i = reg.n;
    let id = CrsId(u8::try_from(usize::from(BUILTIN) + i).map_err(|_| "too many CRSs")?);
    let r = Registered {
        def: CrsDef {
            iri: Cow::Owned(n.clone()),
            kind: CrsKind::Projected,
            lat_first,
            projection: Projection::Registered(i as u8),
        },
        proj4: proj4.to_string(),
        auto: auto.is_some(),
        accuracy,
        label,
        #[cfg(feature = "geo-proj4")]
        proj: proj::read(proj4)?,
    };
    #[cfg(not(feature = "geo-proj4"))]
    {
        let _ = (r, id, &mut reg);
        Err("this build has no proj4 support (cargo feature \"geo-proj4\")".into())
    }
    #[cfg(feature = "geo-proj4")]
    {
        if REGISTERED[i].set(r).is_err() {
            return Err("registered concurrently".into());
        }
        reg.n += 1;
        reg.ids.insert(n, id);
        Ok(id)
    }
}

// ---------------------------------------------------------------- datum accuracy --

/// How the coordinates of a registered CRS relate to WGS 84, the datum every transform
/// goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accuracy {
    /// The datum is WGS 84, or a GRS 80 datum (ETRS89, NAD83, GDA94 and others) that
    /// Sparkles takes as WGS 84. Transforms are exact up to the projection formulas.
    Exact,
    /// The definition shifts its datum to WGS 84 by a 3- or 7-parameter Helmert
    /// transformation (`+towgs84`, or a proj4 datum that implies one). Transformed
    /// coordinates are approximate, typically within a few metres.
    Helmert,
    /// The definition is on another ellipsoid and gives no datum shift. Its coordinates
    /// are taken as WGS 84 ones, which can be tens or hundreds of metres off.
    NoDatumShift,
}

/// The proj4 datums whose definitions in `proj4rs` are Helmert shifts.
const HELMERT_DATUMS: [&str; 14] = [
    "ggrs87",
    "potsdam",
    "carthage",
    "hermannskogel",
    "ire65",
    "nzgd49",
    "osgb36",
    "ch1903",
    "osni52",
    "rassadiran",
    "s_jtsk",
    "beduaram",
    "gunung_segara",
    "rnb72",
];

/// How a proj4 definition's datum relates to WGS 84, by the precedence `proj4rs` gives
/// the parameters (`+nadgrids`, then `+towgs84`, then `+datum`). A definition whose shift
/// needs grid files is refused, with the reason.
pub fn datum_accuracy(proj4: &str) -> Result<Accuracy, String> {
    let params: Vec<(String, &str)> = proj4
        .split_whitespace()
        .map(|t| {
            let t = t.strip_prefix('+').unwrap_or(t);
            let (k, v) = t.split_once('=').unwrap_or((t, ""));
            (k.to_ascii_lowercase(), v)
        })
        .collect();
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
    let num = |k: &str| get(k).and_then(|v| v.parse::<f64>().ok());
    let datum = get("datum").map(str::to_ascii_lowercase);
    // the ellipsoid of WGS 84 or GRS 80 (whose datums Sparkles takes as WGS 84)
    let wgs84_ellipsoid = if let Some(e) = get("ellps") {
        e.eq_ignore_ascii_case("WGS84") || e.eq_ignore_ascii_case("GRS80")
    } else if let Some(d) = &datum {
        d == "wgs84" || d == "nad83"
    } else if let Some(a) = num("a") {
        a == WGS84_A
            && match (num("b"), num("rf")) {
                (Some(b), _) => (b - 6_356_752.314).abs() < 0.01,
                (None, Some(rf)) => (rf - 298.257_2).abs() < 1e-3,
                (None, None) => false,
            }
    } else {
        // `proj4rs` defaults to WGS 84
        get("r").is_none()
    };
    if let Some(grids) = get("nadgrids") {
        let named: Vec<&str> = grids
            .split(',')
            .map(|g| g.trim().trim_start_matches('@'))
            .filter(|g| !g.is_empty() && *g != "null")
            .collect();
        if !named.is_empty() {
            return Err(format!(
                "its datum shift needs the grid file{} {}, and Sparkles has no grid files",
                if named.len() == 1 { "" } else { "s" },
                named.join(", ")
            ));
        }
        // the null grid: no shift, the coordinates are WGS 84 ones (the Web Mercator
        // definitions on a sphere of WGS 84's semi-major axis among them)
        return Ok(if wgs84_ellipsoid || num("a") == Some(WGS84_A) {
            Accuracy::Exact
        } else {
            Accuracy::NoDatumShift
        });
    }
    if let Some(t) = get("towgs84") {
        let zero = t.split(',').all(|v| v.trim().parse::<f64>() == Ok(0.0));
        return Ok(if zero && wgs84_ellipsoid {
            Accuracy::Exact
        } else {
            Accuracy::Helmert
        });
    }
    match datum.as_deref() {
        Some("wgs84" | "nad83") => Ok(Accuracy::Exact),
        Some("nad27") => Err("its datum, NAD27, needs the grid files conus, alaska, \
             ntv2_0.gsb and ntv1_can.dat to shift to WGS 84, and Sparkles has no grid files"
            .into()),
        Some(d) if HELMERT_DATUMS.contains(&d) => Ok(Accuracy::Helmert),
        // a datum `proj4rs` does not know, which it refuses
        Some(_) => Ok(Accuracy::Helmert),
        None if wgs84_ellipsoid => Ok(Accuracy::Exact),
        None => Ok(Accuracy::NoDatumShift),
    }
}

/// How the registered CRS `id` relates to WGS 84 (built-in CRSs are exact).
pub fn accuracy(id: CrsId) -> Accuracy {
    id.registered_index()
        .and_then(|i| REGISTERED[i].get())
        .map_or(Accuracy::Exact, |r| r.accuracy)
}

/// Why transforms through `id` are approximate, for the plan's warnings (`None`:
/// exact).
pub fn approximation(id: CrsId) -> Option<String> {
    let r = REGISTERED[id.registered_index()?].get()?;
    match r.accuracy {
        Accuracy::Exact => None,
        Accuracy::Helmert => Some(format!(
            "{} shifts its datum to WGS 84 by a Helmert transformation (+towgs84), which \
             is approximate: coordinates transformed to or from other CRSs are typically \
             within a few metres of the official ones",
            r.label
        )),
        Accuracy::NoDatumShift => Some(format!(
            "{} gives no datum shift to WGS 84, so its coordinates are taken as WGS 84 \
             ones: coordinates transformed to or from other CRSs can be tens or hundreds of \
             metres off",
            r.label
        )),
    }
}

/// Why the EPSG CRS `iri` names is not available, when it is in the build's EPSG table
/// but was refused (`None`: available, or not an EPSG code of the table).
pub fn refusal(iri: &str) -> Option<String> {
    #[cfg(feature = "geo-epsg")]
    {
        let n = normalize(iri);
        if builtin(&n).is_some() {
            return None;
        }
        epsg::refusal(&n)
    }
    #[cfg(not(feature = "geo-epsg"))]
    {
        let _ = iri;
        None
    }
}

/// A hash of the definitions the operator registered (0 when there are none), part of the
/// spatial index's identity: literals in a CRS registered later are indexed by a
/// rebuild. CRSs of the build's EPSG table are left out, since every process of the
/// build has them.
pub fn registry_fingerprint() -> u64 {
    let Ok(reg) = registry().read() else {
        return 0;
    };
    let mut h: u64 = 0;
    for slot in REGISTERED.iter().take(reg.n) {
        if let Some(r) = slot.get().filter(|r| !r.auto) {
            let mut f = super::Fnv::new();
            f.field(r.def.iri.as_bytes());
            f.field(r.proj4.as_bytes());
            f.field(&[u8::from(r.def.lat_first)]);
            h = h.wrapping_add(f.finish());
        }
    }
    h
}

/// The registered CRSs: (IRI, proj4 definition, latitude first).
pub fn registered() -> Vec<(String, String, bool)> {
    let n = registry().read().map_or(0, |r| r.n);
    REGISTERED
        .iter()
        .take(n)
        .filter_map(|s| s.get())
        .map(|r| (r.def.iri.to_string(), r.proj4.clone(), r.def.lat_first))
        .collect()
}

/// One entry of a `crs.json` file.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrsFileEntry {
    pub proj4: String,
    /// `en` (easting first, the default) or `ne` (northing first)
    #[serde(default)]
    pub axis: Option<String>,
}

/// Register the CRSs of a `crs.json` file: `{ "<CRS IRI>": { "proj4": "+proj=…",
/// "axis": "en" | "ne" } }`. Returns how many it holds.
pub fn register_file(text: &str) -> Result<usize, RegisterError> {
    let entries: std::collections::BTreeMap<String, CrsFileEntry> =
        serde_json::from_str(text).map_err(|e| RegisterError(format!("crs.json: {e}")))?;
    for (iri, e) in &entries {
        let lat_first = match e.axis.as_deref().map(str::trim) {
            None | Some("en") | Some("EN") => false,
            Some("ne") | Some("NE") => true,
            Some(a) => {
                return Err(RegisterError(format!(
                    "CRS <{iri}>: axis {a:?} is not \"en\" or \"ne\""
                )));
            }
        };
        register(iri, &e.proj4, lat_first)?;
    }
    Ok(entries.len())
}

#[cfg(feature = "geo-proj4")]
mod proj {
    use super::REGISTERED;
    use std::sync::LazyLock;

    static WGS84: LazyLock<proj4rs::Proj> = LazyLock::new(|| {
        proj4rs::Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")
            .expect("the WGS 84 definition reads")
    });

    /// A projected CRS's proj4 definition, read.
    pub(super) fn read(proj4: &str) -> Result<proj4rs::Proj, String> {
        let p = proj4rs::Proj::from_proj_string(proj4)
            .map_err(|e| format!("the proj4 definition does not read: {e}"))?;
        if p.is_latlong() || p.is_geocent() {
            return Err(
                "only projected CRSs can be registered (geographic CRSs other than \
                        the built-in WGS 84 ones are not supported)"
                    .into(),
            );
        }
        if !p.has_forward() || !p.has_inverse() {
            return Err("the projection has no inverse".into());
        }
        Ok(p)
    }

    fn get(i: u8) -> Option<&'static proj4rs::Proj> {
        REGISTERED.get(usize::from(i))?.get().map(|r| &r.proj)
    }

    /// Projected coordinates of registered CRS `i` → longitude, latitude in degrees.
    pub(super) fn to_lonlat(i: u8, x: f64, y: f64) -> Option<(f64, f64)> {
        let mut p = (x, y, 0.0);
        proj4rs::transform::transform(get(i)?, &WGS84, &mut p).ok()?;
        Some((p.0.to_degrees(), p.1.to_degrees()))
    }

    /// Longitude, latitude in degrees → projected coordinates of registered CRS `i`.
    pub(super) fn from_lonlat(i: u8, lon: f64, lat: f64) -> Option<(f64, f64)> {
        let mut p = (lon.to_radians(), lat.to_radians(), 0.0);
        proj4rs::transform::transform(&WGS84, get(i)?, &mut p).ok()?;
        Some((p.0, p.1))
    }
}

#[cfg(not(feature = "geo-proj4"))]
mod proj {
    pub(super) fn to_lonlat(_: u8, _: f64, _: f64) -> Option<(f64, f64)> {
        None
    }
    pub(super) fn from_lonlat(_: u8, _: f64, _: f64) -> Option<(f64, f64)> {
        None
    }
}

/// EPSG codes from the EPSG-derived table of `crs-definitions`, registered on first use.
#[cfg(feature = "geo-epsg")]
mod epsg {
    use super::{CrsId, EPSG_PREFIX, register_as};
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    /// Codes that resolve to no CRS, so they are not tried again: those the table lacks
    /// (`None`), and those whose definition was refused, with the reason (geographic
    /// CRSs, and those whose datum shift needs grid files, among them).
    static MISSES: LazyLock<Mutex<HashMap<u16, Option<String>>>> = LazyLock::new(Default::default);

    pub(super) fn resolve(n: &str) -> Option<CrsId> {
        let code: u16 = n.strip_prefix(EPSG_PREFIX)?.parse().ok()?;
        if MISSES.lock().ok()?.contains_key(&code) {
            return None;
        }
        match definition(n, code) {
            Ok(id) => Some(id),
            Err(why) => {
                if let Ok(mut m) = MISSES.lock() {
                    m.insert(code, why);
                }
                None
            }
        }
    }

    /// Why the table's CRS of the normalized IRI `n` was refused.
    pub(super) fn refusal(n: &str) -> Option<String> {
        let code: u16 = n.strip_prefix(EPSG_PREFIX)?.parse().ok()?;
        if !MISSES.lock().ok()?.contains_key(&code) {
            // not tried yet (a registered CRS is found, and a miss is recorded)
            resolve(n)?;
        }
        MISSES.lock().ok()?.get(&code).cloned().flatten()
    }

    /// `EPSG:27700 (OSGB 1936 / British National Grid)`: the code and the name the
    /// definition's WKT gives.
    pub(super) fn label(code: u16, wkt: &str) -> String {
        match wkt.split('"').nth(1).filter(|n| !n.is_empty()) {
            Some(name) => format!("EPSG:{code} ({name})"),
            None => format!("EPSG:{code}"),
        }
    }

    /// Register the table's definition of `code` (`Err(None)`: not in the table).
    fn definition(n: &str, code: u16) -> Result<CrsId, Option<String>> {
        let def = crs_definitions::from_code(code).ok_or(None)?;
        let label = label(code, def.wkt);
        if def.proj4.trim().is_empty() {
            return Err(Some(format!(
                "{label} is not supported: the EPSG table has no proj4 definition of it"
            )));
        }
        // the projected axes as the definition's WKT gives them, easting first otherwise
        let lat_first = {
            let wkt = def.wkt;
            let first = wkt.find("AXIS[").map(|i| &wkt[i..]);
            first.is_some_and(|a| {
                a.split(']')
                    .next()
                    .is_some_and(|a| a.contains("NORTH") || a.contains("North"))
            })
        };
        register_as(n, def.proj4, lat_first, Some(code), label.clone())
            .map_err(|why| Some(format!("{label} is not supported: {why}")))
    }
}

/// The `http://www.opengis.net/def/crs/…` form of a CRS IRI: `https` → `http`, OGC URNs
/// (`urn:ogc:def:crs:EPSG::4326`) and short forms (`EPSG:4326`, `CRS:84`) → the
/// `http` form, any EPSG version → `0`, Web Mercator's old code 900913 → 3857. The
/// legacy `…/EPSG/4326` (no version) is a CRS of its own and stays.
pub fn normalize(iri: &str) -> Cow<'_, str> {
    const DEF: &str = "http://www.opengis.net/def/crs/";
    let iri = iri.trim();
    let rest = if let Some(r) = iri.strip_prefix(DEF) {
        Some(r)
    } else {
        iri.strip_prefix("https://www.opengis.net/def/crs/")
    };
    let (authority, version, code): (String, &str, &str) = if let Some(rest) = rest {
        let parts: Vec<&str> = rest.split('/').collect();
        match parts.as_slice() {
            [auth, ver, code] => (auth.to_string(), ver, code),
            // the legacy EPSG form without a version
            [auth, code] if auth.eq_ignore_ascii_case("EPSG") => {
                return Cow::Owned(format!("{DEF}EPSG/{code}"));
            }
            _ => return Cow::Borrowed(iri),
        }
    } else if let Some(rest) = strip_prefix_ci(iri, "urn:ogc:def:crs:") {
        // urn:ogc:def:crs:{authority}:{version}:{code}
        let parts: Vec<&str> = rest.split(':').collect();
        match parts.as_slice() {
            [auth, ver, code] => (auth.to_ascii_uppercase(), ver, code),
            _ => return Cow::Borrowed(iri),
        }
    } else if let Some(code) = strip_prefix_ci(iri, "EPSG:") {
        ("EPSG".into(), "0", code)
    } else if iri.eq_ignore_ascii_case("CRS:84") {
        ("OGC".into(), "1.3", "CRS84")
    } else {
        return Cow::Borrowed(iri);
    };
    match authority.as_str() {
        "EPSG" => {
            let code = if code == "900913" { "3857" } else { code };
            Cow::Owned(format!("{DEF}EPSG/0/{code}"))
        }
        "OGC" => {
            // CRS84 is defined in OGC version 1.3, CRS84h in version 0
            let (version, code) = if code.eq_ignore_ascii_case("CRS84") {
                ("1.3", "CRS84")
            } else if code.eq_ignore_ascii_case("CRS84h") {
                ("0", "CRS84h")
            } else if version.is_empty() {
                ("0", code)
            } else {
                (version, code)
            };
            Cow::Owned(format!("{DEF}OGC/{version}/{code}"))
        }
        _ => Cow::Borrowed(iri),
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Internal coordinates of `crs` → (longitude, latitude) in degrees (`None`: not finite).
pub fn to_lonlat(crs: CrsId, x: f64, y: f64) -> Option<(f64, f64)> {
    let (lon, lat) = match crs.def().projection {
        Projection::None => (x, y),
        Projection::WebMercator => (
            (x / WGS84_A).to_degrees(),
            (y / WGS84_A).sinh().atan().to_degrees(),
        ),
        Projection::Utm { zone, south } => tm::inverse(zone, south, x, y)?,
        Projection::Registered(i) => proj::to_lonlat(i, x, y)?,
    };
    (lon.is_finite() && lat.is_finite()).then_some((lon, lat))
}

/// (longitude, latitude) in degrees → internal coordinates of `crs` (`None`: outside
/// the projection's domain, such as a pole in Web Mercator).
pub fn from_lonlat(crs: CrsId, lon: f64, lat: f64) -> Option<(f64, f64)> {
    let (x, y) = match crs.def().projection {
        Projection::None => (lon, lat),
        Projection::WebMercator => {
            if lat.abs() >= 90.0 {
                return None;
            }
            (
                WGS84_A * lon.to_radians(),
                // = R ln tan(π/4 + φ/2), exact at the equator
                WGS84_A * lat.to_radians().tan().asinh(),
            )
        }
        Projection::Utm { zone, south } => tm::forward(zone, south, lon, lat)?,
        Projection::Registered(i) => proj::from_lonlat(i, lon, lat)?,
    };
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// A box `[min_x, min_y, max_x, max_y]` in internal coordinates of `crs` → a box in
/// (longitude, latitude) that holds the image of every point inside it (`None`: not
/// finite). Web Mercator maps each axis monotonically, so its corners suffice; the
/// meridians and parallels of transverse Mercator curve, so its edges are sampled and
/// the result is widened by more than the curves can bulge between samples.
pub fn box_to_lonlat(crs: CrsId, b: [f64; 4]) -> Option<[f64; 4]> {
    match crs.def().projection {
        Projection::None | Projection::WebMercator => {
            let (x0, y0) = to_lonlat(crs, b[0], b[1])?;
            let (x1, y1) = to_lonlat(crs, b[2], b[3])?;
            Some([x0, y0, x1, y1])
        }
        Projection::Utm { .. } | Projection::Registered(_) => {
            const STEPS: usize = 64;
            let mut out = [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ];
            for i in 0..=STEPS {
                let t = i as f64 / STEPS as f64;
                let (x, y) = (b[0] + (b[2] - b[0]) * t, b[1] + (b[3] - b[1]) * t);
                for (px, py) in [(x, b[1]), (x, b[3]), (b[0], y), (b[2], y)] {
                    let (lon, lat) = to_lonlat(crs, px, py)?;
                    out = [
                        out[0].min(lon),
                        out[1].min(lat),
                        out[2].max(lon),
                        out[3].max(lat),
                    ];
                }
            }
            let pad = 0.01 * (out[2] - out[0]).max(out[3] - out[1]) / STEPS as f64 + 1e-9;
            Some([
                out[0] - pad,
                (out[1] - pad).max(-90.0),
                out[2] + pad,
                (out[3] + pad).min(90.0),
            ])
        }
    }
}

/// Internal coordinates of `from` → internal coordinates of `to`.
pub fn transform(from: CrsId, to: CrsId, x: f64, y: f64) -> Option<(f64, f64)> {
    if from.def().projection == to.def().projection {
        return Some((x, y));
    }
    let (lon, lat) = to_lonlat(from, x, y)?;
    from_lonlat(to, lon, lat)
}

/// The transverse Mercator projection of the WGS 84 ellipsoid by Krüger's series to the
/// sixth order in the third flattening `n` (Karney, "Transverse Mercator with an
/// accuracy of a few nanometers", J. Geodesy 85, 2011, eqs. 7–36): far below a
/// millimetre within 4,000 km of the central meridian.
mod tm {
    use super::WGS84_A;
    use std::sync::LazyLock;

    /// Flattening of WGS 84.
    const F: f64 = 1.0 / 298.257_223_563;
    /// Scale on the central meridian of UTM.
    const K0: f64 = 0.9996;
    const FALSE_EASTING: f64 = 500_000.0;
    /// The false northing of the southern zones.
    const FALSE_NORTHING_SOUTH: f64 = 10_000_000.0;
    /// The farthest longitude from the central meridian that projects (toward 90° the
    /// series lose their accuracy, and the projection its use).
    const MAX_DLON: f64 = 60.0;

    struct Series {
        /// eccentricity
        e: f64,
        /// the rectifying radius times `K0`
        ka: f64,
        alpha: [f64; 6],
        beta: [f64; 6],
    }

    static SERIES: LazyLock<Series> = LazyLock::new(|| {
        let n = F / (2.0 - F);
        let (n2, n3) = (n * n, n * n * n);
        let (n4, n5, n6) = (n2 * n2, n2 * n3, n3 * n3);
        let a = WGS84_A / (1.0 + n) * (1.0 + n2 / 4.0 + n4 / 64.0 + n6 / 256.0);
        Series {
            e: (F * (2.0 - F)).sqrt(),
            ka: K0 * a,
            alpha: [
                n / 2.0 - 2.0 / 3.0 * n2 + 5.0 / 16.0 * n3 + 41.0 / 180.0 * n4 - 127.0 / 288.0 * n5
                    + 7891.0 / 37800.0 * n6,
                13.0 / 48.0 * n2 - 3.0 / 5.0 * n3 + 557.0 / 1440.0 * n4 + 281.0 / 630.0 * n5
                    - 1_983_433.0 / 1_935_360.0 * n6,
                61.0 / 240.0 * n3 - 103.0 / 140.0 * n4
                    + 15061.0 / 26880.0 * n5
                    + 167_603.0 / 181_440.0 * n6,
                49561.0 / 161_280.0 * n4 - 179.0 / 168.0 * n5 + 6_601_661.0 / 7_257_600.0 * n6,
                34729.0 / 80640.0 * n5 - 3_418_889.0 / 1_995_840.0 * n6,
                212_378_941.0 / 319_334_400.0 * n6,
            ],
            beta: [
                n / 2.0 - 2.0 / 3.0 * n2 + 37.0 / 96.0 * n3 - 1.0 / 360.0 * n4 - 81.0 / 512.0 * n5
                    + 96199.0 / 604_800.0 * n6,
                1.0 / 48.0 * n2 + 1.0 / 15.0 * n3 - 437.0 / 1440.0 * n4 + 46.0 / 105.0 * n5
                    - 1_118_711.0 / 3_870_720.0 * n6,
                17.0 / 480.0 * n3 - 37.0 / 840.0 * n4 - 209.0 / 4480.0 * n5 + 5569.0 / 90720.0 * n6,
                4397.0 / 161_280.0 * n4 - 11.0 / 504.0 * n5 - 830_251.0 / 7_257_600.0 * n6,
                4583.0 / 161_280.0 * n5 - 108_847.0 / 3_991_680.0 * n6,
                20_648_693.0 / 638_668_800.0 * n6,
            ],
        }
    });

    /// The longitude of a zone's central meridian.
    fn central_meridian(zone: u8) -> f64 {
        f64::from(zone) * 6.0 - 183.0
    }

    /// tan of the conformal latitude from tan of the geodetic latitude (Karney eq. 7).
    fn conformal(tau: f64, e: f64) -> f64 {
        let sigma = (e * (e * tau / tau.hypot(1.0)).atanh()).sinh();
        tau * sigma.hypot(1.0) - sigma * tau.hypot(1.0)
    }

    /// (longitude, latitude) in degrees → (easting, northing) in metres.
    pub(super) fn forward(zone: u8, south: bool, lon: f64, lat: f64) -> Option<(f64, f64)> {
        let s = &*SERIES;
        let dlon = (lon - central_meridian(zone) + 540.0).rem_euclid(360.0) - 180.0;
        if !(lat.abs() <= 90.0 && dlon.abs() <= MAX_DLON) {
            return None;
        }
        let (phi, lam) = (lat.to_radians(), dlon.to_radians());
        let tau_p = conformal(phi.tan(), s.e);
        let xi_p = tau_p.atan2(lam.cos());
        let eta_p = (lam.sin() / tau_p.hypot(lam.cos())).asinh();
        let (mut xi, mut eta) = (xi_p, eta_p);
        for (j, a) in s.alpha.iter().enumerate() {
            let k = 2.0 * (j + 1) as f64;
            xi += a * (k * xi_p).sin() * (k * eta_p).cosh();
            eta += a * (k * xi_p).cos() * (k * eta_p).sinh();
        }
        let north = if south { FALSE_NORTHING_SOUTH } else { 0.0 };
        Some((FALSE_EASTING + s.ka * eta, north + s.ka * xi))
    }

    /// (easting, northing) in metres → (longitude, latitude) in degrees.
    pub(super) fn inverse(zone: u8, south: bool, x: f64, y: f64) -> Option<(f64, f64)> {
        let s = &*SERIES;
        let north = if south { FALSE_NORTHING_SOUTH } else { 0.0 };
        let (xi, eta) = ((y - north) / s.ka, (x - FALSE_EASTING) / s.ka);
        if !(xi.is_finite() && eta.is_finite()) {
            return None;
        }
        let (mut xi_p, mut eta_p) = (xi, eta);
        for (j, b) in s.beta.iter().enumerate() {
            let k = 2.0 * (j + 1) as f64;
            xi_p -= b * (k * xi).sin() * (k * eta).cosh();
            eta_p -= b * (k * xi).cos() * (k * eta).sinh();
        }
        let tau_p = xi_p.sin() / eta_p.sinh().hypot(xi_p.cos());
        let lam = eta_p.sinh().atan2(xi_p.cos());
        // Newton's method for the geodetic latitude (Karney eqs. 19–21)
        let e2 = s.e * s.e;
        let mut tau = tau_p;
        for _ in 0..8 {
            let tp = conformal(tau, s.e);
            let d = (tau_p - tp) / tp.hypot(1.0) * (1.0 + (1.0 - e2) * tau * tau)
                / ((1.0 - e2) * tau.hypot(1.0));
            tau += d;
            if d.abs() <= 1e-14 * tau.abs().max(1.0) {
                break;
            }
        }
        let lon = central_meridian(zone) + lam.to_degrees();
        Some((lon, tau.atan().to_degrees()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases() {
        let epsg = "http://www.opengis.net/def/crs/EPSG/0/4326";
        for a in [
            epsg,
            "https://www.opengis.net/def/crs/EPSG/0/4326",
            "urn:ogc:def:crs:EPSG::4326",
            "urn:ogc:def:crs:EPSG:6.6:4326",
            "URN:OGC:DEF:CRS:epsg::4326",
            "EPSG:4326",
            "http://www.opengis.net/def/crs/EPSG/9.9.1/4326",
        ] {
            assert_eq!(lookup(a), Some(EPSG_4326), "{a}");
        }
        for a in [
            CRS84_IRI,
            "https://www.opengis.net/def/crs/OGC/1.3/CRS84",
            "urn:ogc:def:crs:OGC:1.3:CRS84",
            "urn:ogc:def:crs:OGC::CRS84",
            "http://www.opengis.net/def/crs/OGC/0/CRS84",
            "CRS:84",
        ] {
            assert_eq!(lookup(a), Some(CRS84), "{a}");
        }
        assert_eq!(lookup("urn:ogc:def:crs:OGC::CRS84h"), Some(CRS84H));
        assert_eq!(lookup("EPSG:4979"), Some(EPSG_4979));
        assert_eq!(
            lookup("http://www.opengis.net/def/crs/EPSG/4326"),
            Some(EPSG_4326_LEGACY)
        );
        assert!(!EPSG_4326_LEGACY.lat_first());
        for a in [
            "http://www.opengis.net/def/crs/EPSG/0/3857",
            "http://www.opengis.net/def/crs/EPSG/0/900913",
            "EPSG:900913",
            "urn:ogc:def:crs:EPSG::3857",
        ] {
            assert_eq!(lookup(a), Some(WEB_MERCATOR), "{a}");
        }
        assert_eq!(lookup("http://example.org/crs/mars"), None);
        #[cfg(not(feature = "geo-epsg"))]
        assert_eq!(lookup("http://www.opengis.net/def/crs/EPSG/0/27700"), None);
        assert_eq!(lookup("EPSG"), None);
    }

    /// A CRS registered from a proj4 string transforms like the built-in one it
    /// duplicates (UTM zone 31 north), and a national grid round-trips.
    #[cfg(feature = "geo-proj4")]
    #[test]
    fn registered_crss_transform() {
        let utm = register(
            "http://example.org/crs/test-utm31",
            "+proj=utm +zone=31 +datum=WGS84 +units=m +no_defs",
            false,
        )
        .unwrap();
        assert_eq!(lookup("http://example.org/crs/test-utm31"), Some(utm));
        assert_eq!(utm.kind(), CrsKind::Projected);
        let builtin = CrsId::utm(31, false).unwrap();
        for (lon, lat) in [(3.0, 0.0), (2.35, 48.85), (0.5, 60.0), (5.9, -10.0)] {
            let a = from_lonlat(utm, lon, lat).unwrap();
            let b = from_lonlat(builtin, lon, lat).unwrap();
            assert!(
                (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3,
                "{a:?} {b:?}"
            );
            let (lo, la) = to_lonlat(utm, a.0, a.1).unwrap();
            assert!((lo - lon).abs() < 1e-9 && (la - lat).abs() < 1e-9);
            assert_eq!(
                transform(utm, CRS84, a.0, a.1).map(|p| p.0.round()),
                Some(lon.round())
            );
        }
        // the same definition again is the same CRS; another one is refused
        assert_eq!(
            register(
                "http://example.org/crs/test-utm31",
                "+proj=utm +zone=31 +datum=WGS84 +units=m +no_defs",
                false
            ),
            Ok(utm)
        );
        assert!(register("http://example.org/crs/test-utm31", "+proj=merc", false).is_err());
        // built-in, geographic and unreadable definitions are refused
        assert!(register("EPSG:32631", "+proj=utm +zone=31 +datum=WGS84", false).is_err());
        assert!(
            register(
                "http://example.org/crs/test-ll",
                "+proj=longlat +ellps=GRS80",
                false
            )
            .is_err()
        );
        assert!(register("http://example.org/crs/test-bad", "+proj=nonsense", false).is_err());
        // the British National Grid (OSGB 1936, a datum shift by +towgs84)
        let n = register_file(
            r#"{"http://example.org/crs/test-bng": {"proj4": "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.9996012717 +x_0=400000 +y_0=-100000 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs", "axis": "en"}}"#,
        )
        .unwrap();
        assert_eq!(n, 1);
        let bng = lookup("http://example.org/crs/test-bng").unwrap();
        // Greenwich: about 538,900 E, 177,300 N
        let (e, nn) = from_lonlat(bng, 0.0, 51.4779).unwrap();
        assert!(
            (538_000.0..540_000.0).contains(&e) && (176_500.0..178_500.0).contains(&nn),
            "{e} {nn}"
        );
        let (lon, lat) = to_lonlat(bng, e, nn).unwrap();
        assert!(lon.abs() < 1e-7 && (lat - 51.4779).abs() < 1e-7);
        assert!(
            registered()
                .iter()
                .any(|(i, _, _)| i == "http://example.org/crs/test-bng")
        );
        assert!(registry_fingerprint() != 0);
        assert!(
            register_file(r#"{"http://example.org/crs/x": {"proj4": "+proj=merc", "axis": "up"}}"#)
                .is_err()
        );
    }

    /// With the EPSG table, projected EPSG codes resolve on first use.
    #[cfg(feature = "geo-epsg")]
    #[test]
    fn epsg_codes_resolve() {
        let bng = lookup("http://www.opengis.net/def/crs/EPSG/0/27700").unwrap();
        assert_eq!(lookup("EPSG:27700"), Some(bng));
        let (e, n) = from_lonlat(bng, 0.0, 51.4779).unwrap();
        assert!((538_000.0..540_000.0).contains(&e) && (176_500.0..178_500.0).contains(&n));
        // Lambert-93
        assert!(lookup("urn:ogc:def:crs:EPSG::2154").is_some());
        // geographic codes and unknown ones do not
        assert_eq!(lookup("EPSG:4258"), None);
        assert_eq!(lookup("EPSG:4258"), None);
        assert_eq!(lookup("EPSG:1"), None);
        // and they leave no fingerprint
        assert!(
            !registered().iter().any(|(i, _, _)| i.ends_with("/27700")) || {
                let before = registry_fingerprint();
                lookup("EPSG:3035");
                registry_fingerprint() == before
            }
        );
    }

    /// The datum handling of proj4 definitions.
    #[test]
    fn datum_accuracies() {
        use Accuracy::*;
        for (def, want) in [
            ("+proj=utm +zone=32 +datum=WGS84 +units=m", Exact),
            (
                "+proj=utm +zone=32 +ellps=GRS80 +towgs84=0,0,0,0,0,0,0",
                Exact,
            ),
            ("+proj=aea +lat_1=29.5 +lat_2=45.5 +datum=NAD83", Exact),
            ("+proj=laea +lat_0=52 +lon_0=10 +ellps=GRS80", Exact),
            ("+proj=merc +lon_0=0", Exact),
            (
                "+proj=merc +a=6378137 +b=6378137 +nadgrids=@null +wktext +no_defs",
                Exact,
            ),
            ("+proj=tmerc +a=6378137 +rf=298.257222101 +k=0.9996", Exact),
            ("+proj=tmerc +lat_0=49 +lon_0=-2 +datum=OSGB36", Helmert),
            (
                "+proj=sterea +ellps=bessel +towgs84=565.2369,50.0087,465.658,-0.406857,0.350733,-1.87035,4.0812",
                Helmert,
            ),
            (
                "+proj=somerc +ellps=bessel +towgs84=674.374,15.056,405.346",
                Helmert,
            ),
            ("+proj=lcc +ellps=intl +towgs84=0,0,0,0,0,0,0", Helmert),
            (
                "+proj=utm +zone=17 +datum=NAD27 +towgs84=-8,160,176",
                Helmert,
            ),
            (
                "+proj=tmerc +lon_0=-62 +ellps=clrk80 +units=m",
                NoDatumShift,
            ),
            ("+proj=laea +a=6370997 +b=6370997", NoDatumShift),
            ("+proj=merc +R=6371000", NoDatumShift),
        ] {
            assert_eq!(datum_accuracy(def), Ok(want), "{def}");
        }
        let e = datum_accuracy("+proj=utm +zone=17 +datum=NAD27 +units=m").unwrap_err();
        assert!(e.contains("NAD27") && e.contains("grid files"), "{e}");
        let e = datum_accuracy(
            "+proj=nzmg +datum=nzgd49 +towgs84=59.47,-5.04,187.44,0.47,-0.1,1.024,-4.5993 +nadgrids=nzgd2kgrid0005.gsb",
        )
        .unwrap_err();
        assert!(e.contains("grid file nzgd2kgrid0005.gsb"), "{e}");
        let e = datum_accuracy("+proj=tmerc +nadgrids=@a.gsb,b.gsb").unwrap_err();
        assert!(e.contains("grid files a.gsb, b.gsb"), "{e}");
        assert!(accuracy(CRS84) == Exact && accuracy(WEB_MERCATOR) == Exact);
        assert_eq!(approximation(CRS84), None);
    }

    /// `((lon, lat), (easting, northing))`
    #[cfg(feature = "geo-epsg")]
    type RefPoint = ((f64, f64), (f64, f64));

    /// A projected CRS from the EPSG table against PROJ 9.9: `(lon, lat)` on ETRS89 or
    /// RGF93 (taken as WGS 84) and the easting and northing PROJ gives.
    #[cfg(feature = "geo-epsg")]
    fn check_points(code: &str, points: &[RefPoint], tolerance: f64) {
        let id = lookup(code).unwrap_or_else(|| panic!("{code}: {:?}", refusal(code)));
        for &((lon, lat), (e, n)) in points {
            let (x, y) = from_lonlat(id, lon, lat).unwrap();
            let off = (x - e).hypot(y - n);
            assert!(
                off < tolerance,
                "{code} {lon} {lat}: {x} {y} is {off} m off"
            );
            eprintln!("{code} {lon} {lat}: {off:.3} m from the reference");
            // the round trip, to about a centimetre (the inverse Helmert shift iterates)
            let (lo, la) = to_lonlat(id, x, y).unwrap();
            assert!(
                (lo - lon).abs() < 1e-7 && (la - lat).abs() < 1e-7,
                "{code}: {lo} {la}"
            );
        }
    }

    /// EPSG codes against reference values. PROJ 9.9 (`cs2cs`, with `PROJ_NETWORK=ON`)
    /// gave them, through the national grids where they exist: OSTN15 for the British
    /// National Grid and RDNAPTRANS 2018 for RD New.
    #[cfg(feature = "geo-epsg")]
    #[test]
    fn epsg_reference_points() {
        // Lambert-93 and ETRS89 / UTM 32N are on GRS 80 datums: exact to the millimetre
        check_points(
            "EPSG:2154",
            &[((2.3488, 48.8534), (652_216.626, 6_861_681.5))],
            0.005,
        );
        check_points(
            "EPSG:25832",
            &[((8.6821, 50.1109), (477_269.508, 5_551_009.575))],
            0.005,
        );
        for c in ["EPSG:2154", "EPSG:25832"] {
            let id = lookup(c).unwrap();
            assert_eq!(accuracy(id), Accuracy::Exact, "{c}");
            assert_eq!(approximation(id), None, "{c}");
        }
        // the British National Grid shifts OSGB36 by a 7-parameter Helmert
        // transformation: within 5 m of OSTN15 (Greenwich, Edinburgh, Land's End)
        check_points(
            "EPSG:27700",
            &[
                ((0.0, 51.4779), (538_985.228, 177_334.188)),
                ((-3.1883, 55.9533), (325_897.321, 674_001.742)),
                ((-5.7147, 50.0657), (134_266.494, 25_011.147)),
            ],
            5.0,
        );
        // RD New shifts Amersfoort by one too: within 1 m of RDNAPTRANS 2018
        // (Amsterdam, Eindhoven)
        check_points(
            "EPSG:28992",
            &[
                ((4.8924, 52.3731), (121_304.185, 487_362.172)),
                ((5.4697, 51.4416), (160_735.892, 383_614.797)),
            ],
            1.0,
        );
        for c in ["EPSG:27700", "EPSG:28992"] {
            let id = lookup(c).unwrap();
            assert_eq!(accuracy(id), Accuracy::Helmert, "{c}");
            let w = approximation(id).unwrap();
            assert!(w.contains("Helmert") && w.contains(&c[5..]), "{w}");
        }
        assert!(
            approximation(lookup("EPSG:27700").unwrap())
                .unwrap()
                .starts_with("EPSG:27700 (OSGB 1936 / British National Grid)")
        );
        // a definition without a datum shift (Anguilla 1957)
        let id = lookup("EPSG:2000").unwrap();
        assert_eq!(accuracy(id), Accuracy::NoDatumShift);
        // definitions whose shift needs grids are refused, and the reason is kept
        for (c, why) in [
            ("EPSG:26717", "NAD27"),
            ("urn:ogc:def:crs:EPSG::27200", "nzgd2kgrid0005.gsb"),
            ("EPSG:4258", "geographic"),
        ] {
            assert_eq!(lookup(c), None, "{c}");
            let r = refusal(c).unwrap_or_else(|| panic!("{c}"));
            assert!(
                r.contains(why) && r.contains("is not supported"),
                "{c}: {r}"
            );
        }
        assert!(
            refusal("EPSG:26717")
                .unwrap()
                .starts_with("EPSG:26717 (NAD27 / UTM zone 17N) is not supported")
        );
        // codes outside the table, built-in ones and registered ones have no refusal
        for c in ["EPSG:1", "EPSG:4326", "EPSG:32631", "EPSG:27700"] {
            assert_eq!(refusal(c), None, "{c}");
        }
    }

    /// Every definition of the EPSG table either reads with a known accuracy or is
    /// refused for a reason. Prints the counts the docs give.
    #[cfg(feature = "geo-epsg")]
    #[test]
    fn epsg_table_census() {
        let mut counts = std::collections::BTreeMap::<&str, u32>::new();
        for code in 0..=u16::MAX {
            let Some(def) = crs_definitions::from_code(code) else {
                continue;
            };
            let kind = match datum_accuracy(def.proj4) {
                Err(e) => {
                    assert!(e.contains("grid file"), "{code}: {e}");
                    "refused: grid files"
                }
                Ok(a) => match proj::read(def.proj4) {
                    Err(_) if def.proj4.contains("+proj=longlat") => "refused: geographic",
                    Err(_) if def.proj4.contains("+proj=geocent") => "refused: geocentric",
                    Err(_) => "refused: unreadable",
                    Ok(_) => match a {
                        Accuracy::Exact => "exact",
                        Accuracy::Helmert => "Helmert",
                        Accuracy::NoDatumShift => "no datum shift",
                    },
                },
            };
            *counts.entry(kind).or_default() += 1;
        }
        eprintln!("EPSG table: {counts:?}");
        assert!(counts["refused: grid files"] > 200);
        assert!(counts["exact"] > 3000 && counts["Helmert"] > 1000);
    }

    #[test]
    fn table() {
        assert_eq!(CRS84.iri(), CRS84_IRI);
        assert!(EPSG_4326.lat_first() && EPSG_4979.lat_first() && !CRS84.lat_first());
        assert_eq!(EPSG_4979.kind(), CrsKind::Geographic3D);
        assert!(!WEB_MERCATOR.is_geographic());
        for id in CrsId::all() {
            assert_eq!(lookup(id.iri()), Some(id));
        }
        assert_eq!(
            CrsRef::from_iri(Some("http://example.org/crs/mars")).iri(),
            "http://example.org/crs/mars"
        );
        assert_eq!(CrsRef::from_iri(None), CrsRef::Known(CRS84));
    }

    #[test]
    fn utm_zones() {
        let z33 = lookup("http://www.opengis.net/def/crs/EPSG/0/32633").unwrap();
        assert_eq!(Some(z33), CrsId::utm(33, false));
        assert_eq!(z33.iri(), "http://www.opengis.net/def/crs/EPSG/0/32633");
        assert_eq!(z33.kind(), CrsKind::Projected);
        assert!(!z33.is_geographic() && !z33.lat_first());
        for a in ["EPSG:32760", "urn:ogc:def:crs:EPSG::32760"] {
            assert_eq!(lookup(a), CrsId::utm(60, true), "{a}");
        }
        assert_eq!(lookup("EPSG:32601"), CrsId::utm(1, false));
        for a in ["EPSG:32600", "EPSG:32661", "EPSG:32700", "EPSG:32761"] {
            assert!(builtin(&normalize(a)).is_none(), "{a}");
        }
        assert_eq!(CrsId::utm(0, false), None);
        assert_eq!(CrsId::utm(61, true), None);
        assert_eq!(CrsId::all().count(), 126);
    }

    /// Forward and inverse UTM against published values and the WGS 84 meridian arc.
    #[test]
    #[cfg(feature = "geo")]
    fn utm_reference_points() {
        // GeographicLib's GeoConvert documentation: 33.3°N 44.4°E is 38n 444140.54
        // 3684706.36 (to the centimetre it prints)
        let z38 = CrsId::utm(38, false).unwrap();
        let (x, y) = from_lonlat(z38, 44.4, 33.3).unwrap();
        assert!((x - 444_140.54).abs() < 0.005, "{x}");
        assert!((y - 3_684_706.36).abs() < 0.005, "{y}");
        // on the central meridian the northing is 0.9996 times the meridian arc from the
        // equator (the WGS 84 geodesic from 0°N to 50°N along it)
        let arc = crate::geo::ops::distance::geodesic(
            georust::Coord { x: 3.0, y: 0.0 },
            georust::Coord { x: 3.0, y: 50.0 },
        );
        let z31 = CrsId::utm(31, false).unwrap();
        let (x, y) = from_lonlat(z31, 3.0, 50.0).unwrap();
        assert!((x - 500_000.0).abs() < 1e-6, "{x}");
        assert!((y - 0.9996 * arc).abs() < 0.001, "{y} {arc}");
        // the southern zones: a false northing of 10,000 km, mirrored
        let s31 = CrsId::utm(31, true).unwrap();
        let (xs, ys) = from_lonlat(s31, 5.5, -50.0).unwrap();
        let (xn, yn) = from_lonlat(z31, 5.5, 50.0).unwrap();
        assert!((xs - xn).abs() < 1e-6 && (ys - (10_000_000.0 - yn)).abs() < 1e-6);
        // round trips stay under a micrometre, in the zone and well beyond it
        for (lon, lat) in [
            (44.4, 33.3),
            (39.0, 0.0),
            (47.9, 84.0),
            (36.1, -79.9),
            (50.0, 10.0),
            (25.0, -45.0),
        ] {
            for south in [false, true] {
                let z = CrsId::utm(38, south).unwrap();
                let (x, y) = from_lonlat(z, lon, lat).unwrap();
                let (lon2, lat2) = to_lonlat(z, x, y).unwrap();
                let (x2, y2) = from_lonlat(z, lon2, lat2).unwrap();
                assert!(
                    (x - x2).abs() < 1e-6 && (y - y2).abs() < 1e-6,
                    "{lon} {lat}: {x2} {y2}"
                );
                assert!((lon - lon2).abs() < 1e-11 && (lat - lat2).abs() < 1e-11);
            }
        }
        // far from the central meridian there are no coordinates
        assert!(from_lonlat(z38, 45.0 + 61.0, 0.0).is_none());
        assert!(from_lonlat(z38, -135.0, 0.0).is_none());
        // between UTM and the other CRSs
        let (x, y) = transform(EPSG_4326, z38, 44.4, 33.3).unwrap();
        let (mx, my) = transform(z38, WEB_MERCATOR, x, y).unwrap();
        let (lon, lat) = to_lonlat(WEB_MERCATOR, mx, my).unwrap();
        assert!((lon - 44.4).abs() < 1e-9 && (lat - 33.3).abs() < 1e-9);
        let (x2, y2) = transform(z38, CrsId::utm(37, false).unwrap(), x, y).unwrap();
        assert!(x2 > 500_000.0 && (y2 - y).abs() > 1.0);
    }

    /// A UTM box in longitude/latitude holds every point of the box, although its edges
    /// curve (a parallel bulges between the corners).
    #[test]
    fn utm_boxes_in_lonlat() {
        let z = CrsId::utm(32, false).unwrap();
        let b = [200_000.0, 5_000_000.0, 800_000.0, 6_000_000.0];
        let ll = box_to_lonlat(z, b).unwrap();
        let corners = [
            to_lonlat(z, b[0], b[1]).unwrap(),
            to_lonlat(z, b[2], b[3]).unwrap(),
        ];
        // the corners alone would miss the bulge of the northern edge
        assert!(ll[3] > corners[1].1 + 0.01, "{ll:?} {corners:?}");
        for i in 0..=20 {
            for j in 0..=20 {
                let x = b[0] + (b[2] - b[0]) * f64::from(i) / 20.0;
                let y = b[1] + (b[3] - b[1]) * f64::from(j) / 20.0;
                let (lon, lat) = to_lonlat(z, x, y).unwrap();
                assert!(
                    lon >= ll[0] && lon <= ll[2] && lat >= ll[1] && lat <= ll[3],
                    "{x} {y}: {lon} {lat} outside {ll:?}"
                );
            }
        }
    }

    #[test]
    fn web_mercator() {
        // the half circumference of the WGS 84 equator, and the latitude where the
        // square Web Mercator world ends
        let (x, y) = from_lonlat(WEB_MERCATOR, 180.0, 0.0).unwrap();
        assert!((x - 20_037_508.342_789_244).abs() < 1e-6 && y.abs() < 1e-9);
        let (_, y) = from_lonlat(WEB_MERCATOR, 0.0, 85.051_128_779_806_59).unwrap();
        assert!((y - 20_037_508.342_789_244).abs() < 1e-3, "{y}");
        let (lon, lat) = to_lonlat(WEB_MERCATOR, x, y).unwrap();
        assert!((lon - 180.0).abs() < 1e-9 && (lat - 85.051_128_779_806_59).abs() < 1e-9);
        assert!(from_lonlat(WEB_MERCATOR, 0.0, 90.0).is_none());
        let (x, y) = transform(EPSG_4326, WEB_MERCATOR, 2.3522, 48.8566).unwrap();
        let (lon, lat) = transform(WEB_MERCATOR, CRS84, x, y).unwrap();
        assert!((lon - 2.3522).abs() < 1e-9 && (lat - 48.8566).abs() < 1e-9);
        assert_eq!(transform(EPSG_4326, CRS84, 1.0, 2.0), Some((1.0, 2.0)));
    }
}
