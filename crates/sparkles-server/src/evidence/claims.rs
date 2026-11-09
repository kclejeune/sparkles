//! `POST /{ds}/claims` (spec C25 §5 and §7): authenticated claim proposals.
//!
//! The body is one batch of source revisions, renditions, passages, evidence
//! references, claim occurrences, evidence links and assessments, with `dryRun`,
//! `ifHead`, `idempotencyKey`, the chosen graphs and a commit message. The batch
//! publishes as one commit of the evidence owner on the request's branch, through
//! [`sparkles::evidence::propose`].
//!
//! Authorization: `write` on `{ds}` (or the branch) through the update endpoint (the
//! route table in `auth/routes.rs`), update grants on every graph the batch writes,
//! and read access to every record and stored fact it references, decided as `POST
//! /{ds}/claims/resolve` decides it. A reference the caller cannot read answers as a
//! missing one. The caller's credential and the access policy are checked again right
//! before the commit publishes, under the authority registry's fence, so a revocation
//! is ordered before that check or after the commit.

use super::{StoredFactJson, bad, kind_name, named, parse_triple, policy, stored_fact_ref};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, err};
use crate::state::AppState;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use oxrdf::{Literal, NamedNode, Term};
use serde::Deserialize;
use serde_json::{Value as J, json};
use sparkles::evidence::{
    AssessmentInput, ClaimInput, ClaimKind, Confidence, ConfidenceFacets, EvidenceInput,
    EvidenceLinkInput, EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, IdentityInput,
    IdentityRelation, MethodRef, PassageInput, Polarity, ProposeOptions, RecordKey, RecordRef,
    RecordVersion, RenditionContent, RenditionInput, Scope, Sha256Digest, SourceContent,
    SourceLineage, SourceMetadata, SourceRevisionInput, SourceTimes,
};
use std::collections::BTreeMap;
use std::sync::Arc;

type St = State<Arc<AppState>>;

/// The most bytes a proposal body holds: the 16 MiB of decoded input of spec C25 §8 and
/// the JSON and base64 framing around it.
const MAX_CLAIMS_BYTES: usize = 24 << 20;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimsRequest {
    #[serde(default)]
    dry_run: bool,
    if_head: Option<u64>,
    idempotency_key: Option<String>,
    source_graph: Option<String>,
    proposal_graph: Option<String>,
    identity_graph: Option<String>,
    message: Option<String>,
    #[serde(default)]
    revisions: Vec<RevisionJson>,
    #[serde(default)]
    renditions: Vec<RenditionJson>,
    #[serde(default)]
    passages: Vec<PassageJson>,
    #[serde(default)]
    references: Vec<ReferenceJson>,
    #[serde(default)]
    claims: Vec<ClaimJson>,
    #[serde(default)]
    links: Vec<LinkJson>,
    #[serde(default)]
    assessments: Vec<AssessmentJson>,
    #[serde(default)]
    identities: Vec<IdentityJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityJson {
    key: String,
    occurrence_key: String,
    entities: Vec<String>,
    /// `possibleMatch` or `distinctEntities`
    relation: String,
    preferred_entity: Option<String>,
    author: String,
    method: Option<MethodJson>,
}

/// A record of the batch (`{"key"}`) or an existing record (`{"iri"}`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RefJson {
    key: Option<String>,
    iri: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MethodJson {
    method: String,
    version: String,
}

/// Exactly one of the retained `text` (UTF-8), the retained `base64` bytes, or the
/// `digest` of content that is not retained.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContentJson {
    text: Option<String>,
    base64: Option<String>,
    digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionJson {
    key: String,
    source: String,
    source_key: String,
    upstream_version: Option<String>,
    content: ContentJson,
    expected_digest: Option<String>,
    #[serde(default)]
    locators: Vec<String>,
    /// N-Triples terms
    #[serde(default)]
    reported_rights: Vec<String>,
    published_at: Option<String>,
    reported_available_at: Option<String>,
    #[serde(default)]
    upstream_revisions: Vec<RefJson>,
    #[serde(default)]
    families: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenditionJson {
    key: String,
    revision: RefJson,
    occurrence_key: String,
    conversion: MethodJson,
    media_type: String,
    language: Option<String>,
    content: ContentJson,
    expected_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PassageJson {
    key: String,
    revision: RefJson,
    rendition: RefJson,
    occurrence_key: String,
    start: u64,
    end: u64,
    text_digest: String,
    quote: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceJson {
    key: String,
    occurrence_key: String,
    /// the stored-fact form of `POST /{ds}/claims/resolve`
    stored_fact: Option<StoredFactJson>,
    passage: Option<PassageRefJson>,
    structured: Option<StructuredJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PassageRefJson {
    revision: RefJson,
    rendition: RefJson,
    passage: RefJson,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StructuredJson {
    revision: RefJson,
    pointer: String,
    method: MethodJson,
    value_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfidenceJson {
    scale: String,
    /// an N-Triples literal, or a decimal such as `0.8`
    value: String,
    evaluator: String,
    method: Option<MethodJson>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FacetsJson {
    source: Option<ConfidenceJson>,
    extraction: Option<ConfidenceJson>,
    analyst: Option<ConfidenceJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimJson {
    key: String,
    occurrence_key: String,
    revision: RefJson,
    /// one N-Triples 1.2 triple without its final `.`
    proposition: String,
    kind: String,
    polarity: String,
    author: String,
    /// predicate IRI to N-Triples terms
    #[serde(default)]
    scope: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    confidence: FacetsJson,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkJson {
    key: String,
    occurrence_key: String,
    target: RefJson,
    evidence: RefJson,
    relation: String,
    author: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssessmentJson {
    key: String,
    occurrence_key: String,
    target: RefJson,
    /// an N-Triples term
    conclusion: String,
    method: MethodJson,
    configuration_digest: String,
    evaluated_at: String,
    evaluator: String,
    confidence: Option<ConfidenceJson>,
    #[serde(default)]
    scope: BTreeMap<String, Vec<String>>,
    /// the records the evaluation consumed
    #[serde(default)]
    inputs: Vec<RefJson>,
}

fn record_key(field: &str, value: &str) -> ApiResult<RecordKey> {
    if value.len() > 256 {
        return Err(bad(field.to_string(), "a key has 1 to 256 bytes"));
    }
    RecordKey::new(value).map_err(|e| bad(field.to_string(), e))
}

pub(super) fn digest(field: &str, value: &str) -> ApiResult<Sha256Digest> {
    value.parse().map_err(|e| bad(field.to_string(), e))
}

fn reference(field: &str, r: &RefJson) -> ApiResult<RecordRef> {
    match (&r.key, &r.iri) {
        (Some(k), None) => Ok(RecordRef::Local(record_key(&format!("{field}.key"), k)?)),
        (None, Some(i)) => Ok(RecordRef::Existing(named(&format!("{field}.iri"), i)?)),
        _ => Err(bad(
            field.to_string(),
            "a record reference has exactly one of key and iri",
        )),
    }
}

fn method(field: &str, m: &MethodJson) -> ApiResult<MethodRef> {
    Ok(MethodRef {
        method: named(&format!("{field}.method"), &m.method)?,
        version: record_key(&format!("{field}.version"), &m.version)?,
    })
}

/// One N-Triples term.
fn term(field: &str, text: &str) -> ApiResult<Term> {
    Ok(parse_triple(
        field,
        &format!("<urn:x-sparkles:s> <urn:x-sparkles:p> {text}"),
    )?
    .object)
}

fn date_time(field: &str, value: &str) -> Literal {
    let _ = field;
    Literal::new_typed_literal(value, NamedNode::new_unchecked(format!("{XSD}dateTime")))
}

/// Exactly one of a revision's retained bytes, retained text or metadata-only digest.
type Content = (Option<Vec<u8>>, Option<String>, Option<Sha256Digest>);

fn content_of(field: &str, c: &ContentJson) -> ApiResult<Content> {
    use base64::Engine as _;
    match (&c.text, &c.base64, &c.digest) {
        (Some(t), None, None) => Ok((None, Some(t.clone()), None)),
        (None, Some(b), None) => Ok((
            Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b)
                    .map_err(|e| bad(format!("{field}.base64"), e))?,
            ),
            None,
            None,
        )),
        (None, None, Some(d)) => Ok((None, None, Some(digest(&format!("{field}.digest"), d)?))),
        _ => Err(bad(
            field.to_string(),
            "content has exactly one of text, base64 and digest",
        )),
    }
}

fn scope(field: &str, s: &BTreeMap<String, Vec<String>>) -> ApiResult<Scope> {
    let mut out = Scope::new();
    for (p, values) in s {
        named(&format!("{field}.scope"), p)?;
        let mut terms = Vec::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            terms.push(term(&format!("{field}.scope[{p}][{i}]"), v)?);
        }
        out.insert(p.clone(), terms);
    }
    Ok(out)
}

fn confidence(field: &str, c: &ConfidenceJson) -> ApiResult<Confidence> {
    let value = if c.value.starts_with('"') {
        match term(&format!("{field}.value"), &c.value)? {
            Term::Literal(l) => l,
            _ => {
                return Err(bad(
                    format!("{field}.value"),
                    "a confidence value is a literal",
                ));
            }
        }
    } else {
        Literal::new_typed_literal(
            c.value.as_str(),
            NamedNode::new_unchecked(format!("{XSD}decimal")),
        )
    };
    Ok(Confidence {
        scale: named(&format!("{field}.scale"), &c.scale)?,
        value,
        evaluator: named(&format!("{field}.evaluator"), &c.evaluator)?,
        method: c
            .method
            .as_ref()
            .map(|m| method(&format!("{field}.method"), m))
            .transpose()?,
    })
}

fn kind(field: &str, value: &str) -> ApiResult<ClaimKind> {
    Ok(match value {
        "reported" => ClaimKind::Reported,
        "observation" => ClaimKind::Observation,
        "hypothesis" => ClaimKind::Hypothesis,
        "preference" => ClaimKind::Preference,
        _ => {
            return Err(bad(
                field.to_string(),
                "kind is reported, observation, hypothesis or preference",
            ));
        }
    })
}

fn polarity(field: &str, value: &str) -> ApiResult<Polarity> {
    Ok(match value {
        "positive" => Polarity::Positive,
        "negative" => Polarity::Negative,
        "uncertain" => Polarity::Uncertain,
        _ => {
            return Err(bad(
                field.to_string(),
                "polarity is positive, negative or uncertain",
            ));
        }
    })
}

fn relation(field: &str, value: &str) -> ApiResult<EvidenceRelation> {
    Ok(match value {
        "supports" => EvidenceRelation::Supports,
        "contradicts" => EvidenceRelation::Contradicts,
        "mentions" => EvidenceRelation::Mentions,
        _ => {
            return Err(bad(
                field.to_string(),
                "relation is supports, contradicts or mentions",
            ));
        }
    })
}

fn input_of(r: ClaimsRequest) -> ApiResult<(EvidenceInput, Request)> {
    let mut input = EvidenceInput::default();
    for (i, v) in r.revisions.iter().enumerate() {
        let at = format!("revisions[{i}]");
        let (bytes, text, metadata) = content_of(&format!("{at}.content"), &v.content)?;
        let expected = v
            .expected_digest
            .as_deref()
            .map(|d| digest(&format!("{at}.expectedDigest"), d))
            .transpose()?;
        let content = match (bytes, text, metadata) {
            (Some(b), None, None) => SourceContent::Retained {
                bytes: b,
                expected_digest: expected,
            },
            (None, Some(t), None) => SourceContent::Retained {
                bytes: t.into_bytes(),
                expected_digest: expected,
            },
            (None, None, Some(d)) => {
                if expected.is_some_and(|e| e != d) {
                    return Err(bad(
                        format!("{at}.expectedDigest"),
                        "differs from content.digest",
                    ));
                }
                SourceContent::MetadataOnly { digest: d }
            }
            _ => unreachable!("one content form"),
        };
        let mut rights = Vec::new();
        for (j, t) in v.reported_rights.iter().enumerate() {
            rights.push(term(&format!("{at}.reportedRights[{j}]"), t)?);
        }
        let mut locators = Vec::new();
        for (j, l) in v.locators.iter().enumerate() {
            locators.push(named(&format!("{at}.locators[{j}]"), l)?);
        }
        let mut upstream = Vec::new();
        for (j, u) in v.upstream_revisions.iter().enumerate() {
            upstream.push(reference(&format!("{at}.upstreamRevisions[{j}]"), u)?);
        }
        let mut families = Vec::new();
        for (j, f) in v.families.iter().enumerate() {
            families.push(named(&format!("{at}.families[{j}]"), f)?);
        }
        input.revisions.push(SourceRevisionInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            source: named(&format!("{at}.source"), &v.source)?,
            source_key: record_key(&format!("{at}.sourceKey"), &v.source_key)?,
            upstream_version: v
                .upstream_version
                .as_deref()
                .map(|u| record_key(&format!("{at}.upstreamVersion"), u))
                .transpose()?,
            content,
            metadata: SourceMetadata {
                locators,
                reported_rights: rights,
            },
            times: SourceTimes {
                published_at: v.published_at.as_deref().map(|t| date_time(&at, t)),
                reported_available_at: v
                    .reported_available_at
                    .as_deref()
                    .map(|t| date_time(&at, t)),
            },
            lineage: SourceLineage {
                upstream_revisions: upstream,
                families,
            },
        });
    }
    for (i, v) in r.renditions.iter().enumerate() {
        let at = format!("renditions[{i}]");
        let (bytes, text, metadata) = content_of(&format!("{at}.content"), &v.content)?;
        let content = match (bytes, text, metadata) {
            (Some(b), None, None) => RenditionContent::RetainedBytes(b),
            (None, Some(t), None) => RenditionContent::NormalizedText(t),
            (None, None, Some(d)) => RenditionContent::MetadataOnly(d),
            _ => unreachable!("one content form"),
        };
        input.renditions.push(RenditionInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            revision: reference(&format!("{at}.revision"), &v.revision)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            conversion: method(&format!("{at}.conversion"), &v.conversion)?,
            media_type: v.media_type.clone(),
            language: v.language.clone(),
            content,
            expected_digest: v
                .expected_digest
                .as_deref()
                .map(|d| digest(&format!("{at}.expectedDigest"), d))
                .transpose()?,
        });
    }
    for (i, v) in r.passages.iter().enumerate() {
        let at = format!("passages[{i}]");
        input.passages.push(PassageInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            revision: reference(&format!("{at}.revision"), &v.revision)?,
            rendition: reference(&format!("{at}.rendition"), &v.rendition)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            start: v.start,
            end: v.end,
            text_digest: digest(&format!("{at}.textDigest"), &v.text_digest)?,
            quote: v.quote.clone(),
        });
    }
    for (i, v) in r.references.into_iter().enumerate() {
        let at = format!("references[{i}]");
        let target = match (v.stored_fact, v.passage, v.structured) {
            (Some(f), None, None) => {
                EvidenceRefTarget::StoredFact(stored_fact_ref(&format!("{at}.storedFact"), f)?)
            }
            (None, Some(p), None) => EvidenceRefTarget::Passage {
                revision: reference(&format!("{at}.passage.revision"), &p.revision)?,
                rendition: reference(&format!("{at}.passage.rendition"), &p.rendition)?,
                passage: reference(&format!("{at}.passage.passage"), &p.passage)?,
            },
            (None, None, Some(s)) => EvidenceRefTarget::Structured {
                revision: reference(&format!("{at}.structured.revision"), &s.revision)?,
                pointer: s.pointer.clone(),
                method: method(&format!("{at}.structured.method"), &s.method)?,
                value_digest: digest(&format!("{at}.structured.valueDigest"), &s.value_digest)?,
            },
            _ => {
                return Err(bad(
                    at,
                    "a reference has exactly one of storedFact, passage and structured",
                ));
            }
        };
        input.references.push(EvidenceRefInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            target,
        });
    }
    for (i, v) in r.claims.iter().enumerate() {
        let at = format!("claims[{i}]");
        let facet = |name: &str, c: &Option<ConfidenceJson>| {
            c.as_ref()
                .map(|c| confidence(&format!("{at}.confidence.{name}"), c))
                .transpose()
        };
        input.claims.push(ClaimInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            revision: reference(&format!("{at}.revision"), &v.revision)?,
            proposition: parse_triple(&format!("{at}.proposition"), &v.proposition)?,
            kind: kind(&format!("{at}.kind"), &v.kind)?,
            polarity: polarity(&format!("{at}.polarity"), &v.polarity)?,
            attributed_author: named(&format!("{at}.author"), &v.author)?,
            scope: scope(&at, &v.scope)?,
            confidence: ConfidenceFacets {
                source: facet("source", &v.confidence.source)?,
                extraction: facet("extraction", &v.confidence.extraction)?,
                analyst: facet("analyst", &v.confidence.analyst)?,
            },
        });
    }
    for (i, v) in r.links.iter().enumerate() {
        let at = format!("links[{i}]");
        input.links.push(EvidenceLinkInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            target: reference(&format!("{at}.target"), &v.target)?,
            evidence: reference(&format!("{at}.evidence"), &v.evidence)?,
            relation: relation(&format!("{at}.relation"), &v.relation)?,
            attributed_author: named(&format!("{at}.author"), &v.author)?,
        });
    }
    for (i, v) in r.assessments.iter().enumerate() {
        let at = format!("assessments[{i}]");
        input.assessments.push(AssessmentInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            target: reference(&format!("{at}.target"), &v.target)?,
            conclusion: term(&format!("{at}.conclusion"), &v.conclusion)?,
            method: method(&format!("{at}.method"), &v.method)?,
            configuration_digest: digest(
                &format!("{at}.configurationDigest"),
                &v.configuration_digest,
            )?,
            evaluated_at: date_time(&at, &v.evaluated_at),
            attributed_evaluator: named(&format!("{at}.evaluator"), &v.evaluator)?,
            confidence: v
                .confidence
                .as_ref()
                .map(|c| confidence(&format!("{at}.confidence"), c))
                .transpose()?,
            scope: scope(&at, &v.scope)?,
            inputs: {
                if v.inputs.len() > sparkles::evidence::MAX_ASSESSMENT_INPUTS {
                    return Err(bad(
                        format!("{at}.inputs"),
                        "an assessment names at most 64 inputs",
                    ));
                }
                v.inputs
                    .iter()
                    .enumerate()
                    .map(|(j, x)| reference(&format!("{at}.inputs[{j}]"), x))
                    .collect::<ApiResult<Vec<_>>>()?
            },
        });
    }
    for (i, v) in r.identities.iter().enumerate() {
        let at = format!("identities[{i}]");
        if v.entities.len() > sparkles::evidence::MAX_IDENTITY_ENTITIES {
            return Err(bad(
                format!("{at}.entities"),
                "a proposal names 2 to 64 entities",
            ));
        }
        input.identities.push(IdentityInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            entities: v
                .entities
                .iter()
                .enumerate()
                .map(|(j, e)| named(&format!("{at}.entities[{j}]"), e))
                .collect::<ApiResult<_>>()?,
            relation: IdentityRelation::from_name(&v.relation).ok_or_else(|| {
                bad(
                    format!("{at}.relation"),
                    "use possibleMatch or distinctEntities",
                )
            })?,
            preferred: v
                .preferred_entity
                .as_deref()
                .map(|e| named(&format!("{at}.preferredEntity"), e))
                .transpose()?,
            attributed_author: named(&format!("{at}.author"), &v.author)?,
            method: v
                .method
                .as_ref()
                .map(|m| method(&format!("{at}.method"), m))
                .transpose()?,
        });
    }
    if let Some(k) = &r.idempotency_key
        && (k.is_empty() || k.len() > 256)
    {
        return Err(bad("idempotencyKey".into(), "a key has 1 to 256 bytes"));
    }
    let request = Request {
        dry_run: r.dry_run,
        if_head: r.if_head,
        idempotency_key: r.idempotency_key,
        source_graph: r
            .source_graph
            .as_deref()
            .map(|g| named("sourceGraph", g))
            .transpose()?,
        proposal_graph: r
            .proposal_graph
            .as_deref()
            .map(|g| named("proposalGraph", g))
            .transpose()?,
        identity_graph: r
            .identity_graph
            .as_deref()
            .map(|g| named("identityGraph", g))
            .transpose()?,
        message: r.message,
    };
    Ok((input, request))
}

struct Request {
    dry_run: bool,
    if_head: Option<u64>,
    idempotency_key: Option<String>,
    source_graph: Option<NamedNode>,
    proposal_graph: Option<NamedNode>,
    identity_graph: Option<NamedNode>,
    message: Option<String>,
}

/// The authenticated principal's IRI: `urn:x-sparkles:principal:` and its log name,
/// percent-encoded, as `assert_facts` records it.
pub(crate) fn principal_iri(p: &Principal) -> NamedNode {
    use std::fmt::Write as _;
    let name: String = match crate::http::author(p) {
        Some(a) => a.to_string(),
        None if p.is_anonymous() => "anonymous".into(),
        None => "local".into(),
    };
    let mut out = String::from("urn:x-sparkles:principal:");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~:@!$&'()*+,;=".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    NamedNode::new_unchecked(out)
}

/// The caller's authority, checked under the authority registry's shared fence right
/// before the commit publishes and held until it is published: the access generation
/// the request was authenticated under is unchanged, and its session or token has not
/// expired or been revoked.
pub(super) struct CallerFence {
    pub(super) st: Arc<AppState>,
    pub(super) p: Principal,
}

impl sparkles::evidence::AuthorityFence for CallerFence {
    fn enter(&self) -> sparkles::Result<Box<dyn std::any::Any>> {
        #[cfg(feature = "auth")]
        if let (Some(auth), Some(epoch)) = (&self.st.auth, self.p.access_epoch()) {
            let hold = auth.authority_registry().begin_hold();
            if hold.generation() != epoch
                || auth.credential_expired(&self.p)
                || !auth.credential_live(&self.p)
            {
                return Err(sparkles::managed::ManagedError::error(
                    sparkles::managed::ManagedErrorKind::AuthorityChanged,
                    "authorization changed before the proposal published; nothing was written",
                ));
            }
            return Ok(Box::new(hold));
        }
        let _ = (&self.st, &self.p);
        Ok(Box::new(()))
    }
}

pub(crate) fn record_json(r: &sparkles::evidence::ProposedRecord) -> J {
    json!({
        "key": r.key,
        "kind": kind_name(r.kind),
        "iri": r.iri.as_str(),
        "graph": r.graph.as_str(),
        "recordDigest": r.digest.to_string(),
        "existing": r.existing,
    })
}

/// The caller's authority fence, for a proposal made outside `POST /{ds}/claims`.
pub(crate) fn caller_fence(
    st: Arc<AppState>,
    p: Principal,
) -> Arc<dyn sparkles::evidence::AuthorityFence> {
    Arc::new(CallerFence { st, p })
}

/// Propose `input` on `logical`, a branch of `main` or `main` itself, as `p`, reading
/// references with `p`'s query view as `POST /{ds}/claims` does. A claims ingestion
/// publishes its batch and its source registration this way.
pub(crate) fn propose_ingested(
    st: &Arc<AppState>,
    (main, logical): (&Arc<crate::state::Dataset>, &Arc<crate::state::Dataset>),
    p: &Principal,
    input: &sparkles::evidence::EvidenceInput,
    options: &ProposeOptions,
    control: &sparkles::task::Control,
) -> ApiResult<sparkles::evidence::Proposed> {
    type Policy = Option<Arc<sparkles::access::GraphAccess>>;
    let mut policies: std::collections::HashMap<(uuid::Uuid, String), Policy> =
        std::collections::HashMap::new();
    let mut read = |ds: &sparkles::Dataset, branch: &str| -> Policy {
        policies
            .entry((ds.dataset_id(), branch.to_string()))
            .or_insert_with(|| policy(st, p, ds, branch))
            .clone()
    };
    Ok(sparkles::evidence::propose(
        Some(&st.catalog),
        &main.dataset,
        &logical.dataset,
        input,
        options,
        &mut read,
        control,
    )?)
}

/// `POST /{ds}/claims`: propose one batch. `200` with the operation and its records,
/// committed, replayed or previewed. `400` for a malformed or inconsistent batch, `409`
/// for `head-changed`, `idempotency-conflict`, `immutable-record-conflict`,
/// `hold-conflict` and `unsupported-profile`, `422` for the reference outcomes
/// `evidence-unavailable`, `incomplete-evidence`, `unanchored-evidence`,
/// `span-mismatch` and `origin-not-reproduced`, `403` for a written graph the caller
/// may not update, and `503 authority-changed` when its credential or the policy
/// changed before publication.
pub(super) async fn propose(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    super::run_http("propose", st, name, p, bytes).await
}

pub(super) fn propose_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: super::Execution,
) -> ApiResult<J> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if bytes.len() > MAX_CLAIMS_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a proposal holds at most {MAX_CLAIMS_BYTES} bytes"),
        ));
    }
    let request: ClaimsRequest = serde_json::from_slice(&bytes).map_err(|e| {
        crate::http::err_code(
            StatusCode::BAD_REQUEST,
            "invalid-proposal",
            format!("invalid proposal: {e}"),
        )
    })?;
    let (input, request) = input_of(request)?;
    let branch = execution.branch.clone();
    let logical = execution.dataset(&st, &name)?;
    let main = if branch == sparkles::branch::MAIN {
        logical.clone()
    } else {
        crate::http::main_dataset(&st, &name)?
    };
    use crate::auth::{Endpoint, on_branch};
    let here = on_branch(&main.name, &branch);
    let caller = crate::http::caller::CallerContext::for_caller(&st, &logical, &p);
    let opts = caller.options_on(&crate::http::Params::empty(), &here, Endpoint::Update);
    let control = execution.control(&opts);
    let mut options = ProposeOptions::new(principal_iri(&p));
    options.idempotency_key = request.idempotency_key;
    options.if_head = request.if_head;
    options.dry_run = request.dry_run;
    options.source_graph = request.source_graph;
    options.proposal_graph = request.proposal_graph;
    options.identity_graph = request.identity_graph;
    options.message = request.message.map(Arc::from);
    options.author = crate::http::author(&p);
    options.write = opts.graphs.clone();
    options.authority = Some(Arc::new(CallerFence {
        st: st.clone(),
        p: p.clone(),
    }));
    type Policy = Option<Arc<sparkles::access::GraphAccess>>;
    let mut policies: std::collections::HashMap<(uuid::Uuid, String), Policy> =
        std::collections::HashMap::new();
    let mut read = |ds: &sparkles::Dataset, branch: &str| -> Policy {
        policies
            .entry((ds.dataset_id(), branch.to_string()))
            .or_insert_with(|| policy(&st, &p, ds, branch))
            .clone()
    };
    let proposed = sparkles::evidence::propose(
        Some(&st.catalog),
        &main.dataset,
        &logical.dataset,
        &input,
        &options,
        &mut read,
        &control,
    )?;
    let mut out = json!({
        "operation": proposed.operation.as_str(),
        "committed": proposed.committed,
        "replayed": proposed.replayed,
        "commit": proposed.commit,
        "branchId": proposed.branch,
        "firstAvailableAt": proposed.first_available_at,
        "hold": proposed.hold,
        "records": proposed.records.iter().map(record_json).collect::<Vec<_>>(),
    });
    if let Some(preview) = &proposed.preview {
        let commit = preview.receipt_commit();
        out["dryRun"] = json!({
            "wouldCommit": matches!(preview.outcome(), sparkles::preview::Outcome::Commit),
            "inserted": commit.inserted,
            "deleted": commit.deleted,
            "error": preview.error(),
        });
    }
    Ok(out)
}
