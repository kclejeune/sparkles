//! Units of measure accepted by the `geof:` functions (OGC, QUDT and EPSG IRIs). Pure
//! data, compiled without the `geo` feature too.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitKind {
    Length,
    Angle,
    Area,
}

/// A unit: its kind and its size in the base unit of the kind (metre, radian, square
/// metre).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unit {
    pub kind: UnitKind,
    pub factor: f64,
}

impl Unit {
    pub const METRE: Unit = Unit {
        kind: UnitKind::Length,
        factor: 1.0,
    };
    pub const KILOMETRE: Unit = Unit {
        kind: UnitKind::Length,
        factor: 1000.0,
    };
    pub const SQUARE_METRE: Unit = Unit {
        kind: UnitKind::Area,
        factor: 1.0,
    };
    pub const DEGREE: Unit = Unit {
        kind: UnitKind::Angle,
        factor: std::f64::consts::PI / 180.0,
    };

    /// `value` in this unit → the base unit of its kind.
    pub fn to_base(self, value: f64) -> f64 {
        value * self.factor
    }

    /// `value` in the base unit of this unit's kind → this unit.
    pub fn from_base(self, value: f64) -> f64 {
        value / self.factor
    }
}

const OGC: &str = "http://www.opengis.net/def/uom/OGC/1.0/";
const QUDT: &str = "http://qudt.org/vocab/unit/";
const EPSG: &str = "urn:ogc:def:uom:EPSG::";

const MILE: f64 = 1609.344;
const YARD: f64 = 0.9144;
const FOOT: f64 = 0.3048;
const SURVEY_FOOT: f64 = 1200.0 / 3937.0;
const DEG: f64 = std::f64::consts::PI / 180.0;

fn length(f: f64) -> Option<Unit> {
    Some(Unit {
        kind: UnitKind::Length,
        factor: f,
    })
}

fn angle(f: f64) -> Option<Unit> {
    Some(Unit {
        kind: UnitKind::Angle,
        factor: f,
    })
}

fn area(f: f64) -> Option<Unit> {
    Some(Unit {
        kind: UnitKind::Area,
        factor: f,
    })
}

/// The unit an IRI names.
pub fn unit(iri: &str) -> Option<Unit> {
    let iri = iri.trim();
    if let Some(name) = iri
        .strip_prefix(OGC)
        .or_else(|| iri.strip_prefix("https://www.opengis.net/def/uom/OGC/1.0/"))
    {
        ogc(name)
    } else if let Some(name) = iri
        .strip_prefix(QUDT)
        .or_else(|| iri.strip_prefix("https://qudt.org/vocab/unit/"))
    {
        qudt(name)
    } else if let Some(code) = iri.strip_prefix(EPSG) {
        epsg(code)
    } else {
        None
    }
}

fn ogc(name: &str) -> Option<Unit> {
    match name {
        "metre" | "meter" => length(1.0),
        "kilometre" | "kilometer" => length(1000.0),
        "centimetre" | "centimeter" => length(0.01),
        "millimetre" | "millimeter" => length(0.001),
        "mile" | "statuteMile" => length(MILE),
        "nauticalMile" => length(1852.0),
        "yard" => length(YARD),
        "foot" => length(FOOT),
        "inch" => length(0.0254),
        "surveyFootUS" => length(SURVEY_FOOT),
        "radian" => angle(1.0),
        "microRadian" => angle(1e-6),
        "degree" => angle(DEG),
        "minute" => angle(DEG / 60.0),
        "second" => angle(DEG / 3600.0),
        "grad" => angle(std::f64::consts::PI / 200.0),
        "squareMetre" | "square_metre" | "squareMeter" | "square_meter" => area(1.0),
        "squareKilometre" | "square_kilometre" | "squareKilometer" | "square_kilometer" => {
            area(1e6)
        }
        "hectare" => area(1e4),
        "acre" => area(4_046.856_422_4),
        _ => None,
    }
}

fn qudt(name: &str) -> Option<Unit> {
    match name {
        "M" => length(1.0),
        "KiloM" => length(1000.0),
        "CentiM" => length(0.01),
        "MilliM" => length(0.001),
        "MI" => length(MILE),
        "MI_N" => length(1852.0),
        "YD" => length(YARD),
        "FT" => length(FOOT),
        "IN" => length(0.0254),
        "FT_US" => length(SURVEY_FOOT),
        "RAD" => angle(1.0),
        "MicroRAD" => angle(1e-6),
        "DEG" => angle(DEG),
        "ARCMIN" => angle(DEG / 60.0),
        "ARCSEC" => angle(DEG / 3600.0),
        "GON" => angle(std::f64::consts::PI / 200.0),
        "M2" => area(1.0),
        "KiloM2" => area(1e6),
        "HA" => area(1e4),
        "AC" => area(4_046.856_422_4),
        "ARE" => area(100.0),
        "MI2" => area(MILE * MILE),
        "FT2" => area(FOOT * FOOT),
        "YD2" => area(YARD * YARD),
        _ => None,
    }
}

fn epsg(code: &str) -> Option<Unit> {
    match code {
        "9001" => length(1.0),
        "9036" => length(1000.0),
        "1033" => length(0.01),
        "1025" => length(0.001),
        "9093" => length(MILE),
        "9030" => length(1852.0),
        "9096" => length(YARD),
        "9002" => length(FOOT),
        "9003" => length(SURVEY_FOOT),
        "9101" => angle(1.0),
        "9109" => angle(1e-6),
        "9102" => angle(DEG),
        "9103" => angle(DEG / 60.0),
        "9104" => angle(DEG / 3600.0),
        "9105" => angle(std::f64::consts::PI / 200.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(iri: &str) -> (UnitKind, f64) {
        let u = unit(iri).unwrap_or_else(|| panic!("{iri}"));
        (u.kind, u.factor)
    }

    #[test]
    fn the_three_vocabularies_agree() {
        let same = [
            ("kilometre", "KiloM", "9036"),
            ("metre", "M", "9001"),
            ("centimetre", "CentiM", "1033"),
            ("millimetre", "MilliM", "1025"),
            ("mile", "MI", "9093"),
            ("nauticalMile", "MI_N", "9030"),
            ("yard", "YD", "9096"),
            ("foot", "FT", "9002"),
            ("surveyFootUS", "FT_US", "9003"),
            ("radian", "RAD", "9101"),
            ("microRadian", "MicroRAD", "9109"),
            ("degree", "DEG", "9102"),
            ("minute", "ARCMIN", "9103"),
            ("second", "ARCSEC", "9104"),
            ("grad", "GON", "9105"),
        ];
        for (o, q, e) in same {
            let a = f(&format!("{OGC}{o}"));
            assert_eq!(a, f(&format!("{QUDT}{q}")), "{o}");
            assert_eq!(a, f(&format!("{EPSG}{e}")), "{o}");
        }
        assert_eq!(f(&format!("{OGC}kilometer")), (UnitKind::Length, 1000.0));
        assert_eq!(f(&format!("{OGC}statuteMile")).1, 1609.344);
        assert_eq!(f(&format!("{OGC}inch")), f(&format!("{QUDT}IN")));
        assert_eq!(f(&format!("{OGC}degree")).1, std::f64::consts::PI / 180.0);
        assert_eq!(
            f(&format!("{OGC}squareKilometre")),
            f(&format!("{QUDT}KiloM2"))
        );
        assert_eq!(f(&format!("{OGC}square_metre")), (UnitKind::Area, 1.0));
        assert_eq!(f(&format!("{OGC}hectare")), f(&format!("{QUDT}HA")));
        assert_eq!(f(&format!("{OGC}acre")), f(&format!("{QUDT}AC")));
        assert_eq!(f(&format!("{QUDT}ARE")).1, 100.0);
        assert_eq!(
            f("https://www.opengis.net/def/uom/OGC/1.0/metre"),
            (UnitKind::Length, 1.0)
        );
    }

    #[test]
    fn unknown_units() {
        for iri in [
            "http://www.opengis.net/def/uom/OGC/1.0/parsec",
            "http://qudt.org/vocab/unit/PARSEC",
            "urn:ogc:def:uom:EPSG::9999",
            "metre",
            "",
        ] {
            assert!(unit(iri).is_none(), "{iri}");
        }
    }

    #[test]
    fn conversion() {
        assert_eq!(Unit::KILOMETRE.from_base(111_319.490_793), 111.319_490_793);
        assert_eq!(Unit::KILOMETRE.to_base(2.0), 2000.0);
        assert_eq!(unit(&format!("{OGC}kilometre")), Some(Unit::KILOMETRE));
    }
}
