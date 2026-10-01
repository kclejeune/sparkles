//! GeoSPARQL and Jena spatial IRIs: namespaces, datatypes, the topological relations and
//! the `spatial:` property functions. Pure data, compiled with or without the `geo`
//! feature, so the planner recognizes the terms either way.

/// GeoSPARQL ontology (`geo:`): datatypes, properties, topological properties.
pub const GEO: &str = "http://www.opengis.net/ont/geosparql#";
/// GeoSPARQL functions (`geof:`).
pub const GEOF: &str = "http://www.opengis.net/def/function/geosparql/";
/// Simple Features geometry types (`sf:`), the results of `geof:geometryType`.
pub const SF: &str = "http://www.opengis.net/ont/sf#";
/// OGC units of measure (`uom:`).
pub const UOM: &str = "http://www.opengis.net/def/uom/OGC/1.0/";
/// QUDT units, also accepted where a unit is expected.
pub const QUDT_UNIT: &str = "http://qudt.org/vocab/unit/";
/// Jena property functions (`spatial:`).
pub const SPATIAL: &str = "http://jena.apache.org/spatial#";
/// Jena filter functions (`spatialF:`).
pub const SPATIALF: &str = "http://jena.apache.org/function/spatial#";

pub const WKT_LITERAL: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";
pub const GEOJSON_LITERAL: &str = "http://www.opengis.net/ont/geosparql#geoJSONLiteral";
pub const GML_LITERAL: &str = "http://www.opengis.net/ont/geosparql#gmlLiteral";
pub const KML_LITERAL: &str = "http://www.opengis.net/ont/geosparql#kmlLiteral";

/// Serialization predicates (geometry → literal).
pub const AS_WKT: &str = "http://www.opengis.net/ont/geosparql#asWKT";
pub const AS_GEOJSON: &str = "http://www.opengis.net/ont/geosparql#asGeoJSON";
pub const HAS_SERIALIZATION: &str = "http://www.opengis.net/ont/geosparql#hasSerialization";
/// Feature links (feature → geometry).
pub const HAS_DEFAULT_GEOMETRY: &str = "http://www.opengis.net/ont/geosparql#hasDefaultGeometry";
pub const HAS_GEOMETRY: &str = "http://www.opengis.net/ont/geosparql#hasGeometry";

/// Whether `dt` is a geometry literal datatype this build understands
/// (`geo:wktLiteral`, `geo:geoJSONLiteral`).
pub fn is_geometry_datatype(dt: &str) -> bool {
    dt == WKT_LITERAL || dt == GEOJSON_LITERAL
}

/// The 24 topological relations of GeoSPARQL (Simple Features, Egenhofer, RCC8), as
/// `geof:` functions and (with query rewrite) `geo:` properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Relation {
    SfEquals,
    SfDisjoint,
    SfIntersects,
    SfTouches,
    SfWithin,
    SfContains,
    SfOverlaps,
    SfCrosses,
    EhEquals,
    EhDisjoint,
    EhMeet,
    EhOverlap,
    EhCovers,
    EhCoveredBy,
    EhInside,
    EhContains,
    Rcc8Eq,
    Rcc8Dc,
    Rcc8Ec,
    Rcc8Po,
    Rcc8Tppi,
    Rcc8Tpp,
    Rcc8Ntpp,
    Rcc8Ntppi,
}

impl Relation {
    pub const ALL: [Relation; 24] = [
        Relation::SfEquals,
        Relation::SfDisjoint,
        Relation::SfIntersects,
        Relation::SfTouches,
        Relation::SfWithin,
        Relation::SfContains,
        Relation::SfOverlaps,
        Relation::SfCrosses,
        Relation::EhEquals,
        Relation::EhDisjoint,
        Relation::EhMeet,
        Relation::EhOverlap,
        Relation::EhCovers,
        Relation::EhCoveredBy,
        Relation::EhInside,
        Relation::EhContains,
        Relation::Rcc8Eq,
        Relation::Rcc8Dc,
        Relation::Rcc8Ec,
        Relation::Rcc8Po,
        Relation::Rcc8Tppi,
        Relation::Rcc8Tpp,
        Relation::Rcc8Ntpp,
        Relation::Rcc8Ntppi,
    ];

    /// The relation named by a local name (`sfWithin`, `rcc8po`, …), the same in the
    /// `geof:` and `geo:` namespaces.
    pub fn from_local(name: &str) -> Option<Relation> {
        Relation::ALL.into_iter().find(|r| r.local() == name)
    }

    /// The relation of a `geof:` function IRI.
    pub fn from_function(iri: &str) -> Option<Relation> {
        Relation::from_local(iri.strip_prefix(GEOF)?)
    }

    /// The relation of a `geo:` topological property IRI.
    pub fn from_property(iri: &str) -> Option<Relation> {
        Relation::from_local(iri.strip_prefix(GEO)?)
    }

    pub fn local(self) -> &'static str {
        use Relation::*;
        match self {
            SfEquals => "sfEquals",
            SfDisjoint => "sfDisjoint",
            SfIntersects => "sfIntersects",
            SfTouches => "sfTouches",
            SfWithin => "sfWithin",
            SfContains => "sfContains",
            SfOverlaps => "sfOverlaps",
            SfCrosses => "sfCrosses",
            EhEquals => "ehEquals",
            EhDisjoint => "ehDisjoint",
            EhMeet => "ehMeet",
            EhOverlap => "ehOverlap",
            EhCovers => "ehCovers",
            EhCoveredBy => "ehCoveredBy",
            EhInside => "ehInside",
            EhContains => "ehContains",
            Rcc8Eq => "rcc8eq",
            Rcc8Dc => "rcc8dc",
            Rcc8Ec => "rcc8ec",
            Rcc8Po => "rcc8po",
            Rcc8Tppi => "rcc8tppi",
            Rcc8Tpp => "rcc8tpp",
            Rcc8Ntpp => "rcc8ntpp",
            Rcc8Ntppi => "rcc8ntppi",
        }
    }

    /// Whether the relation implies that the two geometries intersect, so a window query
    /// on the envelope of one finds every candidate for the other (every relation but
    /// the three disjoint ones).
    pub fn index_usable(self) -> bool {
        !matches!(
            self,
            Relation::SfDisjoint | Relation::EhDisjoint | Relation::Rcc8Dc
        )
    }

    /// RCC8 relations hold between regions only: false when either argument is not
    /// areal.
    pub fn areal_only(self) -> bool {
        use Relation::*;
        matches!(
            self,
            Rcc8Eq | Rcc8Dc | Rcc8Ec | Rcc8Po | Rcc8Tppi | Rcc8Tpp | Rcc8Ntpp | Rcc8Ntppi
        )
    }

    /// `R(a, b)` ⇔ `R(b, a)`.
    pub fn symmetric(self) -> bool {
        use Relation::*;
        matches!(
            self,
            SfEquals
                | SfDisjoint
                | SfIntersects
                | SfTouches
                | SfOverlaps
                | EhEquals
                | EhDisjoint
                | EhMeet
                | EhOverlap
                | Rcc8Eq
                | Rcc8Dc
                | Rcc8Ec
                | Rcc8Po
        )
    }

    /// The relation `C` with `R(a, b)` ⇔ `C(b, a)`, if there is one among the 24
    /// (`sfCrosses` has none: it is false for a line and a point, true for some point
    /// and line).
    pub fn converse(self) -> Option<Relation> {
        use Relation::*;
        if self.symmetric() {
            return Some(self);
        }
        Some(match self {
            SfWithin => SfContains,
            SfContains => SfWithin,
            EhCovers => EhCoveredBy,
            EhCoveredBy => EhCovers,
            EhInside => EhContains,
            EhContains => EhInside,
            Rcc8Tppi => Rcc8Tpp,
            Rcc8Tpp => Rcc8Tppi,
            Rcc8Ntpp => Rcc8Ntppi,
            Rcc8Ntppi => Rcc8Ntpp,
            _ => return None,
        })
    }
}

/// The Jena `spatial:` property functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpatialPfKind {
    Nearby,
    WithinCircle,
    NearbyGeom,
    WithinCircleGeom,
    WithinBox,
    WithinBoxGeom,
    IntersectBox,
    IntersectBoxGeom,
    North,
    South,
    East,
    West,
    NorthGeom,
    SouthGeom,
    EastGeom,
    WestGeom,
}

impl SpatialPfKind {
    pub const ALL: [SpatialPfKind; 16] = [
        SpatialPfKind::Nearby,
        SpatialPfKind::WithinCircle,
        SpatialPfKind::NearbyGeom,
        SpatialPfKind::WithinCircleGeom,
        SpatialPfKind::WithinBox,
        SpatialPfKind::WithinBoxGeom,
        SpatialPfKind::IntersectBox,
        SpatialPfKind::IntersectBoxGeom,
        SpatialPfKind::North,
        SpatialPfKind::South,
        SpatialPfKind::East,
        SpatialPfKind::West,
        SpatialPfKind::NorthGeom,
        SpatialPfKind::SouthGeom,
        SpatialPfKind::EastGeom,
        SpatialPfKind::WestGeom,
    ];

    /// The property function of an IRI in the `spatial:` namespace.
    pub fn from_iri(iri: &str) -> Option<SpatialPfKind> {
        let local = iri.strip_prefix(SPATIAL)?;
        SpatialPfKind::ALL.into_iter().find(|k| k.local() == local)
    }

    pub fn local(self) -> &'static str {
        use SpatialPfKind::*;
        match self {
            Nearby => "nearby",
            WithinCircle => "withinCircle",
            NearbyGeom => "nearbyGeom",
            WithinCircleGeom => "withinCircleGeom",
            WithinBox => "withinBox",
            WithinBoxGeom => "withinBoxGeom",
            IntersectBox => "intersectBox",
            IntersectBoxGeom => "intersectBoxGeom",
            North => "north",
            South => "south",
            East => "east",
            West => "west",
            NorthGeom => "northGeom",
            SouthGeom => "southGeom",
            EastGeom => "eastGeom",
            WestGeom => "westGeom",
        }
    }

    /// `spatial:<local>`, the prefix of the function's error messages.
    pub fn name(self) -> String {
        format!("spatial:{}", self.local())
    }

    /// Whether the query geometry is a geometry literal argument (the `…Geom` forms)
    /// rather than latitude/longitude numbers.
    pub fn takes_geometry(self) -> bool {
        use SpatialPfKind::*;
        matches!(
            self,
            NearbyGeom
                | WithinCircleGeom
                | WithinBoxGeom
                | IntersectBoxGeom
                | NorthGeom
                | SouthGeom
                | EastGeom
                | WestGeom
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_names_round_trip() {
        for r in Relation::ALL {
            assert_eq!(Relation::from_local(r.local()), Some(r));
            assert_eq!(
                Relation::from_function(&format!("{GEOF}{}", r.local())),
                Some(r)
            );
            assert_eq!(
                Relation::from_property(&format!("{GEO}{}", r.local())),
                Some(r)
            );
            if let Some(c) = r.converse() {
                assert_eq!(c.converse(), Some(r), "{r:?}");
            }
        }
        assert_eq!(Relation::from_local("sfwithin"), None);
        assert_eq!(Relation::from_function("http://example.org/sfWithin"), None);
        assert_eq!(
            Relation::ALL.iter().filter(|r| !r.index_usable()).count(),
            3
        );
        assert_eq!(Relation::SfCrosses.converse(), None);
    }

    #[test]
    fn property_function_names_round_trip() {
        for k in SpatialPfKind::ALL {
            assert_eq!(
                SpatialPfKind::from_iri(&format!("{SPATIAL}{}", k.local())),
                Some(k)
            );
            assert_eq!(k.takes_geometry(), k.local().ends_with("Geom"));
        }
        assert_eq!(SpatialPfKind::from_iri(&format!("{SPATIALF}nearby")), None);
    }
}
