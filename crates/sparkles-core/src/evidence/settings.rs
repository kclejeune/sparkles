//! Pure C19 evidence settings data. This module does not admit a profile, choose
//! an authoritative configuration revision or grant managed graph ownership.

use super::{ClaimKind, Sha256Digest};
use crate::{Budget, BudgetKind, Error, Result, nesting, task::Control};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// The current required-manifest graph bound. Other owners share the same capacity;
/// only the admission factory can determine the remaining dataset capacity.
pub const MAX_SETTINGS_GRAPHS: usize = 1024;
pub const MAX_SETTINGS_GRAPH_IRI_BYTES: usize = 4096;
pub const MAX_SETTINGS_BYTES: usize = 16 * 1024 * 1024;
/// The most named review policies one settings object declares.
pub const MAX_POLICIES: usize = 64;
/// The longest policy name, in bytes of `[a-z0-9-]`.
pub const MAX_POLICY_NAME_BYTES: usize = 64;
/// The most scope predicates one policy requires.
pub const MAX_POLICY_SCOPE: usize = 64;

// One decoded UTF-8 byte takes at most six raw JSON bytes (\u00xx).
// Surrogate pairs take twelve raw bytes for four decoded bytes. This therefore
// preserves every supported IRI while bounding Serde scratch before any visitor.
const MAX_RAW_STRING_BYTES: usize = 6 * MAX_SETTINGS_GRAPH_IRI_BYTES;

/// Complete effective settings of the evidence profile, as `evidence.json` holds them.
/// Unknown fields are refused even while disabled. The roles added after the first
/// two default to empty, so an object that names only `sourceRecords` and `proposals`
/// still reads. Canonical bytes always name every field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub format: u32,
    pub profile: u32,
    pub enabled: bool,
    /// Graphs of source revisions, renditions and passages.
    #[serde(deserialize_with = "graphs")]
    pub source_records: Vec<String>,
    /// Graphs of claim occurrences, evidence references and evidence links.
    #[serde(deserialize_with = "graphs")]
    pub proposals: Vec<String>,
    /// Graphs of review decisions.
    #[serde(default, deserialize_with = "graphs")]
    pub decisions: Vec<String>,
    /// Graphs of acceptance supports, which own projected triples.
    #[serde(default, deserialize_with = "graphs")]
    pub supports: Vec<String>,
    /// Graphs of identity proposals and resolution maps.
    #[serde(default, deserialize_with = "graphs")]
    pub identity: Vec<String>,
    /// Dedicated graphs of accepted projections. A projection graph has no other role.
    #[serde(default, deserialize_with = "graphs")]
    pub projections: Vec<String>,
    /// Named review policies of claims and assessments.
    #[serde(default, deserialize_with = "policies")]
    pub policies: BTreeMap<String, Policy>,
    /// Named matching policies of identity proposals. Their names share one namespace
    /// and one bound with `policies`.
    #[serde(default, deserialize_with = "policies")]
    pub matching_policies: BTreeMap<String, MatchingPolicy>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: 1,
            profile: 1,
            enabled: false,
            source_records: Vec::new(),
            proposals: Vec::new(),
            decisions: Vec::new(),
            supports: Vec::new(),
            identity: Vec::new(),
            projections: Vec::new(),
            policies: BTreeMap::new(),
            matching_policies: BTreeMap::new(),
        }
    }
}

/// What a projection does with two admitted positive claims that a policy records as
/// conflicting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictMode {
    /// Refuse the admission that would make the projection inconsistent.
    Refuse,
    /// Admit both, in a projection labelled as holding recorded conflicts.
    AdmitLabelled,
}

impl ConflictMode {
    fn as_str(self) -> &'static str {
        match self {
            ConflictMode::Refuse => "refuse",
            ConflictMode::AdmitLabelled => "admitLabelled",
        }
    }
}

/// One named review policy. A review request names the policy and its `version`, and a
/// changed version refuses a stale request. The policy admits positive claims of its
/// kinds only. Negative and uncertain claims never enter a projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    /// Raised by the administrator whenever the policy's meaning changes.
    pub version: u32,
    /// The claim kinds the policy can admit.
    pub admitted_kinds: Vec<ClaimKind>,
    /// Scope predicates an admitted claim must carry. Empty requires none.
    #[serde(default, deserialize_with = "graphs")]
    pub required_scope: Vec<String>,
    pub conflicts: ConflictMode,
    /// The evidence boundary: the source-record and proposal graphs whose records the
    /// policy's reviews may use. The projection is built inside this boundary.
    #[serde(deserialize_with = "graphs")]
    pub boundary: Vec<String>,
    /// The decisions graph the policy's reviews append to.
    pub decisions: String,
    /// The supports graph that owns the policy's projected triples.
    pub supports: String,
    /// The projection graph the policy writes. Each projection graph has one policy.
    pub output: String,
}

/// One named matching policy. A review of identity proposals names the policy and its
/// `version`, as a claim review does. The policy's accepted `possibleMatch` proposals
/// form clusters of entities, and its output graph holds the current resolution map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MatchingPolicy {
    /// Raised by the administrator whenever the policy's meaning changes.
    pub version: u32,
    /// The identity, source-record and proposal graphs whose records the policy's
    /// reviews may use. It holds at least one identity graph, where the proposals are.
    #[serde(deserialize_with = "graphs")]
    pub boundary: Vec<String>,
    /// The decisions graph the policy's reviews append to.
    pub decisions: String,
    /// The identity graph that holds the policy's resolution map revisions.
    pub maps: String,
    /// The projection graph that holds the current resolution map. Each projection graph
    /// has one policy of either kind.
    pub output: String,
}

fn kind_name(kind: ClaimKind) -> &'static str {
    match kind {
        ClaimKind::Reported => "reported",
        ClaimKind::Observation => "observation",
        ClaimKind::Hypothesis => "hypothesis",
        ClaimKind::Preference => "preference",
    }
}

fn policy_name(name: &str) -> std::result::Result<(), &'static str> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_POLICY_NAME_BYTES {
        return Err("a policy name has 1 to 64 bytes");
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        || bytes[0] == b'-'
    {
        return Err("a policy name uses a-z, 0-9 and '-' and starts with a letter or digit");
    }
    Ok(())
}

/// Canonical settings data, never an authenticated configuration or owner token.
/// Private fields keep the exact bytes, JSON value and digest consistent.
#[derive(Clone, Debug)]
pub struct CanonicalSettings {
    value: serde_json::Value,
    bytes: Vec<u8>,
    digest: Sha256Digest,
}

impl CanonicalSettings {
    pub fn value(&self) -> &serde_json::Value {
        &self.value
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }
}

impl Settings {
    /// Decode a complete effective object with a version preflight, preserving
    /// duplicate-field refusal rather than first collapsing it into a JSON map.
    pub fn from_slice(bytes: &[u8], control: &Control) -> Result<Self> {
        control.check()?;
        input_bound(bytes.len())?;
        let mut scanner = nesting::Scanner::new(nesting::Syntax::Json, nesting::MAX_ELEMENTS);
        let mut strings = StringBounds::default();
        for chunk in bytes.chunks(64 * 1024) {
            control.check()?;
            scanner
                .feed(chunk)
                .map_err(|_| Error::invalid("evidence settings exceed the JSON nesting bound"))?;
            strings.feed(chunk)?;
        }
        #[derive(Deserialize)]
        struct Header {
            format: u32,
            profile: u32,
        }
        let header: Header = serde_json::from_slice(bytes).map_err(|error| {
            Error::invalid(format!("invalid evidence settings header: {error}"))
        })?;
        versions(header.format, header.profile)?;
        control.check()?;
        let settings: Self = serde_json::from_slice(bytes)
            .map_err(|error| Error::invalid(format!("invalid evidence settings: {error}")))?;
        settings.validate(control)?;
        Ok(settings)
    }

    /// Validate syntax, roles and policies. Collision with another owner, branch
    /// origins, caller grants and admission are deliberately not proved here.
    pub fn validate(&self, control: &Control) -> Result<()> {
        control.check()?;
        versions(self.format, self.profile)?;
        if self.enabled && (self.source_records.is_empty() || self.proposals.is_empty()) {
            return Err(Error::invalid(
                "enabled evidence needs sourceRecords and proposals graphs",
            ));
        }
        let mut unique = BTreeSet::new();
        for (role, graphs) in self.roles() {
            if graphs.len() > MAX_SETTINGS_GRAPHS {
                return Err(Error::invalid(format!(
                    "{role} exceeds the managed graph count bound"
                )));
            }
            let mut seen = BTreeSet::new();
            for graph in graphs {
                control.check()?;
                graph_iri(graph).map_err(|message| Error::invalid(format!("{role}: {message}")))?;
                if !seen.insert(graph.as_str()) {
                    return Err(Error::invalid(format!("{role} contains a duplicate graph")));
                }
                unique.insert(graph.as_str());
                if unique.len() > MAX_SETTINGS_GRAPHS {
                    return Err(Error::invalid(
                        "evidence settings exceed the unique managed graph bound",
                    ));
                }
            }
        }
        let projections: BTreeSet<&str> = self.projections.iter().map(String::as_str).collect();
        for (role, graphs) in self.roles() {
            if role == "projections" {
                continue;
            }
            if let Some(graph) = graphs.iter().find(|g| projections.contains(g.as_str())) {
                return Err(Error::invalid(format!(
                    "projections: <{graph}> also has the role {role}; a projection graph is dedicated"
                )));
            }
        }
        if self.policies.len() > MAX_POLICIES {
            return Err(Error::invalid(format!(
                "more than {MAX_POLICIES} review policies"
            )));
        }
        let boundary: BTreeSet<&str> = self
            .source_records
            .iter()
            .chain(&self.proposals)
            .map(String::as_str)
            .collect();
        let mut outputs = BTreeMap::new();
        for (name, policy) in &self.policies {
            control.check()?;
            let invalid = |message: String| Error::invalid(format!("policies.{name}: {message}"));
            policy_name(name).map_err(|m| invalid(m.to_string()))?;
            if policy.version == 0 {
                return Err(invalid("version starts at 1".into()));
            }
            if policy.admitted_kinds.is_empty() {
                return Err(invalid("admittedKinds names no claim kind".into()));
            }
            let mut kinds = BTreeSet::new();
            if !policy
                .admitted_kinds
                .iter()
                .all(|k| kinds.insert(kind_name(*k)))
            {
                return Err(invalid("admittedKinds names a kind twice".into()));
            }
            if policy.required_scope.len() > MAX_POLICY_SCOPE {
                return Err(invalid(format!(
                    "requiredScope names more than {MAX_POLICY_SCOPE} predicates"
                )));
            }
            let mut seen = BTreeSet::new();
            for predicate in &policy.required_scope {
                graph_iri(predicate)
                    .map_err(|_| invalid("requiredScope needs absolute IRIs".into()))?;
                if !seen.insert(predicate.as_str()) {
                    return Err(invalid("requiredScope names a predicate twice".into()));
                }
            }
            if policy.boundary.is_empty() {
                return Err(invalid("boundary names no graph".into()));
            }
            let mut seen = BTreeSet::new();
            for graph in &policy.boundary {
                if !seen.insert(graph.as_str()) {
                    return Err(invalid("boundary names a graph twice".into()));
                }
                if !boundary.contains(graph.as_str()) {
                    return Err(invalid(format!(
                        "boundary graph <{graph}> is not a sourceRecords or proposals graph"
                    )));
                }
            }
            for (field, graph, role) in [
                ("decisions", &policy.decisions, &self.decisions),
                ("supports", &policy.supports, &self.supports),
                ("output", &policy.output, &self.projections),
            ] {
                graph_iri(graph).map_err(|m| invalid(format!("{field}: {m}")))?;
                if !role.contains(graph) {
                    let named = if field == "output" {
                        "projections"
                    } else {
                        field
                    };
                    return Err(invalid(format!(
                        "{field} graph <{graph}> is not one of the {named} graphs"
                    )));
                }
            }
            if let Some(other) = outputs.insert(policy.output.as_str(), name.as_str()) {
                return Err(invalid(format!(
                    "output graph <{}> is already the output of policy {other}",
                    policy.output
                )));
            }
        }
        if self.policies.len() + self.matching_policies.len() > MAX_POLICIES {
            return Err(Error::invalid(format!(
                "more than {MAX_POLICIES} review and matching policies"
            )));
        }
        let identity_boundary: BTreeSet<&str> = boundary
            .iter()
            .copied()
            .chain(self.identity.iter().map(String::as_str))
            .collect();
        for (name, policy) in &self.matching_policies {
            control.check()?;
            let invalid =
                |message: String| Error::invalid(format!("matchingPolicies.{name}: {message}"));
            policy_name(name).map_err(|m| invalid(m.to_string()))?;
            if self.policies.contains_key(name) {
                return Err(invalid(
                    "a review policy has the same name; policy names are shared".into(),
                ));
            }
            if policy.version == 0 {
                return Err(invalid("version starts at 1".into()));
            }
            let mut seen = BTreeSet::new();
            for graph in &policy.boundary {
                if !seen.insert(graph.as_str()) {
                    return Err(invalid("boundary names a graph twice".into()));
                }
                if !identity_boundary.contains(graph.as_str()) {
                    return Err(invalid(format!(
                        "boundary graph <{graph}> is not an identity, sourceRecords or proposals graph"
                    )));
                }
            }
            if !policy.boundary.iter().any(|g| self.identity.contains(g)) {
                return Err(invalid("boundary names no identity graph".into()));
            }
            for (field, graph, role, named) in [
                ("decisions", &policy.decisions, &self.decisions, "decisions"),
                ("maps", &policy.maps, &self.identity, "identity"),
                ("output", &policy.output, &self.projections, "projections"),
            ] {
                graph_iri(graph).map_err(|m| invalid(format!("{field}: {m}")))?;
                if !role.contains(graph) {
                    return Err(invalid(format!(
                        "{field} graph <{graph}> is not one of the {named} graphs"
                    )));
                }
            }
            if let Some(other) = outputs.insert(policy.output.as_str(), name.as_str()) {
                return Err(invalid(format!(
                    "output graph <{}> is already the output of policy {other}",
                    policy.output
                )));
            }
        }
        if let Some(graph) = self
            .projections
            .iter()
            .find(|g| !outputs.contains_key(g.as_str()))
        {
            return Err(Error::invalid(format!(
                "projections: <{graph}> is not the output of any policy"
            )));
        }
        control.check()
    }

    fn roles(&self) -> [(&'static str, &Vec<String>); 6] {
        [
            ("sourceRecords", &self.source_records),
            ("proposals", &self.proposals),
            ("decisions", &self.decisions),
            ("supports", &self.supports),
            ("identity", &self.identity),
            ("projections", &self.projections),
        ]
    }

    /// Every graph of every role, sorted and without duplicates: the graphs the
    /// profile manages while it is enabled.
    pub fn managed_graphs(&self) -> Vec<String> {
        let all: BTreeSet<&str> = self
            .roles()
            .into_iter()
            .flat_map(|(_, graphs)| graphs.iter().map(String::as_str))
            .collect();
        all.into_iter().map(str::to_owned).collect()
    }

    /// Version 1 uses sorted graph sets and lexically ordered JSON object keys.
    /// Every field is explicit, including disabled/empty settings. IRI lexical
    /// values are preserved; cross-role overlap does not duplicate ownership.
    pub fn canonical(&self, control: &Control) -> Result<CanonicalSettings> {
        self.validate(control)?;
        fn sorted(graphs: &[String]) -> Vec<&str> {
            let mut out: Vec<_> = graphs.iter().map(String::as_str).collect();
            out.sort_unstable();
            out
        }
        // Wire-key declaration order is lexical even if serde_json later enables
        // insertion-ordered maps. A generic sorted-key envelope gets identical bytes.
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PolicyBody<'a> {
            admitted_kinds: Vec<&'static str>,
            boundary: Vec<&'a str>,
            conflicts: &'static str,
            decisions: &'a str,
            output: &'a str,
            required_scope: Vec<&'a str>,
            supports: &'a str,
            version: u32,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct MatchingBody<'a> {
            boundary: Vec<&'a str>,
            decisions: &'a str,
            maps: &'a str,
            output: &'a str,
            version: u32,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            decisions: Vec<&'a str>,
            enabled: bool,
            format: u32,
            identity: Vec<&'a str>,
            matching_policies: BTreeMap<&'a str, MatchingBody<'a>>,
            policies: BTreeMap<&'a str, PolicyBody<'a>>,
            profile: u32,
            projections: Vec<&'a str>,
            proposals: Vec<&'a str>,
            source_records: Vec<&'a str>,
            supports: Vec<&'a str>,
        }
        control.check()?;
        let policies = self
            .policies
            .iter()
            .map(|(name, p)| {
                let mut kinds: Vec<_> = p.admitted_kinds.iter().map(|k| kind_name(*k)).collect();
                kinds.sort_unstable();
                (
                    name.as_str(),
                    PolicyBody {
                        admitted_kinds: kinds,
                        boundary: sorted(&p.boundary),
                        conflicts: p.conflicts.as_str(),
                        decisions: &p.decisions,
                        output: &p.output,
                        required_scope: sorted(&p.required_scope),
                        supports: &p.supports,
                        version: p.version,
                    },
                )
            })
            .collect();
        let bytes = serde_json::to_vec(&Body {
            decisions: sorted(&self.decisions),
            enabled: self.enabled,
            format: self.format,
            identity: sorted(&self.identity),
            matching_policies: self
                .matching_policies
                .iter()
                .map(|(name, p)| {
                    (
                        name.as_str(),
                        MatchingBody {
                            boundary: sorted(&p.boundary),
                            decisions: &p.decisions,
                            maps: &p.maps,
                            output: &p.output,
                            version: p.version,
                        },
                    )
                })
                .collect(),
            policies,
            profile: self.profile,
            projections: sorted(&self.projections),
            proposals: sorted(&self.proposals),
            source_records: sorted(&self.source_records),
            supports: sorted(&self.supports),
        })
        .map_err(|error| Error::invalid(format!("cannot encode evidence settings: {error}")))?;
        input_bound(bytes.len())?;
        let digest = Sha256Digest::of_bytes(&bytes, control)?;
        let value = serde_json::from_slice(&bytes).map_err(|error| {
            Error::invalid(format!(
                "cannot decode canonical evidence settings: {error}"
            ))
        })?;
        control.check()?;
        Ok(CanonicalSettings {
            value,
            bytes,
            digest,
        })
    }
}

/// No decoding, allocation or semantic interpretation. Full JSON/Unicode and
/// duplicate-field validation still belongs to Serde after universal input bounds.
#[derive(Default)]
struct StringBounds {
    quoted: bool,
    escaped: bool,
    bytes: usize,
}

impl StringBounds {
    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        for byte in bytes {
            if !self.quoted {
                if *byte == b'"' {
                    self.quoted = true;
                    self.escaped = false;
                    self.bytes = 0;
                }
            } else if *byte == b'"' && !self.escaped {
                self.quoted = false;
            } else {
                self.bytes += 1;
                if self.bytes > MAX_RAW_STRING_BYTES {
                    return Err(Error::BudgetExceeded(Budget {
                        kind: BudgetKind::DecompressedBytes,
                        limit: MAX_RAW_STRING_BYTES as u64,
                        requested: self.bytes as u64,
                    }));
                }
                self.escaped = !self.escaped && *byte == b'\\';
            }
        }
        Ok(())
    }
}

fn versions(format: u32, profile: u32) -> Result<()> {
    if format != 1 || profile != 1 {
        return Err(Error::Unsupported(format!(
            "unsupported-profile: evidence settings format {format}, profile {profile}"
        )));
    }
    Ok(())
}

fn input_bound(bytes: usize) -> Result<()> {
    if bytes > MAX_SETTINGS_BYTES {
        return Err(Error::BudgetExceeded(Budget {
            kind: BudgetKind::DecompressedBytes,
            limit: MAX_SETTINGS_BYTES as u64,
            requested: bytes as u64,
        }));
    }
    Ok(())
}

fn graph_iri(graph: &str) -> std::result::Result<(), &'static str> {
    if graph.len() > MAX_SETTINGS_GRAPH_IRI_BYTES {
        return Err("graph IRI exceeds the required-manifest byte bound");
    }
    oxiri::Iri::parse(graph).map_err(|_| "graph requires an absolute IRI")?;
    Ok(())
}

fn graphs<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    struct Graph(String);
    impl<'de> Deserialize<'de> for Graph {
        fn deserialize<D: Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct Iri;
            impl<'de> de::Visitor<'de> for Iri {
                type Value = Graph;
                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("an absolute graph IRI within the format byte bound")
                }
                fn visit_str<E: de::Error>(self, graph: &str) -> std::result::Result<Graph, E> {
                    graph_iri(graph).map_err(E::custom)?;
                    Ok(Graph(graph.to_owned()))
                }
            }
            deserializer.deserialize_str(Iri)
        }
    }
    struct Graphs;
    impl<'de> de::Visitor<'de> for Graphs {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded array of absolute graph IRIs")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<Vec<String>, A::Error> {
            let mut values = Vec::new();
            loop {
                if values.len() == MAX_SETTINGS_GRAPHS {
                    return if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                        Err(de::Error::custom(
                            "graph role exceeds the managed graph count bound",
                        ))
                    } else {
                        Ok(values)
                    };
                }
                match sequence.next_element::<Graph>()? {
                    Some(Graph(graph)) => values.push(graph),
                    None => return Ok(values),
                }
            }
        }
    }
    deserializer.deserialize_seq(Graphs)
}

/// A bounded object of named policies that refuses a name given twice, which a plain
/// map would silently collapse.
fn policies<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, T>, D::Error> {
    struct Policies<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> de::Visitor<'de> for Policies<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object of at most 64 named review policies")
        }
        fn visit_map<A: de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(name) = map.next_key::<String>()? {
                if out.len() == MAX_POLICIES {
                    return Err(de::Error::custom(format!(
                        "more than {MAX_POLICIES} review policies"
                    )));
                }
                policy_name(&name).map_err(de::Error::custom)?;
                if out.contains_key(&name) {
                    return Err(de::Error::custom(format!("policy {name} is defined twice")));
                }
                let policy = map.next_value::<T>()?;
                out.insert(name, policy);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(Policies(std::marker::PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> Settings {
        Settings {
            enabled: true,
            source_records: vec!["urn:z".into(), "urn:a".into()],
            proposals: vec!["urn:p".into()],
            ..Settings::default()
        }
    }

    #[test]
    fn canonical_settings_are_sorted_explicit_and_match_an_independent_golden() {
        let settings = enabled();
        let canonical = settings.canonical(&Control::none()).unwrap();
        assert_eq!(canonical.bytes(), br#"{"decisions":[],"enabled":true,"format":1,"identity":[],"matchingPolicies":{},"policies":{},"profile":1,"projections":[],"proposals":["urn:p"],"sourceRecords":["urn:a","urn:z"],"supports":[]}"#);
        assert_eq!(
            canonical.digest().to_string(),
            "sha256:31ae14441a76c5997057785d3bd60f5e658d15708611adc18f006b8e08c7e9cf"
        );
        assert_eq!(
            serde_json::to_vec(canonical.value()).unwrap(),
            canonical.bytes()
        );
        let decoded = Settings::from_slice(canonical.bytes(), &Control::none()).unwrap();
        assert_eq!(
            decoded.canonical(&Control::none()).unwrap().digest(),
            canonical.digest()
        );
        let mut reordered = settings.clone();
        reordered.source_records.reverse();
        assert_eq!(
            reordered.canonical(&Control::none()).unwrap().digest(),
            canonical.digest()
        );
        let default = Settings::default().canonical(&Control::none()).unwrap();
        assert_eq!(
            default.bytes(),
            br#"{"decisions":[],"enabled":false,"format":1,"identity":[],"matchingPolicies":{},"policies":{},"profile":1,"projections":[],"proposals":[],"sourceRecords":[],"supports":[]}"#
        );
        assert_eq!(
            default.digest().to_string(),
            "sha256:824b515f8aa27bb74ca59d2395e1e025ddcb895570ade6bb4975fee237bad518"
        );
    }

    fn reviewed() -> Settings {
        let mut settings = enabled();
        settings.decisions = vec!["urn:d".into()];
        settings.supports = vec!["urn:s".into()];
        settings.identity = vec!["urn:i".into()];
        settings.projections = vec!["urn:out".into()];
        settings.policies.insert(
            "advisories".into(),
            Policy {
                version: 2,
                admitted_kinds: vec![ClaimKind::Reported, ClaimKind::Observation],
                required_scope: vec!["urn:scope:release".into()],
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec!["urn:p".into(), "urn:a".into()],
                decisions: "urn:d".into(),
                supports: "urn:s".into(),
                output: "urn:out".into(),
            },
        );
        settings
    }

    #[test]
    fn policies_and_review_roles_have_an_independent_canonical_golden() {
        let settings = reviewed();
        let canonical = settings.canonical(&Control::none()).unwrap();
        assert_eq!(
            canonical.bytes(),
            br#"{"decisions":["urn:d"],"enabled":true,"format":1,"identity":["urn:i"],"matchingPolicies":{},"policies":{"advisories":{"admittedKinds":["observation","reported"],"boundary":["urn:a","urn:p"],"conflicts":"admitLabelled","decisions":"urn:d","output":"urn:out","requiredScope":["urn:scope:release"],"supports":"urn:s","version":2}},"profile":1,"projections":["urn:out"],"proposals":["urn:p"],"sourceRecords":["urn:a","urn:z"],"supports":["urn:s"]}"#
        );
        assert_eq!(
            canonical.digest().to_string(),
            "sha256:7a2edb8c95bf9f818e9cdd0ced09077b50230c4ae144e7a64d69c3504d95c084"
        );
        let decoded = Settings::from_slice(canonical.bytes(), &Control::none()).unwrap();
        assert_eq!(
            decoded.canonical(&Control::none()).unwrap().bytes(),
            canonical.bytes()
        );
        assert_eq!(
            settings.managed_graphs(),
            [
                "urn:a", "urn:d", "urn:i", "urn:out", "urn:p", "urn:s", "urn:z"
            ]
        );
        // an object that names only the first two roles reads with empty review roles
        let old = br#"{"format":1,"profile":1,"enabled":true,"sourceRecords":["urn:a"],"proposals":["urn:p"]}"#;
        let old = Settings::from_slice(old, &Control::none()).unwrap();
        assert!(old.decisions.is_empty() && old.policies.is_empty());
    }

    #[test]
    fn policies_must_reference_their_roles_and_projections_stay_dedicated() {
        let check = |change: &dyn Fn(&mut Settings)| {
            let mut settings = reviewed();
            change(&mut settings);
            settings.validate(&Control::none())
        };
        assert!(check(&|_| {}).is_ok());
        let policy = |s: &mut Settings| s.policies.get_mut("advisories").unwrap().clone();
        let set = |s: &mut Settings, p: Policy| {
            s.policies.insert("advisories".into(), p);
        };
        type Change = Box<dyn Fn(&mut Settings)>;
        let refusals: Vec<(&str, Change)> = vec![
            (
                "projection reused",
                Box::new(|s| s.proposals.push("urn:out".into())),
            ),
            (
                "projection as identity",
                Box::new(|s| s.identity.push("urn:out".into())),
            ),
            (
                "output not a projection",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.output = "urn:d".into();
                    set(s, p)
                }),
            ),
            (
                "decisions not a decisions graph",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.decisions = "urn:s".into();
                    set(s, p)
                }),
            ),
            (
                "supports not a supports graph",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.supports = "urn:d".into();
                    set(s, p)
                }),
            ),
            (
                "boundary outside the evidence roles",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.boundary = vec!["urn:d".into()];
                    set(s, p)
                }),
            ),
            (
                "empty boundary",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.boundary.clear();
                    set(s, p)
                }),
            ),
            (
                "duplicate boundary",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.boundary = vec!["urn:a".into(), "urn:a".into()];
                    set(s, p)
                }),
            ),
            (
                "no kinds",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.admitted_kinds.clear();
                    set(s, p)
                }),
            ),
            (
                "kind twice",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.admitted_kinds = vec![ClaimKind::Reported, ClaimKind::Reported];
                    set(s, p)
                }),
            ),
            (
                "version zero",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.version = 0;
                    set(s, p)
                }),
            ),
            (
                "relative scope",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.required_scope = vec!["release".into()];
                    set(s, p)
                }),
            ),
            (
                "scope twice",
                Box::new(move |s| {
                    let mut p = policy(s);
                    p.required_scope = vec!["urn:x".into(), "urn:x".into()];
                    set(s, p)
                }),
            ),
            (
                "two policies, one output",
                Box::new(move |s| {
                    let p = policy(s);
                    s.policies.insert("second".into(), p);
                }),
            ),
            (
                "bad name",
                Box::new(move |s| {
                    let p = policy(s);
                    s.policies.insert("Upper".into(), p);
                }),
            ),
            (
                "leading dash",
                Box::new(move |s| {
                    let p = policy(s);
                    s.policies.clear();
                    s.policies.insert("-x".into(), p);
                }),
            ),
        ];
        for (case, change) in refusals {
            assert!(check(&*change).is_err(), "{case}");
        }
        // too many policies
        let mut settings = reviewed();
        let template = settings.policies["advisories"].clone();
        settings.policies.clear();
        settings.projections.clear();
        for index in 0..=MAX_POLICIES {
            let output = format!("urn:out:{index}");
            settings.projections.push(output.clone());
            settings.policies.insert(
                format!("p{index}"),
                Policy {
                    output,
                    ..template.clone()
                },
            );
        }
        assert!(settings.validate(&Control::none()).is_err());
        let (_, last) = settings.policies.pop_last().unwrap();
        settings.projections.retain(|g| *g != last.output);
        assert!(settings.validate(&Control::none()).is_ok());
        // every projection graph needs its policy
        settings.projections.push("urn:out:unowned".into());
        assert!(settings.validate(&Control::none()).is_err());
        let mut settings = reviewed();
        settings.policies.clear();
        assert!(settings.validate(&Control::none()).is_err());
        settings.projections.clear();
        assert!(settings.validate(&Control::none()).is_ok());
    }

    fn matched() -> Settings {
        let mut settings = reviewed();
        settings.identity.push("urn:m".into());
        settings.projections.push("urn:map".into());
        settings.matching_policies.insert(
            "people".into(),
            MatchingPolicy {
                version: 3,
                boundary: vec!["urn:p".into(), "urn:i".into()],
                decisions: "urn:d".into(),
                maps: "urn:m".into(),
                output: "urn:map".into(),
            },
        );
        settings
    }

    #[test]
    fn matching_policies_have_an_independent_golden_and_reference_their_roles() {
        let settings = matched();
        let canonical = settings.canonical(&Control::none()).unwrap();
        assert_eq!(
            canonical.bytes(),
            br#"{"decisions":["urn:d"],"enabled":true,"format":1,"identity":["urn:i","urn:m"],"matchingPolicies":{"people":{"boundary":["urn:i","urn:p"],"decisions":"urn:d","maps":"urn:m","output":"urn:map","version":3}},"policies":{"advisories":{"admittedKinds":["observation","reported"],"boundary":["urn:a","urn:p"],"conflicts":"admitLabelled","decisions":"urn:d","output":"urn:out","requiredScope":["urn:scope:release"],"supports":"urn:s","version":2}},"profile":1,"projections":["urn:map","urn:out"],"proposals":["urn:p"],"sourceRecords":["urn:a","urn:z"],"supports":["urn:s"]}"#
        );
        assert_eq!(
            canonical.digest().to_string(),
            "sha256:603171de274f97d4af635aeb6b7f6ddcd170aafde6dbffa0c6d1bb56b9642303"
        );
        let decoded = Settings::from_slice(canonical.bytes(), &Control::none()).unwrap();
        assert_eq!(decoded, settings);
        let text = std::str::from_utf8(canonical.bytes()).unwrap();
        for (from, to) in [
            (
                r#""version":3"#,
                r#""version":3,"relations":["possibleMatch"]"#,
            ),
            (r#""maps":"urn:m""#, r#""maps":["urn:m"]"#),
        ] {
            assert!(text.contains(from), "{from}");
            let changed = text.replace(from, to);
            assert!(Settings::from_slice(changed.as_bytes(), &Control::none()).is_err());
        }
        let check = |change: &dyn Fn(&mut Settings)| {
            let mut settings = matched();
            change(&mut settings);
            settings.validate(&Control::none())
        };
        assert!(check(&|_| {}).is_ok());
        type Change = Box<dyn Fn(&mut Settings)>;
        let with = |f: fn(&mut MatchingPolicy)| -> Change {
            Box::new(move |s: &mut Settings| f(s.matching_policies.get_mut("people").unwrap()))
        };
        let refusals: Vec<(&str, Change)> = vec![
            ("version zero", with(|p| p.version = 0)),
            ("maps outside identity", with(|p| p.maps = "urn:p".into())),
            (
                "decisions outside decisions",
                with(|p| p.decisions = "urn:s".into()),
            ),
            (
                "output outside projections",
                with(|p| p.output = "urn:i".into()),
            ),
            (
                "output of a review policy",
                with(|p| p.output = "urn:out".into()),
            ),
            (
                "boundary without identity",
                with(|p| p.boundary = vec!["urn:p".into()]),
            ),
            (
                "boundary outside roles",
                with(|p| p.boundary = vec!["urn:i".into(), "urn:d".into()]),
            ),
            (
                "boundary twice",
                with(|p| p.boundary = vec!["urn:i".into(), "urn:i".into()]),
            ),
            ("empty boundary", with(|p| p.boundary.clear())),
            (
                "shared name",
                Box::new(|s| {
                    let p = s.matching_policies.remove("people").unwrap();
                    s.matching_policies.insert("advisories".into(), p);
                }),
            ),
            (
                "bad name",
                Box::new(|s| {
                    let p = s.matching_policies.remove("people").unwrap();
                    s.matching_policies.insert("People".into(), p);
                }),
            ),
            (
                "unowned map output",
                Box::new(|s| {
                    s.matching_policies.clear();
                }),
            ),
        ];
        for (case, change) in refusals {
            assert!(check(&*change).is_err(), "{case}");
        }
        // the two kinds of policy share one bound
        let mut settings = matched();
        let template = settings.matching_policies["people"].clone();
        for index in 0..(MAX_POLICIES - 1) {
            let output = format!("urn:map:{index}");
            settings.projections.push(output.clone());
            settings.matching_policies.insert(
                format!("m{index}"),
                MatchingPolicy {
                    output,
                    ..template.clone()
                },
            );
        }
        assert!(settings.validate(&Control::none()).is_err());
        settings.matching_policies.remove("m0");
        settings.projections.retain(|g| g != "urn:map:0");
        assert!(settings.validate(&Control::none()).is_ok());
    }

    #[test]
    fn policy_objects_refuse_duplicate_names_unknown_members_and_bad_enums() {
        let base = reviewed().canonical(&Control::none()).unwrap();
        let text = std::str::from_utf8(base.bytes()).unwrap();
        let body = r#"{"admittedKinds":["reported"],"boundary":["urn:a"],"conflicts":"refuse","decisions":"urn:d","output":"urn:out","requiredScope":[],"supports":"urn:s","version":1}"#;
        let twice = text.replace(
            r#""policies":{"#,
            &format!(r#""policies":{{"advisories":{body},"#),
        );
        assert!(Settings::from_slice(twice.as_bytes(), &Control::none()).is_err());
        for (from, to) in [
            (r#""version":2"#, r#""version":2,"reviewers":["urn:x"]"#),
            (r#""conflicts":"admitLabelled""#, r#""conflicts":"merge""#),
            (
                r#""admittedKinds":["observation","reported"]"#,
                r#""admittedKinds":["claim"]"#,
            ),
            (r#""output":"urn:out""#, r#""output":["urn:out"]"#),
            (r#""advisories""#, r#""Advisories""#),
        ] {
            assert!(text.contains(from), "{from}");
            let changed = text.replace(from, to);
            assert!(
                Settings::from_slice(changed.as_bytes(), &Control::none()).is_err(),
                "{to}"
            );
        }
        let without_scope = text.replace(r#""requiredScope":["urn:scope:release"],"#, "");
        let decoded = Settings::from_slice(without_scope.as_bytes(), &Control::none()).unwrap();
        assert!(decoded.policies["advisories"].required_scope.is_empty());
    }

    #[test]
    fn role_overlap_does_not_merge_field_meaning_or_iri_lexical_values() {
        let mut settings = enabled();
        settings.proposals = settings.source_records.clone();
        assert!(settings.validate(&Control::none()).is_ok());
        let first = settings.canonical(&Control::none()).unwrap().digest();
        settings.enabled = false;
        assert_ne!(
            settings.canonical(&Control::none()).unwrap().digest(),
            first
        );
        settings.enabled = true;
        settings.source_records = vec!["urn:é".into()];
        let composed = settings.canonical(&Control::none()).unwrap().digest();
        settings.source_records = vec!["urn:e\u{301}".into()];
        assert_ne!(
            settings.canonical(&Control::none()).unwrap().digest(),
            composed
        );
        settings.source_records = vec!["urn:%61".into()];
        let percent = settings.canonical(&Control::none()).unwrap().digest();
        settings.source_records = vec!["urn:a".into()];
        assert_ne!(
            settings.canonical(&Control::none()).unwrap().digest(),
            percent
        );
        let escaped = br#"{"format":1,"profile":1,"enabled":true,"sourceRecords":["urn:\u0061"],"proposals":["urn:p"]}"#;
        let normal = br#"{"format":1,"profile":1,"enabled":true,"sourceRecords":["urn:a"],"proposals":["urn:p"]}"#;
        assert_eq!(
            Settings::from_slice(escaped, &Control::none())
                .unwrap()
                .canonical(&Control::none())
                .unwrap()
                .digest(),
            Settings::from_slice(normal, &Control::none())
                .unwrap()
                .canonical(&Control::none())
                .unwrap()
                .digest()
        );
    }

    #[test]
    fn unsupported_versions_refuse_before_future_shapes_without_default_fallback() {
        for input in [
            r#"{"format":2,"profile":1,"future":{"shape":"different"}}"#,
            r#"{"format":1,"profile":2,"sourceRecords":{"new":"shape"}}"#,
            r#"{"format":0,"profile":1}"#,
        ] {
            assert!(
                matches!(
                    Settings::from_slice(input.as_bytes(), &Control::none()),
                    Err(Error::Unsupported(_))
                ),
                "{input}"
            );
        }
        for version in [0, 2, u32::MAX] {
            assert!(matches!(
                Settings {
                    profile: version,
                    ..Settings::default()
                }
                .validate(&Control::none()),
                Err(Error::Unsupported(_))
            ));
            assert!(matches!(
                Settings {
                    format: version,
                    ..Settings::default()
                }
                .validate(&Control::none()),
                Err(Error::Unsupported(_))
            ));
        }
    }

    #[test]
    fn unknown_roles_duplicate_fields_and_invalid_shapes_are_refused_while_disabled() {
        for input in [
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[],"reviews":[]}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[],"decisions":[],"decisions":[]}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[],"normalization":{"version":2}}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[],"receipt":{"actor":"urn:forged"}}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"sourceRecords":[],"proposals":[]}"#,
            r#"{"format":1,"format":1,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[]}"#,
            r#"{"format":1.0,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[]}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":"urn:a","proposals":[]}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[null],"proposals":[]}"#,
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[]}"#,
        ] {
            assert!(
                Settings::from_slice(input.as_bytes(), &Control::none()).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn roles_validate_native_values_and_format_graph_bounds() {
        for graph in ["", "relative", "_:blank", "urn:has space"] {
            let mut settings = enabled();
            settings.source_records = vec![graph.into()];
            assert!(settings.validate(&Control::none()).is_err(), "{graph}");
        }
        let mut settings = enabled();
        settings.source_records.push("urn:a".into());
        assert!(settings.validate(&Control::none()).is_err());
        let mut settings = enabled();
        settings.proposals.clear();
        assert!(settings.validate(&Control::none()).is_err());
        settings.enabled = false;
        assert!(settings.validate(&Control::none()).is_ok());
        settings.source_records = vec![format!(
            "urn:{}",
            "a".repeat(MAX_SETTINGS_GRAPH_IRI_BYTES - 4)
        )];
        assert!(settings.validate(&Control::none()).is_ok());
        settings.source_records[0].push('a');
        assert!(settings.validate(&Control::none()).is_err());
        settings.source_records = (0..MAX_SETTINGS_GRAPHS)
            .map(|index| format!("urn:{index}"))
            .collect();
        settings.proposals = vec![settings.source_records[0].clone()];
        assert!(settings.validate(&Control::none()).is_ok());
        settings.proposals = vec!["urn:additional".into()];
        assert!(settings.validate(&Control::none()).is_err());
        settings.source_records.push("urn:too-many".into());
        assert!(settings.validate(&Control::none()).is_err());
        let bytes = serde_json::to_vec(&settings).unwrap();
        assert!(Settings::from_slice(&bytes, &Control::none()).is_err());
    }

    fn escaped(value: &str) -> String {
        value
            .encode_utf16()
            .map(|unit| format!("\\u{unit:04x}"))
            .collect()
    }

    #[test]
    fn preflight_preserves_limit_escaped_ascii_unicode_and_surrogate_iris_in_both_roles() {
        for graph in [
            format!("urn:{}", "a".repeat(MAX_SETTINGS_GRAPH_IRI_BYTES - 4)),
            format!("urn:{}", "é".repeat((MAX_SETTINGS_GRAPH_IRI_BYTES - 4) / 2)),
            format!(
                "urn:{}",
                "🚀".repeat((MAX_SETTINGS_GRAPH_IRI_BYTES - 4) / 4)
            ),
        ] {
            assert_eq!(graph.len(), MAX_SETTINGS_GRAPH_IRI_BYTES);
            let encoded = escaped(&graph);
            let json = format!(
                r#"{{"format":1,"profile":1,"enabled":true,"sourceRecords":["{encoded}"],"proposals":["{encoded}"]}}"#
            );
            let settings = Settings::from_slice(json.as_bytes(), &Control::none()).unwrap();
            assert_eq!(
                settings.source_records.as_slice(),
                std::slice::from_ref(&graph)
            );
            assert_eq!(settings.proposals, [graph]);
            settings.canonical(&Control::none()).unwrap();
        }
        for graph in [r"urn:\uD800", r"urn:\uDC00", r"urn:\uD800\u0061"] {
            let json = format!(
                r#"{{"format":1,"profile":1,"enabled":true,"sourceRecords":["{graph}"],"proposals":["urn:p"]}}"#
            );
            assert!(Settings::from_slice(json.as_bytes(), &Control::none()).is_err());
        }
    }

    #[test]
    fn string_preflight_does_not_decode_or_drop_escaped_field_and_version_semantics() {
        let valid = br#"{"\u0066ormat":1,"profile":1,"enabled":false,"source\u0052ecords":[],"proposals":[]}"#;
        assert_eq!(
            Settings::from_slice(valid, &Control::none()).unwrap(),
            Settings::default()
        );
        let duplicate = br#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"source\u0052ecords":[],"proposals":[]}"#;
        assert!(Settings::from_slice(duplicate, &Control::none()).is_err());
        let future = br#"{"\u0066ormat":2,"profile":1,"sourceRecords":{"future":"shape"}}"#;
        assert!(matches!(
            Settings::from_slice(future, &Control::none()),
            Err(Error::Unsupported(_))
        ));
        // Universal syntax/resource bounds precede version interpretation, as the
        // raw-input and nesting limits already do.
        let oversized = format!(
            r#"{{"format":1,"profile":1,"{}":0}}"#,
            "a".repeat(MAX_RAW_STRING_BYTES + 1)
        );
        assert!(matches!(
            Settings::from_slice(oversized.as_bytes(), &Control::none()),
            Err(Error::BudgetExceeded(_))
        ));
        let mut scanner = StringBounds::default();
        scanner.feed(br#""a\""#).unwrap();
        scanner.feed(br#"b\\c""#).unwrap();
        assert!(!scanner.quoted);
    }

    #[test]
    fn settings_bound_input_nesting_cancellation_and_deadline() {
        assert!(matches!(
            Settings::from_slice(&vec![b' '; MAX_SETTINGS_BYTES + 1], &Control::none()),
            Err(Error::BudgetExceeded(_))
        ));
        let nested = format!(
            "{}0{}",
            "[".repeat(nesting::MAX_ELEMENTS + 1),
            "]".repeat(nesting::MAX_ELEMENTS + 1)
        );
        assert!(Settings::from_slice(nested.as_bytes(), &Control::none()).is_err());
        let control = Control::none();
        control.cancel.cancel();
        assert!(matches!(
            Settings::default().canonical(&control),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            Settings::from_slice(b"{}", &control),
            Err(Error::Cancelled)
        ));
        let control = Control {
            deadline: Some(std::time::Instant::now()),
            ..Control::none()
        };
        assert!(matches!(
            Settings::default().canonical(&control),
            Err(Error::Timeout)
        ));
    }
}
