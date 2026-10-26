use crate::{Error, Result, task::Control};
use oxrdf::{Literal, NamedNode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};

/// Version of the managed record encoding, independent of RDF resource revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub enum RecordVersion {
    V1,
}

impl TryFrom<u32> for RecordVersion {
    type Error = Error;
    fn try_from(version: u32) -> Result<Self> {
        match version {
            1 => Ok(Self::V1),
            _ => Err(Error::Unsupported(format!(
                "unsupported-profile: evidence record version {version}"
            ))),
        }
    }
}

impl From<RecordVersion> for u32 {
    fn from(_: RecordVersion) -> Self {
        1
    }
}

/// A full lowercase SHA-256 digest of one explicitly identified byte encoding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(pub(super) [u8; 32]);

impl Sha256Digest {
    pub fn of_bytes(bytes: &[u8], control: &Control) -> Result<Self> {
        control.check()?;
        let mut hash = Sha256::new();
        for chunk in bytes.chunks(64 * 1024) {
            control.check()?;
            hash.update(chunk);
        }
        control.check()?;
        Ok(Self(hash.finalize().into()))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl FromStr for Sha256Digest {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(Error::invalid(
                "digest requires sha256: and 64 lowercase hex digits",
            ));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::invalid(
                "digest requires sha256: and 64 lowercase hex digits",
            ));
        }
        let mut bytes = [0; 32];
        let digit = |b: u8| {
            if b.is_ascii_digit() {
                b - b'0'
            } else {
                b - b'a' + 10
            }
        };
        for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            bytes[index] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.to_string()
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("sha256:")?;
        for byte in self.0 {
            write!(out, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, out)
    }
}

/// An importer key, never a global identity derived only from proposition text.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RecordKey(String);

impl RecordKey {
    pub const MAX_BYTES: usize = 16 * 1024 * 1024;

    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > Self::MAX_BYTES {
            return Err(Error::invalid(
                "record key requires 1 through 16 MiB of UTF-8 input",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RecordKey {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<RecordKey> for String {
    fn from(value: RecordKey) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordKind {
    SourceRevision,
    Rendition,
    Passage,
    EvidenceRef,
    Claim,
    EvidenceLink,
    Assessment,
    Note,
    Decision,
    Task,
    ReviewDecision,
    IdentityProposal,
}

impl RecordKind {
    pub(super) fn tag(self) -> u8 {
        match self {
            Self::SourceRevision => 1,
            Self::Rendition => 2,
            Self::Passage => 3,
            Self::EvidenceRef => 4,
            Self::Claim => 5,
            Self::EvidenceLink => 6,
            Self::Assessment => 7,
            Self::Note => 8,
            Self::Decision => 9,
            Self::Task => 10,
            Self::ReviewDecision => 11,
            Self::IdentityProposal => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimKind {
    Reported,
    Observation,
    Hypothesis,
    Preference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Polarity {
    Positive,
    Negative,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceRelation {
    Supports,
    Contradicts,
    Mentions,
}

/// Graph kind remains distinct even when a named graph uses a vocabulary IRI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphIdentity {
    Default,
    Named(NamedNode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodRef {
    pub method: NamedNode,
    pub version: RecordKey,
}

/// Qualified source/evaluation data, not an authenticated creation principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confidence {
    pub scale: NamedNode,
    pub value: Literal,
    pub evaluator: NamedNode,
    pub method: Option<MethodRef>,
}

impl Confidence {
    pub fn validate(&self, extraction: bool) -> Result<()> {
        validate_iri(&self.scale)?;
        validate_iri(&self.evaluator)?;
        if let Some(method) = &self.method {
            validate_iri(&method.method)?;
        }
        if extraction && self.method.is_none() {
            return Err(Error::invalid(
                "extraction confidence requires its evaluation method",
            ));
        }
        if self.scale.as_str() == "urn:x-sparkles:evidence:ZeroToOne" && !zero_to_one(&self.value) {
            return Err(Error::invalid(
                "zero-to-one confidence requires an exact decimal in [0,1]",
            ));
        }
        Ok(())
    }
}

pub(super) fn validate_iri(value: &NamedNode) -> Result<()> {
    oxiri::Iri::parse(value.as_str()).map_err(|_| Error::invalid("expected an absolute IRI"))?;
    Ok(())
}

fn zero_to_one(value: &Literal) -> bool {
    if value.datatype() != oxrdf::vocab::xsd::DECIMAL {
        return false;
    }
    let value = value.value().trim_matches([' ', '\t', '\r', '\n']);
    let negative = value.starts_with('-');
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if integer.is_empty() && fraction.is_empty()
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return false;
    }
    if negative {
        return integer.bytes().chain(fraction.bytes()).all(|b| b == b'0');
    }
    match integer.trim_start_matches('0') {
        "" => true,
        "1" => fraction.bytes().all(|b| b == b'0'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_checks_the_named_encoding_and_known_sha256_vector() {
        let digest = Sha256Digest::of_bytes(b"abc", &Control::none()).unwrap();
        assert_eq!(
            digest.to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(digest.to_string().parse::<Sha256Digest>().unwrap(), digest);
        for bad in [
            "sha256:abc".to_string(),
            format!(
                "sha256:{}",
                digest
                    .to_string()
                    .strip_prefix("sha256:")
                    .unwrap()
                    .to_uppercase()
            ),
            digest.to_string().to_uppercase(),
            format!("{}0", digest),
        ] {
            assert!(bad.parse::<Sha256Digest>().is_err());
        }
        let control = Control::none();
        control.cancel.cancel();
        assert!(matches!(
            Sha256Digest::of_bytes(b"", &control),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn record_versions_refuse_unknown_instead_of_falling_back() {
        assert_eq!(serde_json::to_string(&RecordVersion::V1).unwrap(), "1");
        for version in [0, 2, u32::MAX] {
            assert!(RecordVersion::try_from(version).is_err());
            assert!(serde_json::from_str::<RecordVersion>(&version.to_string()).is_err());
        }
    }

    #[test]
    fn record_keys_bound_decoded_utf8_bytes_without_ambiguous_separator_rules() {
        assert!(RecordKey::new("").is_err());
        assert!(RecordKey::new("é".repeat(RecordKey::MAX_BYTES / 2 + 1)).is_err());
        assert!(RecordKey::new("é".repeat(RecordKey::MAX_BYTES / 2)).is_ok());
        assert_eq!(
            RecordKey::new("source\0occurrence").unwrap().as_str(),
            "source\0occurrence"
        );
    }

    #[test]
    fn confidence_matches_the_independent_decimal_oracle_cases() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../../testsuite/evidence/v1/confidence-cases.json"
        ))
        .unwrap();
        for case in cases {
            let confidence = Confidence {
                scale: NamedNode::new_unchecked("urn:x-sparkles:evidence:ZeroToOne"),
                value: Literal::new_typed_literal(
                    case["lexical"].as_str().unwrap(),
                    NamedNode::new(format!(
                        "http://www.w3.org/2001/XMLSchema#{}",
                        case["datatype"].as_str().unwrap()
                    ))
                    .unwrap(),
                ),
                evaluator: NamedNode::new_unchecked("urn:evaluator"),
                method: None,
            };
            assert_eq!(
                confidence.validate(false).is_ok(),
                case["conforms"].as_bool().unwrap(),
                "{case}"
            );
        }
        let other = Confidence {
            scale: NamedNode::new_unchecked("urn:ordinal"),
            value: Literal::new_simple_literal("high"),
            evaluator: NamedNode::new_unchecked("urn:evaluator"),
            method: None,
        };
        assert!(other.validate(false).is_ok());
        assert!(other.validate(true).is_err());
    }
}
