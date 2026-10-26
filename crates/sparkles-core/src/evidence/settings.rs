//! Pure C19 evidence settings data. This module does not admit a profile, choose
//! an authoritative configuration revision or grant managed graph ownership.

use super::Sha256Digest;
use crate::{Budget, BudgetKind, Error, Result, nesting, task::Control};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{collections::BTreeSet, fmt};

/// The current required-manifest graph bound. Other owners share the same capacity;
/// only the admission factory can determine the remaining dataset capacity.
pub const MAX_SETTINGS_GRAPHS: usize = 1024;
pub const MAX_SETTINGS_GRAPH_IRI_BYTES: usize = 4096;
pub const MAX_SETTINGS_BYTES: usize = 16 * 1024 * 1024;

// One decoded UTF-8 byte takes at most six raw JSON bytes (\u00xx).
// Surrogate pairs take twelve raw bytes for four decoded bytes. This therefore
// preserves every supported IRI while bounding Serde scratch before any visitor.
const MAX_RAW_STRING_BYTES: usize = 6 * MAX_SETTINGS_GRAPH_IRI_BYTES;

/// Complete effective settings, after C19's declared/runtime layering and locks.
/// Unknown fields are refused even while disabled. Main-effective committed
/// configuration and target-branch materialization remain separate runtime work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub format: u32,
    pub profile: u32,
    pub enabled: bool,
    #[serde(deserialize_with = "graphs")]
    pub source_records: Vec<String>,
    #[serde(deserialize_with = "graphs")]
    pub proposals: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: 1,
            profile: 1,
            enabled: false,
            source_records: Vec::new(),
            proposals: Vec::new(),
        }
    }
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

    /// Validate syntax and supported roles. Collision with another owner, branch
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
        for (role, graphs) in [
            ("sourceRecords", &self.source_records),
            ("proposals", &self.proposals),
        ] {
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
        control.check()
    }

    /// Version 1 uses sorted graph sets and lexically ordered JSON object keys.
    /// Every field is explicit, including disabled/empty settings. IRI lexical
    /// values are preserved; cross-role overlap does not duplicate ownership.
    pub fn canonical(&self, control: &Control) -> Result<CanonicalSettings> {
        self.validate(control)?;
        let mut source_records: Vec<_> = self.source_records.iter().map(String::as_str).collect();
        let mut proposals: Vec<_> = self.proposals.iter().map(String::as_str).collect();
        source_records.sort_unstable();
        proposals.sort_unstable();
        // Wire-key declaration order is lexical even if serde_json later enables
        // insertion-ordered maps. A generic sorted-key envelope gets identical bytes.
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            enabled: bool,
            format: u32,
            profile: u32,
            proposals: Vec<&'a str>,
            source_records: Vec<&'a str>,
        }
        control.check()?;
        let bytes = serde_json::to_vec(&Body {
            enabled: self.enabled,
            format: self.format,
            profile: self.profile,
            proposals,
            source_records,
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
        assert_eq!(canonical.bytes(), br#"{"enabled":true,"format":1,"profile":1,"proposals":["urn:p"],"sourceRecords":["urn:a","urn:z"]}"#);
        assert_eq!(
            canonical.digest().to_string(),
            "sha256:13833d93e559becb65995635c5e3af96f9249dd7c6ef0761321734100d90edfd"
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
            br#"{"enabled":false,"format":1,"profile":1,"proposals":[],"sourceRecords":[]}"#
        );
        assert_eq!(
            default.digest().to_string(),
            "sha256:282fb3dd74b4dfb0ba9f0396e43ae0106a8e2e54872e62204897228f07f9deaf"
        );
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
            r#"{"format":1,"profile":1,"enabled":false,"sourceRecords":[],"proposals":[],"decisions":[]}"#,
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
