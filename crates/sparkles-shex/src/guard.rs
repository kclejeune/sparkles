//! Write-time ShEx validation: a [`CommitGuard`] that validates the post-state of every
//! commit against a schema and a query shape map, configured per dataset in
//! `<db>/validation.json` (format 2, `"language": "shex"`).
//!
//! The decisions are those of write-time SHACL validation (`sparkles_shacl::guard`):
//!
//! * `reject`: a commit that would leave a nonconformant association is not written
//!   (the store reports [`sparkles::Error::Rejected`]); enabling it requires the current
//!   data to conform;
//! * `warn`: the commit is written; its receipt carries the findings.
//!
//! ShEx has no severities and no threshold: every nonconformant association blocks, and
//! counts as one violation of the summary. The schema is copied into the database when
//! the configuration is set, with its imports resolved then, so a write never fetches
//! anything; the shape map is expanded again on every validated state, so new focus
//! nodes are picked up.

use crate::resolve::Resolver;
use crate::{CompiledSchema, ShapeMap};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sparkles::guard::config::{Baseline, CONFIG_FILE, Counters, DataGraphSel};
use sparkles::guard::{
    Candidate, CommitGuard, GuardLanguage, GuardMode, ValidationSummary, WriteOptions,
};
use sparkles::store::{Snapshot, Store};
use std::path::Path;
use std::sync::Arc;

pub use sparkles::guard::config::{SHEX_SCHEMA_SHEXC_FILE, SHEX_SCHEMA_SHEXJ_FILE};

/// The `format` of a ShEx `validation.json`.
pub const CONFIG_FORMAT: u32 = 2;

/// Write-time ShEx validation of one dataset (`validation.json`, format 2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShexValidationConfig {
    #[serde(default = "two")]
    pub format: u32,
    /// `shex`
    pub language: GuardLanguage,
    pub mode: GuardMode,
    pub schema: SchemaSource,
    /// a query shape map, expanded on every validated state
    pub shape_map: MapSource,
    #[serde(default)]
    pub data_graph: DataGraphSel,
    #[serde(default)]
    pub include_inferences: bool,
    #[serde(default = "ten")]
    pub timeout_seconds: f64,
    #[serde(default = "hundred")]
    pub report_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

fn two() -> u32 {
    CONFIG_FORMAT
}
fn ten() -> f64 {
    10.0
}
fn hundred() -> usize {
    100
}

/// The schema: a file copied into the database (`validation-schema.shex`, or
/// `validation-schema.json` for ShExJ and for schemas whose imports were resolved and
/// merged when the configuration was set).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaSource {
    /// the copied file, in the database directory (set when the configuration is set)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// the syntax: `shexc` or `shexj` (of the copied file); of `inline`, also `shexr`
    /// (Turtle). Default: sniffed (`{` → ShExJ, otherwise ShExC)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// the base IRI the schema's relative IRIs resolve against
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// informational: where the schema came from, and the SHA-256 of its text as given
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// schema text given when setting the configuration (not stored in the file)
    #[serde(default, skip_serializing)]
    pub inline: Option<String>,
}

/// A query shape map: compact syntax, or the JSON form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MapSource {
    Compact(String),
    Json(Vec<serde_json::Value>),
}

impl ShexValidationConfig {
    /// Check the fields (not the schema or the shape map).
    pub fn check(&self) -> Result<()> {
        if self.format != CONFIG_FORMAT {
            bail!(
                "a ShEx configuration is validation.json format {CONFIG_FORMAT}, not {}",
                self.format
            );
        }
        if self.language != GuardLanguage::Shex {
            bail!("language must be \"shex\"");
        }
        if !(self.timeout_seconds.is_finite() && self.timeout_seconds > 0.0) {
            bail!("timeoutSeconds must be a positive number");
        }
        if !(1..=10_000).contains(&self.report_limit) {
            bail!("reportLimit must be between 1 and 10000");
        }
        self.data_graph.check()?;
        if self.mode != GuardMode::Off
            && usize::from(self.schema.file.is_some()) + usize::from(self.schema.inline.is_some())
                != 1
        {
            bail!("schema: give the schema inline (or the file it was copied to)");
        }
        Ok(())
    }
}

/// `GET /$/validation/{ds}` status of a ShEx guard.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShexValidationStatus {
    pub mode: GuardMode,
    /// shape declarations of the schema (imports included)
    pub shape_count: usize,
    /// associations of the last validated state's fixed map
    pub associations: Option<u64>,
    pub baseline: Option<Baseline>,
    pub last_full_millis: Option<u64>,
    pub counters: Counters,
    pub warnings: Vec<String>,
}

/// Write-time ShEx validation of one store.
pub struct ShexGuard {
    cfg: ShexValidationConfig,
    #[allow(dead_code)]
    schema: Arc<CompiledSchema>,
    #[allow(dead_code)]
    map: ShapeMap,
}

impl ShexGuard {
    pub fn config(&self) -> &ShexValidationConfig {
        &self.cfg
    }

    pub fn status(&self) -> ShexValidationStatus {
        ShexValidationStatus {
            mode: self.cfg.mode,
            shape_count: 0,
            associations: None,
            baseline: None,
            last_full_millis: None,
            counters: Counters::default(),
            warnings: Vec::new(),
        }
    }

    /// Validate `view` and summarize under this configuration.
    #[allow(dead_code)]
    fn validate_state(
        &self,
        _view: &Arc<Snapshot>,
        _o: &WriteOptions,
    ) -> sparkles::Result<ValidationSummary> {
        Err(not_implemented())
    }
}

fn not_implemented() -> sparkles::Error {
    sparkles::Error::Unsupported("write-time ShEx validation is not implemented yet".into())
}

impl CommitGuard for ShexGuard {
    fn check(&self, _c: &Candidate<'_>) -> sparkles::Result<ValidationSummary> {
        Err(not_implemented())
    }

    fn describe(&self) -> String {
        format!(
            "write-time ShEx validation ({})",
            match self.cfg.mode {
                GuardMode::Reject => "reject",
                GuardMode::Warn => "warn",
                GuardMode::Off => "off",
            }
        )
    }

    fn language(&self) -> GuardLanguage {
        GuardLanguage::Shex
    }
}

// ------------------------------------------------------------ configuration ------

/// The ShEx configuration of a database, if `validation.json` is one (`None` without a
/// file; an error for a SHACL configuration or one that does not parse).
pub fn read_config(root: &Path) -> Result<Option<ShexValidationConfig>> {
    let path = root.join(CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let cfg: ShexValidationConfig =
        serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    cfg.check()?;
    Ok(Some(cfg))
}

/// Install the guard of a persistent store from its ShEx `validation.json` (after
/// [`Store::open`]). Without a configuration nothing happens. A configuration that
/// cannot be loaded is an error, and the store keeps refusing writes (fail closed).
pub fn install(store: &Store) -> Result<Option<Arc<ShexGuard>>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let Some(cfg) = read_config(root)? else {
        return Ok(None);
    };
    if cfg.mode == GuardMode::Off {
        store.set_guard_required(false);
        return Ok(None);
    }
    Err(not_implemented().into())
}

/// The outcome of [`set_config`].
pub enum SetOutcome {
    /// installed; the validation of the current state
    Installed(Arc<ShexGuard>, ValidationSummary),
    /// `reject` was refused: the current state has nonconformant associations
    NotConforming(ValidationSummary),
    /// validation is off
    Removed,
}

/// Set (or with `None` / mode `off`, remove) the write-time ShEx validation of a store.
/// The schema is parsed from `cfg.schema.inline`, its imports resolved through
/// `resolver` and checked, and copied into the database with the configuration. Runs
/// under the writer lock: the current state is validated with the new configuration,
/// and `reject` is refused when an association does not conform, so no write can commit
/// between the check and the switch.
pub fn set_config(
    store: &Store,
    cfg: Option<ShexValidationConfig>,
    resolver: &dyn Resolver,
) -> Result<SetOutcome> {
    let _ = resolver;
    let Some(cfg) = cfg.filter(|c| c.mode != GuardMode::Off) else {
        let txn = store.write_as(sparkles::commit::CommitKind::Transaction);
        store.set_guard(None);
        store.set_guard_required(false);
        if let Some(r) = store.root() {
            sparkles::guard::config::remove_files(r, &[])?;
        }
        drop(txn);
        return Ok(SetOutcome::Removed);
    };
    cfg.check()?;
    Err(not_implemented().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(j: serde_json::Value) -> Result<ShexValidationConfig> {
        let c: ShexValidationConfig = serde_json::from_value(j)?;
        c.check()?;
        Ok(c)
    }

    #[test]
    fn configuration_fields() {
        let c = config(serde_json::json!({
            "format": 2, "language": "shex", "mode": "reject",
            "schema": {"file": "validation-schema.shex", "format": "shexc",
                       "source": "/abs/s.shex", "sha256": "00"},
            "shapeMap": "{FOCUS a <http://ex.org/Person>}@<http://ex.org/Person>",
        }))
        .unwrap();
        assert_eq!(c.data_graph, DataGraphSel::default());
        assert_eq!((c.timeout_seconds, c.report_limit), (10.0, 100));
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!(back["language"], "shex");
        assert_eq!(back["schema"]["file"], "validation-schema.shex");
        assert!(back.get("updated").is_none());
        // JSON shape maps
        let c = config(serde_json::json!({
            "language": "shex", "mode": "warn", "schema": {"inline": "<http://ex.org/S> {}"},
            "shapeMap": [{"node": "<http://ex.org/a>", "shape": "<http://ex.org/S>"}],
        }))
        .unwrap();
        assert_eq!(c.format, 2);
        assert!(matches!(c.shape_map, MapSource::Json(_)));
        // the inline schema is not written back
        assert!(
            serde_json::to_value(&c).unwrap()["schema"]
                .get("inline")
                .is_none()
        );
    }

    #[test]
    fn rejected_configurations() {
        let base = || {
            serde_json::json!({"format": 2, "language": "shex", "mode": "reject",
                "schema": {"inline": "<http://ex.org/S> {}"}, "shapeMap": "<http://ex.org/a>@START"})
        };
        let with = |k: &str, v: serde_json::Value| {
            let mut j = base();
            j[k] = v;
            config(j)
        };
        assert!(config(base()).is_ok());
        assert!(with("format", 1.into()).is_err());
        assert!(with("language", "shacl".into()).is_err());
        // no severities in ShEx
        assert!(with("threshold", "warning".into()).is_err());
        assert!(with("reportLimit", 0.into()).is_err());
        assert!(with("timeoutSeconds", 0.into()).is_err());
        assert!(with("dataGraph", "other".into()).is_err());
        assert!(with("schema", serde_json::json!({})).is_err());
    }

    #[test]
    fn stubs_fail_cleanly() {
        let store = Store::in_memory(Default::default());
        let cfg = config(serde_json::json!({"language": "shex", "mode": "reject",
            "schema": {"inline": "<http://ex.org/S> {}"}, "shapeMap": "<http://ex.org/a>@<http://ex.org/S>"}))
        .unwrap();
        assert!(set_config(&store, Some(cfg), &crate::NoImports).is_err());
        assert!(matches!(
            set_config(&store, None, &crate::NoImports).unwrap(),
            SetOutcome::Removed
        ));
        assert!(install(&store).unwrap().is_none());
    }
}
