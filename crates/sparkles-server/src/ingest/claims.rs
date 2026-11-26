//! Claims mode (spec C25, "Ingestion into claim proposals"): the document is registered
//! as a C18 source with its chunks, and its extracted facts become claim proposals of
//! one evidence batch instead of asserted memory. The registration and the batch
//! publish in one commit of the evidence owner, so a reader sees both or neither.
//!
//! The run checks the caller's grants and the evidence settings before it converts
//! anything, plans the registration without writing it, answers `already-proposed`
//! without a model call when its idempotency key already published, extracts and
//! links the facts, and hands the batch with the registration's quads to
//! [`sparkles::evidence::propose`] with `ifHead` set to the head the plan read. A head
//! that moved is planned again, without a model call, up to three times.

use super::Status;
use super::convert::{self, Format, Refusal};
use super::extract::{Fact, Link, Linked, Obj, Vocabulary};
use super::pipeline::{Failed, Request, Run, expand_iri};
use crate::auth::{Endpoint, Level, on_branch};
use oxrdf::vocab::{rdf, rdfs, xsd};
use oxrdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};
use serde_json::{Map, Value, json};
use sparkles::evidence::{
    ClaimInput, ClaimKind, Confidence, ConfidenceFacets, EvidenceInput, EvidenceLinkInput,
    EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, IdentityNamespace, MethodRef,
    OrdinaryChanges, PassageInput, Polarity, RecordKey, RecordKind, RecordRef, RecordVersion,
    RenditionContent, RenditionInput, Sha256Digest, SourceContent, SourceRevisionInput,
};
use sparkles::task::Control;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

/// How many times a moved head is planned again.
const REPLANS: usize = 3;
const ZERO_TO_ONE: &str = "urn:x-sparkles:evidence:ZeroToOne";
/// Maximum C18 renditions visited when walking `prov:wasRevisionOf` lineage.
const MAX_LINEAGE_HOPS: usize = 64;
/// Hard ceiling on candidate evidence revisions/renditions inspected in one lookup.
const MAX_LINEAGE_CANDIDATES: usize = 4_096;
/// Hard ceiling on C18 `prov:wasRevisionOf` predecessor edges per rendition node.
const MAX_C18_EDGES_PER_NODE: usize = 256;
/// Default scanned-quad budget when the server sets no `max_rows_produced`.
const DEFAULT_LINEAGE_MAX_SCANS: u64 = 1_000_000;

const EV_SOURCE_REVISION: &str = "urn:x-sparkles:evidence:SourceRevision";
const EV_RENDITION: &str = "urn:x-sparkles:evidence:Rendition";
const EV_SOURCE_KEY: &str = "urn:x-sparkles:evidence:sourceKey";
const EV_AVAILABILITY_RECEIPT: &str = "urn:x-sparkles:evidence:availabilityReceipt";
const PROV_SPECIALIZATION_OF: &str = "http://www.w3.org/ns/prov#specializationOf";
const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
const PROV_WAS_REVISION_OF: &str = "http://www.w3.org/ns/prov#wasRevisionOf";
const PROV_WAS_ASSOCIATED_WITH: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
const PROV_INVALIDATED_AT_TIME: &str = "http://www.w3.org/ns/prov#invalidatedAtTime";
const SPK_RENDITION: &str = "urn:x-sparkles:rendition";
const SPK_CONTENT_DIGEST: &str = "urn:x-sparkles:contentDigest";

fn failed(code: &str, status: u16, message: impl Into<String>) -> Failed {
    Failed(json!({ "code": code, "status": status, "message": message.into() }))
}

fn api(e: crate::http::ApiError) -> Failed {
    let (status, body) = e.into_parts();
    let code = body["code"].as_str().unwrap_or(match status.as_u16() {
        400 => "bad-argument",
        403 => "forbidden",
        413 => "too-large",
        404 => "not-found",
        409 => "conflict",
        _ => "error",
    });
    let mut j = json!({
        "code": code,
        "status": status.as_u16(),
        "message": body["error"].as_str().or(body["message"].as_str()).unwrap_or(""),
    });
    for k in ["reference", "field"] {
        if let Some(v) = body.get(k) {
            j[k] = v.clone();
        }
    }
    Failed(j)
}

fn key(s: impl Into<String>) -> RecordKey {
    RecordKey::new(s).expect("batch keys are nonempty")
}

fn sha(bytes: &[u8]) -> Result<Sha256Digest, Failed> {
    Sha256Digest::of_bytes(bytes, &Control::none()).map_err(|e| failed("error", 500, e.to_string()))
}

/// What the evidence settings and grants allow this run, checked before conversion.
struct Target {
    main: Arc<crate::state::Dataset>,
    logical: Arc<crate::state::Dataset>,
    branch: String,
    source_graph: NamedNode,
    proposal_graph: NamedNode,
}

/// The caller needs `write` on the update endpoint of the branch, update grants on the
/// evidence graphs the batch writes, and enabled evidence settings (spec C25, "Grants
/// and settings"). Nothing is converted or sent to a model before this passes.
fn target(r: &Run, req: &Request) -> Result<Target, Failed> {
    let ctx = r.ctx;
    let st = &ctx.server.state;
    let p = ctx.principal;
    let main = crate::http::main_dataset(st, &req.dataset).map_err(api)?;
    let branch = req
        .branch
        .clone()
        .unwrap_or_else(|| sparkles::branch::MAIN.to_string());
    let logical = if branch == sparkles::branch::MAIN {
        main.clone()
    } else {
        st.branch_dataset(&main, &branch)
            .map_err(|e| api(e.into()))?
    };
    let here = on_branch(&main.name, &branch);
    if !p.can(&here, Level::Write) || !p.can_at(&here, Endpoint::Update, Level::Write) {
        return Err(failed(
            "forbidden",
            403,
            format!(
                "write access to the update endpoint of dataset {} required",
                main.name
            ),
        ));
    }
    crate::settings::evidence::require_reconciled(st, &main).map_err(|e| api(e.into()))?;
    let status = sparkles::evidence::settings(&main.dataset).map_err(|e| api(e.into()))?;
    let settings = status.settings;
    if !settings.enabled {
        return Err(failed(
            "unsupported-profile",
            409,
            format!(
                "dataset {} has no enabled evidence settings: configure them with PUT /$/evidence/{}/settings",
                main.name, main.name
            ),
        ));
    }
    let pick = |chosen: &Option<String>, all: &[String], field: &str| {
        let g = match chosen {
            Some(g) if all.iter().any(|x| x == g) => g.clone(),
            Some(g) => {
                return Err(failed(
                    "bad-argument",
                    400,
                    format!("{field} <{g}> is not a graph of that role in the evidence settings"),
                ));
            }
            None if all.len() == 1 => all[0].clone(),
            None => {
                return Err(failed(
                    "bad-argument",
                    400,
                    format!(
                        "the evidence settings name several graphs of that role: choose one with {field}"
                    ),
                ));
            }
        };
        NamedNode::new(g).map_err(|e| failed("bad-argument", 400, e.to_string()))
    };
    // a review verifies every cited passage against the rendition's retained text, so a
    // dataset that keeps no source text could never accept these claims
    if !crate::mcp::memory::ingest::ingest_settings(st, &main).keep_text {
        return Err(failed(
            "no-text",
            409,
            format!(
                "dataset {} keeps no source text (keepText is false), and a review verifies claims against the retained text: claims mode needs keepText",
                main.name
            ),
        ));
    }
    let source_graph = pick(&req.source_graph, &settings.source_records, "sourceGraph")?;
    let proposal_graph = pick(&req.proposal_graph, &settings.proposals, "proposalGraph")?;
    let view = write_view(r, &main, &logical, &branch);
    if let Some(v) = &view {
        for g in [&source_graph, &proposal_graph] {
            let t = Term::NamedNode(g.clone());
            if !v.writable(Some(&t)) {
                return Err(failed(
                    "forbidden",
                    403,
                    format!(
                        "write access to graph <{}> of dataset {} required",
                        g.as_str(),
                        main.name
                    ),
                ));
            }
        }
    }
    Ok(Target {
        main,
        logical,
        branch,
        source_graph,
        proposal_graph,
    })
}

/// The caller's write view of the update endpoint, `None` for every graph.
fn write_view(
    r: &Run,
    main: &Arc<crate::state::Dataset>,
    logical: &Arc<crate::state::Dataset>,
    branch: &str,
) -> Option<Arc<sparkles::access::GraphAccess>> {
    let st = &r.ctx.server.state;
    let caller = crate::http::caller::CallerContext::for_caller(st, logical, r.ctx.principal);
    caller
        .options_on(
            &crate::http::Params::empty(),
            &on_branch(&main.name, branch),
            Endpoint::Update,
        )
        .graphs
}

/// The registration planned at the branch head: its result, its ordinary changes and
/// the head it read.
struct Planned {
    registered: Value,
    changes: OrdinaryChanges,
    head: u64,
}

fn plan(r: &Run, reg: &Map<String, Value>, t: &Target) -> Result<Planned, Failed> {
    let mut args = reg.clone();
    args.insert("plan".into(), true.into());
    let branch = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let registered = r.tool("register_converted", Value::Object(args), branch)?;
    let head = registered["head"]
        .as_u64()
        .ok_or_else(|| failed("error", 500, "the registration plan has no head"))?;
    let changes = match registered["update"].as_str() {
        Some(u) => ordinary(u)?,
        None => OrdinaryChanges::default(),
    };
    // the source graph must be ordinary: no profile owns it
    let graph = expand_iri(
        registered["graph"].as_str().unwrap_or(""),
        &registered["prefixes"],
    );
    if let Ok(Some(snapshot)) = t.logical.store.managed_snapshot()
        && snapshot.graphs.owner_of(&graph).is_some()
    {
        return Err(failed(
            "profile-managed-graph",
            409,
            format!("graph <{graph}> is managed by a profile; name an ordinary source graph"),
        ));
    }
    let view = write_view(r, &t.main, &t.logical, &t.branch);
    if let (Some(v), Ok(g)) = (&view, NamedNode::new(graph.clone()))
        && !v.writable(Some(&Term::NamedNode(g)))
    {
        return Err(failed(
            "forbidden",
            403,
            format!(
                "write access to graph <{graph}> of dataset {} required",
                t.main.name
            ),
        ));
    }
    Ok(Planned {
        registered,
        changes,
        head,
    })
}

/// The quads of a registration's `DELETE DATA` and `INSERT DATA`.
fn ordinary(update: &str) -> Result<OrdinaryChanges, Failed> {
    use spargebra::GraphUpdateOperation as Op;
    use spargebra::term::GraphName as G;
    let parsed = spargebra::SparqlParser::new()
        .parse_update(update)
        .map_err(|e| failed("error", 500, format!("the registration plan: {e}")))?;
    let graph = |g: G| match g {
        G::NamedNode(n) => GraphName::NamedNode(n),
        G::DefaultGraph => GraphName::DefaultGraph,
    };
    let mut out = OrdinaryChanges::default();
    for op in parsed.operations {
        match op {
            Op::InsertData { data } => out.insert.extend(
                data.into_iter()
                    .map(|q| Quad::new(q.subject, q.predicate, q.object, graph(q.graph_name))),
            ),
            Op::DeleteData { data } => out.delete.extend(data.into_iter().map(|q| {
                Quad::new(
                    q.subject,
                    q.predicate,
                    Term::from(q.object),
                    graph(q.graph_name),
                )
            })),
            _ => return Err(failed("error", 500, "the registration plan is not data")),
        }
    }
    Ok(out)
}

/// The idempotency key of a claims ingestion (spec C25, "Idempotent re-ingestion"): a
/// versioned digest of a canonical framing of its source, its graphs, its content and
/// its profile, so that the same text under another source or graph is another
/// ingestion.
fn idempotency_key(
    t: &Target,
    source: &str,
    graph: &str,
    revision: &Sha256Digest,
    rendition: &Sha256Digest,
    profile: &str,
) -> Result<String, Failed> {
    let framed = json!([
        "ingest-claims",
        1,
        source,
        graph,
        t.source_graph.as_str(),
        t.proposal_graph.as_str(),
        revision.to_string(),
        rendition.to_string(),
        profile,
    ])
    .to_string();
    Ok(format!("ingest-claims:1:{}", sha(framed.as_bytes())?))
}

/// The source key of the revision: the source IRI framed with the C18 graph and the
/// evidence graphs, so that the same source and text ingested into other graphs gets
/// other record identities instead of colliding with the first ones.
fn source_key(t: &Target, source: &str, graph: &str) -> Result<String, Failed> {
    let framed = json!([
        "ingest-claims-source",
        1,
        source,
        graph,
        t.source_graph.as_str(),
        t.proposal_graph.as_str(),
    ])
    .to_string();
    Ok(format!(
        "ingest-claims-source:1:{}",
        sha(framed.as_bytes())?
    ))
}

/// Parse a concrete SHA-256 content digest observed in a C18 source record, returning
/// `None` when the digest is absent or unparseable.
fn observed_digest(v: &Value) -> Option<Sha256Digest> {
    v.as_str().and_then(|s| s.parse::<Sha256Digest>().ok())
}

/// The revision digest: the content digest that the C18 registration records, which is
/// the normalized text's unless the registration retained other original bytes.
fn recorded_digest(v: &Value, text: &Sha256Digest) -> Sha256Digest {
    observed_digest(v).unwrap_or(*text)
}

/// Whether the caller's operation of `key` is recorded in the evidence graphs of the
/// selected branch.
fn already(t: &Target, principal: &NamedNode, key: &str) -> Result<Option<NamedNode>, Failed> {
    let id = sparkles::evidence::operation_id(t.main.dataset.dataset_id(), principal, key);
    let op = sparkles::evidence::operation_iri(id);
    let q = format!(
        "ASK {{ VALUES ?g {{ <{}> <{}> }} GRAPH ?g {{ <{}> a <urn:x-sparkles:evidence:Operation> }} }}",
        t.source_graph.as_str(),
        t.proposal_graph.as_str(),
        op.as_str()
    );
    let found = t.logical.dataset.ask(&q).map_err(|e| api(e.into()))?;
    Ok(found.then_some(op))
}

/// Map an engine/resolution failure during predecessor lookup to the task error contract.
fn resolve_err(e: sparkles::Error) -> Failed {
    match e {
        sparkles::Error::Cancelled => Failed::new("cancelled", "the ingestion was cancelled"),
        sparkles::Error::Timeout => Failed::new("timeout", "the ingestion's deadline passed"),
        sparkles::Error::BudgetExceeded(_) | sparkles::Error::Unsupported(_) => failed(
            "incomplete-evidence",
            422,
            "source revision predecessor lookup could not be verified within its resolution limit",
        ),
        other => api(other.into()),
    }
}

fn incomplete_lineage() -> Failed {
    failed(
        "incomplete-evidence",
        422,
        "source revision predecessor lookup exceeded its resolution limit",
    )
}

enum Verify {
    Verified(
        Box<sparkles::evidence::resolve::VerifiedRecord>,
        sparkles::access::Reservation,
    ),
    Absent,
}

/// Copy a validated [`RecordKey`] from `iri` into a preparation-charged reservation.
pub(super) fn charge_record_key(
    prep: &sparkles::access::Preparation,
    iri: &str,
) -> Result<Option<(RecordKey, sparkles::access::Reservation)>, Failed> {
    if RecordKey::validate(iri).is_err() {
        return Ok(None);
    }
    let mut held = prep.reserve(0).map_err(resolve_err)?;
    let key = RecordKey::new(prep.copy_string(iri, &mut held).map_err(resolve_err)?)
        .expect("validated record key");
    Ok(Some((key, held)))
}

/// Copy a [`NamedNode`] IRI into a preparation-charged reservation.
pub(super) fn charge_named_node(
    prep: &sparkles::access::Preparation,
    iri: &str,
) -> Result<(NamedNode, sparkles::access::Reservation), Failed> {
    let mut held = prep.reserve(0).map_err(resolve_err)?;
    let node = NamedNode::new_unchecked(prep.copy_string(iri, &mut held).map_err(resolve_err)?);
    Ok((node, held))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedDigest {
    /// Source has exactly one visible `spk:contentDigest` in `c18_graph` and it parses
    /// as a valid SHA-256 digest.
    Unique(Sha256Digest),
    /// Source has no visible `spk:contentDigest`, or a single unparseable value.
    MissingOrInvalid,
    /// Source has multiple visible `spk:contentDigest` objects in `c18_graph`.
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedSource {
    /// Source is live in `c18_graph` at `at_commit` and still carries
    /// `expected_rendition` as a visible `spk:rendition`. `rendition_unique` is true
    /// only when `expected_rendition` is the sole visible `spk:rendition` object.
    /// `digest` is observed independently so an extra `spk:rendition` triple at a
    /// stable head never discards a unique recorded original-byte digest.
    Live {
        rendition_unique: bool,
        digest: ObservedDigest,
    },
    /// Source is missing, unreadable, invalidated, or does not carry
    /// `expected_rendition` at `at_commit`.
    UnavailableOrChanged,
}

/// Inspect a registered source's liveness, exact rendition, and digest cardinality at
/// `at_commit` under the caller's `Endpoint::Query` policy without row truncation or
/// collapsing multiple `spk:contentDigest` values.
fn observe_source_at_commit(
    r: &Run,
    t: &Target,
    source: &NamedNode,
    c18_graph: &str,
    expected_rendition: &str,
    at_commit: u64,
) -> Result<ObservedSource, Failed> {
    use sparkles::evidence::resolve::SingleObject;

    r.check()?;
    let Some(access) = crate::evidence::policy(
        &r.ctx.server.state,
        r.ctx.principal,
        &t.main.dataset,
        &t.branch,
    ) else {
        return Ok(ObservedSource::UnavailableOrChanged);
    };
    let (Ok(c18_g), Ok(expected_rend_node)) = (
        NamedNode::new(c18_graph),
        NamedNode::new(expected_rendition),
    ) else {
        return Ok(ObservedSource::UnavailableOrChanged);
    };
    let limits = &r.ctx.server.state.limits;
    let mem_limit = limits
        .query_memory_bytes
        .map_or(sparkles::evidence::resolve::MEMORY_BYTES, |m| {
            m.min(sparkles::evidence::resolve::MEMORY_BYTES)
        });
    let mut resolution =
        sparkles::evidence::resolve::Resolution::with_memory(control_of(r), mem_limit)
            .map_err(resolve_err)?;
    resolution.pin_commit(&t.main.dataset, &t.logical.dataset, at_commit);

    let prov_invalidated = NamedNode::new_unchecked(PROV_INVALIDATED_AT_TIME);
    let spk_rendition = NamedNode::new_unchecked(SPK_RENDITION);
    let spk_content_digest = NamedNode::new_unchecked(SPK_CONTENT_DIGEST);

    let is_unavailable = |e: &sparkles::Error| {
        matches!(
            e,
            sparkles::Error::Managed(m)
                if m.kind == sparkles::managed::ManagedErrorKind::EvidenceUnavailable
        )
    };
    let invalidated = match resolution.has_any_object_in_graph(
        "source",
        &t.main.dataset,
        &t.logical.dataset,
        &access,
        &c18_g,
        source,
        &prov_invalidated,
    ) {
        Ok(v) => v,
        Err(e) if is_unavailable(&e) => return Ok(ObservedSource::UnavailableOrChanged),
        Err(e) => return Err(resolve_err(e)),
    };
    if invalidated {
        return Ok(ObservedSource::UnavailableOrChanged);
    }
    let rend_match = match resolution.single_object_in_graph(
        "source",
        &t.main.dataset,
        &t.logical.dataset,
        &access,
        &c18_g,
        source,
        &spk_rendition,
        |term| {
            Ok(matches!(
                term,
                Some(Term::NamedNode(n)) if n == &expected_rend_node
            ))
        },
    ) {
        Ok(v) => v,
        Err(e) if is_unavailable(&e) => return Ok(ObservedSource::UnavailableOrChanged),
        Err(e) => return Err(resolve_err(e)),
    };
    let rendition_unique = match rend_match {
        SingleObject::One(true) => true,
        SingleObject::Many => {
            let expected_present = match resolution.has_triple(
                "source",
                &t.main.dataset,
                &t.logical.dataset,
                &access,
                &c18_g,
                source,
                &spk_rendition,
                &Term::NamedNode(expected_rend_node),
            ) {
                Ok(v) => v,
                Err(e) if is_unavailable(&e) => return Ok(ObservedSource::UnavailableOrChanged),
                Err(e) => return Err(resolve_err(e)),
            };
            if !expected_present {
                return Ok(ObservedSource::UnavailableOrChanged);
            }
            false
        }
        SingleObject::None | SingleObject::One(false) => {
            return Ok(ObservedSource::UnavailableOrChanged);
        }
    };
    let digest = match resolution.single_object_in_graph(
        "source",
        &t.main.dataset,
        &t.logical.dataset,
        &access,
        &c18_g,
        source,
        &spk_content_digest,
        |term| {
            Ok(match term {
                Some(Term::Literal(l)) => l.value().parse::<Sha256Digest>().ok(),
                _ => None,
            })
        },
    ) {
        Ok(SingleObject::One(Some(d))) => ObservedDigest::Unique(d),
        Ok(SingleObject::None | SingleObject::One(None)) => ObservedDigest::MissingOrInvalid,
        Ok(SingleObject::Many) => ObservedDigest::Ambiguous,
        Err(e) if is_unavailable(&e) => return Ok(ObservedSource::UnavailableOrChanged),
        Err(e) => return Err(resolve_err(e)),
    };
    Ok(ObservedSource::Live {
        rendition_unique,
        digest,
    })
}

/// Authenticate a record under the caller's query policy, mapping absence and refusal
/// outcomes to [`Verify::Absent`] while propagating cancellation, timeout, budget and
/// storage failures. Keeps the verified record's variable-sized fields charged until the
/// returned reservation is dropped.
fn verify_record(
    res: &mut sparkles::evidence::resolve::Resolution,
    main: &sparkles::Dataset,
    logical: &sparkles::Dataset,
    access: &Arc<sparkles::access::GraphAccess>,
    iri: &NamedNode,
    kind: RecordKind,
) -> Result<Verify, Failed> {
    match res.record_charged(
        "predecessor",
        main,
        logical,
        Some(access.clone()),
        iri,
        &[kind],
        None,
    ) {
        Ok((v, held)) => Ok(Verify::Verified(Box::new(v), held)),
        Err(sparkles::Error::Managed(m))
            if matches!(
                m.kind,
                sparkles::managed::ManagedErrorKind::EvidenceUnavailable
                    | sparkles::managed::ManagedErrorKind::OriginNotReproduced
                    | sparkles::managed::ManagedErrorKind::ImmutableRecordConflict
            ) =>
        {
            Ok(Verify::Absent)
        }
        Err(e) => Err(resolve_err(e)),
    }
}

/// Result of resolving an ingestion's source revision against existing evidence records
/// and C18 lineage under the caller's query policy.
#[derive(Clone)]
enum RevisionLink {
    /// This exact `SourceRevision` IRI already exists as an authenticated record in
    /// `t.source_graph`. Reference it via `RecordRef::Existing` so re-ingesting under
    /// another profile preserves its immutable content without disclosing hidden edges.
    Existing {
        iri: NamedNode,
        previous: Option<NamedNode>,
    },
    /// This `SourceRevision` IRI is new and should be minted with `was_revision_of` set
    /// to `previous`.
    New { previous: Option<NamedNode> },
}

impl RevisionLink {
    fn previous(&self) -> Option<&NamedNode> {
        match self {
            Self::Existing { previous, .. } | Self::New { previous } => previous.as_ref(),
        }
    }
}

pub(super) struct LineageCandidate {
    pub(super) rev: NamedNode,
    pub(super) rev_key: RecordKey,
    pub(super) ev_rend: NamedNode,
    pub(super) rend_receipt: NamedNode,
    pub(super) rend_who: NamedNode,
}

#[derive(Clone)]
pub(super) struct MatchedRev {
    c18_rendition: NamedNode,
    iri: NamedNode,
    is_other_principal: bool,
    branch: uuid::Uuid,
    commit: u64,
}

/// Match a C18 rendition against enumerated candidates, charging rendition-key and
/// principal copies to the preparation allowance and authenticating both the evidence
/// Rendition and SourceRevision in `source_graph`.
#[allow(clippy::too_many_arguments)]
pub(super) fn match_rendition(
    resolution: &mut sparkles::evidence::resolve::Resolution,
    held: &mut sparkles::access::Reservation,
    remaining_scans: &mut u64,
    main: &sparkles::Dataset,
    logical: &sparkles::Dataset,
    source_graph: &NamedNode,
    access: &Arc<sparkles::access::GraphAccess>,
    principal: &NamedNode,
    dataset_id: uuid::Uuid,
    control: &Control,
    prov_was_associated_with: &NamedNode,
    candidates: &[LineageCandidate],
    rend_iri: &NamedNode,
    exclude_digest: Option<Sha256Digest>,
) -> Result<Vec<MatchedRev>, Failed> {
    let Some((rend_key, _rend_held)) =
        charge_record_key(resolution.preparation(), rend_iri.as_str())?
    else {
        return Ok(Vec::new());
    };
    let mut matched = Vec::new();
    for cand in candidates {
        resolution.preparation().check().map_err(resolve_err)?;
        let (who, who_held) = charge_named_node(resolution.preparation(), cand.rend_who.as_str())?;
        let expected = IdentityNamespace::new(dataset_id, who)
            .map_err(resolve_err)?
            .record_id(RecordKind::Rendition, &cand.rev_key, &rend_key, control)
            .map_err(resolve_err)?;
        drop(who_held);
        if expected != cand.ev_rend {
            continue;
        }
        let Verify::Verified(v_rend, v_rend_held) = verify_record(
            resolution,
            main,
            logical,
            access,
            &cand.ev_rend,
            RecordKind::Rendition,
        )?
        else {
            continue;
        };
        let rend_ok = v_rend.graph == *source_graph && v_rend.receipt.iri == cand.rend_receipt;
        drop(v_rend);
        drop(v_rend_held);
        if !rend_ok {
            continue;
        }
        let Verify::Verified(v_rev, v_rev_held) = verify_record(
            resolution,
            main,
            logical,
            access,
            &cand.rev,
            RecordKind::SourceRevision,
        )?
        else {
            continue;
        };
        if v_rev.graph != *source_graph
            || (exclude_digest.is_some() && v_rev.content_digest == exclude_digest)
        {
            drop(v_rev);
            drop(v_rev_held);
            continue;
        }
        let whos = resolution
            .objects_in_graph(
                "predecessor",
                main,
                logical,
                access,
                source_graph,
                &v_rev.receipt.iri,
                prov_was_associated_with,
                2,
                remaining_scans,
            )
            .map_err(resolve_err)?;
        if whos.is_truncated() {
            return Err(incomplete_lineage());
        }
        let is_other_principal = !matches!(whos.nodes(), [w] if w == principal);
        drop(whos);
        let v_rev_branch = v_rev.receipt.branch;
        let v_rev_commit = v_rev.receipt.commit;
        drop(v_rev);
        drop(v_rev_held);
        let rend_copy = resolution
            .preparation()
            .copy_string(rend_iri.as_str(), held)
            .map_err(resolve_err)?;
        let rev_copy = resolution
            .preparation()
            .copy_string(cand.rev.as_str(), held)
            .map_err(resolve_err)?;
        held.grow_by(std::mem::size_of::<MatchedRev>() as u64)
            .map_err(resolve_err)?;
        matched
            .try_reserve(1)
            .map_err(|_| resolve_err(resolution.preparation().exhausted()))?;
        matched.push(MatchedRev {
            c18_rendition: NamedNode::new_unchecked(rend_copy),
            iri: NamedNode::new_unchecked(rev_copy),
            is_other_principal,
            branch: v_rev_branch,
            commit: v_rev_commit,
        });
    }
    Ok(matched)
}

/// Choose one predecessor from authenticated matches on a single C18 rendition.
fn pick_on_rendition(mut hits: Vec<MatchedRev>) -> Option<NamedNode> {
    if hits.is_empty() {
        return None;
    }
    hits.sort_by(|a, b| {
        (a.is_other_principal, std::cmp::Reverse(a.commit), &a.iri).cmp(&(
            b.is_other_principal,
            std::cmp::Reverse(b.commit),
            &b.iri,
        ))
    });
    Some(hits.remove(0).iri)
}

/// Reduce frontier matches using authenticated evidence `prov:wasRevisionOf` ancestry
/// and branch commit chronology, returning `None` when distinct C18 renditions remain
/// genuinely ambiguous.
#[allow(clippy::too_many_arguments)]
fn select_from_frontier(
    resolution: &mut sparkles::evidence::resolve::Resolution,
    t: &Target,
    access: &Arc<sparkles::access::GraphAccess>,
    prov_was_revision_of: &NamedNode,
    mut hits: Vec<MatchedRev>,
    remaining_scans: &mut u64,
) -> Result<Option<NamedNode>, Failed> {
    if hits.is_empty() {
        return Ok(None);
    }
    let mut work_held = resolution.preparation().reserve(0).map_err(resolve_err)?;
    // Deduplicate identical revision IRIs using borrowed slices under work_held.
    work_held
        .grow_by((hits.len() as u64).saturating_mul(64))
        .map_err(resolve_err)?;
    let keep: Vec<bool> = {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        hits.iter().map(|m| seen.insert(m.iri.as_str())).collect()
    };
    let mut keep_iter = keep.into_iter();
    hits.retain(|_| keep_iter.next().unwrap_or(false));
    if hits.len() == 1 {
        return Ok(Some(hits.remove(0).iri));
    }
    // Discard any match that is an ancestor of another match via authenticated
    // prov:wasRevisionOf chains across configured evidence graphs (bounded walk).
    work_held
        .grow_by((hits.len() as u64).saturating_mul(64))
        .map_err(resolve_err)?;
    let match_iris: BTreeSet<&str> = hits.iter().map(|m| m.iri.as_str()).collect();
    let mut superseded: BTreeSet<&str> = BTreeSet::new();
    for m in &hits {
        let mut chain_held = resolution.preparation().reserve(0).map_err(resolve_err)?;
        let mut cur_iri = NamedNode::new_unchecked(
            resolution
                .preparation()
                .copy_string(m.iri.as_str(), &mut chain_held)
                .map_err(resolve_err)?,
        );
        let mut cur_graph = NamedNode::new_unchecked(
            resolution
                .preparation()
                .copy_string(t.source_graph.as_str(), &mut chain_held)
                .map_err(resolve_err)?,
        );
        let mut chain_seen: BTreeSet<String> = BTreeSet::new();
        let start_key = resolution
            .preparation()
            .copy_string(cur_iri.as_str(), &mut chain_held)
            .map_err(resolve_err)?;
        chain_held.grow_by(64).map_err(resolve_err)?;
        chain_seen.insert(start_key);
        for _ in 0..MAX_LINEAGE_HOPS {
            resolution.preparation().check().map_err(resolve_err)?;
            let objs = resolution
                .objects_in_graph(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    access,
                    &cur_graph,
                    &cur_iri,
                    prov_was_revision_of,
                    2,
                    remaining_scans,
                )
                .map_err(resolve_err)?;
            if objs.is_truncated() {
                return Err(incomplete_lineage());
            }
            let [next] = objs.nodes() else {
                break;
            };
            if *next == cur_iri {
                break;
            }
            if let Some(&hit_iri) = match_iris.get(next.as_str()) {
                if hit_iri != m.iri.as_str() {
                    superseded.insert(hit_iri);
                }
                if chain_seen.contains(next.as_str()) {
                    break;
                }
                let next_seen = resolution
                    .preparation()
                    .copy_string(next.as_str(), &mut chain_held)
                    .map_err(resolve_err)?;
                let next_iri = NamedNode::new_unchecked(
                    resolution
                        .preparation()
                        .copy_string(next.as_str(), &mut chain_held)
                        .map_err(resolve_err)?,
                );
                let next_graph = NamedNode::new_unchecked(
                    resolution
                        .preparation()
                        .copy_string(t.source_graph.as_str(), &mut chain_held)
                        .map_err(resolve_err)?,
                );
                chain_held.grow_by(64).map_err(resolve_err)?;
                drop(objs);
                chain_seen.insert(next_seen);
                cur_iri = next_iri;
                cur_graph = next_graph;
            } else {
                if chain_seen.contains(next.as_str()) {
                    break;
                }
                let next_iri = NamedNode::new_unchecked(
                    resolution
                        .preparation()
                        .copy_string(next.as_str(), &mut chain_held)
                        .map_err(resolve_err)?,
                );
                drop(objs);
                let Verify::Verified(v_next, v_next_held) = verify_record(
                    resolution,
                    &t.main.dataset,
                    &t.logical.dataset,
                    access,
                    &next_iri,
                    RecordKind::SourceRevision,
                )?
                else {
                    break;
                };
                let next_seen = resolution
                    .preparation()
                    .copy_string(next_iri.as_str(), &mut chain_held)
                    .map_err(resolve_err)?;
                let next_graph = NamedNode::new_unchecked(
                    resolution
                        .preparation()
                        .copy_string(v_next.graph.as_str(), &mut chain_held)
                        .map_err(resolve_err)?,
                );
                drop(v_next);
                drop(v_next_held);
                chain_held.grow_by(64).map_err(resolve_err)?;
                chain_seen.insert(next_seen);
                cur_iri = next_iri;
                cur_graph = next_graph;
            }
        }
    }
    if superseded.len() < hits.len() {
        let keep: Vec<bool> = hits
            .iter()
            .map(|m| !superseded.contains(m.iri.as_str()))
            .collect();
        drop(superseded);
        drop(match_iris);
        let mut iter = keep.into_iter();
        hits.retain(|_| iter.next().unwrap_or(false));
    } else {
        drop(superseded);
        drop(match_iris);
    }
    let distinct_renditions = |list: &[MatchedRev]| -> usize {
        list.iter()
            .map(|m| m.c18_rendition.as_str())
            .collect::<BTreeSet<_>>()
            .len()
    };
    if distinct_renditions(&hits) == 1 {
        return Ok(pick_on_rendition(hits));
    }
    // Recover chronology from branch commit order when all surviving matches were
    // published on the same branch.
    let first_branch = hits[0].branch;
    if hits.iter().all(|m| m.branch == first_branch)
        && let Some(max_commit) = hits.iter().map(|m| m.commit).max()
    {
        hits.retain(|m| m.commit == max_commit);
    }
    if distinct_renditions(&hits) == 1 {
        return Ok(pick_on_rendition(hits));
    }
    // Multiple distinct C18 renditions survived with no provable strict chronological
    // order: omit prov:wasRevisionOf rather than guessing an immutable edge.
    Ok(None)
}

enum C18Predecessors {
    List(Vec<NamedNode>, sparkles::access::Reservation),
    Ambiguous,
}

/// Whether `access` carries a triple-level read protection with a pattern that could
/// selectively hide a competing `prov:wasDerivedFrom` membership in `graph` while
/// leaving the requested source's own membership visible.
///
/// Rules without a `pattern` depend only on `(subject, predicate, graph)`, so for any
/// given rendition they hide either all of its `prov:wasDerivedFrom` triples in `graph`
/// (already rejected by membership checks) or none of them.
fn may_hide_competing_membership(
    access: &sparkles::access::GraphAccess,
    graph: &NamedNode,
) -> bool {
    let Some(rules) = &access.triples else {
        return false;
    };
    let g_term = Term::NamedNode(graph.clone());
    rules.rules.iter().any(|rule| {
        !rule.read.is_all()
            && !rule.read.allows(Some(&g_term))
            && rule.protection.pattern.is_some()
            && rule.protection.covers_predicate(PROV_WAS_DERIVED_FROM)
            && rule
                .protection
                .graphs
                .as_ref()
                .is_none_or(|gs| gs.may_allow(Some(&g_term)))
    })
}

/// Resolve the source revision and predecessor link for a claims ingestion of `source` in
/// `c18_graph` at the exact branch commit `at_commit`, bounded by the server's query
/// memory and row limits, task deadline, and cancellation, and strictly enforced under
/// the caller's `Endpoint::Query` policy.
///
/// - When the revision IRI this batch would mint already exists as an authenticated record
///   in `t.source_graph`, returns [`RevisionLink::Existing`] so multi-profile
///   re-ingestion preserves the immutable revision without disclosing masked edges.
/// - Otherwise verifies the C18 relationship under the caller's query policy at
///   `at_commit`, combines same-rendition prior byte variants with source-attributable
///   C18 `prov:wasRevisionOf` predecessors across up to 64 visited renditions, and
///   omits `prov:wasRevisionOf` whenever lineage is ambiguous, includes an
///   unattributable shared transition, or would require choosing from a partial frontier.
#[allow(clippy::too_many_arguments)]
fn predecessor_revision(
    r: &Run,
    t: &Target,
    principal: &NamedNode,
    source: &NamedNode,
    source_key: &str,
    revision_digest: Sha256Digest,
    c18_graph: &str,
    cur_rendition: &str,
    planned_prev_rendition: Option<&str>,
    at_commit: u64,
) -> Result<RevisionLink, Failed> {
    r.check()?;
    let Some(access) = crate::evidence::policy(
        &r.ctx.server.state,
        r.ctx.principal,
        &t.main.dataset,
        &t.branch,
    ) else {
        return Ok(RevisionLink::New { previous: None });
    };
    let limits = &r.ctx.server.state.limits;
    let mem_limit = limits
        .query_memory_bytes
        .map_or(sparkles::evidence::resolve::MEMORY_BYTES, |m| {
            m.min(sparkles::evidence::resolve::MEMORY_BYTES)
        });
    let candidate_limit = limits.max_rows.min(MAX_LINEAGE_CANDIDATES);
    if candidate_limit == 0 {
        return Err(incomplete_lineage());
    }
    let mut remaining_scans = limits
        .max_rows_produced
        .unwrap_or(DEFAULT_LINEAGE_MAX_SCANS);
    let dataset_id = t.main.dataset.dataset_id();
    let control = control_of(r);
    let ns = IdentityNamespace::new(dataset_id, principal.clone()).map_err(resolve_err)?;
    let new_rev_iri = ns
        .record_id(
            RecordKind::SourceRevision,
            &key(source_key),
            &key(revision_digest.to_string()),
            &control,
        )
        .map_err(resolve_err)?;
    let mut resolution =
        sparkles::evidence::resolve::Resolution::with_memory(control.clone(), mem_limit)
            .map_err(resolve_err)?;
    resolution.pin_commit(&t.main.dataset, &t.logical.dataset, at_commit);
    let mut held = resolution.preparation().reserve(0).map_err(resolve_err)?;

    let prov_was_revision_of = NamedNode::new_unchecked(PROV_WAS_REVISION_OF);
    // 1. If this exact SourceRevision already exists as an authenticated record in
    //    t.source_graph, reference it directly via RecordRef::Existing so re-ingestion
    //    under another profile preserves its immutable record even if its predecessor
    //    edge or target is masked from the caller.
    if let Verify::Verified(v, v_held) = verify_record(
        &mut resolution,
        &t.main.dataset,
        &t.logical.dataset,
        &access,
        &new_rev_iri,
        RecordKind::SourceRevision,
    )? {
        let in_source_graph = v.graph == t.source_graph;
        drop(v);
        drop(v_held);
        if in_source_graph {
            let objs = resolution
                .objects_in_graph(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &t.source_graph,
                    &new_rev_iri,
                    &prov_was_revision_of,
                    2,
                    &mut remaining_scans,
                )
                .map_err(resolve_err)?;
            if objs.is_truncated() {
                return Err(incomplete_lineage());
            }
            let previous = if let [prev] = objs.nodes()
                && *prev != new_rev_iri
                && matches!(
                    verify_record(
                        &mut resolution,
                        &t.main.dataset,
                        &t.logical.dataset,
                        &access,
                        prev,
                        RecordKind::SourceRevision
                    )?,
                    Verify::Verified(_, _)
                ) {
                let prev_copy = resolution
                    .preparation()
                    .copy_string(prev.as_str(), &mut held)
                    .map_err(resolve_err)?;
                Some(NamedNode::new_unchecked(prev_copy))
            } else {
                None
            };
            return Ok(RevisionLink::Existing {
                iri: new_rev_iri,
                previous,
            });
        }
    }

    // 2. Verify C18 visibility under the caller's Endpoint::Query policy at at_commit.
    let (Ok(c18_g), Ok(cur_rend)) = (NamedNode::new(c18_graph), NamedNode::new(cur_rendition))
    else {
        return Ok(RevisionLink::New { previous: None });
    };
    let spk_rendition = NamedNode::new_unchecked(SPK_RENDITION);
    let immediate_prev: Option<NamedNode> = match planned_prev_rendition {
        Some(prev_str) => {
            let Ok(prev_node) = NamedNode::new(prev_str) else {
                return Ok(RevisionLink::New { previous: None });
            };
            let visible = resolution
                .has_triple(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &c18_g,
                    source,
                    &spk_rendition,
                    &Term::NamedNode(prev_node.clone()),
                )
                .map_err(resolve_err)?;
            if !visible {
                return Ok(RevisionLink::New { previous: None });
            }
            Some(prev_node)
        }
        None => {
            let visible = resolution
                .has_triple(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &c18_g,
                    source,
                    &spk_rendition,
                    &Term::NamedNode(cur_rend.clone()),
                )
                .map_err(resolve_err)?;
            if !visible {
                return Ok(RevisionLink::New { previous: None });
            }
            None
        }
    };

    // 3. Enumerate bounded, memory-charged candidate (SourceRevision, Rendition) pairs
    //    in t.source_graph for this (source, source_key).
    let ev_source_key = NamedNode::new_unchecked(EV_SOURCE_KEY);
    let sk_term = Term::Literal(Literal::new_simple_literal(source_key));
    let rev_nodes = resolution
        .subjects_in_graph(
            "predecessor",
            &t.main.dataset,
            &t.logical.dataset,
            &access,
            &t.source_graph,
            &ev_source_key,
            &sk_term,
            candidate_limit,
            &mut remaining_scans,
        )
        .map_err(resolve_err)?;
    if rev_nodes.is_truncated() {
        return Err(incomplete_lineage());
    }
    if rev_nodes.is_empty() {
        return Ok(RevisionLink::New { previous: None });
    }
    let rdf_type = rdf::TYPE.into_owned();
    let ev_source_revision_term = Term::NamedNode(NamedNode::new_unchecked(EV_SOURCE_REVISION));
    let ev_rendition_term = Term::NamedNode(NamedNode::new_unchecked(EV_RENDITION));
    let prov_specialization_of = NamedNode::new_unchecked(PROV_SPECIALIZATION_OF);
    let prov_was_derived_from = NamedNode::new_unchecked(PROV_WAS_DERIVED_FROM);
    let ev_availability_receipt = NamedNode::new_unchecked(EV_AVAILABILITY_RECEIPT);
    let prov_was_associated_with = NamedNode::new_unchecked(PROV_WAS_ASSOCIATED_WITH);
    let source_term = Term::NamedNode(source.clone());

    let mut candidates: Vec<LineageCandidate> = Vec::new();
    for rev in rev_nodes.iter() {
        resolution.preparation().check().map_err(resolve_err)?;
        #[cfg(test)]
        t.main.store.failpoint("ingest-claims-candidate-enum");
        resolution.preparation().check().map_err(resolve_err)?;
        if *rev == new_rev_iri {
            continue;
        }
        let is_rev = resolution
            .has_triple(
                "predecessor",
                &t.main.dataset,
                &t.logical.dataset,
                &access,
                &t.source_graph,
                rev,
                &rdf_type,
                &ev_source_revision_term,
            )
            .map_err(resolve_err)?;
        if !is_rev {
            continue;
        }
        let matches_source = resolution
            .has_triple(
                "predecessor",
                &t.main.dataset,
                &t.logical.dataset,
                &access,
                &t.source_graph,
                rev,
                &prov_specialization_of,
                &source_term,
            )
            .map_err(resolve_err)?;
        if !matches_source {
            continue;
        }
        if RecordKey::validate(rev.as_str()).is_err() {
            continue;
        }
        let (rev_node, rev_term_held) = charge_named_node(resolution.preparation(), rev.as_str())?;
        let rev_term = Term::NamedNode(rev_node);
        let ev_rends = resolution
            .subjects_in_graph(
                "predecessor",
                &t.main.dataset,
                &t.logical.dataset,
                &access,
                &t.source_graph,
                &prov_was_derived_from,
                &rev_term,
                candidate_limit,
                &mut remaining_scans,
            )
            .map_err(resolve_err)?;
        drop(rev_term);
        drop(rev_term_held);
        if ev_rends.is_truncated() {
            return Err(incomplete_lineage());
        }
        for ev_rend in ev_rends.iter() {
            resolution.preparation().check().map_err(resolve_err)?;
            let is_rend = resolution
                .has_triple(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &t.source_graph,
                    ev_rend,
                    &rdf_type,
                    &ev_rendition_term,
                )
                .map_err(resolve_err)?;
            if !is_rend {
                continue;
            }
            let ops = resolution
                .objects_in_graph(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &t.source_graph,
                    ev_rend,
                    &ev_availability_receipt,
                    2,
                    &mut remaining_scans,
                )
                .map_err(resolve_err)?;
            if ops.is_truncated() {
                return Err(incomplete_lineage());
            }
            let [rend_receipt] = ops.nodes() else {
                continue;
            };
            let whos = resolution
                .objects_in_graph(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &t.source_graph,
                    rend_receipt,
                    &prov_was_associated_with,
                    2,
                    &mut remaining_scans,
                )
                .map_err(resolve_err)?;
            if whos.is_truncated() {
                return Err(incomplete_lineage());
            }
            let [rend_who] = whos.nodes() else {
                continue;
            };
            if candidates.len() >= candidate_limit {
                return Err(incomplete_lineage());
            }
            // Reserve ownership for every retained copy before allocating.
            let rev_s = resolution
                .preparation()
                .copy_string(rev.as_str(), &mut held)
                .map_err(resolve_err)?;
            let rev_key_s = resolution
                .preparation()
                .copy_string(rev.as_str(), &mut held)
                .map_err(resolve_err)?;
            let ev_rend_s = resolution
                .preparation()
                .copy_string(ev_rend.as_str(), &mut held)
                .map_err(resolve_err)?;
            let rend_receipt_s = resolution
                .preparation()
                .copy_string(rend_receipt.as_str(), &mut held)
                .map_err(resolve_err)?;
            let rend_who_s = resolution
                .preparation()
                .copy_string(rend_who.as_str(), &mut held)
                .map_err(resolve_err)?;
            held.grow_by(std::mem::size_of::<LineageCandidate>() as u64)
                .map_err(resolve_err)?;
            candidates
                .try_reserve(1)
                .map_err(|_| resolve_err(resolution.preparation().exhausted()))?;
            candidates.push(LineageCandidate {
                rev: NamedNode::new_unchecked(rev_s),
                rev_key: RecordKey::new(rev_key_s).expect("validated non-empty IRI"),
                ev_rend: NamedNode::new_unchecked(ev_rend_s),
                rend_receipt: NamedNode::new_unchecked(rend_receipt_s),
                rend_who: NamedNode::new_unchecked(rend_who_s),
            });
        }
    }
    drop(rev_nodes);
    if candidates.is_empty() {
        return Ok(RevisionLink::New { previous: None });
    }

    // Match a C18 rendition against candidates, authenticating BOTH the evidence
    // Rendition and SourceRevision in t.source_graph.
    let match_rendition = |resolution: &mut sparkles::evidence::resolve::Resolution,
                           held: &mut sparkles::access::Reservation,
                           remaining_scans: &mut u64,
                           rend_iri: &NamedNode,
                           exclude_digest: Option<Sha256Digest>|
     -> Result<Vec<MatchedRev>, Failed> {
        #[cfg(test)]
        t.main.store.failpoint("ingest-claims-match-rendition");
        match_rendition(
            resolution,
            held,
            remaining_scans,
            &t.main.dataset,
            &t.logical.dataset,
            &t.source_graph,
            &access,
            principal,
            dataset_id,
            &control,
            &prov_was_associated_with,
            &candidates,
            rend_iri,
            exclude_digest,
        )
    };

    // Read all query-visible C18 prov:wasRevisionOf predecessors of `rend` that are
    // unambiguously attributable to `source` in `c18_g`. If a predecessor belongs to
    // `source` and shares another visible source with `rend`, or if a query protection
    // pattern can selectively hide competing `prov:wasDerivedFrom` memberships in
    // `c18_g`, C18's unqualified edge cannot prove `source` made the transition and is
    // treated as ambiguous.
    let edge_limit = limits.max_rows.min(MAX_C18_EDGES_PER_NODE);
    let may_hide_competing = may_hide_competing_membership(&access, &c18_g);
    let c18_predecessors = |resolution: &mut sparkles::evidence::resolve::Resolution,
                            remaining_scans: &mut u64,
                            rend: &NamedNode|
     -> Result<C18Predecessors, Failed> {
        let objs = resolution
            .objects_in_graph(
                "predecessor",
                &t.main.dataset,
                &t.logical.dataset,
                &access,
                &c18_g,
                rend,
                &prov_was_revision_of,
                edge_limit,
                remaining_scans,
            )
            .map_err(resolve_err)?;
        if objs.is_truncated() {
            return Err(incomplete_lineage());
        }
        if objs.is_empty() {
            let empty_held = resolution.preparation().reserve(0).map_err(resolve_err)?;
            return Ok(C18Predecessors::List(Vec::new(), empty_held));
        }
        let rend_sources = resolution
            .objects_in_graph(
                "predecessor",
                &t.main.dataset,
                &t.logical.dataset,
                &access,
                &c18_g,
                rend,
                &prov_was_derived_from,
                edge_limit,
                remaining_scans,
            )
            .map_err(resolve_err)?;
        if rend_sources.is_truncated() {
            return Err(incomplete_lineage());
        }
        let rend_has_source = rend_sources.iter().any(|s| s == source);
        let mut out_held = resolution.preparation().reserve(0).map_err(resolve_err)?;
        let mut out = Vec::new();
        for prev in objs.iter() {
            resolution.preparation().check().map_err(resolve_err)?;
            let prev_sources = resolution
                .objects_in_graph(
                    "predecessor",
                    &t.main.dataset,
                    &t.logical.dataset,
                    &access,
                    &c18_g,
                    prev,
                    &prov_was_derived_from,
                    edge_limit,
                    remaining_scans,
                )
                .map_err(resolve_err)?;
            if prev_sources.is_truncated() {
                return Err(incomplete_lineage());
            }
            if !prev_sources.iter().any(|s| s == source) {
                continue;
            }
            let shared_with_other = !rend_has_source
                || may_hide_competing
                || prev_sources
                    .iter()
                    .any(|ps| ps != source && rend_sources.iter().any(|rs| rs == ps));
            if shared_with_other {
                return Ok(C18Predecessors::Ambiguous);
            }
            let copy = resolution
                .preparation()
                .copy_string(prev.as_str(), &mut out_held)
                .map_err(resolve_err)?;
            out_held
                .grow_by(std::mem::size_of::<NamedNode>() as u64)
                .map_err(resolve_err)?;
            out.try_reserve(1)
                .map_err(|_| resolve_err(resolution.preparation().exhausted()))?;
            out.push(NamedNode::new_unchecked(copy));
        }
        Ok(C18Predecessors::List(out, out_held))
    };

    // 4. Check immediate predecessor (or combine same-rendition byte variants with C18
    //    predecessors), then walk source-attributable C18 lineage with bounded BFS.
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<NamedNode> = VecDeque::new();
    let mut frontier: Vec<MatchedRev> = Vec::new();
    let cur_seen = resolution
        .preparation()
        .copy_string(cur_rend.as_str(), &mut held)
        .map_err(resolve_err)?;
    held.grow_by(64).map_err(resolve_err)?;
    visited.insert(cur_seen);

    if let Some(prev_node) = immediate_prev {
        let hop0 = match_rendition(
            &mut resolution,
            &mut held,
            &mut remaining_scans,
            &prev_node,
            None,
        )?;
        if !hop0.is_empty() {
            let chosen = select_from_frontier(
                &mut resolution,
                t,
                &access,
                &prov_was_revision_of,
                hop0,
                &mut remaining_scans,
            )?;
            return Ok(RevisionLink::New { previous: chosen });
        }
        if !visited.contains(prev_node.as_str()) {
            let prev_seen = resolution
                .preparation()
                .copy_string(prev_node.as_str(), &mut held)
                .map_err(resolve_err)?;
            held.grow_by(64).map_err(resolve_err)?;
            visited.insert(prev_seen);
        }
        match c18_predecessors(&mut resolution, &mut remaining_scans, &prev_node)? {
            C18Predecessors::Ambiguous => {
                return Ok(RevisionLink::New { previous: None });
            }
            C18Predecessors::List(preds, _preds_held) => {
                for pred in preds {
                    if visited.contains(pred.as_str()) {
                        continue;
                    }
                    if visited.len() >= MAX_LINEAGE_HOPS {
                        return Ok(RevisionLink::New { previous: None });
                    }
                    let pred_seen = resolution
                        .preparation()
                        .copy_string(pred.as_str(), &mut held)
                        .map_err(resolve_err)?;
                    let pred_q = NamedNode::new_unchecked(
                        resolution
                            .preparation()
                            .copy_string(pred.as_str(), &mut held)
                            .map_err(resolve_err)?,
                    );
                    held.grow_by(128).map_err(resolve_err)?;
                    visited.insert(pred_seen);
                    queue.push_back(pred_q);
                }
            }
        }
    } else {
        // Same normalized text with updated original bytes produces a new revision_digest
        // on the same C18 rendition; a restored rendition also carries C18 edges to the
        // rendition it replaced. Collect both into the frontier and resolve chronology
        // across all candidates.
        let same_rend = match_rendition(
            &mut resolution,
            &mut held,
            &mut remaining_scans,
            &cur_rend,
            Some(revision_digest),
        )?;
        frontier.extend(same_rend);
        match c18_predecessors(&mut resolution, &mut remaining_scans, &cur_rend)? {
            C18Predecessors::Ambiguous => {
                return Ok(RevisionLink::New { previous: None });
            }
            C18Predecessors::List(preds, _preds_held) => {
                for pred in preds {
                    if visited.contains(pred.as_str()) {
                        continue;
                    }
                    if visited.len() >= MAX_LINEAGE_HOPS {
                        return Ok(RevisionLink::New { previous: None });
                    }
                    let pred_seen = resolution
                        .preparation()
                        .copy_string(pred.as_str(), &mut held)
                        .map_err(resolve_err)?;
                    let pred_q = NamedNode::new_unchecked(
                        resolution
                            .preparation()
                            .copy_string(pred.as_str(), &mut held)
                            .map_err(resolve_err)?,
                    );
                    held.grow_by(128).map_err(resolve_err)?;
                    visited.insert(pred_seen);
                    queue.push_back(pred_q);
                }
            }
        }
    }

    while let Some(rend) = queue.pop_front() {
        resolution.preparation().check().map_err(resolve_err)?;
        let hits = match_rendition(
            &mut resolution,
            &mut held,
            &mut remaining_scans,
            &rend,
            None,
        )?;
        if !hits.is_empty() {
            frontier.extend(hits);
            continue;
        }
        match c18_predecessors(&mut resolution, &mut remaining_scans, &rend)? {
            C18Predecessors::Ambiguous => {
                return Ok(RevisionLink::New { previous: None });
            }
            C18Predecessors::List(preds, _preds_held) => {
                for pred in preds {
                    if visited.contains(pred.as_str()) {
                        continue;
                    }
                    if visited.len() >= MAX_LINEAGE_HOPS {
                        return Ok(RevisionLink::New { previous: None });
                    }
                    let pred_seen = resolution
                        .preparation()
                        .copy_string(pred.as_str(), &mut held)
                        .map_err(resolve_err)?;
                    let pred_q = NamedNode::new_unchecked(
                        resolution
                            .preparation()
                            .copy_string(pred.as_str(), &mut held)
                            .map_err(resolve_err)?,
                    );
                    held.grow_by(128).map_err(resolve_err)?;
                    visited.insert(pred_seen);
                    queue.push_back(pred_q);
                }
            }
        }
    }
    let chosen = select_from_frontier(
        &mut resolution,
        t,
        &access,
        &prov_was_revision_of,
        frontier,
        &mut remaining_scans,
    )?;
    Ok(RevisionLink::New { previous: chosen })
}

/// A model or converter as an IRI.
fn agent_iri(prefix: &str, name: &str) -> NamedNode {
    let enc =
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
    NamedNode::new_unchecked(format!("{prefix}{enc}"))
}

/// The facts, entities and spans of an extraction as one evidence batch.
struct Batch {
    input: EvidenceInput,
    /// the facts left out, with their code
    failed: Vec<Value>,
    flagged: Vec<Value>,
    /// the possible duplicates of new entities, reported and written nowhere
    candidates: Vec<Value>,
    entities: Value,
    claims: usize,
}

struct Source<'a> {
    source: NamedNode,
    source_key: String,
    revision_digest: Sha256Digest,
    revision_link: RevisionLink,
    locator: Option<NamedNode>,
    rendition_key: String,
    media_type: &'a str,
    format: &'a str,
    text: &'a str,
}

fn revision_template(src: &Source) -> SourceRevisionInput {
    SourceRevisionInput {
        version: RecordVersion::V1,
        key: key("rev"),
        source: src.source.clone(),
        source_key: key(src.source_key.clone()),
        upstream_version: None,
        content: SourceContent::MetadataOnly {
            digest: src.revision_digest,
        },
        metadata: sparkles::evidence::SourceMetadata {
            locators: src.locator.iter().cloned().collect(),
            reported_rights: Vec::new(),
        },
        times: Default::default(),
        lineage: Default::default(),
        was_revision_of: None,
    }
}

/// Apply `link` to `input`, either minting `template` in `input.revisions` with local
/// references or referencing an already-authenticated `SourceRevision` via
/// `RecordRef::Existing`.
fn apply_revision_link(
    input: &mut EvidenceInput,
    template: &SourceRevisionInput,
    link: &RevisionLink,
) {
    let rev_ref = match link {
        RevisionLink::Existing { iri, .. } => {
            input.revisions.clear();
            RecordRef::Existing(iri.clone())
        }
        RevisionLink::New { previous } => {
            let mut rev = template.clone();
            rev.was_revision_of = previous.clone().map(RecordRef::Existing);
            input.revisions = vec![rev];
            RecordRef::Local(key("rev"))
        }
    };
    for rend in &mut input.renditions {
        rend.revision = rev_ref.clone();
    }
    for passage in &mut input.passages {
        passage.revision = rev_ref.clone();
    }
    for reference in &mut input.references {
        if let EvidenceRefTarget::Passage { revision, .. } = &mut reference.target {
            *revision = rev_ref.clone();
        }
    }
    for claim in &mut input.claims {
        claim.revision = rev_ref.clone();
    }
}

/// One claim of an extraction with its span, confidence and the model whose answer
/// gave it.
struct Proposition {
    triple: Triple,
    span: (usize, usize),
    confidence: Option<f64>,
    model: NamedNode,
}

/// The claims that an extraction makes once its entities are linked.
struct Claims {
    claims: Vec<Proposition>,
    /// the facts left out, with their code
    failed: Vec<Value>,
    flagged: Vec<Value>,
    candidates: Vec<Value>,
    entities: Value,
}

/// The claims of `facts` with the entities of `linked`, refused with `too-large` when
/// they are more than one batch holds. A new entity's IRI is a name-based UUID of the
/// revision.
fn claims_of(
    r: &Run,
    v: &Vocabulary,
    facts: &[Fact],
    linked: &Linked,
    revision_digest: &Sha256Digest,
    mut failed_facts: Vec<Value>,
) -> Result<Claims, Failed> {
    let dataset_id = r
        .ctx
        .server
        .state
        .get(&r.dataset)
        .map(|d| d.store.dataset_id())
        .unwrap_or_default();
    let prefixes = merged(&v.prefixes, &linked.prefixes);
    let named = |s: &str| -> Option<NamedNode> {
        let s = s.trim();
        let s = s
            .strip_prefix('<')
            .and_then(|x| x.strip_suffix('>'))
            .unwrap_or(s);
        NamedNode::new(expand_iri(s, &prefixes)).ok()
    };
    // the entities: existing IRIs, or a name-based UUID of the revision for a new one
    let mut flagged: Vec<Value> = Vec::new();
    let mut candidates: Vec<Value> = Vec::new();
    let entity_iri: Vec<Option<NamedNode>> = linked
        .entities
        .iter()
        .zip(&linked.links)
        .map(|((text, class), l)| match l {
            Link::Existing(i) => named(i),
            Link::Ambiguous(i, c) => {
                flagged.push(json!({ "entity": text, "chosen": i, "candidates": c }));
                named(i)
            }
            Link::New(c) => {
                if !c.is_empty() {
                    let c: Vec<String> = c
                        .iter()
                        .filter_map(|i| named(i).map(|n| n.as_str().to_string()))
                        .collect();
                    candidates.push(json!({ "entity": text, "candidates": c }));
                }
                let norm = crate::mcp::memory::ingest::fold_ws(text).to_lowercase();
                let id = crate::mcp::memory::assert::uuid_v5(
                    &dataset_id,
                    &format!("ingest-claims-entity\0{}\0{norm}\0{class}", revision_digest),
                );
                Some(NamedNode::new_unchecked(format!(
                    "urn:uuid:{}",
                    id.hyphenated()
                )))
            }
        })
        .collect();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut claims: Vec<Proposition> = Vec::new();
    let mut first_span: HashMap<usize, (usize, usize, NamedNode)> = HashMap::new();
    for f in facts {
        let term_of = |k: &str| linked.entity_of.get(k).and_then(|e| entity_iri[*e].clone());
        let Some(s) = term_of(&f.s) else {
            failed_facts.push(json!({ "code": "unknown-mention", "message": "the subject has no entity", "p": f.p }));
            continue;
        };
        let Some(p) = named(&f.p) else {
            failed_facts.push(json!({ "code": "unknown-predicate", "message": format!("{} is not an IRI", f.p), "p": f.p }));
            continue;
        };
        let o: Term = match &f.o {
            Obj::Key(k) => match term_of(k) {
                Some(o) => o.into(),
                None => {
                    failed_facts.push(json!({ "code": "unknown-mention", "message": "the object has no entity", "p": f.p }));
                    continue;
                }
            },
            Obj::Lit(l) => match (&l.datatype, &l.lang) {
                (Some(dt), _) => {
                    let dt = match dt.strip_prefix("xsd:") {
                        Some(local) => {
                            NamedNode::new(format!("http://www.w3.org/2001/XMLSchema#{local}")).ok()
                        }
                        None => named(dt),
                    };
                    match dt {
                        Some(dt) => Literal::new_typed_literal(l.value.clone(), dt).into(),
                        None => {
                            failed_facts.push(json!({ "code": "invalid-literal", "message": "the datatype is not an IRI", "p": f.p }));
                            continue;
                        }
                    }
                }
                (None, Some(lang)) => {
                    match Literal::new_language_tagged_literal(l.value.clone(), lang) {
                        Ok(l) => l.into(),
                        Err(_) => {
                            failed_facts.push(json!({ "code": "invalid-literal", "message": "the language tag is invalid", "p": f.p }));
                            continue;
                        }
                    }
                }
                (None, None) => Literal::new_simple_literal(l.value.clone()).into(),
            },
        };
        let entities_of = std::iter::once(&f.s).chain(match &f.o {
            Obj::Key(k) => Some(k),
            Obj::Lit(_) => None,
        });
        let model = model_iri(f.pair.as_ref());
        for k in entities_of {
            if let Some(e) = linked.entity_of.get(k) {
                first_span
                    .entry(*e)
                    .or_insert((f.start, f.end, model.clone()));
            }
        }
        let triple = Triple::new(s, p, o);
        let id = format!("{triple} {}-{}", f.start, f.end);
        if seen.contains_key(&id) {
            continue;
        }
        seen.insert(id, claims.len());
        claims.push(Proposition {
            triple,
            span: (f.start, f.end),
            confidence: f.confidence,
            model,
        });
    }
    // a new entity's class and label are claims too, cited by its first passage
    let label_p = v
        .label_predicate
        .as_deref()
        .and_then(named)
        .unwrap_or_else(|| rdfs::LABEL.into_owned());
    let mut new_entities = 0usize;
    for (e, ((text, class), l)) in linked.entities.iter().zip(&linked.links).enumerate() {
        if !matches!(l, Link::New(_)) {
            continue;
        }
        let (Some(iri), Some((a, b, model))) = (&entity_iri[e], first_span.get(&e)) else {
            continue;
        };
        let (a, b) = (*a, *b);
        new_entities += 1;
        let span = (a, b);
        if let Some(c) = named(class) {
            claims.push(Proposition {
                triple: Triple::new(iri.clone(), rdf::TYPE, c),
                span,
                confidence: None,
                model: model.clone(),
            });
        }
        let label: Term = match &v.language {
            Some(lang) => Literal::new_language_tagged_literal(text.clone(), lang)
                .map(Term::from)
                .unwrap_or_else(|_| Literal::new_simple_literal(text.clone()).into()),
            None => Literal::new_simple_literal(text.clone()).into(),
        };
        claims.push(Proposition {
            triple: Triple::new(iri.clone(), label_p.clone(), label),
            span,
            confidence: None,
            model: model.clone(),
        });
    }
    if claims.len() > sparkles::evidence::MAX_BATCH_CLAIMS {
        return Err(too_large(format!(
            "the extraction gives {} claims; one batch holds at most {}",
            claims.len(),
            sparkles::evidence::MAX_BATCH_CLAIMS
        )));
    }
    let count = |f: fn(&Link) -> bool| linked.links.iter().filter(|l| f(l)).count();
    let entities = json!({
        "linked": count(|l| matches!(l, Link::Existing(_))),
        "new": new_entities,
        "ambiguous": count(|l| matches!(l, Link::Ambiguous(..))),
    });
    flagged.dedup();
    Ok(Claims {
        claims,
        failed: failed_facts,
        flagged,
        candidates,
        entities,
    })
}

/// The claims of an extraction as one evidence batch, refused with `too-large` when its
/// preparation exceeds the decoded bound of spec C25 §8.
fn batch(src: &Source, c: Claims, control: &Control) -> Result<Batch, Failed> {
    let mut input = EvidenceInput::default();
    let rev_ref = match &src.revision_link {
        RevisionLink::Existing { iri, .. } => RecordRef::Existing(iri.clone()),
        RevisionLink::New { previous } => {
            let mut rev = revision_template(src);
            rev.was_revision_of = previous.clone().map(RecordRef::Existing);
            input.revisions.push(rev);
            RecordRef::Local(key("rev"))
        }
    };
    input.renditions.push(RenditionInput {
        version: RecordVersion::V1,
        key: key("text"),
        revision: rev_ref.clone(),
        occurrence_key: key(src.rendition_key.clone()),
        conversion: MethodRef {
            method: agent_iri("urn:x-sparkles:ingest:convert:", src.format),
            version: key(env!("CARGO_PKG_VERSION")),
        },
        media_type: src.media_type.to_string(),
        language: None,
        content: RenditionContent::NormalizedText(src.text.to_string()),
        expected_digest: None,
    });
    let chars: Vec<char> = src.text.chars().collect();
    // one passage and one reference per distinct span, in the order the claims cite them
    let mut spans: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (n, claim) in c.claims.iter().enumerate() {
        let (a, b) = claim.span;
        let i = match spans.get(&(a, b)) {
            Some(i) => *i,
            None => {
                let i = spans.len();
                let quote: String = chars[a..b].iter().collect();
                input.passages.push(PassageInput {
                    version: RecordVersion::V1,
                    key: key(format!("p{i}")),
                    revision: rev_ref.clone(),
                    rendition: RecordRef::Local(key("text")),
                    occurrence_key: key(format!("{a}-{b}")),
                    start: a as u64,
                    end: b as u64,
                    text_digest: sha(quote.as_bytes())?,
                    quote: Some(quote),
                });
                input.references.push(EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key(format!("r{i}")),
                    occurrence_key: key(format!("{a}-{b}")),
                    target: EvidenceRefTarget::Passage {
                        revision: rev_ref.clone(),
                        rendition: RecordRef::Local(key("text")),
                        passage: RecordRef::Local(key(format!("p{i}"))),
                    },
                });
                spans.insert((a, b), i);
                i
            }
        };
        let digest = sha(format!("{} {a}-{b}", claim.triple).as_bytes())?;
        let confidence = claim.confidence.map(|v| Confidence {
            scale: NamedNode::new_unchecked(ZERO_TO_ONE),
            value: Literal::new_typed_literal(decimal(v), xsd::DECIMAL),
            evaluator: claim.model.clone(),
            method: Some(MethodRef {
                method: NamedNode::new_unchecked("urn:x-sparkles:ingest:extract"),
                version: key(env!("CARGO_PKG_VERSION")),
            }),
        });
        input.claims.push(ClaimInput {
            version: RecordVersion::V1,
            key: key(format!("c{n}")),
            occurrence_key: key(digest.to_string()),
            revision: rev_ref.clone(),
            proposition: claim.triple.clone(),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: src.source.clone(),
            scope: Default::default(),
            confidence: ConfidenceFacets {
                extraction: confidence,
                ..Default::default()
            },
            validity: None,
        });
        input.links.push(EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key(format!("l{n}")),
            occurrence_key: key(digest.to_string()),
            target: RecordRef::Local(key(format!("c{n}"))),
            evidence: RecordRef::Local(key(format!("r{i}"))),
            relation: EvidenceRelation::Supports,
            attributed_author: claim.model.clone(),
        });
    }
    // the decoded bound is the one the proposal's preparation charges
    match sparkles::evidence::prepare_input(&input, control) {
        Ok(_) => {}
        Err(sparkles::Error::BudgetExceeded(b))
            if matches!(b.kind, sparkles::BudgetKind::DecompressedBytes) =>
        {
            return Err(too_large(format!(
                "the batch decodes to at least {} bytes; one batch holds at most {}",
                b.requested, b.limit
            )));
        }
        Err(e) => return Err(api(e.into())),
    }
    Ok(Batch {
        claims: input.claims.len(),
        input,
        failed: c.failed,
        flagged: c.flagged,
        candidates: c.candidates,
        entities: c.entities,
    })
}

/// A zero-to-one confidence as an exact decimal lexical form.
fn decimal(c: f64) -> String {
    let s = format!("{:.6}", c.clamp(0.0, 1.0));
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_string()
    }
}

fn too_large(message: String) -> Failed {
    failed("too-large", 413, message)
}

fn merged(a: &Value, b: &Value) -> Value {
    let mut out = a.clone();
    if !out.is_object() {
        out = json!({});
    }
    if let (Some(o), Some(b)) = (out.as_object_mut(), b.as_object()) {
        for (k, v) in b {
            o.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    out
}

/// How a moved branch head is handled during publication.
enum ReplanMode<'a> {
    /// Upload mode: replan the C18 `register_converted` call and recompute lineage at
    /// the new planned head.
    Upload { reg: &'a Map<String, Value> },
    /// Registered-source mode: confirm the source still points to the same live
    /// rendition and observed digest at the new head before recomputing lineage; if the
    /// source changed or its digest is not provably equal, refuse with `409 head-changed`.
    Source {
        rendition: &'a str,
        observed_digest: Option<Sha256Digest>,
    },
}

/// Context for refreshing the revision's `prov:wasRevisionOf` link when a moved head
/// requires replanning.
struct Replan<'a> {
    principal: &'a NamedNode,
    source: &'a NamedNode,
    source_key: &'a str,
    revision_digest: Sha256Digest,
    c18_graph: &'a str,
    revision_template: SourceRevisionInput,
    mode: ReplanMode<'a>,
}

/// Publish the batch fenced at `head` (and carrying `planned.changes` in upload mode),
/// resolving `prov:wasRevisionOf` at `head` and replanning when the head moved.
#[allow(clippy::too_many_arguments)]
fn publish(
    r: &Run,
    t: &Target,
    input: &mut EvidenceInput,
    mut head: u64,
    mut planned: Option<Planned>,
    replan: &Replan<'_>,
    idempotency_key: &str,
    message: String,
) -> Result<(sparkles::evidence::Proposed, Option<Value>, RevisionLink), Failed> {
    let st = r.ctx.server.state.clone();
    let p = r.ctx.principal.clone();
    let control = control_of(r);
    let mut attempt = 0;

    let refresh_for_moved_head =
        |planned: &mut Option<Planned>, head: &mut u64| -> Result<(), Failed> {
            match &replan.mode {
                ReplanMode::Upload { reg } => {
                    let replanned = plan(r, reg, t)?;
                    *head = replanned.head;
                    *planned = Some(replanned);
                    Ok(())
                }
                ReplanMode::Source {
                    rendition,
                    observed_digest: initial_digest,
                } => {
                    let Some(expected_digest) = *initial_digest else {
                        return Err(failed(
                            "head-changed",
                            409,
                            "the registered source changed while claims were extracted",
                        ));
                    };
                    let new_head = t.logical.store.snapshot().commit;
                    let still_current = matches!(
                        observe_source_at_commit(
                            r,
                            t,
                            replan.source,
                            replan.c18_graph,
                            rendition,
                            new_head,
                        )?,
                        ObservedSource::Live {
                            rendition_unique: true,
                            digest: ObservedDigest::Unique(d),
                        } if d == expected_digest
                    );
                    if !still_current {
                        return Err(failed(
                            "head-changed",
                            409,
                            "the registered source changed while claims were extracted",
                        ));
                    }
                    *head = new_head;
                    Ok(())
                }
            }
        };

    loop {
        r.check()?;
        // If the branch head already moved since planning/reading, refresh before
        // running predecessor resolution so non-retained commits do not fail early.
        if t.logical.store.snapshot().commit != head {
            if attempt >= REPLANS {
                return Err(failed(
                    "head-changed",
                    409,
                    "the branch head moved before publication",
                ));
            }
            attempt += 1;
            refresh_for_moved_head(&mut planned, &mut head)?;
        }
        let (cur_rend, prev_rend) = match (&replan.mode, &planned) {
            (ReplanMode::Upload { .. }, Some(pl)) => {
                let rend = expand_iri(
                    pl.registered["rendition"].as_str().unwrap_or(""),
                    &pl.registered["prefixes"],
                );
                let prev = pl
                    .registered
                    .get("previousRendition")
                    .and_then(Value::as_str)
                    .map(|s| expand_iri(s, &pl.registered["prefixes"]));
                (rend, prev)
            }
            (ReplanMode::Source { rendition, .. }, _) => ((*rendition).to_string(), None),
            (ReplanMode::Upload { .. }, None) => {
                return Err(failed("error", 500, "upload publication missing C18 plan"));
            }
        };
        let link = match predecessor_revision(
            r,
            t,
            replan.principal,
            replan.source,
            replan.source_key,
            replan.revision_digest,
            replan.c18_graph,
            &cur_rend,
            prev_rend.as_deref(),
            head,
        ) {
            Ok(link) => link,
            Err(f)
                if f.0["code"] == "evidence-unavailable"
                    && t.logical.store.snapshot().commit != head =>
            {
                if attempt >= REPLANS {
                    return Err(failed(
                        "head-changed",
                        409,
                        "the branch head moved before publication",
                    ));
                }
                attempt += 1;
                refresh_for_moved_head(&mut planned, &mut head)?;
                continue;
            }
            Err(f) => return Err(f),
        };
        apply_revision_link(input, &replan.revision_template, &link);

        let mut options =
            sparkles::evidence::ProposeOptions::new(crate::evidence::principal_iri(&p));
        options.idempotency_key = Some(idempotency_key.to_string());
        options.source_graph = Some(t.source_graph.clone());
        // a batch of the revision and rendition alone writes no proposal graph
        let proposes = !(input.references.is_empty()
            && input.claims.is_empty()
            && input.links.is_empty()
            && input.assessments.is_empty());
        options.proposal_graph = proposes.then(|| t.proposal_graph.clone());
        options.message = Some(Arc::from(message.as_str()));
        options.author = crate::http::author(&p);
        options.write = write_view(r, &t.main, &t.logical, &t.branch);
        options.authority = Some(
            crate::evidence::caller_fence(st.clone(), &t.main, p.clone())
                .map_err(|e| api(e.into()))?,
        );
        options.if_head = Some(head);
        if let Some(pl) = &planned {
            options.ordinary = Some(pl.changes.clone());
        }
        t.main.store.failpoint("ingest-claims-before-publish");
        match crate::evidence::propose_ingested(
            &st,
            (&t.main, &t.logical),
            &p,
            input,
            &options,
            &control,
        ) {
            Ok(proposed) => return Ok((proposed, planned.map(|p| p.registered), link)),
            Err(e) => {
                let f = api(e);
                let moved = f.0["code"] == "head-changed";
                if moved && attempt < REPLANS {
                    attempt += 1;
                    refresh_for_moved_head(&mut planned, &mut head)?;
                    continue;
                }
                return Err(f);
            }
        }
    }
}

fn proposed_json(p: &sparkles::evidence::Proposed, out: &mut Value) {
    let mut by_kind: BTreeMap<&str, u64> = BTreeMap::new();
    for rec in &p.records {
        *by_kind
            .entry(crate::evidence::kind_name(rec.kind))
            .or_default() += 1;
    }
    out["operation"] = p.operation.as_str().into();
    out["committed"] = p.committed.into();
    out["replayed"] = p.replayed.into();
    out["commit"] = p.commit.into();
    out["branchId"] = json!(p.branch);
    out["firstAvailableAt"] = json!(p.first_available_at);
    out["hold"] = json!(p.hold);
    out["recordCounts"] = json!(by_kind);
    out["records"] = p
        .records
        .iter()
        .map(crate::evidence::record_json)
        .collect::<Vec<_>>()
        .into();
    for rec in &p.records {
        match rec.key.as_str() {
            "rev" => out["revision"] = rec.iri.as_str().into(),
            "text" => out["evidenceRendition"] = rec.iri.as_str().into(),
            _ => {}
        }
    }
}

/// The extraction pairs, required in claims mode.
fn models_and_pairs<'m>(
    r: &Run<'m, '_>,
    ds: &crate::state::Dataset,
    req: &Request,
) -> Result<(&'m crate::models::Models, Vec<crate::models::Pair>), Failed> {
    let settings = crate::assistant::settings(&r.ctx.server.state, ds);
    let pairs = if req.pairs.is_empty() {
        super::pipeline::extract_pairs(r.ctx.models, &settings)
    } else {
        req.pairs.clone()
    };
    match r.ctx.models.filter(|_| !pairs.is_empty()) {
        Some(m) => Ok((m, pairs)),
        None => Err(Failed::new(
            "no-model",
            "claims mode extracts facts, and the dataset's ingestion has no provider and model in the extract role: enable ingest in its assistant settings, with a provider that may receive documents",
        )),
    }
}

fn model_iri(pair: Option<&crate::models::Pair>) -> NamedNode {
    match pair {
        Some(p) => agent_iri(
            "urn:x-sparkles:model:",
            &format!("{}/{}", p.provider, p.model),
        ),
        None => NamedNode::new_unchecked("urn:x-sparkles:model:unknown"),
    }
}

/// Run a claims ingestion of a document.
/// The run's deadline and cancellation as a task control.
fn control_of(r: &Run) -> Control {
    Control {
        deadline: Some(r.ctx.deadline),
        ..Control::with_cancel(r.ctx.cancel.clone())
    }
}

/// Extract the facts of `text` and link their entities on `branch`. Linking follows the
/// extraction chunk by chunk, so the extraction stops calling the model as soon as the
/// claims collected, with the class and label claims of new entities, pass the bound of
/// one batch.
fn extract_and_link(
    r: &mut Run,
    (models, pairs): (&crate::models::Models, &[crate::models::Pair]),
    vocab: &Vocabulary,
    (text, chunks): (&str, &[(usize, usize)]),
    branch: Option<&str>,
    revision_digest: &Sha256Digest,
) -> Result<(super::extract::Extraction, Linked), Failed> {
    let mut linker = super::extract::Linker::new();
    let mut check = |r: &mut Run, ex: &super::extract::Extraction| -> Result<(), Failed> {
        linker.update(r, ex, branch)?;
        claims_of(
            r,
            vocab,
            &ex.facts,
            &linker.linked(ex),
            revision_digest,
            Vec::new(),
        )
        .map(|_| ())
    };
    let ex = super::extract::extract(r, models, pairs, vocab, text, chunks, Some(&mut check))?;
    r.ctx.progress.status(Status::Linking, 0.82, None);
    linker.update(r, &ex, branch)?;
    let linked = linker.linked(&ex);
    Ok((ex, linked))
}

pub(super) fn run(r: &mut Run, req: &Request) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let t = target(r, req)?;
    let (models, pairs) = models_and_pairs(r, &t.main, req)?;
    let principal = crate::evidence::principal_iri(ctx.principal);
    let name = req.name.clone().or_else(|| {
        req.url
            .as_deref()
            .map(|u| super::pipeline::base_name(u).to_string())
    });
    let format = Format::detect(req.media_type.as_deref(), name.as_deref(), &req.bytes)?;
    if matches!(format, Format::Csv | Format::Tsv) || format.is_spreadsheet() {
        return Err(Refusal::new(
            "unsupported-format",
            415,
            "claims mode ingests documents: a table is mapped with a draft in the other modes",
        )
        .into());
    }
    // 1. conversion
    ctx.progress.status(Status::Converting, 0.05, None);
    r.check()?;
    let converted = convert::convert(
        &req.bytes,
        format,
        &convert::Options {
            allow_partial: req.allow_partial,
            max_input: convert::MAX_INPUT_BYTES,
            max_text: convert::MAX_TEXT_BYTES,
            deadline: ctx.deadline,
            pdf: ctx.pdf.clone(),
            max_images: 0,
        },
    )?;
    r.check()?;
    // the text exactly as the registration normalizes it, so spans agree with it
    let text = crate::mcp::memory::ingest::normalize(&converted.text);
    let title = req
        .title
        .clone()
        .or_else(|| converted.title.clone())
        .or_else(|| name.clone());
    let iri = req.iri.clone().or_else(|| {
        req.url
            .clone()
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    });
    let profile = req.profile.clone().unwrap_or_else(|| "default".into());
    // 2. the registration, planned without writing it
    ctx.progress.status(Status::Registering, 0.1, None);
    let mut reg = Map::new();
    if let Some(g) = &req.graph {
        reg.insert("graph".into(), g.clone().into());
    }
    if let Some(i) = &iri {
        reg.insert("iri".into(), i.clone().into());
    }
    if let Some(t) = &title {
        reg.insert(
            "title".into(),
            t.chars().take(1000).collect::<String>().into(),
        );
    }
    reg.insert("format".into(), converted.media_type.clone().into());
    reg.insert("text".into(), text.clone().into());
    reg.insert("profile".into(), profile.clone().into());
    if let Some(m) = &req.message {
        reg.insert("message".into(), m.clone().into());
    }
    reg.insert(
        "rendition".into(),
        super::pipeline::rendition_extras(&converted),
    );
    let planned = plan(r, &reg, &t)?;
    let registered = planned.registered.clone();
    let prefixes = registered["prefixes"].clone();
    let source = expand_iri(registered["source"].as_str().unwrap_or(""), &prefixes);
    let rendition = expand_iri(registered["rendition"].as_str().unwrap_or(""), &prefixes);
    let graph = expand_iri(registered["graph"].as_str().unwrap_or(""), &prefixes);
    let rendition_digest = sha(text.as_bytes())?;
    let revision_digest = recorded_digest(&registered["digest"], &rendition_digest);
    let key = idempotency_key(
        &t,
        &source,
        &graph,
        &revision_digest,
        &rendition_digest,
        &profile,
    )?;
    let mut out = json!({
        "mode": "claims",
        "format": format.name(),
        "source": source,
        "graph": graph,
        "rendition": rendition,
        "digest": registered["digest"],
        "length": registered["length"],
        "profile": profile,
        "factGuard": "not-run",
    });
    if t.branch != sparkles::branch::MAIN {
        out["branch"] = t.branch.clone().into();
    }
    // 3. a repeat of a published ingestion answers without a model call
    if let Some(op) = already(&t, &principal, &key)? {
        out["outcome"] = "already-proposed".into();
        out["operation"] = op.as_str().into();
        out["message"] =
            "this revision was already ingested as claims; nothing was extracted or written".into();
        return Ok(out);
    }
    // 4. the estimate, the extraction and linking
    let chunks = crate::mcp::memory::ingest::chunk_bounds(&text, false);
    let branch = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let vocab = Vocabulary::read_on(r, &profile, branch)?;
    estimate(r, req, models, &pairs, &vocab, &text, &chunks, &t)?;
    let (ex, linked) = extract_and_link(
        r,
        (models, &pairs),
        &vocab,
        (&text, &chunks),
        branch,
        &revision_digest,
    )?;
    ctx.progress.status(Status::Writing, 0.9, None);
    #[cfg(test)]
    t.main.store.failpoint("ingest-claims-after-extract");
    let source_node =
        NamedNode::new(source.clone()).map_err(|e| failed("error", 500, e.to_string()))?;
    let sk = source_key(&t, &source, &graph)?;
    let planned_head = planned.head;
    let src = Source {
        source: source_node.clone(),
        source_key: sk.clone(),
        revision_digest,
        revision_link: RevisionLink::New { previous: None },
        locator: req
            .url
            .as_deref()
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            .and_then(|u| NamedNode::new(u).ok()),
        rendition_key: rendition.clone(),
        media_type: &converted.media_type,
        format: format.name(),
        text: &text,
    };
    let rev_template = revision_template(&src);
    let c = claims_of(
        r,
        &vocab,
        &ex.facts,
        &linked,
        &revision_digest,
        ex.failed.clone(),
    )?;
    let mut b = batch(&src, c, &control_of(r))?;
    let message = format!(
        "Ingest claims from {}",
        title.as_deref().unwrap_or(source.as_str())
    );
    let replan = Replan {
        principal: &principal,
        source: &source_node,
        source_key: &sk,
        revision_digest,
        c18_graph: &graph,
        revision_template: rev_template,
        mode: ReplanMode::Upload { reg: &reg },
    };
    let (proposed, registered, final_link) = publish(
        r,
        &t,
        &mut b.input,
        planned_head,
        Some(planned),
        &replan,
        &key,
        message,
    )?;
    if let Some(reg) = registered {
        out["alreadyRegistered"] = reg["alreadyRegistered"].clone();
        out["chunks"] = json!(reg["chunks"].as_array().map_or(0, Vec::len));
        if let Some(p) = reg.get("previousRendition") {
            out["previousRendition"] = p.clone();
        }
    }
    if let Some(prev) = final_link.previous() {
        out["previousRevision"] = prev.as_str().into();
    }
    finish(&mut out, &proposed, &b, &ex);
    if let RevisionLink::Existing { iri, .. } = &final_link
        && out.get("revision").is_none()
    {
        out["revision"] = iri.as_str().into();
    }
    Ok(out)
}

fn finish(
    out: &mut Value,
    proposed: &sparkles::evidence::Proposed,
    b: &Batch,
    ex: &super::extract::Extraction,
) {
    out["outcome"] = if b.claims > 0 { "proposed" } else { "no-facts" }.into();
    out["claims"] = b.claims.into();
    out["extracted"] = ex.facts.len().into();
    out["failed"] = b.failed.clone().into();
    out["entities"] = b.entities.clone();
    if !b.flagged.is_empty() {
        out["flagged"] = b.flagged.clone().into();
    }
    if !b.candidates.is_empty() {
        out["candidates"] = b.candidates.clone().into();
    }
    if ex.chunks_failed > 0 {
        out["chunksFailed"] = ex.chunks_failed.into();
    }
    proposed_json(proposed, out);
}

#[allow(clippy::too_many_arguments)]
fn estimate(
    r: &Run,
    req: &Request,
    models: &crate::models::Models,
    pairs: &[crate::models::Pair],
    vocab: &Vocabulary,
    text: &str,
    chunks: &[(usize, usize)],
    t: &Target,
) -> Result<(), Failed> {
    let st = &r.ctx.server.state;
    let settings = crate::assistant::settings(st, &t.main);
    let ingest = crate::mcp::memory::ingest::ingest_settings(st, &t.main);
    let limits = super::pipeline::Limits {
        confirm_tokens: ingest
            .confirm_tokens
            .unwrap_or(super::pipeline::DEFAULT_CONFIRM_TOKENS),
        auto_confidence: ingest
            .auto_confidence
            .unwrap_or(super::pipeline::DEFAULT_AUTO_CONFIDENCE),
    };
    super::pipeline::confirm_estimate(
        r,
        req,
        models,
        pairs,
        vocab,
        text,
        chunks,
        &limits,
        (&t.main, &settings),
    )?;
    r.check()
}

/// Run a claims ingestion of a registered source, named by its graph or IRI. Its C18
/// records exist, so the commit holds the batch alone.
pub(super) fn run_source(r: &mut Run, req: &Request, source: &str) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let t = target(r, req)?;
    let (models, pairs) = models_and_pairs(r, &t.main, req)?;
    let principal = crate::evidence::principal_iri(ctx.principal);
    let on = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let read = super::pipeline::read_source(r, source, on)?;
    let profile = req.profile.clone().unwrap_or_else(|| "default".into());
    let rendition_digest = sha(read.text.as_bytes())?;
    let source_iri = expand_iri(read.found["source"].as_str().unwrap_or(""), &read.prefixes);
    let source_node =
        NamedNode::new(source_iri.clone()).map_err(|e| failed("error", 500, e.to_string()))?;
    let read_head = read
        .commit
        .unwrap_or_else(|| t.logical.store.snapshot().commit);
    let (revision_digest, observed_source_digest) = match observe_source_at_commit(
        r,
        &t,
        &source_node,
        &read.graph,
        &read.rendition,
        read_head,
    )? {
        ObservedSource::Live {
            rendition_unique,
            digest: ObservedDigest::Unique(d),
        } => (d, rendition_unique.then_some(d)),
        ObservedSource::Live {
            digest: ObservedDigest::MissingOrInvalid,
            ..
        } => (rendition_digest, None),
        ObservedSource::Live {
            digest: ObservedDigest::Ambiguous,
            ..
        } => {
            return Err(failed(
                "conflict",
                409,
                "the registered source has multiple visible content digests",
            ));
        }
        ObservedSource::UnavailableOrChanged => {
            if t.logical.store.snapshot().commit != read_head {
                return Err(failed(
                    "head-changed",
                    409,
                    "the registered source changed while it was being read",
                ));
            }
            return Err(failed(
                "unknown-source",
                404,
                format!(
                    "no live source you can read is registered in graph <{}>",
                    read.graph
                ),
            ));
        }
    };
    let key = idempotency_key(
        &t,
        &source_iri,
        &read.graph,
        &revision_digest,
        &rendition_digest,
        &profile,
    )?;
    let mut out = json!({
        "mode": "claims",
        "source": source_iri,
        "graph": read.graph,
        "rendition": read.rendition,
        "length": read.found["length"],
        "chunks": read.chunks.len(),
        "profile": profile,
        "factGuard": "not-run",
    });
    if t.branch != sparkles::branch::MAIN {
        out["branch"] = t.branch.clone().into();
    }
    if let Some(op) = already(&t, &principal, &key)? {
        out["outcome"] = "already-proposed".into();
        out["operation"] = op.as_str().into();
        return Ok(out);
    }
    let branch = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let vocab = Vocabulary::read_on(r, &profile, branch)?;
    estimate(r, req, models, &pairs, &vocab, &read.text, &read.chunks, &t)?;
    let (ex, linked) = extract_and_link(
        r,
        (models, &pairs),
        &vocab,
        (&read.text, &read.chunks),
        branch,
        &revision_digest,
    )?;
    ctx.progress.status(Status::Writing, 0.9, None);
    #[cfg(test)]
    t.main.store.failpoint("ingest-claims-after-extract");
    let media_type = read.found["format"]
        .as_str()
        .unwrap_or("text/markdown")
        .to_string();
    let sk = source_key(&t, &source_iri, &read.graph)?;
    let src = Source {
        source: source_node.clone(),
        source_key: sk.clone(),
        revision_digest,
        revision_link: RevisionLink::New { previous: None },
        locator: None,
        rendition_key: read.rendition.clone(),
        media_type: &media_type,
        format: "registered",
        text: &read.text,
    };
    let rev_template = revision_template(&src);
    let c = claims_of(
        r,
        &vocab,
        &ex.facts,
        &linked,
        &revision_digest,
        ex.failed.clone(),
    )?;
    let mut b = batch(&src, c, &control_of(r))?;
    let message = format!("Ingest claims from {source_iri}");
    let replan = Replan {
        principal: &principal,
        source: &source_node,
        source_key: &sk,
        revision_digest,
        c18_graph: &read.graph,
        revision_template: rev_template,
        mode: ReplanMode::Source {
            rendition: &read.rendition,
            observed_digest: observed_source_digest,
        },
    };
    let (proposed, _, final_link) =
        publish(r, &t, &mut b.input, read_head, None, &replan, &key, message)?;
    if let Some(prev) = final_link.previous() {
        out["previousRevision"] = prev.as_str().into();
    }
    finish(&mut out, &proposed, &b, &ex);
    if let RevisionLink::Existing { iri, .. } = &final_link
        && out.get("revision").is_none()
    {
        out["revision"] = iri.as_str().into();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn decimals_are_exact_lexical_forms() {
        assert_eq!(super::decimal(0.9), "0.9");
        assert_eq!(super::decimal(1.0), "1.0");
        assert_eq!(super::decimal(0.0), "0.0");
        assert_eq!(super::decimal(0.125), "0.125");
    }

    #[test]
    fn a_registration_plan_becomes_ordinary_changes() {
        let c = super::ordinary(
            "DELETE DATA { GRAPH <urn:g> { <urn:s> <urn:p> \"old\" . } } ; INSERT DATA { GRAPH <urn:g> { <urn:s> <urn:p> \"new\" . } }",
        )
        .ok()
        .unwrap();
        assert_eq!(c.delete.len(), 1);
        assert_eq!(c.insert.len(), 1);
        assert_eq!(c.insert[0].graph_name.to_string(), "<urn:g>");
    }

    /// One claim whose literal object holds `bytes` bytes, as a batch.
    fn one_claim(bytes: usize) -> Result<super::Batch, super::Failed> {
        use oxrdf::{Literal, NamedNode, Triple};
        let text = "Ana Lima moved to the payments team this week.";
        let src = super::Source {
            source: NamedNode::new_unchecked("urn:source"),
            source_key: "k".into(),
            revision_digest: super::sha(text.as_bytes()).ok().unwrap(),
            revision_link: super::RevisionLink::New { previous: None },
            locator: None,
            rendition_key: "urn:rendition".into(),
            media_type: "text/markdown",
            format: "markdown",
            text,
        };
        let c = super::Claims {
            claims: vec![super::Proposition {
                triple: Triple::new(
                    NamedNode::new_unchecked("urn:s"),
                    NamedNode::new_unchecked("urn:p"),
                    Literal::new_simple_literal("x".repeat(bytes)),
                ),
                span: (0, 8),
                confidence: Some(0.9),
                model: NamedNode::new_unchecked("urn:x-sparkles:model:m"),
            }],
            failed: Vec::new(),
            flagged: Vec::new(),
            candidates: Vec::new(),
            entities: serde_json::json!({}),
        };
        super::batch(&src, c, &sparkles::task::Control::default())
    }

    #[test]
    fn the_decoded_bound_is_the_one_the_proposal_charges() {
        // a batch whose content is most of the 16 MiB is admitted, though an estimate
        // that counted its fields twice would refuse it
        let b = one_claim(15 << 20).ok().unwrap();
        assert_eq!(b.claims, 1);
        let e = one_claim(16 << 20).err().unwrap();
        assert_eq!(e.0["code"], "too-large", "{}", e.0);
        assert_eq!(e.0["status"], 413);
    }
}
