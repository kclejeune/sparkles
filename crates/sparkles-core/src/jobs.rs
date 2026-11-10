//! Durable jobs: the `jobs.json` settings file, the ledger vocabulary and the lease
//! token primitives (spec C26 §2, §4 and §7).
//!
//! The jobs owner ([`crate::managed::OwnerKind::Jobs`]) manages one control graph,
//! [`LEDGER_GRAPH`], in the dataset's managed-graph registry. Its settings file holds the
//! registered definitions:
//!
//! ```json
//! {"format": 1, "profile": 1, "enabled": true, "maxQueued": 1024,
//!  "definitions": {"collect": {"kind": "quads", "version": 1, "graphs": ["urn:g"],
//!                              "workers": ["urn:x-sparkles:principal:worker"]}}}
//! ```
//!
//! A definition's revision is the SHA-256 of its canonical JSON. A run records the
//! revision it was accepted under, and a claim refuses a run whose definition has
//! since changed. A dataset with jobs enabled needs dataset reader 9 ([`READER`]), which
//! enablement raises durably before `jobs.json` and `managed.json` are written, so a
//! release that does not know the ledger refuses to open the dataset.
//!
//! The ledger logic itself, submission, claims, leases and publication, lives in the
//! `sparkles` crate, which owns the dataset handle. This module holds what the format
//! registry, the managed-graph registry and the server share.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The settings file at the dataset root.
pub const FILE: &str = "jobs.json";

/// The `jobs.json` format this build reads and writes.
pub const FORMAT: u32 = 1;

/// The ledger profile this build enforces: the vocabulary and record shapes of the
/// control graph.
pub const PROFILE: u32 = 1;

/// The dataset reader capability that knows the job ledger (`minimumReader` of
/// `dataset.json`).
pub const READER: u32 = 9;

/// The `job:` namespace of the ledger vocabulary and record IRIs.
pub const NAMESPACE: &str = "urn:x-sparkles:jobs:";

/// The control graph that holds every ledger record of the dataset.
pub const LEDGER_GRAPH: &str = "urn:x-sparkles:jobs:ledger";

/// The largest `jobs.json` this build reads, in bytes.
pub const MAX_SETTINGS_BYTES: usize = 1 << 20;

/// The most definitions one dataset registers.
pub const MAX_DEFINITIONS: usize = 256;

/// The longest idempotency key, in UTF-8 bytes (spec C26 §3).
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;

/// The longest definition name, in bytes.
pub const MAX_NAME_BYTES: usize = 128;

/// The registered kind whose typed result is bounded insert and delete quads in the
/// definition's graphs, with an opaque source checkpoint. It is the generic kind of an
/// external collector.
pub const KIND_QUADS: &str = "quads";

/// The kinds this build registers, with the versions it accepts.
pub const KINDS: &[(&str, &[u32])] = &[(KIND_QUADS, &[1])];

/// Whether this build runs `kind` at `version`.
pub fn supports_kind(kind: &str, version: u32) -> bool {
    KINDS
        .iter()
        .any(|(k, versions)| *k == kind && versions.contains(&version))
}

/// The complete settings of the jobs owner, as `jobs.json` holds them. Unknown members
/// are refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub format: u32,
    pub profile: u32,
    pub enabled: bool,
    /// The most runs that may wait in `queued` or `retry-wait` at once. Admission past
    /// this bound is refused without storing a run.
    #[serde(default = "default_max_queued")]
    pub max_queued: u32,
    /// The registered definitions by name.
    #[serde(default)]
    pub definitions: BTreeMap<String, Definition>,
}

fn default_max_queued() -> u32 {
    1024
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            format: FORMAT,
            profile: PROFILE,
            enabled: false,
            max_queued: default_max_queued(),
            definitions: BTreeMap::new(),
        }
    }
}

/// One registered definition: a kind, its output scope, who may submit and claim it,
/// and its retry and lease policy. Every duration is in milliseconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Definition {
    pub kind: String,
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The named graphs a publication may change. A publication into any other graph,
    /// or into a managed graph, is refused.
    #[serde(default)]
    pub graphs: Vec<String>,
    /// The principals that may claim runs of this definition. An empty list admits every
    /// principal the server lets claim on the dataset.
    #[serde(default)]
    pub workers: Vec<String>,
    /// The principals that may submit runs. An empty list admits every principal the
    /// server lets submit on the dataset.
    #[serde(default)]
    pub submitters: Vec<String>,
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_retry_horizon_ms")]
    pub retry_horizon_ms: u64,
    #[serde(default = "default_lease_ms")]
    pub lease_ms: u64,
    #[serde(default = "default_max_lease_extension_ms")]
    pub max_lease_extension_ms: u64,
    #[serde(default = "default_max_input_bytes")]
    pub max_input_bytes: u64,
    #[serde(default = "default_max_output_quads")]
    pub max_output_quads: u64,
    /// The most runs of this definition leased at once.
    #[serde(default = "one")]
    pub concurrency: u32,
}

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
fn default_attempts() -> u32 {
    8
}
fn default_retry_horizon_ms() -> u64 {
    24 * 60 * 60 * 1000
}
fn default_lease_ms() -> u64 {
    60_000
}
fn default_max_lease_extension_ms() -> u64 {
    300_000
}
fn default_max_input_bytes() -> u64 {
    1 << 20
}
fn default_max_output_quads() -> u64 {
    100_000
}

impl Definition {
    /// A definition of the `quads` kind that publishes into `graphs`, with the default
    /// policy.
    pub fn quads(graphs: Vec<String>) -> Definition {
        Definition {
            kind: KIND_QUADS.into(),
            version: 1,
            enabled: true,
            graphs,
            workers: Vec::new(),
            submitters: Vec::new(),
            attempts: default_attempts(),
            retry_horizon_ms: default_retry_horizon_ms(),
            lease_ms: default_lease_ms(),
            max_lease_extension_ms: default_max_lease_extension_ms(),
            max_input_bytes: default_max_input_bytes(),
            max_output_quads: default_max_output_quads(),
            concurrency: 1,
        }
    }

    /// `sha256:` and the hex digest of the definition's canonical JSON. Two definitions
    /// with the same revision mean the same thing to an accepted run.
    pub fn revision(&self) -> String {
        let value = serde_json::to_value(self).expect("a definition serializes");
        digest_hex(&canonical_json(&value))
    }

    fn validate(&self, name: &str) -> Result<()> {
        let invalid = |why: String| Error::invalid(format!("{FILE}: definition {name:?}: {why}"));
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(invalid(format!(
                "the name is empty or longer than {MAX_NAME_BYTES} bytes"
            )));
        }
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        {
            return Err(invalid(
                "the name uses characters other than letters, digits, '-', '_' and '.'".into(),
            ));
        }
        if !supports_kind(&self.kind, self.version) {
            return Err(Error::Unsupported(format!(
                "{FILE}: definition {name:?}: kind {:?} version {} is not registered by Sparkles {}",
                self.kind,
                self.version,
                crate::formats::SPARKLES_VERSION
            )));
        }
        crate::managed::check_graphs(&self.graphs).map_err(|e| invalid(format!("graphs: {e}")))?;
        for p in self.workers.iter().chain(&self.submitters) {
            if oxrdf::NamedNode::new(p).is_err() {
                return Err(invalid(format!("{p:?} is not an IRI")));
            }
        }
        if self.attempts == 0 || self.attempts > 1000 {
            return Err(invalid("attempts is 0 or more than 1000".into()));
        }
        if self.lease_ms == 0 || self.lease_ms > self.max_lease_extension_ms {
            return Err(invalid(
                "leaseMs is 0 or larger than maxLeaseExtensionMs".into(),
            ));
        }
        if self.max_lease_extension_ms > 24 * 60 * 60 * 1000 {
            return Err(invalid("maxLeaseExtensionMs is more than one day".into()));
        }
        if self.concurrency == 0 {
            return Err(invalid("concurrency is 0".into()));
        }
        Ok(())
    }
}

impl Settings {
    /// Decode `bytes`, refusing a `format` or `profile` this build does not read before
    /// the rest is interpreted, and validating every definition.
    pub fn from_slice(bytes: &[u8]) -> Result<Settings> {
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(Error::invalid(format!(
                "{FILE} is larger than {MAX_SETTINGS_BYTES} bytes"
            )));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::Corrupt(format!("{FILE} is not JSON: {e}")))?;
        if !value.is_object() {
            return Err(Error::Corrupt(format!("{FILE} is not a JSON object")));
        }
        crate::formats::check_settings(FILE, &value)?;
        match value.get("profile").and_then(serde_json::Value::as_u64) {
            None | Some(1) => {}
            Some(p) => {
                return Err(Error::Unsupported(format!(
                    "{FILE} has profile {p}; Sparkles {} enforces profile {PROFILE}",
                    crate::formats::SPARKLES_VERSION
                )));
            }
        }
        let settings: Settings =
            serde_json::from_value(value).map_err(|e| Error::invalid(format!("{FILE}: {e}")))?;
        settings.validate()?;
        Ok(settings)
    }

    /// Check the settings' bounds and every definition.
    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT || self.profile != PROFILE {
            return Err(Error::Unsupported(format!(
                "{FILE} has format {} and profile {}; Sparkles {} writes format {FORMAT} and profile {PROFILE}",
                self.format,
                self.profile,
                crate::formats::SPARKLES_VERSION
            )));
        }
        if self.definitions.len() > MAX_DEFINITIONS {
            return Err(Error::invalid(format!(
                "{FILE} registers more than {MAX_DEFINITIONS} definitions"
            )));
        }
        if self.max_queued == 0 {
            return Err(Error::invalid(format!("{FILE}: maxQueued is 0")));
        }
        for (name, d) in &self.definitions {
            d.validate(name)?;
        }
        Ok(())
    }

    /// The canonical bytes of the settings: pretty JSON with every member present and
    /// sorted. The digest `managed.json` records is over these bytes.
    pub fn canonical(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).expect("settings serialize");
        let mut out = serde_json::to_vec_pretty(&sorted(value)).expect("settings serialize");
        out.push(b'\n');
        out
    }

    /// The graphs the owner manages: the ledger graph while enabled.
    pub fn managed_graphs(&self) -> Vec<String> {
        if self.enabled {
            vec![LEDGER_GRAPH.to_string()]
        } else {
            Vec::new()
        }
    }
}

/// Why this build cannot read the settings `bytes` at `profile`, if it cannot, for the
/// managed-graph registry.
pub(crate) fn check_settings(bytes: &[u8], profile: u32) -> Option<String> {
    if profile != PROFILE {
        return Some(format!(
            "{FILE}: the managed profile {profile} is not the profile {PROFILE} this build enforces"
        ));
    }
    Settings::from_slice(bytes).err().map(|e| e.to_string())
}

/// `value` with every object's members sorted by key, so that member order and
/// whitespace do not change a digest.
fn sorted(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(m) => {
            let sorted_map: BTreeMap<String, serde_json::Value> =
                m.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            serde_json::Value::Object(sorted_map.into_iter().collect())
        }
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.into_iter().map(sorted).collect())
        }
        v => v,
    }
}

/// The canonical JSON of `value`: compact, with every object's members sorted by key.
/// Numbers keep serde's rendering, so `1.0` and `1` differ.
pub fn canonical_json(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&sorted(value.clone())).expect("JSON serializes")
}

/// `sha256:` and the lowercase hex SHA-256 of `bytes`.
pub fn digest_hex(bytes: &[u8]) -> String {
    crate::managed::settings_digest(bytes)
}

/// A new random bearer lease token and its durable verifier. The token is 32 random
/// bytes in URL-safe base64 without padding, and the verifier is `sha256:` and the hex
/// digest of the token's bytes. Only the verifier is ever written.
pub fn new_token() -> (String, String) {
    use base64::Engine as _;
    let bytes: [u8; 32] = rand::random();
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let verifier = digest_hex(token.as_bytes());
    (token, verifier)
}

/// The verifier of a presented token, compared in constant time by
/// [`token_matches`].
pub fn token_verifier(token: &str) -> String {
    digest_hex(token.as_bytes())
}

/// Whether `token` verifies against the recorded `verifier`, without an early exit on
/// the first differing byte.
pub fn token_matches(token: &str, verifier: &str) -> bool {
    let a = token_verifier(token);
    let a = a.as_bytes();
    let b = verifier.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The retry delay before attempt number `attempt` runs again: one second doubled per
/// earlier attempt, capped at `cap_ms`, with a random jitter of up to a quarter either
/// way (spec C26 §4.4).
pub fn backoff_ms(attempt: u32, cap_ms: u64) -> u64 {
    let base = 1000u64
        .checked_shl(attempt.saturating_sub(1).min(40))
        .unwrap_or(u64::MAX)
        .min(cap_ms.max(1));
    let quarter = base / 4;
    let jitter: u64 = rand::random::<u64>() % (2 * quarter + 1);
    (base - quarter + jitter).max(1)
}

/// The UUID of a job operation derived from its scope: a UUID version 5 over a
/// versioned, length-framed tuple of the dataset, the branch, the principal, the
/// definition revision and the idempotency key (spec C26 §3).
pub fn operation_id(
    dataset: uuid::Uuid,
    branch: uuid::Uuid,
    principal: &str,
    revision: &str,
    key: &str,
) -> uuid::Uuid {
    use sha1::Digest as _;
    let mut hash = sha1::Sha1::new();
    hash.update(dataset.as_bytes());
    hash.update(branch.as_bytes());
    hash.update(b"sparkles-jobs-operation-v1\0");
    for field in [principal, revision, key] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let hash = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Builder::from_sha1_bytes(bytes).into_uuid()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> Settings {
        let mut s = Settings {
            enabled: true,
            ..Settings::default()
        };
        s.definitions
            .insert("collect".into(), Definition::quads(vec!["urn:g".into()]));
        s
    }

    #[test]
    fn canonical_bytes_round_trip_and_ignore_member_order() {
        let s = enabled();
        let bytes = s.canonical();
        let again = Settings::from_slice(&bytes).unwrap();
        assert_eq!(again, s);
        assert_eq!(again.canonical(), bytes);
        let reordered = br#"{"profile":1,"enabled":true,"format":1,
            "definitions":{"collect":{"graphs":["urn:g"],"kind":"quads"}}}"#;
        let parsed = Settings::from_slice(reordered).unwrap();
        assert_eq!(parsed, s);
        assert_eq!(
            parsed.definitions["collect"].revision(),
            s.definitions["collect"].revision()
        );
    }

    #[test]
    fn newer_versions_unknown_kinds_and_bad_bounds_are_refused() {
        let newer = br#"{"format":2,"profile":1,"enabled":false}"#;
        assert!(matches!(
            Settings::from_slice(newer),
            Err(Error::Unsupported(_))
        ));
        let profile = br#"{"format":1,"profile":2,"enabled":false}"#;
        assert!(matches!(
            Settings::from_slice(profile),
            Err(Error::Unsupported(_))
        ));
        let kind = br#"{"format":1,"profile":1,"enabled":true,
            "definitions":{"x":{"kind":"shell","graphs":[]}}}"#;
        assert!(matches!(
            Settings::from_slice(kind),
            Err(Error::Unsupported(_))
        ));
        let unknown = br#"{"format":1,"profile":1,"enabled":true,"extra":1}"#;
        assert!(matches!(
            Settings::from_slice(unknown),
            Err(Error::Invalid(_))
        ));
        let mut s = enabled();
        s.definitions.get_mut("collect").unwrap().lease_ms = 0;
        assert!(s.validate().is_err());
        let mut s = enabled();
        s.definitions.get_mut("collect").unwrap().concurrency = 0;
        assert!(s.validate().is_err());
    }

    #[test]
    fn revisions_follow_content_and_tokens_verify() {
        let a = Definition::quads(vec!["urn:g".into()]);
        let mut b = a.clone();
        assert_eq!(a.revision(), b.revision());
        b.attempts += 1;
        assert_ne!(a.revision(), b.revision());
        let (token, verifier) = new_token();
        assert!(token_matches(&token, &verifier));
        assert!(!token_matches("other", &verifier));
        assert!(!token.contains('='));
        let d = uuid::Uuid::new_v4();
        let b1 = uuid::Uuid::new_v4();
        let id = operation_id(d, b1, "urn:p", "sha256:x", "k");
        assert_eq!(id, operation_id(d, b1, "urn:p", "sha256:x", "k"));
        assert_ne!(id, operation_id(d, b1, "urn:p", "sha256:x", "k2"));
        assert_ne!(
            id,
            operation_id(d, uuid::Uuid::new_v4(), "urn:p", "sha256:x", "k")
        );
    }

    #[test]
    fn canonical_json_sorts_members() {
        let v: serde_json::Value = serde_json::from_str(r#"{"b":[{"y":1,"x":2}],"a":1}"#).unwrap();
        assert_eq!(canonical_json(&v), br#"{"a":1,"b":[{"x":2,"y":1}]}"#);
    }
}
