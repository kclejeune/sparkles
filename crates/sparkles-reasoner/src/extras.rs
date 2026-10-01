//! What a materialization adds to its profile's rules: built-in vocabularies whose
//! axioms join the TBox (`infer --vocab geosparql`), and GeoSPARQL's default geometry
//! (`infer --geo-default-geometry`: `geo:hasDefaultGeometry` for features with exactly
//! one `geo:hasGeometry`, as Jena's `applyDefaultGeometry`).

use std::fmt;
use std::str::FromStr;

/// A vocabulary built into the reasoner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Vocabulary {
    /// the GeoSPARQL 1.1 classes and properties with the Simple Features geometry types:
    /// their subclass, subproperty, domain and range axioms
    GeoSparql,
}

/// The GeoSPARQL axioms (`vocab/geosparql.rules`): written from the standard's class and
/// property definitions, as rules without premises.
pub const GEOSPARQL_RULES: &str = include_str!("../vocab/geosparql.rules");

impl Vocabulary {
    pub fn name(self) -> &'static str {
        match self {
            Vocabulary::GeoSparql => "geosparql",
        }
    }

    /// The vocabulary's axioms as rule text.
    pub fn text(self) -> &'static str {
        match self {
            Vocabulary::GeoSparql => GEOSPARQL_RULES,
        }
    }

    pub fn rules(self) -> Result<Vec<crate::Rule>, crate::RuleParseError> {
        crate::parse_rules(self.text())
    }
}

impl fmt::Display for Vocabulary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown vocabulary '{0}' (expected geosparql)")]
pub struct UnknownVocabulary(pub String);

impl FromStr for Vocabulary {
    type Err = UnknownVocabulary;
    fn from_str(s: &str) -> Result<Vocabulary, UnknownVocabulary> {
        match s.trim().to_ascii_lowercase().as_str() {
            "geosparql" => Ok(Vocabulary::GeoSparql),
            _ => Err(UnknownVocabulary(s.to_string())),
        }
    }
}

/// The additions of one materialization (none by default).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extras {
    /// sorted, without duplicates
    pub vocabularies: Vec<Vocabulary>,
    /// materialize `geo:hasDefaultGeometry` for features with exactly one geometry
    pub geo_default_geometry: bool,
}

impl Extras {
    /// The extras of vocabulary names and the default-geometry switch.
    pub fn parse<S: AsRef<str>>(
        vocabularies: &[S],
        geo_default_geometry: bool,
    ) -> Result<Extras, UnknownVocabulary> {
        let mut v = vocabularies
            .iter()
            .map(|s| s.as_ref().parse())
            .collect::<Result<Vec<Vocabulary>, _>>()?;
        v.sort_unstable();
        v.dedup();
        Ok(Extras {
            vocabularies: v,
            geo_default_geometry,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.vocabularies.is_empty() && !self.geo_default_geometry
    }

    /// The vocabulary names (for the recorded reasoning status).
    pub fn names(&self) -> Vec<String> {
        self.vocabularies
            .iter()
            .map(|v| v.name().to_string())
            .collect()
    }

    /// Whether this build can materialize these extras (every extra can).
    pub fn validate(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse() {
        let e = Extras::parse(&["GeoSPARQL", "geosparql"], false).unwrap();
        assert_eq!(e.vocabularies, [Vocabulary::GeoSparql]);
        assert_eq!(e.names(), ["geosparql"]);
        assert!(Extras::parse(&["dublin-core"], false).is_err());
        assert!(Extras::default().is_empty());
        assert!(Extras::default().validate().is_ok());
        assert!(e.validate().is_ok());
    }

    #[test]
    fn the_geosparql_axioms_parse() {
        let rules = Vocabulary::GeoSparql.rules().unwrap();
        assert!(rules.len() > 100, "{}", rules.len());
        assert!(rules.iter().all(|r| r.body.is_empty()));
    }
}
