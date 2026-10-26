//! Evidence input models and deterministic preparation.
//!
//! These functions handle data. They do not register a profile, obtain a managed
//! writer, authenticate an author or manufacture an availability receipt. Runtime
//! publication must use the engine's separate admitted typed writer.

mod canonical;
mod model;
mod prepare;
mod primitives;
mod structured;

pub use canonical::{IdentityNamespace, canonical_term_digest};
pub use primitives::{
    ClaimKind, Confidence, EvidenceRelation, GraphIdentity, MethodRef, Polarity, RecordKey,
    RecordKind, RecordVersion, Sha256Digest,
};

pub use model::{
    AssessmentInput, BlankAnchor, ClaimInput, ConfidenceFacets, EvidenceInput, EvidenceLinkInput,
    EvidenceRefInput, EvidenceRefTarget, PassageInput, RecordRef, RenditionContent, RenditionInput,
    Scope, SourceContent, SourceLineage, SourceMetadata, SourceRevisionInput, SourceTimes,
    StoredFactRef,
};

pub use prepare::{
    MAX_CLAIMS, MAX_LINKS, NormalizedSourceTime, NormalizedSourceTimes, PreparedContent,
    PreparedInput, ResolutionRequirement, prepare_input,
};

pub use structured::{
    JSON_LEXICAL_UTF8_V1, JSON_LEXICAL_UTF8_VERSION, SelectedJsonValue, select_json_lexical_utf8,
};

#[cfg(test)]
mod tests;
