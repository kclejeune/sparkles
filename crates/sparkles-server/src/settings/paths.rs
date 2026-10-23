//! The `paths` settings kind of a dataset (spec U09, open question 1): the predicates the
//! Explore path finder skips unless the reader changes the list, kept in
//! `<db>/paths.json`. A dataset with a hub predicate of its own, such as `dct:subject`,
//! names it here so that every reader's search starts without it. The file carries the
//! settings layer's `format` member, registered in `sparkles::formats`.

use crate::settings::{Providers, Typed};
use serde::{Deserialize, Serialize};

/// The file in a dataset's directory that keeps the runtime layer.
pub const PATHS_FILE: &str = "paths.json";

/// The kind's top-level members.
pub const MEMBERS: &[&str] = &["excludedPredicates"];

/// The most predicates the list may hold, so that a search's block stays small.
pub const MAX_EXCLUDED: usize = 50;

fn default_excluded() -> Vec<String> {
    vec![
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into(),
        "http://www.w3.org/2002/07/owl#sameAs".into(),
    ]
}

/// The path finder's defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PathSettings {
    /// predicate IRIs whose triples a search skips by default
    #[serde(default = "default_excluded")]
    pub excluded_predicates: Vec<String>,
}

impl Default for PathSettings {
    fn default() -> PathSettings {
        PathSettings {
            excluded_predicates: default_excluded(),
        }
    }
}

impl Typed for PathSettings {
    fn validate(&self, _: Providers) -> Result<(), String> {
        let list = &self.excluded_predicates;
        if list.len() > MAX_EXCLUDED {
            return Err(format!(
                "excludedPredicates holds {} predicates, more than the {MAX_EXCLUDED} allowed",
                list.len()
            ));
        }
        for (i, p) in list.iter().enumerate() {
            if oxrdf::NamedNode::new(p.as_str()).is_err() {
                return Err(format!("excludedPredicates: {p:?} is not an absolute IRI"));
            }
            if list[..i].contains(p) {
                return Err(format!("excludedPredicates: {p} is listed twice"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(v: serde_json::Value) -> Result<(), String> {
        let s: PathSettings = serde_json::from_value(v).map_err(|e| e.to_string())?;
        s.validate(Providers::Unchecked)
    }

    #[test]
    fn defaults_skip_types_and_same_as() {
        let d = serde_json::to_value(PathSettings::default()).unwrap();
        assert_eq!(
            d,
            serde_json::json!({"excludedPredicates": [
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://www.w3.org/2002/07/owl#sameAs"
            ]})
        );
        assert!(check(serde_json::json!({})).is_ok());
    }

    #[test]
    fn the_list_is_checked() {
        assert!(check(serde_json::json!({"excludedPredicates": []})).is_ok());
        let e = check(serde_json::json!({"excludedPredicates": ["dc subject"]})).unwrap_err();
        assert!(e.contains("not an absolute IRI"), "{e}");
        let p = "http://purl.org/dc/terms/subject";
        let e = check(serde_json::json!({"excludedPredicates": [p, p]})).unwrap_err();
        assert!(e.contains("listed twice"), "{e}");
        let many: Vec<String> = (0..=MAX_EXCLUDED)
            .map(|i| format!("http://ex/p{i}"))
            .collect();
        let e = check(serde_json::json!({ "excludedPredicates": many })).unwrap_err();
        assert!(e.contains("more than the 50 allowed"), "{e}");
        let e = check(serde_json::json!({"other": 1})).unwrap_err();
        assert!(e.contains("unknown field"), "{e}");
    }
}
