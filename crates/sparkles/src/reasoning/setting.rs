//! The dataset's query-time reasoning setting (spec F17 §2.2): the mode of a query that
//! names none, the profile and rules that derive on demand, the input graphs, the
//! budget of a derivation and whether `auto` reads a stale materialization.
//!
//! A persistent dataset keeps the setting in `query-reasoning.json`, next to the record
//! of its last materialization (`reasoning.json`), and [`Dataset::open`] loads it. The
//! setting has its own file because the record describes a materialization, and every
//! reader of the record treats its presence as one. Without a profile of its own, the
//! setting's queries derive with the profile, rules and inputs of the last
//! materialization, so `auto` compares like with like.
//!
//! [`Dataset::open`]: crate::Dataset::open

use crate::error::{Error, Result};
use crate::reasoning::mode::{Demand, ReasoningMode};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The setting of a persistent dataset.
pub const SETTING_FILE: &str = "query-reasoning.json";

/// The modes the setting's `default` may name.
pub const DEFAULTS: [&str; 4] = ["auto", "demand", "materialized", "false"];

/// What `query-reasoning.json` holds, and what `PUT /$/reason/{ds}/query` takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySetting {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub query_reasoning_format: u32,
    /// the mode of a query that names none: `auto`, `demand`, `materialized` or `false`;
    /// absent, the dataset's queries read its materialized inferences as before
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// `rdfs`, `rdfs-simple`, `owl-rl` or `rules`; absent, the last materialization's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// the Jena rule text of the profile `rules`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    /// the input graphs (C08 §11), as a reasoning request names them; absent, the last
    /// materialization's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Budget::is_empty")]
    pub budget: Budget,
    /// `derive` or `allow`: what `auto` does with a stale materialization (§4.2);
    /// absent is `derive`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The budget of one query's derivation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// the most facts a query derives; absent, the reasoner's or the server's default
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_derived: Option<usize>,
    /// the longest a query's derivation runs, in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_millis: Option<u64>,
}

impl Budget {
    fn is_empty(&self) -> bool {
        *self == Budget::default()
    }
}

impl QuerySetting {
    /// Check the setting's values: the mode, the profile with its rules, the stale
    /// policy and the budget.
    pub fn validate(&self) -> Result<()> {
        if let Some(d) = &self.default
            && !DEFAULTS.contains(&d.as_str())
        {
            return Err(Error::invalid(format!(
                "default must be one of {}, not {d}",
                DEFAULTS.join(", ")
            )));
        }
        self.profile()?;
        if self.rules.is_some() && self.profile.as_deref() != Some("rules") {
            return Err(Error::invalid(
                "rules are given, so the profile must be \"rules\"",
            ));
        }
        if let Some(s) = &self.stale
            && s != "derive"
            && s != "allow"
        {
            return Err(Error::invalid(format!(
                "stale must be \"derive\" or \"allow\", not {s}"
            )));
        }
        if self.budget.max_derived == Some(0) {
            return Err(Error::invalid("budget.maxDerived must be positive"));
        }
        if self.budget.max_millis == Some(0) {
            return Err(Error::invalid("budget.maxMillis must be positive"));
        }
        if let Some(i) = &self.inputs {
            #[cfg(feature = "reasoning")]
            serde_json::from_value::<sparkles_reasoner::Inputs>(i.clone())
                .map_err(|e| Error::invalid(format!("inputs: {e}")))?;
            #[cfg(not(feature = "reasoning"))]
            let _ = i;
        }
        Ok(())
    }

    /// The profile the setting names, when it names one.
    #[cfg(feature = "reasoning")]
    pub fn profile(&self) -> Result<Option<sparkles_reasoner::Profile>> {
        match self.profile.as_deref() {
            None => Ok(None),
            Some("rules") => match &self.rules {
                Some(t) => Ok(Some(sparkles_reasoner::Profile::Rules(t.clone()))),
                None => Err(Error::invalid("the profile \"rules\" needs rule text")),
            },
            Some(p) => p
                .parse::<sparkles_reasoner::Profile>()
                .map(Some)
                .map_err(|e| Error::invalid(e.to_string())),
        }
    }

    /// The profile the setting names, when it names one.
    #[cfg(not(feature = "reasoning"))]
    pub fn profile(&self) -> Result<Option<()>> {
        match self.profile.as_deref() {
            None => Ok(None),
            Some(_) => Err(Error::unsupported("built without the `reasoning` feature")),
        }
    }

    /// The input graphs the setting names, when it names them.
    #[cfg(feature = "reasoning")]
    pub fn run_inputs(&self) -> Result<Option<sparkles_reasoner::Inputs>> {
        match &self.inputs {
            None => Ok(None),
            Some(j) => serde_json::from_value(j.clone())
                .map(Some)
                .map_err(|e| Error::invalid(format!("inputs: {e}"))),
        }
    }

    /// The mode of a query that names none, when the setting names one.
    pub fn default_mode(&self) -> Option<ReasoningMode> {
        let d = self.default.as_deref()?;
        // `Demand` has a profile only with the reasoner
        #[allow(clippy::needless_update)]
        let demand = Demand {
            max_derived: self.budget.max_derived,
            ..Demand::default()
        };
        Some(match d {
            "auto" => ReasoningMode::Auto(demand),
            "demand" => ReasoningMode::Demand(demand),
            "false" => ReasoningMode::Off,
            _ => ReasoningMode::Materialized,
        })
    }

    /// Whether `auto` reads a stale materialization.
    pub fn stale_allowed(&self) -> bool {
        self.stale.as_deref() == Some("allow")
    }
}

/// The setting kept in a store's directory, if any. The store refused a newer file at
/// open, so this fallback covers a file that does not parse: it is logged and ignored.
pub(crate) fn load(store: &Store) -> Option<QuerySetting> {
    read(store.root()?)
}

/// The setting kept in a database directory, if it has one that reads, as
/// [`read_record`](super::read_record) reads the reasoning record.
pub fn read(root: &Path) -> Option<QuerySetting> {
    let bytes = std::fs::read(root.join(SETTING_FILE)).ok()?;
    let open = || -> Result<QuerySetting> {
        let v: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::invalid(format!("{SETTING_FILE}: {e}")))?;
        crate::formats::check_settings(SETTING_FILE, &v)?;
        let s: QuerySetting = serde_json::from_value(v)
            .map_err(|e| Error::invalid(format!("{SETTING_FILE}: {e}")))?;
        s.validate()?;
        Ok(s)
    };
    match open() {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(
                "query-time reasoning setting of {}: {e}; queries run without it",
                root.display()
            );
            None
        }
    }
}

/// Write (or with `None` remove) the setting of a database directory durably, as the
/// reasoning record is written. A dataset that is open does not see the change until it
/// is opened again; [`Dataset::set_query_reasoning`](crate::Dataset::set_query_reasoning)
/// changes an open dataset.
pub fn write(root: &Path, setting: Option<&QuerySetting>) -> Result<()> {
    let path = root.join(SETTING_FILE);
    match setting {
        Some(s) => {
            let mut s = s.clone();
            s.query_reasoning_format =
                crate::formats::settings_file(SETTING_FILE).map_or(1, |f| f.writes);
            let bytes = serde_json::to_vec_pretty(&s)
                .map_err(|e| Error::invalid(format!("query-time reasoning setting: {e}")))?;
            crate::guard::config::write_atomic(&path, &bytes)
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => super::sync_dir(root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "reasoning")]
    fn settings_are_checked() {
        let ok = QuerySetting {
            default: Some("auto".into()),
            profile: Some("owl-rl".into()),
            stale: Some("allow".into()),
            ..Default::default()
        };
        ok.validate().unwrap();
        assert!(matches!(ok.default_mode(), Some(ReasoningMode::Auto(_))));
        assert!(ok.stale_allowed());
        let bad = |s: QuerySetting| s.validate().unwrap_err().to_string();
        assert!(
            bad(QuerySetting {
                default: Some("maybe".into()),
                ..Default::default()
            })
            .contains("default must be")
        );
        assert!(
            bad(QuerySetting {
                stale: Some("read".into()),
                ..Default::default()
            })
            .contains("stale must be")
        );
        assert!(
            bad(QuerySetting {
                profile: Some("rules".into()),
                ..Default::default()
            })
            .contains("rule text")
        );
        assert!(
            bad(QuerySetting {
                budget: Budget {
                    max_derived: Some(0),
                    max_millis: None,
                },
                ..Default::default()
            })
            .contains("maxDerived")
        );
    }

    #[test]
    fn a_newer_setting_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        let newer = crate::formats::settings_file(SETTING_FILE).unwrap().writes + 1;
        std::fs::write(
            dir.path().join(SETTING_FILE),
            format!(r#"{{"queryReasoningFormat": {newer}, "default": "auto"}}"#),
        )
        .unwrap();
        assert!(load(&store).is_none());
        write(
            dir.path(),
            Some(&QuerySetting {
                default: Some("auto".into()),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(load(&store).unwrap().default.as_deref(), Some("auto"));
        write(dir.path(), None).unwrap();
        assert!(load(&store).is_none());
    }
}
