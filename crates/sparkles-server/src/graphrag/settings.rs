//! The `graphrag` settings of a dataset (spec C22 §2.1), kept in `<db>/graphrag.json`.
//!
//! The object carries a format `version`. A reader refuses a version it does not know
//! rather than guessing at its members, and every number has a range so that a build
//! cannot be asked for unbounded work.

use crate::settings::{Providers, Typed};
use serde::{Deserialize, Serialize};

/// The settings format this server reads and writes.
pub const SETTINGS_VERSION: u32 = 1;

/// The namespace of reports graphs. Every graph under it is guarded by the disclosure
/// guard of C22 §6, so a reports graph must be named inside it.
pub const GUARDED_PREFIX: &str = "urn:x-sparkles:graphrag:";

/// The deepest level a build may keep, from 0 (coarsest).
pub const MAX_LEVELS: u32 = 7;
/// The most facts a leaf community's input may hold.
pub const MAX_FACTS_PER_COMMUNITY: u32 = 10_000;
/// The largest `minCommunitySize`.
pub const MAX_MIN_COMMUNITY_SIZE: u32 = 10_000;

fn version() -> u32 {
    SETTINGS_VERSION
}
fn levels() -> u32 {
    3
}
fn resolution() -> f64 {
    1.0
}
fn min_community_size() -> u32 {
    3
}
fn reports_graph() -> String {
    format!("{GUARDED_PREFIX}reports")
}
fn language() -> String {
    "en".into()
}
fn max_facts() -> u32 {
    400
}
fn default_exclude() -> Vec<String> {
    vec![
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into(),
        "http://www.w3.org/ns/prov#wasGeneratedBy".into(),
    ]
}

/// What the community build reads (§2.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Projection {
    /// graph IRIs, or patterns ending in `*`
    #[serde(default)]
    pub graphs: Vec<String>,
    /// predicates left out
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    /// an optional weight property on reifiers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<String>,
}

impl Default for Projection {
    fn default() -> Projection {
        Projection {
            graphs: Vec::new(),
            exclude: default_exclude(),
            weight: None,
        }
    }
}

/// `manual` or `auto` refresh (§5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefreshMode {
    #[default]
    Manual,
    Auto,
}

fn debounce() -> u64 {
    300
}
fn changed_fraction() -> f64 {
    0.1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Refresh {
    #[serde(default)]
    pub mode: RefreshMode,
    #[serde(default = "debounce")]
    pub debounce_seconds: u64,
    #[serde(default = "changed_fraction")]
    pub min_changed_fraction: f64,
}

impl Default for Refresh {
    fn default() -> Refresh {
        Refresh {
            mode: RefreshMode::Manual,
            debounce_seconds: debounce(),
            min_changed_fraction: changed_fraction(),
        }
    }
}

fn tokens_per_build() -> u64 {
    2_000_000
}
fn tokens_per_day() -> u64 {
    500_000
}

fn yes() -> bool {
    true
}

fn per_message() -> u64 {
    16
}
fn per_request() -> u64 {
    1_024
}
fn multiplier() -> u64 {
    1
}

/// The allowances of the byte bound for one provider kind (§3.5). The defaults are
/// unmeasured: nothing has checked them against a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ByteBound {
    /// framing tokens per message, the system text included
    #[serde(default = "per_message")]
    pub per_message: u64,
    /// tokens per request for what a provider adds around a schema or a tool
    #[serde(default = "per_request")]
    pub per_request: u64,
    /// tokens per byte of the serialized request
    #[serde(default = "multiplier")]
    pub multiplier: u64,
}

impl Default for ByteBound {
    fn default() -> ByteBound {
        ByteBound {
            per_message: per_message(),
            per_request: per_request(),
            multiplier: multiplier(),
        }
    }
}

/// The provider kinds `budget.byteBound` may name.
pub const BYTE_BOUND_KINDS: [&str; 3] = ["anthropic", "openai", "ollama"];

/// The token budgets of §3.5. Both are hard caps that every model request reserves
/// against before it is sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Budget {
    #[serde(default = "tokens_per_build")]
    pub tokens_per_build: u64,
    #[serde(default = "tokens_per_day")]
    pub tokens_per_day: u64,
    /// whether a provider's own token counting endpoint may bound a request that the
    /// same provider receives
    #[serde(default = "yes")]
    pub provider_counting: bool,
    /// the byte bound's allowances by provider kind; a kind not named uses the defaults
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub byte_bound: std::collections::BTreeMap<String, ByteBound>,
}

impl Budget {
    /// The byte bound's allowances for a provider kind.
    pub fn byte_bound_of(&self, kind: &str) -> ByteBound {
        self.byte_bound.get(kind).copied().unwrap_or_default()
    }
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            tokens_per_build: tokens_per_build(),
            tokens_per_day: tokens_per_day(),
            provider_counting: true,
            byte_bound: Default::default(),
        }
    }
}

/// `graphrag.json` (§2.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GraphragSettings {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub enabled: bool,
    /// whose view the projection and the reports read; required when enabled
    #[serde(default)]
    pub principal: String,
    #[serde(default)]
    pub projection: Projection,
    #[serde(default = "levels")]
    pub levels: u32,
    #[serde(default = "resolution")]
    pub resolution: f64,
    #[serde(default = "min_community_size")]
    pub min_community_size: u32,
    #[serde(default = "reports_graph")]
    pub reports_graph: String,
    #[serde(default = "language")]
    pub language: String,
    #[serde(default = "max_facts")]
    pub max_facts_per_community: u32,
    #[serde(default)]
    pub refresh: Refresh,
    #[serde(default)]
    pub budget: Budget,
}

impl Default for GraphragSettings {
    fn default() -> GraphragSettings {
        GraphragSettings {
            version: SETTINGS_VERSION,
            enabled: false,
            principal: String::new(),
            projection: Projection::default(),
            levels: levels(),
            resolution: resolution(),
            min_community_size: min_community_size(),
            reports_graph: reports_graph(),
            language: language(),
            max_facts_per_community: max_facts(),
            refresh: Refresh::default(),
            budget: Budget::default(),
        }
    }
}

/// Whether `iri` is a graph the disclosure guard protects.
pub fn is_guarded(iri: &str) -> bool {
    iri.starts_with(GUARDED_PREFIX)
}

impl GraphragSettings {
    /// The checks of the values. The version is checked first, so that an object of an
    /// unknown version is refused as a whole.
    pub fn check(&self) -> Result<(), String> {
        if self.version != SETTINGS_VERSION {
            return Err(format!(
                "version {} of the graphrag settings is not supported; this server reads version {SETTINGS_VERSION}",
                self.version
            ));
        }
        if self.levels > MAX_LEVELS {
            return Err(format!("levels must be at most {MAX_LEVELS}"));
        }
        if !(self.resolution.is_finite() && self.resolution > 0.0) {
            return Err("resolution must be a number above 0".into());
        }
        if self.min_community_size == 0 || self.min_community_size > MAX_MIN_COMMUNITY_SIZE {
            return Err(format!(
                "minCommunitySize must be from 1 to {MAX_MIN_COMMUNITY_SIZE}"
            ));
        }
        if self.max_facts_per_community == 0
            || self.max_facts_per_community > MAX_FACTS_PER_COMMUNITY
        {
            return Err(format!(
                "maxFactsPerCommunity must be from 1 to {MAX_FACTS_PER_COMMUNITY}"
            ));
        }
        if !is_guarded(&self.reports_graph) || self.reports_graph.len() == GUARDED_PREFIX.len() {
            return Err(format!(
                "reportsGraph must be an IRI under {GUARDED_PREFIX}, the namespace the disclosure guard protects"
            ));
        }
        if self
            .reports_graph
            .chars()
            .any(|c| c.is_whitespace() || "<>\"{}|^`\\".contains(c))
        {
            return Err("reportsGraph is not a valid IRI".into());
        }
        for g in &self.projection.graphs {
            if g.is_empty() {
                return Err("projection.graphs: an entry is empty".into());
            }
            // a pattern that could match a guarded graph would let reports feed reports
            let wildcard_above = g
                .strip_suffix('*')
                .is_some_and(|stem| GUARDED_PREFIX.starts_with(stem));
            if is_guarded(g) || wildcard_above {
                return Err(format!(
                    "projection.graphs: {g:?} may match the guarded namespace {GUARDED_PREFIX}"
                ));
            }
        }
        if self.language.is_empty() || self.language.len() > 35 {
            return Err("language must be a language tag".into());
        }
        let f = self.refresh.min_changed_fraction;
        if !(f.is_finite() && f > 0.0 && f <= 1.0) {
            return Err("refresh.minChangedFraction must be above 0 and at most 1".into());
        }
        if self.refresh.debounce_seconds == 0 {
            return Err("refresh.debounceSeconds must be at least 1".into());
        }
        if self.budget.tokens_per_build == 0 || self.budget.tokens_per_day == 0 {
            return Err("budget: token budgets must be at least 1".into());
        }
        for (kind, b) in &self.budget.byte_bound {
            if !BYTE_BOUND_KINDS.contains(&kind.as_str()) {
                return Err(format!(
                    "budget.byteBound: {kind:?} is not a provider kind that chats ({})",
                    BYTE_BOUND_KINDS.join(", ")
                ));
            }
            if !(1..=16).contains(&b.multiplier)
                || b.per_message > 4_096
                || b.per_request > 1_000_000
            {
                return Err(format!(
                    "budget.byteBound.{kind}: multiplier must be from 1 to 16, perMessage at most 4096 and perRequest at most 1000000"
                ));
            }
        }
        if self.enabled {
            if self.principal.trim().is_empty() {
                return Err("principal is required when GraphRAG is enabled".into());
            }
            if self.projection.graphs.is_empty() {
                return Err("projection.graphs must name at least one graph".into());
            }
        }
        Ok(())
    }
}

impl Typed for GraphragSettings {
    fn validate(&self, _providers: Providers) -> Result<(), String> {
        self.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Result<GraphragSettings, String> {
        let s: GraphragSettings = serde_json::from_value(v).map_err(|e| e.to_string())?;
        s.check().map(|_| s)
    }

    #[test]
    fn defaults_are_valid_and_disabled() {
        let s = GraphragSettings::default();
        s.check().unwrap();
        assert!(!s.enabled);
        assert_eq!(s.reports_graph, "urn:x-sparkles:graphrag:reports");
        let round: GraphragSettings =
            serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(round, s);
    }

    #[test]
    fn the_spec_example_reads() {
        let s = parse(json!({
            "enabled": true,
            "principal": "reports",
            "projection": { "graphs": ["https://example.org/memory/*", "urn:graph:org"],
                            "exclude": ["rdf:type"], "weight": null },
            "levels": 3, "resolution": 1.0, "minCommunitySize": 3,
            "reportsGraph": "urn:x-sparkles:graphrag:reports",
            "language": "en", "maxFactsPerCommunity": 400,
            "refresh": { "mode": "auto", "debounceSeconds": 300, "minChangedFraction": 0.1 },
            "budget": { "tokensPerBuild": 2000000, "tokensPerDay": 500000 }
        }))
        .unwrap();
        assert_eq!(s.refresh.mode, RefreshMode::Auto);
        assert_eq!(s.version, SETTINGS_VERSION);
    }

    #[test]
    fn byte_bounds_default_per_kind() {
        let s =
            parse(json!({ "budget": { "byteBound": { "anthropic": { "perRequest": 2048 } } } }))
                .unwrap();
        assert_eq!(
            s.budget.byte_bound_of("anthropic"),
            ByteBound {
                per_message: 16,
                per_request: 2048,
                multiplier: 1
            }
        );
        assert_eq!(s.budget.byte_bound_of("openai"), ByteBound::default());
    }

    #[test]
    fn unknown_versions_are_refused() {
        let e = parse(json!({ "version": 2 })).unwrap_err();
        assert!(e.contains("version 2"), "{e}");
        let e = parse(json!({ "version": 0 })).unwrap_err();
        assert!(e.contains("not supported"), "{e}");
    }

    #[test]
    fn unknown_members_are_refused() {
        assert!(parse(json!({ "levelz": 3 })).is_err());
    }

    #[test]
    fn ranges_are_checked() {
        for bad in [
            json!({ "levels": MAX_LEVELS + 1 }),
            json!({ "resolution": 0.0 }),
            json!({ "resolution": -1.0 }),
            json!({ "minCommunitySize": 0 }),
            json!({ "maxFactsPerCommunity": 0 }),
            json!({ "maxFactsPerCommunity": MAX_FACTS_PER_COMMUNITY + 1 }),
            json!({ "refresh": { "minChangedFraction": 0.0 } }),
            json!({ "refresh": { "minChangedFraction": 1.5 } }),
            json!({ "refresh": { "debounceSeconds": 0 } }),
            json!({ "budget": { "tokensPerBuild": 0 } }),
            json!({ "budget": { "byteBound": { "local": {} } } }),
            json!({ "budget": { "byteBound": { "openai": { "multiplier": 0 } } } }),
            json!({ "budget": { "byteBound": { "openai": { "perRequest": 2_000_000 } } } }),
            json!({ "enabled": true, "projection": { "graphs": ["urn:g"] } }),
            json!({ "enabled": true, "principal": "p" }),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn reports_graphs_stay_in_the_guarded_namespace() {
        assert!(parse(json!({ "reportsGraph": "urn:reports" })).is_err());
        assert!(parse(json!({ "reportsGraph": GUARDED_PREFIX })).is_err());
        assert!(parse(json!({ "reportsGraph": "urn:x-sparkles:graphrag:a b" })).is_err());
        parse(json!({ "reportsGraph": "urn:x-sparkles:graphrag:team" })).unwrap();
    }

    #[test]
    fn projections_cannot_read_guarded_graphs() {
        for g in [
            "urn:x-sparkles:graphrag:reports",
            "urn:x-sparkles:*",
            "urn:*",
            "*",
        ] {
            let e = parse(json!({ "enabled": true, "principal": "p",
                                  "projection": { "graphs": [g] } }))
            .unwrap_err();
            assert!(e.contains("guarded"), "{g}: {e}");
        }
        parse(json!({ "enabled": true, "principal": "p",
                      "projection": { "graphs": ["urn:graph:*"] } }))
        .unwrap();
        // an exact IRI above the namespace cannot match a guarded graph (review finding 6)
        parse(json!({ "enabled": true, "principal": "p",
                      "projection": { "graphs": ["urn:x-sparkles:"] } }))
        .unwrap();
    }
}
