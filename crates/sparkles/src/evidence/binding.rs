//! Shared JSON request/response helpers for the Python, Node.js and JVM evidence
//! bindings (spec C25, roadmap item 509).
//!
//! Request and response shapes follow the camelCase contract of the HTTP and MCP
//! evidence operations so all three bindings validate and format records identically.

use super::resolve::{
    BlankAnchor, GraphIdentity, MAX_REFERENCES, RecordKind, RecordRequest, Reference, Resolution,
    StoredFactRef, Verified,
};
use super::{
    AssessmentInput, BoundaryInput, Changed, ClaimInput, ClaimKind, Confidence, ConfidenceFacets,
    DecisionInput, Disposition, EvidenceInput, EvidenceLinkInput, EvidenceRefInput,
    EvidenceRefTarget, EvidenceRelation, FoldChoice, IdentityInput, IdentityRelation,
    MAX_ASSESSMENT_INPUTS, MAX_CHANGED, MAX_COMMITS, MAX_IDENTITY_ENTITIES, MAX_RESOLVE, MethodRef,
    NormalizationInput, NormalizationStatus, PassageInput, Polarity, ProposeOptions,
    ProposedRecord, RecordKey, RecordRef, RecordVersion, RenditionContent, RenditionInput,
    ReviewOptions, Scope, SettingsStatus, Sha256Digest, SourceContent, SourceHold, SourceLineage,
    SourceMetadata, SourceRevisionInput, SourceTimes, ValidityInput, WithdrawalInput,
};
use crate::access::GraphAccess;
use crate::task::Control;
use crate::{Dataset, Error, Result};
use oxrdf::{Literal, NamedNode, Term, Triple};
use serde::{Deserialize, Deserializer, de};
use serde_json::{Value as J, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// The principal IRI recorded when an embedded binding caller does not pass one.
pub const DEFAULT_PRINCIPAL: &str = "urn:x-sparkles:principal:local";

/// The most holds returned in one page.
const MAX_PAGE: usize = 1000;
/// The most bytes of a request key.
const MAX_KEY_BYTES: usize = 256;
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

fn bad(field: impl std::fmt::Display, why: impl std::fmt::Display) -> Error {
    Error::invalid(format!("{field}: {why}"))
}

fn named(field: &str, value: &str) -> Result<NamedNode> {
    NamedNode::new(value).map_err(|e| bad(field, e))
}

fn key_of(field: &str, key: &str) -> Result<String> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(bad(field, format!("a key has 1 to {MAX_KEY_BYTES} bytes")));
    }
    Ok(key.to_string())
}

fn record_key(field: &str, value: &str) -> Result<RecordKey> {
    if value.len() > MAX_KEY_BYTES {
        return Err(bad(field, format!("a key has 1 to {MAX_KEY_BYTES} bytes")));
    }
    RecordKey::new(value).map_err(|e| bad(field, e))
}

fn digest(field: &str, value: &str) -> Result<Sha256Digest> {
    value.parse().map_err(|e| bad(field, e))
}

/// Accept a JSON unsigned integer or a decimal string (for Node.js BigInt serialization).
fn de_u64<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<u64, D::Error> {
    struct Visitor;
    impl de::Visitor<'_> for Visitor {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an unsigned integer")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<u64, E> {
            Ok(v)
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<u64, E> {
            v.parse::<u64>()
                .map_err(|_| E::custom("expected an unsigned integer"))
        }
    }
    deserializer.deserialize_any(Visitor)
}

fn de_opt_u64<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error> {
    struct Visitor;
    impl de::Visitor<'_> for Visitor {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an optional unsigned integer")
        }
        fn visit_none<E: de::Error>(self) -> std::result::Result<Option<u64>, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Option<u64>, E> {
            Ok(None)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Option<u64>, E> {
            Ok(Some(v))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Option<u64>, E> {
            v.parse::<u64>()
                .map(Some)
                .map_err(|_| E::custom("expected an unsigned integer"))
        }
    }
    deserializer.deserialize_any(Visitor)
}

fn de_u32<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<u32, D::Error> {
    let v = de_u64(deserializer)?;
    u32::try_from(v).map_err(|_| de::Error::custom("integer out of u32 range"))
}

/// Resolve the logical dataset (`main` or a named branch) for an evidence operation.
pub fn logical_dataset(main: &Dataset, branch: Option<&str>) -> Result<Dataset> {
    if main.store().is_branch() {
        return Err(Error::invalid(
            "evidence operations run on the main dataset; pass branch to target a branch",
        ));
    }
    match branch {
        None | Some(crate::branch::MAIN) => Ok(main.clone()),
        Some(name) => main.branch(name),
    }
}

fn all_read(_ds: &Dataset, _branch: &str) -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::all()))
}

// ------------------------------------------------------------------ settings ----

fn settings_body(status: SettingsStatus) -> Result<J> {
    let settings = status.settings.canonical(&Control::none())?;
    Ok(json!({
        "settings": settings.value(),
        "digest": status.digest,
        "revision": status.revision,
        "managed": status.managed,
        "detached": status.detached,
    }))
}

/// Read the dataset's evidence settings and current graph protection state.
pub fn settings(dataset: &Dataset) -> Result<J> {
    settings_body(super::settings(dataset)?)
}

/// Parse and record complete evidence settings, protecting the graphs of their roles.
pub fn configure(dataset: &Dataset, bytes: &[u8], control: &Control) -> Result<J> {
    let parsed = super::parse_settings(bytes, control)?;
    settings_body(super::configure(dataset, &parsed, control)?)
}

/// Stop protecting one evidence role graph so ordinary writes can change it.
pub fn detach(dataset: &Dataset, graph: &str) -> Result<J> {
    settings_body(super::detach(dataset, graph)?)
}

// --------------------------------------------------------------------- holds ----

fn branch_names(store: &crate::store::Store) -> Result<HashMap<Uuid, String>> {
    let mut names = HashMap::new();
    names.insert(store.dataset_id(), crate::branch::MAIN.to_string());
    if store.branch_set().is_some() {
        for b in store.branches()? {
            names.insert(b.id, b.name);
        }
    }
    Ok(names)
}

fn branch_id(store: &crate::store::Store, name: &str) -> Result<Uuid> {
    if name == crate::branch::MAIN {
        return Ok(store.dataset_id());
    }
    Ok(store.branch_info(name)?.id)
}

fn hold_json(h: &SourceHold, names: &HashMap<Uuid, String>) -> J {
    json!({
        "id": h.id,
        "owner": h.owner.as_str(),
        "created": crate::commit::rfc3339_ms(h.created_ms),
        "commits": h.commits.iter().map(|c| json!({
            "branch": names.get(&c.branch_id),
            "branchId": c.branch_id,
            "commit": c.seq,
        })).collect::<Vec<_>>(),
    })
}

/// List durable evidence holds, optionally filtered by branch and paginated by id.
pub fn holds(
    dataset: &Dataset,
    on_branch: Option<&str>,
    after: Option<&str>,
    limit: Option<usize>,
) -> Result<J> {
    let limit = limit.unwrap_or(MAX_PAGE);
    if limit == 0 || limit > MAX_PAGE {
        return Err(Error::invalid(format!(
            "limit must be between 1 and {MAX_PAGE}"
        )));
    }
    let after = after
        .map(|s| {
            s.parse::<Uuid>()
                .map_err(|_| bad("after", format!("not a hold id: {s}")))
        })
        .transpose()?;
    let store = dataset.store();
    let names = branch_names(store)?;
    let branch = on_branch.map(|b| branch_id(store, b)).transpose()?;
    let all = super::holds(dataset)?;
    let held: usize = all.iter().map(|h| h.commits.len()).sum();
    let mut page: Vec<J> = Vec::new();
    let mut next = None;
    for h in all.iter().filter(|h| {
        after.is_none_or(|a| h.id > a)
            && branch.is_none_or(|b| h.commits.iter().any(|c| c.branch_id == b))
    }) {
        if page.len() == limit {
            next = page.last().map(|p: &J| p["id"].clone());
            break;
        }
        page.push(hold_json(h, &names));
    }
    Ok(json!({
        "holds": page,
        "next": next,
        "heldCommits": held,
        "maxCommits": MAX_COMMITS,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HeldCommitJson {
    branch: Option<String>,
    branch_id: Option<Uuid>,
    #[serde(deserialize_with = "de_u64")]
    commit: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcquireHoldJson {
    id: Option<Uuid>,
    commits: Vec<HeldCommitJson>,
}

/// Acquire a durable evidence hold over one or more commits.
pub fn acquire_hold(dataset: &Dataset, request: &J) -> Result<J> {
    let req: AcquireHoldJson = serde_json::from_value(request.clone())
        .map_err(|e| Error::invalid(format!("invalid hold request: {e}")))?;
    if req.commits.is_empty() {
        return Err(Error::invalid("a hold names at least one commit"));
    }
    if req.commits.len() > MAX_COMMITS {
        return Err(Error::invalid(format!(
            "a hold names at most {MAX_COMMITS} commits"
        )));
    }
    let store = dataset.store();
    let mut commits = Vec::with_capacity(req.commits.len());
    for c in &req.commits {
        let bid = match (&c.branch, c.branch_id) {
            (Some(b), None) => branch_id(store, b)?,
            (None, Some(id)) => id,
            (None, None) => branch_id(store, crate::branch::MAIN)?,
            (Some(_), Some(_)) => {
                return Err(Error::invalid(
                    "a held commit names its branch by branch or branchId, not both",
                ));
            }
        };
        commits.push(super::CommitRef {
            branch_id: bid,
            seq: c.commit,
        });
    }
    let id = req.id.unwrap_or_else(Uuid::new_v4);
    let acquired = super::acquire_hold(dataset, id, &commits)?;
    let names = branch_names(store)?;
    Ok(json!({
        "created": acquired.created,
        "hold": hold_json(&acquired.hold, &names),
    }))
}

/// Release an evidence hold by UUID, returning whether it existed.
pub fn release_hold(dataset: &Dataset, id: &str) -> Result<bool> {
    let id: Uuid = id
        .parse()
        .map_err(|_| Error::invalid(format!("not a hold id: {id}")))?;
    super::release_hold(dataset, id)
}

// ---------------------------------------------------------------- resolution ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveRequestJson {
    branch: Option<String>,
    references: Vec<ResolveReferenceJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveReferenceJson {
    key: String,
    stored_fact: Option<StoredFactJson>,
    record: Option<RecordJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredFactJson {
    dataset_id: Uuid,
    branch_id: Uuid,
    #[serde(deserialize_with = "de_u64")]
    commit: u64,
    graph: Option<String>,
    triple: String,
    reifier: Option<String>,
    import_map: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordJson {
    iri: String,
    #[serde(default)]
    kinds: Vec<String>,
    content_digest: Option<String>,
}

fn record_kind(name: &str) -> Option<RecordKind> {
    use RecordKind::*;
    [
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
        AcceptanceSupport,
        Withdrawal,
        Projection,
    ]
    .into_iter()
    .find(|k| super::resolve::kind_iri(*k).strip_prefix("urn:x-sparkles:evidence:") == Some(name))
}

fn kind_name(kind: RecordKind) -> &'static str {
    let iri = super::resolve::kind_iri(kind);
    iri.strip_prefix("urn:x-sparkles:evidence:").unwrap_or(iri)
}

fn parse_triple(field: &str, text: &str) -> Result<Triple> {
    let line = format!("{text} .\n");
    let mut quads =
        oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::NTriples).for_slice(line.as_bytes());
    let quad = match quads.next() {
        Some(Ok(q)) => q,
        Some(Err(e)) => return Err(bad(field, e)),
        None => return Err(bad(field, "expected one triple")),
    };
    if quads.next().is_some() {
        return Err(bad(field, "expected one triple"));
    }
    Ok(Triple::new(quad.subject, quad.predicate, quad.object))
}

fn stored_fact_ref(at: &str, f: StoredFactJson) -> Result<StoredFactRef> {
    let graph = match &f.graph {
        None => GraphIdentity::Default,
        Some(g) => GraphIdentity::Named(named(&format!("{at}.graph"), g)?),
    };
    let anchor = match (&f.reifier, &f.import_map) {
        (Some(_), Some(_)) => {
            return Err(bad(at, "name a reifier or an import mapping, not both"));
        }
        (Some(r), None) => Some(BlankAnchor::Reifier(named(&format!("{at}.reifier"), r)?)),
        (None, Some(m)) => Some(BlankAnchor::ImportMap(named(
            &format!("{at}.importMap"),
            m,
        )?)),
        (None, None) => None,
    };
    Ok(StoredFactRef {
        dataset: f.dataset_id,
        branch: f.branch_id,
        commit: f.commit,
        graph,
        expected: parse_triple(&format!("{at}.triple"), &f.triple)?,
        anchor,
    })
}

fn graph_json(graph: &GraphIdentity) -> J {
    match graph {
        GraphIdentity::Default => J::Null,
        GraphIdentity::Named(n) => J::String(n.as_str().to_string()),
    }
}

fn verified_json(key: &str, v: &Verified) -> J {
    match v {
        Verified::Fact(f) => json!({
            "key": key,
            "storedFact": {
                "datasetId": f.dataset,
                "branchId": f.branch,
                "commit": f.commit,
                "servedBy": f.served_by,
                "graph": graph_json(&f.graph),
                "reifier": f.reifier.as_ref().map(|r| r.as_str()),
            },
        }),
        Verified::Record(r) => json!({
            "key": key,
            "record": {
                "iri": r.iri.as_str(),
                "kind": kind_name(r.kind),
                "graph": r.graph.as_str(),
                "receipt": {
                    "iri": r.receipt.iri.as_str(),
                    "datasetId": r.receipt.dataset,
                    "branchId": r.receipt.branch,
                    "commit": r.receipt.commit,
                },
                "contentDigest": r.content_digest.map(|d| d.to_string()),
                "recordDigest": r.record_digest.map(|d| d.to_string()),
                "span": r.span.as_ref().map(|s| json!({
                    "rendition": s.rendition.as_str(),
                    "start": s.start,
                    "end": s.end,
                    "textDigest": s.text_digest.to_string(),
                })),
            },
        }),
    }
}

/// Resolve stored-fact and record references in order under the 64 MiB allowance.
pub fn resolve(main: &Dataset, branch: Option<&str>, request: &J, control: &Control) -> Result<J> {
    let req: ResolveRequestJson = serde_json::from_value(request.clone())
        .map_err(|e| Error::invalid(format!("invalid resolution request: {e}")))?;
    if req.references.len() > MAX_REFERENCES {
        return Err(bad(
            "references",
            format!("at most {MAX_REFERENCES} references"),
        ));
    }
    let mut keys = HashSet::new();
    let mut references = Vec::with_capacity(req.references.len());
    for (i, r) in req.references.into_iter().enumerate() {
        let at = format!("references[{i}]");
        let key = key_of(&format!("{at}.key"), &r.key)?;
        if !keys.insert(key.clone()) {
            return Err(bad(format!("{at}.key"), "keys are unique"));
        }
        let reference = match (r.stored_fact, r.record) {
            (Some(f), None) => {
                Reference::StoredFact(stored_fact_ref(&format!("{at}.storedFact"), f)?)
            }
            (None, Some(rec)) => {
                let at = format!("{at}.record");
                let mut kinds = Vec::new();
                for (j, k) in rec.kinds.iter().enumerate() {
                    let kind = record_kind(k).ok_or_else(|| {
                        bad(
                            format!("{at}.kinds[{j}]"),
                            format!("unknown record kind {k}"),
                        )
                    })?;
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                }
                let content_digest = rec
                    .content_digest
                    .as_deref()
                    .map(|d| digest(&format!("{at}.contentDigest"), d))
                    .transpose()?;
                Reference::Record(RecordRequest {
                    iri: named(&format!("{at}.iri"), &rec.iri)?,
                    kinds,
                    content_digest,
                })
            }
            _ => {
                return Err(bad(
                    at,
                    "a reference has exactly one of storedFact and record",
                ));
            }
        };
        references.push((key, reference));
    }
    let effective_branch = branch.or(req.branch.as_deref());
    let logical = logical_dataset(main, effective_branch)?;
    let mut resolution = Resolution::new(control.clone())?;
    let mut access = all_read;
    let verified = resolution.all(None, main, &logical, &references, &mut access)?;
    let resolved: Vec<J> = references
        .iter()
        .zip(verified.iter())
        .map(|((k, _), v)| verified_json(k, v))
        .collect();
    Ok(json!({ "resolved": resolved }))
}

// ------------------------------------------------------------------ propose ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimsRequestJson {
    branch: Option<String>,
    principal: Option<String>,
    author: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default, deserialize_with = "de_opt_u64")]
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
    references: Vec<EvidenceRefJson>,
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
    relation: String,
    preferred_entity: Option<String>,
    author: String,
    method: Option<MethodJson>,
}

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
    #[serde(deserialize_with = "de_u64")]
    start: u64,
    #[serde(deserialize_with = "de_u64")]
    end: u64,
    text_digest: String,
    quote: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvidenceRefJson {
    key: String,
    occurrence_key: String,
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
    proposition: String,
    kind: String,
    polarity: String,
    author: String,
    #[serde(default)]
    scope: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    confidence: FacetsJson,
    validity: Option<ValidityJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidityJson {
    start: BoundaryJson,
    end: BoundaryJson,
    normalization: NormalizationJson,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoundaryJson {
    kind: String,
    earliest: Option<String>,
    latest: Option<String>,
    precision: Option<String>,
    source_value: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NormalizationJson {
    status: String,
    method: String,
    method_version: String,
    calendar: String,
    timezone_policy: Option<String>,
    tzdb_version: Option<String>,
    fold_choice: Option<String>,
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
    conclusion: String,
    method: MethodJson,
    configuration_digest: String,
    evaluated_at: String,
    evaluator: String,
    confidence: Option<ConfidenceJson>,
    #[serde(default)]
    scope: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    inputs: Vec<RefJson>,
}

fn reference(field: &str, r: &RefJson) -> Result<RecordRef> {
    match (&r.key, &r.iri) {
        (Some(k), None) => Ok(RecordRef::Local(record_key(&format!("{field}.key"), k)?)),
        (None, Some(i)) => Ok(RecordRef::Existing(named(&format!("{field}.iri"), i)?)),
        _ => Err(bad(
            field,
            "a record reference has exactly one of key and iri",
        )),
    }
}

fn method_ref(field: &str, m: &MethodJson) -> Result<MethodRef> {
    Ok(MethodRef {
        method: named(&format!("{field}.method"), &m.method)?,
        version: record_key(&format!("{field}.version"), &m.version)?,
    })
}

fn parse_term(field: &str, text: &str) -> Result<Term> {
    Ok(parse_triple(
        field,
        &format!("<urn:x-sparkles:s> <urn:x-sparkles:p> {text}"),
    )?
    .object)
}

fn date_time(value: &str) -> Literal {
    Literal::new_typed_literal(value, NamedNode::new_unchecked(format!("{XSD}dateTime")))
}

type Content = (Option<Vec<u8>>, Option<String>, Option<Sha256Digest>);

fn content_of(field: &str, c: &ContentJson) -> Result<Content> {
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
            field,
            "content has exactly one of text, base64 and digest",
        )),
    }
}

fn parse_scope(field: &str, s: &BTreeMap<String, Vec<String>>) -> Result<Scope> {
    let mut out = Scope::new();
    for (p, values) in s {
        named(&format!("{field}.scope"), p)?;
        let mut terms = Vec::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            terms.push(parse_term(&format!("{field}.scope[{p}][{i}]"), v)?);
        }
        out.insert(p.clone(), terms);
    }
    Ok(out)
}

fn parse_confidence(field: &str, c: &ConfidenceJson) -> Result<Confidence> {
    let value = if c.value.starts_with('"') {
        match parse_term(&format!("{field}.value"), &c.value)? {
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
            .map(|m| method_ref(&format!("{field}.method"), m))
            .transpose()?,
    })
}

fn parse_boundary(at: &str, b: &BoundaryJson) -> Result<BoundaryInput> {
    let finite = b.earliest.is_some() || b.latest.is_some() || b.precision.is_some();
    Ok(match b.kind.as_str() {
        "known" => {
            let (Some(earliest), Some(latest), Some(precision), Some(source_value)) =
                (&b.earliest, &b.latest, &b.precision, &b.source_value)
            else {
                return Err(bad(
                    at,
                    "a known boundary has earliest, latest, precision and sourceValue",
                ));
            };
            BoundaryInput::Known {
                earliest: date_time(earliest),
                latest: date_time(latest),
                precision: named(&format!("{at}.precision"), precision)?,
                source_value: source_value.clone(),
            }
        }
        "unknown" | "unbounded" if finite => {
            return Err(bad(
                at,
                "an unknown or unbounded boundary has no earliest, latest or precision",
            ));
        }
        "unknown" => BoundaryInput::Unknown {
            source_value: b.source_value.clone(),
        },
        "unbounded" => BoundaryInput::Unbounded {
            source_value: b.source_value.clone(),
        },
        _ => {
            return Err(bad(
                format!("{at}.kind"),
                "kind is known, unknown or unbounded",
            ));
        }
    })
}

fn parse_validity(at: &str, v: &ValidityJson) -> Result<ValidityInput> {
    let n = &v.normalization;
    let at_n = format!("{at}.normalization");
    let status = match n.status.as_str() {
        "normalized" => NormalizationStatus::Normalized,
        "unresolved" => NormalizationStatus::Unresolved,
        "unsupportedCalendar" => NormalizationStatus::UnsupportedCalendar,
        "unsupportedPrecision" => NormalizationStatus::UnsupportedPrecision,
        "indeterminateOrder" => NormalizationStatus::IndeterminateOrder,
        "nonexistentLocalTime" => NormalizationStatus::NonexistentLocalTime,
        _ => {
            return Err(bad(
                format!("{at_n}.status"),
                "status is normalized, unresolved, unsupportedCalendar, unsupportedPrecision, indeterminateOrder or nonexistentLocalTime",
            ));
        }
    };
    let fold_choice = match n.fold_choice.as_deref() {
        None => None,
        Some("earlier") => Some(FoldChoice::Earlier),
        Some("later") => Some(FoldChoice::Later),
        Some(_) => {
            return Err(bad(
                format!("{at_n}.foldChoice"),
                "foldChoice is earlier or later",
            ));
        }
    };
    Ok(ValidityInput {
        start: parse_boundary(&format!("{at}.start"), &v.start)?,
        end: parse_boundary(&format!("{at}.end"), &v.end)?,
        normalization: NormalizationInput {
            status,
            method: named(&format!("{at_n}.method"), &n.method)?,
            method_version: n.method_version.clone(),
            calendar: named(&format!("{at_n}.calendar"), &n.calendar)?,
            timezone_policy: n.timezone_policy.clone(),
            tzdb_version: n.tzdb_version.clone(),
            fold_choice,
        },
    })
}

fn parse_claim_kind(field: &str, value: &str) -> Result<ClaimKind> {
    Ok(match value {
        "reported" => ClaimKind::Reported,
        "observation" => ClaimKind::Observation,
        "hypothesis" => ClaimKind::Hypothesis,
        "preference" => ClaimKind::Preference,
        _ => {
            return Err(bad(
                field,
                "kind is reported, observation, hypothesis or preference",
            ));
        }
    })
}

fn parse_polarity(field: &str, value: &str) -> Result<Polarity> {
    Ok(match value {
        "positive" => Polarity::Positive,
        "negative" => Polarity::Negative,
        "uncertain" => Polarity::Uncertain,
        _ => return Err(bad(field, "polarity is positive, negative or uncertain")),
    })
}

fn parse_relation(field: &str, value: &str) -> Result<EvidenceRelation> {
    Ok(match value {
        "supports" => EvidenceRelation::Supports,
        "contradicts" => EvidenceRelation::Contradicts,
        "mentions" => EvidenceRelation::Mentions,
        _ => return Err(bad(field, "relation is supports, contradicts or mentions")),
    })
}

fn record_json(r: &ProposedRecord) -> J {
    json!({
        "key": r.key,
        "kind": kind_name(r.kind),
        "iri": r.iri.as_str(),
        "graph": r.graph.as_str(),
        "recordDigest": r.digest.to_string(),
        "existing": r.existing,
    })
}

fn preview_json(out: &mut J, preview: Option<&crate::preview::Preview>) {
    if let Some(preview) = preview {
        let commit = preview.receipt_commit();
        out["dryRun"] = json!({
            "wouldCommit": matches!(preview.outcome(), crate::preview::Outcome::Commit),
            "inserted": commit.inserted,
            "deleted": commit.deleted,
            "error": preview.error(),
        });
    }
}

/// Publish an authenticated claim/evidence proposal batch.
pub fn propose(
    main: &Dataset,
    branch: Option<&str>,
    principal: Option<&str>,
    author: Option<&str>,
    request: &J,
    control: &Control,
) -> Result<J> {
    let r: ClaimsRequestJson = serde_json::from_value(request.clone())
        .map_err(|e| Error::invalid(format!("invalid proposal: {e}")))?;
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
            rights.push(parse_term(&format!("{at}.reportedRights[{j}]"), t)?);
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
                published_at: v.published_at.as_deref().map(date_time),
                reported_available_at: v.reported_available_at.as_deref().map(date_time),
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
            conversion: method_ref(&format!("{at}.conversion"), &v.conversion)?,
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
                pointer: s.pointer,
                method: method_ref(&format!("{at}.structured.method"), &s.method)?,
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
                .map(|c| parse_confidence(&format!("{at}.confidence.{name}"), c))
                .transpose()
        };
        input.claims.push(ClaimInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            revision: reference(&format!("{at}.revision"), &v.revision)?,
            proposition: parse_triple(&format!("{at}.proposition"), &v.proposition)?,
            kind: parse_claim_kind(&format!("{at}.kind"), &v.kind)?,
            polarity: parse_polarity(&format!("{at}.polarity"), &v.polarity)?,
            attributed_author: named(&format!("{at}.author"), &v.author)?,
            scope: parse_scope(&at, &v.scope)?,
            confidence: ConfidenceFacets {
                source: facet("source", &v.confidence.source)?,
                extraction: facet("extraction", &v.confidence.extraction)?,
                analyst: facet("analyst", &v.confidence.analyst)?,
            },
            validity: v
                .validity
                .as_ref()
                .map(|val| parse_validity(&format!("{at}.validity"), val))
                .transpose()?,
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
            relation: parse_relation(&format!("{at}.relation"), &v.relation)?,
            attributed_author: named(&format!("{at}.author"), &v.author)?,
        });
    }
    for (i, v) in r.assessments.iter().enumerate() {
        let at = format!("assessments[{i}]");
        if v.inputs.len() > MAX_ASSESSMENT_INPUTS {
            return Err(bad(
                format!("{at}.inputs"),
                format!("an assessment names at most {MAX_ASSESSMENT_INPUTS} inputs"),
            ));
        }
        input.assessments.push(AssessmentInput {
            version: RecordVersion::V1,
            key: record_key(&format!("{at}.key"), &v.key)?,
            occurrence_key: record_key(&format!("{at}.occurrenceKey"), &v.occurrence_key)?,
            target: reference(&format!("{at}.target"), &v.target)?,
            conclusion: parse_term(&format!("{at}.conclusion"), &v.conclusion)?,
            method: method_ref(&format!("{at}.method"), &v.method)?,
            configuration_digest: digest(
                &format!("{at}.configurationDigest"),
                &v.configuration_digest,
            )?,
            evaluated_at: date_time(&v.evaluated_at),
            attributed_evaluator: named(&format!("{at}.evaluator"), &v.evaluator)?,
            confidence: v
                .confidence
                .as_ref()
                .map(|c| parse_confidence(&format!("{at}.confidence"), c))
                .transpose()?,
            scope: parse_scope(&at, &v.scope)?,
            inputs: v
                .inputs
                .iter()
                .enumerate()
                .map(|(j, x)| reference(&format!("{at}.inputs[{j}]"), x))
                .collect::<Result<Vec<_>>>()?,
        });
    }
    for (i, v) in r.identities.iter().enumerate() {
        let at = format!("identities[{i}]");
        if v.entities.len() > MAX_IDENTITY_ENTITIES {
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
                .collect::<Result<_>>()?,
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
                .map(|m| method_ref(&format!("{at}.method"), m))
                .transpose()?,
        });
    }
    if let Some(k) = &r.idempotency_key
        && (k.is_empty() || k.len() > MAX_KEY_BYTES)
    {
        return Err(bad(
            "idempotencyKey",
            format!("a key has 1 to {MAX_KEY_BYTES} bytes"),
        ));
    }
    let principal_iri = named(
        "principal",
        principal
            .or(r.principal.as_deref())
            .unwrap_or(DEFAULT_PRINCIPAL),
    )?;
    let mut options = ProposeOptions::new(principal_iri);
    options.idempotency_key = r.idempotency_key;
    options.if_head = r.if_head;
    options.dry_run = r.dry_run;
    options.source_graph = r
        .source_graph
        .as_deref()
        .map(|g| named("sourceGraph", g))
        .transpose()?;
    options.proposal_graph = r
        .proposal_graph
        .as_deref()
        .map(|g| named("proposalGraph", g))
        .transpose()?;
    options.identity_graph = r
        .identity_graph
        .as_deref()
        .map(|g| named("identityGraph", g))
        .transpose()?;
    options.message = r.message.map(Arc::from);
    options.author = author.or(r.author.as_deref()).map(Arc::from);
    let effective_branch = branch.or(r.branch.as_deref());
    let logical = logical_dataset(main, effective_branch)?;
    let mut read = all_read;
    let proposed = super::propose(None, main, &logical, &input, &options, &mut read, control)?;
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
    preview_json(&mut out, proposed.preview.as_deref());
    Ok(out)
}

// ------------------------------------------------------------ review & withdraw

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewRequestJson {
    branch: Option<String>,
    principal: Option<String>,
    author: Option<String>,
    policy: String,
    #[serde(deserialize_with = "de_u32")]
    policy_version: u32,
    #[serde(deserialize_with = "de_u64")]
    if_head: u64,
    idempotency_key: Option<String>,
    #[serde(default)]
    dry_run: bool,
    message: Option<String>,
    decisions: Vec<DecisionJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecisionJson {
    key: String,
    target: String,
    target_digest: String,
    disposition: String,
    supersedes: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawRequestJson {
    branch: Option<String>,
    principal: Option<String>,
    author: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    if_head: Option<u64>,
    idempotency_key: Option<String>,
    #[serde(default)]
    dry_run: bool,
    message: Option<String>,
    withdrawals: Vec<WithdrawalJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawalJson {
    key: String,
    target: String,
    target_digest: String,
}

fn changed_json(c: &Changed) -> J {
    let change = |v: &[super::ProjectionChange]| -> J {
        v.iter()
            .map(|c| json!({ "output": c.output.as_str(), "triple": c.triple.to_string() }))
            .collect::<Vec<_>>()
            .into()
    };
    let names = |v: &[NamedNode]| -> J {
        v.iter()
            .map(|n| J::from(n.as_str()))
            .collect::<Vec<_>>()
            .into()
    };
    let mut out = json!({
        "supportsAdded": names(&c.supports_added),
        "supportsEnded": names(&c.supports_ended),
        "inserted": change(&c.inserted),
        "removed": change(&c.removed),
    });
    if let Some(m) = &c.map {
        out["map"] = json!({
            "policy": m.policy.as_str(),
            "revision": m.revision.as_str(),
            "number": m.number,
            "previous": m.previous.as_ref().map(|p| p.as_str()),
            "decisionsAdded": names(&m.decisions_added),
            "decisionsEnded": names(&m.decisions_ended),
            "entities": names(&m.entities),
        });
    }
    out
}

/// Append review decisions under a named review or matching policy.
pub fn review(
    main: &Dataset,
    branch: Option<&str>,
    principal: Option<&str>,
    author: Option<&str>,
    request: &J,
    control: &Control,
) -> Result<J> {
    let r: ReviewRequestJson = serde_json::from_value(request.clone())
        .map_err(|e| Error::invalid(format!("invalid review: {e}")))?;
    let mut decisions = Vec::with_capacity(r.decisions.len());
    for (i, d) in r.decisions.iter().enumerate() {
        let at = format!("decisions[{i}]");
        decisions.push(DecisionInput {
            key: key_of(&format!("{at}.key"), &d.key)?,
            target: named(&format!("{at}.target"), &d.target)?,
            target_digest: digest(&format!("{at}.targetDigest"), &d.target_digest)?,
            disposition: Disposition::from_name(&d.disposition).ok_or_else(|| {
                bad(
                    format!("{at}.disposition"),
                    "use accepted, rejected or needsEvidence",
                )
            })?,
            supersedes: d
                .supersedes
                .as_deref()
                .map(|s| named(&format!("{at}.supersedes"), s))
                .transpose()?,
        });
    }
    if let Some(k) = &r.idempotency_key
        && (k.is_empty() || k.len() > MAX_KEY_BYTES)
    {
        return Err(bad(
            "idempotencyKey",
            format!("a key has 1 to {MAX_KEY_BYTES} bytes"),
        ));
    }
    let principal_iri = named(
        "principal",
        principal
            .or(r.principal.as_deref())
            .unwrap_or(DEFAULT_PRINCIPAL),
    )?;
    let mut options = ReviewOptions::new(principal_iri);
    options.idempotency_key = r.idempotency_key;
    options.if_head = Some(r.if_head);
    options.dry_run = r.dry_run;
    options.message = r.message.map(Arc::from);
    options.author = author.or(r.author.as_deref()).map(Arc::from);
    let effective_branch = branch.or(r.branch.as_deref());
    let logical = logical_dataset(main, effective_branch)?;
    let mut read = all_read;
    let reviewed = super::review(
        None,
        main,
        &logical,
        &r.policy,
        r.policy_version,
        &decisions,
        &options,
        &mut read,
        control,
    )?;
    let mut out = json!({
        "operation": reviewed.operation.as_str(),
        "committed": reviewed.committed,
        "replayed": reviewed.replayed,
        "commit": reviewed.commit,
        "branchId": reviewed.branch,
        "decisions": reviewed.decisions.iter().map(|d| json!({
            "key": d.key,
            "iri": d.iri.as_str(),
            "target": d.target.as_str(),
            "disposition": d.disposition.name(),
            "supersedes": d.supersedes.as_ref().map(|s| s.as_str()),
            "support": d.support.as_ref().map(|s| s.as_str()),
            "notProjected": d.not_projected,
            "conflicts": d.conflicts.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "changed": changed_json(&reviewed.changed),
        "mapRevision": reviewed.map_revision.as_ref().map(|m| m.as_str()),
    });
    preview_json(&mut out, reviewed.preview.as_deref());
    Ok(out)
}

/// Withdraw one or more claim occurrences.
pub fn withdraw(
    main: &Dataset,
    branch: Option<&str>,
    principal: Option<&str>,
    author: Option<&str>,
    request: &J,
    control: &Control,
) -> Result<J> {
    let r: WithdrawRequestJson = serde_json::from_value(request.clone())
        .map_err(|e| Error::invalid(format!("invalid withdrawal: {e}")))?;
    let mut withdrawals = Vec::with_capacity(r.withdrawals.len());
    for (i, w) in r.withdrawals.iter().enumerate() {
        let at = format!("withdrawals[{i}]");
        withdrawals.push(WithdrawalInput {
            key: key_of(&format!("{at}.key"), &w.key)?,
            target: named(&format!("{at}.target"), &w.target)?,
            target_digest: digest(&format!("{at}.targetDigest"), &w.target_digest)?,
        });
    }
    if let Some(k) = &r.idempotency_key
        && (k.is_empty() || k.len() > MAX_KEY_BYTES)
    {
        return Err(bad(
            "idempotencyKey",
            format!("a key has 1 to {MAX_KEY_BYTES} bytes"),
        ));
    }
    let principal_iri = named(
        "principal",
        principal
            .or(r.principal.as_deref())
            .unwrap_or(DEFAULT_PRINCIPAL),
    )?;
    let mut options = ReviewOptions::new(principal_iri);
    options.idempotency_key = r.idempotency_key;
    options.if_head = r.if_head;
    options.dry_run = r.dry_run;
    options.message = r.message.map(Arc::from);
    options.author = author.or(r.author.as_deref()).map(Arc::from);
    let effective_branch = branch.or(r.branch.as_deref());
    let logical = logical_dataset(main, effective_branch)?;
    let mut read = all_read;
    let withdrawn = super::withdraw(main, &logical, &withdrawals, &options, &mut read, control)?;
    let mut out = json!({
        "operation": withdrawn.operation.as_str(),
        "committed": withdrawn.committed,
        "replayed": withdrawn.replayed,
        "commit": withdrawn.commit,
        "branchId": withdrawn.branch,
        "withdrawals": withdrawn.withdrawals.iter().map(|w| json!({
            "key": w.key,
            "iri": w.iri.as_ref().map(|s| s.as_str()),
            "target": w.target.as_str(),
            "existing": w.existing,
        })).collect::<Vec<_>>(),
        "changed": changed_json(&withdrawn.changed),
    });
    preview_json(&mut out, withdrawn.preview.as_deref());
    Ok(out)
}

// -------------------------------------------------- identities & dependents ----

/// Resolve entities through a matching policy's current resolution map.
pub fn resolve_entities(
    main: &Dataset,
    branch: Option<&str>,
    policy: &str,
    entities: &[String],
    control: &Control,
) -> Result<J> {
    if entities.is_empty() || entities.len() > MAX_RESOLVE {
        return Err(bad(
            "entities",
            format!("a resolution names 1 to {MAX_RESOLVE} entities"),
        ));
    }
    let entities = entities
        .iter()
        .enumerate()
        .map(|(i, e)| named(&format!("entities[{i}]"), e))
        .collect::<Result<Vec<_>>>()?;
    let logical = logical_dataset(main, branch)?;
    let resolved = super::resolve_entities(main, &logical, policy, &entities, None, control)?;
    fn names(v: &[NamedNode]) -> Vec<&str> {
        v.iter().map(|n| n.as_str()).collect()
    }
    Ok(json!({
        "policy": resolved.policy.as_str(),
        "revision": resolved.revision.as_ref().map(|r| r.as_str()),
        "number": resolved.number,
        "commit": resolved.commit,
        "entities": resolved.entities.iter().map(|e| json!({
            "entity": e.entity.as_str(),
            "representative": e.representative.as_str(),
            "members": names(&e.members),
            "membersComplete": e.members_complete,
            "decisions": names(&e.decisions),
        })).collect::<Vec<_>>(),
    }))
}

/// Find authenticated assessments that depend on any of `changed`.
pub fn find_dependents(
    main: &Dataset,
    branch: Option<&str>,
    changed: &[String],
    transitive: bool,
    control: &Control,
) -> Result<J> {
    if changed.is_empty() || changed.len() > MAX_CHANGED {
        return Err(bad(
            "changed",
            format!("a request names 1 to {MAX_CHANGED} identities"),
        ));
    }
    let changed = changed
        .iter()
        .enumerate()
        .map(|(i, e)| named(&format!("changed[{i}]"), e))
        .collect::<Result<Vec<_>>>()?;
    let logical = logical_dataset(main, branch)?;
    let found = super::find_dependents(main, &logical, &changed, transitive, None, control)?;
    Ok(json!({
        "commit": found.commit,
        "dependents": found.dependents.iter().map(|d| json!({
            "assessment": d.assessment.as_str(),
            "graph": d.graph.as_str(),
            "on": d.on.as_str(),
            "dependence": d.dependence.name(),
            "depth": d.depth,
        })).collect::<Vec<_>>(),
    }))
}
