//! How aggregates treat errors and values that are not strings (roadmap item 1064).
//!
//! SPARQL 1.1 and 1.2 leave room for more than one reading of what MIN, MAX, AVG and
//! GROUP_CONCAT give when an argument is an error, such as an unbound variable, and
//! engines differ. A query runs under one of three readings, which its options name and
//! which default to the dataset's setting, and then to the store's.
//!
//! | reading | MIN and MAX over an error | AVG over only errors | GROUP_CONCAT of an IRI or a number |
//! |---|---|---|---|
//! | `jena` | unbound | unbound | its lexical form |
//! | `sparkles` | the other values decide | unbound | its lexical form |
//! | `spec` | MIN unbound, MAX from the other values | `0` | unbound |
//!
//! Every reading makes SUM, AVG and GROUP_CONCAT unbound when one of their arguments is
//! an error, and COUNT and SAMPLE pass over errors.

use crate::error::{Error, Result};
use std::fmt;
use std::str::FromStr;

/// How aggregates treat errors and values that are not strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AggregateErrors {
    /// Apache Jena's reading: MIN and MAX are unbound when an argument is an error, AVG
    /// over only errors is unbound, and GROUP_CONCAT reads the lexical form of any term
    /// that has one.
    #[default]
    Jena,
    /// The engine's earlier reading: MIN and MAX pass over errors, and AVG and
    /// GROUP_CONCAT read as in Jena.
    Sparkles,
    /// The literal reading of SPARQL 1.1 and 1.2. MIN and MAX order the values as ORDER
    /// BY does, with errors first, so MIN is unbound and MAX passes over errors. AVG is
    /// Sum over Count, and Count leaves errors out, so AVG over only errors is `0`.
    /// GROUP_CONCAT is CONCAT over the values, so a value that is not a string is an
    /// error.
    Spec,
}

impl AggregateErrors {
    /// Every reading, the default first.
    pub const ALL: [AggregateErrors; 3] = [
        AggregateErrors::Jena,
        AggregateErrors::Sparkles,
        AggregateErrors::Spec,
    ];

    /// The reading's name: `jena`, `sparkles` or `spec`.
    pub fn as_str(self) -> &'static str {
        match self {
            AggregateErrors::Jena => "jena",
            AggregateErrors::Sparkles => "sparkles",
            AggregateErrors::Spec => "spec",
        }
    }

    /// Whether MIN (`min`) or MAX is unbound when one of its arguments is an error.
    pub(crate) fn best_errs(self, min: bool) -> bool {
        match self {
            AggregateErrors::Jena => true,
            AggregateErrors::Sparkles => false,
            AggregateErrors::Spec => min,
        }
    }

    /// Whether AVG over arguments that are all errors is `0`, as over no rows.
    pub(crate) fn avg_of_errors_zero(self) -> bool {
        self == AggregateErrors::Spec
    }

    /// Whether GROUP_CONCAT takes only strings: simple literals, `xsd:string` and
    /// language-tagged strings.
    pub(crate) fn concat_strings_only(self) -> bool {
        self == AggregateErrors::Spec
    }
}

impl fmt::Display for AggregateErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AggregateErrors {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "jena" => Ok(AggregateErrors::Jena),
            "sparkles" => Ok(AggregateErrors::Sparkles),
            "spec" => Ok(AggregateErrors::Spec),
            other => Err(Error::invalid(format!(
                "aggregate-errors is jena, sparkles or spec, not {other:?}"
            ))),
        }
    }
}

impl serde::Serialize for AggregateErrors {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for AggregateErrors {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
