use super::{
    ClaimKind, Confidence, EvidenceRelation, GraphIdentity, MethodRef, Polarity, RecordKey,
    RecordVersion, Sha256Digest,
};
use oxrdf::{Literal, NamedNode, Term, Triple};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A batch-local alias or an existing record that still needs typed resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordRef {
    Local(RecordKey),
    Existing(NamedNode),
}

/// Exact retained bytes or explicit metadata-only availability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceContent {
    Retained {
        bytes: Vec<u8>,
        expected_digest: Option<Sha256Digest>,
    },
    MetadataOnly {
        digest: Sha256Digest,
    },
}

/// Declared lineage never claims that missing origins are independent sources.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceLineage {
    pub upstream_revisions: Vec<RecordRef>,
    pub families: Vec<NamedNode>,
}

/// Locators and reported rights are data, not permission to fetch or retain content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMetadata {
    pub locators: Vec<NamedNode>,
    pub reported_rights: Vec<Term>,
}

/// Source-reported times. There is no caller-writable first-availability field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceTimes {
    pub published_at: Option<Literal>,
    pub reported_available_at: Option<Literal>,
}

/// Native command data; authenticated creation fields are assigned separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRevisionInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub source: NamedNode,
    pub source_key: RecordKey,
    pub upstream_version: Option<RecordKey>,
    pub content: SourceContent,
    pub metadata: SourceMetadata,
    pub times: SourceTimes,
    pub lineage: SourceLineage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenditionContent {
    NormalizedText(String),
    RetainedBytes(Vec<u8>),
    MetadataOnly(Sha256Digest),
}

/// A translation is another rendition with its own key and conversion method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenditionInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub revision: RecordRef,
    pub occurrence_key: RecordKey,
    pub conversion: MethodRef,
    pub media_type: String,
    pub language: Option<String>,
    pub content: RenditionContent,
    pub expected_digest: Option<Sha256Digest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassageInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub revision: RecordRef,
    pub rendition: RecordRef,
    pub occurrence_key: RecordKey,
    /// Half-open Unicode code point offsets, not UTF-8 byte offsets.
    pub start: u64,
    pub end: u64,
    pub text_digest: Sha256Digest,
    pub quote: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlankAnchor {
    Reifier(NamedNode),
    ImportMap(NamedNode),
}

/// Exact coordinates remain unresolved until the retained-view accessor checks them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFactRef {
    pub dataset: Uuid,
    pub branch: Uuid,
    pub commit: u64,
    pub graph: GraphIdentity,
    pub expected: Triple,
    pub anchor: Option<BlankAnchor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceRefTarget {
    StoredFact(StoredFactRef),
    Passage {
        revision: RecordRef,
        rendition: RecordRef,
        passage: RecordRef,
    },
    Structured {
        revision: RecordRef,
        pointer: String,
        method: MethodRef,
        value_digest: Sha256Digest,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceRefInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub occurrence_key: RecordKey,
    pub target: EvidenceRefTarget,
}

/// Domain scope stays distinct from structural profile and creation metadata.
pub type Scope = BTreeMap<String, Vec<Term>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfidenceFacets {
    pub source: Option<Confidence>,
    pub extraction: Option<Confidence>,
    pub analyst: Option<Confidence>,
}

/// One source-specific occurrence; proposition equality does not merge occurrences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub occurrence_key: RecordKey,
    pub revision: RecordRef,
    pub proposition: Triple,
    pub kind: ClaimKind,
    pub polarity: Polarity,
    /// Attributed source author, never an authenticated operation principal.
    pub attributed_author: NamedNode,
    pub scope: Scope,
    pub confidence: ConfidenceFacets,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceLinkInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub occurrence_key: RecordKey,
    pub target: RecordRef,
    pub evidence: RecordRef,
    pub relation: EvidenceRelation,
    pub attributed_author: NamedNode,
}

/// Immutable evaluation data. A declaration of a method is not review or admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssessmentInput {
    pub version: RecordVersion,
    pub key: RecordKey,
    pub occurrence_key: RecordKey,
    pub target: RecordRef,
    pub conclusion: Term,
    pub method: MethodRef,
    pub configuration_digest: Sha256Digest,
    pub evaluated_at: Literal,
    pub attributed_evaluator: NamedNode,
    pub confidence: Option<Confidence>,
    pub scope: Scope,
}

/// A complete preparation input. It cannot carry an actor, receipt or owner token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvidenceInput {
    pub revisions: Vec<SourceRevisionInput>,
    pub renditions: Vec<RenditionInput>,
    pub passages: Vec<PassageInput>,
    pub references: Vec<EvidenceRefInput>,
    pub claims: Vec<ClaimInput>,
    pub links: Vec<EvidenceLinkInput>,
    pub assessments: Vec<AssessmentInput>,
}
