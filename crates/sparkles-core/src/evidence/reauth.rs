//! Verification and quad planning for administered re-authentication after a detach
//! (spec C25, roadmap item 552).
//!
//! When an evidence graph is detached and protected again, the managed-graph registry
//! assigns it a new protection nonce. Records written under an earlier protection then
//! answer `origin-not-reproduced` and their identities refuse new proposals. This module
//! verifies candidate records in a protected record graph against their independently
//! authenticated publication view (`BoundEvidenceView`), their recorded `ev:recordDigest`,
//! their retained content and passage spans, and their receipt and audit lineage, and
//! plans the managed-write changes that mark verified records under the current
//! protection nonce together with an audit `ev:Operation`.
//!
//! As in [`super::write`] and [`super::review`], the functions here hold no lock and
//! authorize nothing. The runtime that calls them obtains the managed-write capability,
//! checks administrator authority, drift and preconditions, and makes the one commit.

use super::canonical::FrameHash;
use super::prepare::{has_blank, locate_boundaries};
use super::resolve::{KINDS, integer, kind_iri};
use super::structured::select_json_lexical_utf8_prepared;
use super::write::{Reads, Stamp, date_time, ev, no_blank, prov, rdf_type, record_digest, string};
use super::{JSON_LEXICAL_UTF8_V1, JSON_LEXICAL_UTF8_VERSION, RecordKind, Sha256Digest};
use crate::access::{Preparation, Reservation};
use crate::error::BudgetKind;
use crate::id::Id;
use crate::index::Perm;
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
use crate::store::{BoundEvidenceView, Store};
use crate::task::Control;
use crate::{Error, Result};
use base64::Engine as _;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple, vocab::xsd};
use oxsdatatypes::DateTime;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::Read as _;
use uuid::Uuid;

const EV: &str = "urn:x-sparkles:evidence:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const OA: &str = "http://www.w3.org/ns/oa#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_BASE64: &str = "http://www.w3.org/2001/XMLSchema#base64Binary";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

/// The operation-kind marker placed on reconciliation audit `ev:Operation` records.
pub const OPERATION_KIND_REAUTH: &str = "urn:x-sparkles:evidence:Reauthentication";

/// The predicate recording the reconciled graph on a reconciliation audit operation.
pub const EV_REAUTHENTICATED_GRAPH: &str = "urn:x-sparkles:evidence:reauthenticatedGraph";

/// The predicate recording the operation kind on a reconciliation audit operation.
pub const EV_OPERATION_KIND: &str = "urn:x-sparkles:evidence:operationKind";

/// The predicate recording candidates that were already authenticated under the current
/// protection during a keyed reconciliation, so whole-graph replay reproduces every
/// verified candidate identically.
pub const EV_ALREADY_AUTHENTICATED: &str = "urn:x-sparkles:evidence:alreadyAuthenticated";

/// The most records one re-authentication operation reconciles.
pub const MAX_REAUTHENTICATE: usize = 10_000;

/// The most stored triples read for a single record and its fragment sub-nodes.
pub const MAX_RECORD_TRIPLES: usize = 10_000;

/// Maximum number of historical publication views retained concurrently during one
/// reconciliation call.
const MAX_CACHED_VIEWS: usize = 8;

/// Fragment sub-nodes whose triples belong to a record's immutable content digest
/// (see [`super::write`]).
const FRAGMENTS: [&str; 8] = [
    "selector",
    "sourceConfidence",
    "extractionConfidence",
    "analystConfidence",
    "validity",
    "validity-start",
    "validity-end",
    "validity-normalization",
];

/// Predicates that are never permitted on a record's fragment sub-nodes.
const FORBIDDEN_FRAGMENT_PREDICATES: [&str; 8] = [
    "urn:x-sparkles:evidence:retainedBytes",
    "urn:x-sparkles:evidence:normalizedText",
    "urn:x-sparkles:evidence:availabilityReceipt",
    "http://www.w3.org/ns/prov#wasGeneratedBy",
    "urn:x-sparkles:evidence:graphProtection",
    "urn:x-sparkles:evidence:recordDigest",
    "urn:x-sparkles:evidence:firstAvailableAt",
    "http://www.w3.org/ns/prov#invalidatedAtTime",
];

/// One candidate record an administrator asks to verify and re-authenticate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordCheck {
    pub iri: NamedNode,
    /// Optional expected `ev:recordDigest` that the record must match in addition to
    /// matching its own stored `ev:recordDigest`.
    pub expected_digest: Option<Sha256Digest>,
}

/// A candidate record that passed digest, content, span, receipt and publication-view
/// verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCandidate {
    pub iri: NamedNode,
    pub kind: RecordKind,
    pub digest: Sha256Digest,
    pub receipt: NamedNode,
    pub record_nonce: Uuid,
    pub operation_nonce: Uuid,
    /// Whether the record and its receipt already carry the graph's current protection
    /// nonce, so no nonce triple needs to change.
    pub already_authenticated: bool,
}

/// Bounded cache of opened historical [`BoundEvidenceView`]s for one reconciliation
/// pass, keyed by `(branch_id, commit_seq)`.
#[derive(Default)]
pub struct PublicationViews {
    views: BTreeMap<(Uuid, u64), BoundEvidenceView>,
    order: VecDeque<(Uuid, u64)>,
}

impl PublicationViews {
    pub fn new() -> Self {
        Self::default()
    }

    fn open_publication_view(
        main: &Store,
        logical: &Store,
        logical_head: u64,
        receipt_branch: Uuid,
        commit: u64,
        preparation: &Preparation,
        label: &str,
    ) -> Result<BoundEvidenceView> {
        let view = match main.publication_evidence_view(
            logical,
            logical_head,
            receipt_branch,
            commit,
            preparation,
        ) {
            Ok(view) => view,
            Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
            Err(e @ (Error::Io(_) | Error::Corrupt(_) | Error::StorageFull(_))) => return Err(e),
            Err(Error::Managed(m)) if m.kind == ManagedErrorKind::ConfigurationHistoryInactive => {
                return Err(Error::Managed(m));
            }
            Err(e @ (Error::BudgetExceeded(_) | Error::Unsupported(_))) => {
                return Err(classify_error(label, e));
            }
            Err(_) => {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!(
                        "{label}: publication commit {commit} on branch {receipt_branch} is not a retained, configuration-bound commit in this branch's lineage"
                    ),
                ));
            }
        };
        if view.commit().seq != commit {
            return Err(managed(
                ManagedErrorKind::OriginNotReproduced,
                format!("{label}: publication commit {commit} did not resolve to commit {commit}"),
            ));
        }
        Ok(view)
    }

    #[allow(clippy::too_many_arguments)]
    fn get_or_open(
        &mut self,
        main: &Store,
        logical: &Store,
        logical_head: u64,
        receipt_branch: Uuid,
        commit: u64,
        preparation: &Preparation,
        label: &str,
    ) -> Result<&BoundEvidenceView> {
        let key = (receipt_branch, commit);
        if self.views.contains_key(&key) {
            return Ok(self.views.get(&key).expect("checked present"));
        }
        if self.views.len() >= MAX_CACHED_VIEWS
            && let Some(evict) = self.order.pop_front()
        {
            self.views.remove(&evict);
        }
        let view = Self::open_publication_view(
            main,
            logical,
            logical_head,
            receipt_branch,
            commit,
            preparation,
            label,
        )?;
        self.order.push_back(key);
        self.views.insert(key, view);
        Ok(self.views.get(&key).expect("just inserted"))
    }
}

/// Refuse re-authentication when the dataset has not activated configuration history,
/// matching C25's requirement for independently authenticated historical configurations.
pub fn require_configuration_history(main: &Store) -> Result<()> {
    if !main.configuration_activated() {
        return Err(managed(
            ManagedErrorKind::ConfigurationHistoryInactive,
            "configuration history is not activated, so re-authentication cannot verify earlier protections; activate it with the closed-store migration",
        ));
    }
    Ok(())
}

fn managed(kind: ManagedErrorKind, message: impl Into<String>) -> Error {
    ManagedError::error(kind, message)
}

/// Classify local preparation errors consistently at the re-authentication boundary:
/// preserve cancellation, timeout, server-wide pool exhaustion, I/O/corruption/storage
/// errors, and already-classified managed errors, while mapping local preparation memory
/// limits and unsupported encodings to `422 incomplete-evidence`.
pub fn classify_error(context: &str, e: Error) -> Error {
    match e {
        Error::Managed(_) | Error::Cancelled | Error::Timeout => e,
        Error::BudgetExceeded(ref b) if b.kind == BudgetKind::ServerMemory => e,
        Error::BudgetExceeded(_) | Error::Unsupported(_) => managed(
            ManagedErrorKind::IncompleteEvidence,
            format!("{context}: verification exceeded the preparation limit"),
        ),
        e @ (Error::Io(_) | Error::Corrupt(_) | Error::StorageFull(_)) => e,
        other => managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{context}: {other}"),
        ),
    }
}

fn classify(context: &str, e: Error) -> Error {
    classify_error(context, e)
}

/// The payload digest of a re-authentication request, compared when its idempotency key
/// repeats.
pub fn payload_digest(
    graph: &NamedNode,
    records: Option<&[RecordCheck]>,
    control: &Control,
) -> Result<Sha256Digest> {
    let mut frame = FrameHash::new(b"sparkles-evidence-reauthenticate-v1", control)?;
    frame.field(1, graph.as_str().as_bytes())?;
    match records {
        None => {
            frame.field(2, &[0])?;
        }
        Some(list) => {
            frame.field(2, &[1])?;
            frame.field(3, &(list.len() as u64).to_be_bytes())?;
            for item in list {
                control.check()?;
                frame.field(4, item.iri.as_str().as_bytes())?;
                match item.expected_digest {
                    Some(d) => frame.field(5, d.as_bytes())?,
                    None => frame.field(5, &[])?,
                }
            }
        }
    }
    frame.finish()
}

/// Discover every subject typed with a profile [`RecordKind`] in `graph`, sorted, up to
/// `limit`. Refuses with `incomplete-evidence` if `graph` holds more than `limit`
/// profile records. Returns the [`Reservation`] keeping the discovered IRIs charged for
/// as long as the caller retains them.
pub fn discover_records(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    limit: usize,
) -> Result<(Vec<NamedNode>, Reservation)> {
    let label = format!("graph <{}>", graph.as_str());
    let mut found = BTreeSet::new();
    let mut held = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify(&label, e))?;
    for kind in KINDS {
        reads.preparation().check()?;
        let class = Term::NamedNode(NamedNode::new_unchecked(kind_iri(kind)));
        let mut scan_held = reads
            .preparation()
            .reserve(0)
            .map_err(|e| classify(&label, e))?;
        let (subjects, more) = reads
            .subjects_up_to_held(RDF_TYPE, &class, graph.as_str(), limit, &mut scan_held)
            .map_err(|e| classify(&label, e))?;
        if more {
            return Err(managed(
                ManagedErrorKind::IncompleteEvidence,
                format!(
                    "graph <{}> holds more than {limit} records; name records explicitly to reconcile in batches",
                    graph.as_str()
                ),
            ));
        }
        for s in subjects {
            if !found.contains(&s) {
                if found.len() >= limit {
                    return Err(managed(
                        ManagedErrorKind::IncompleteEvidence,
                        format!(
                            "graph <{}> holds more than {limit} records; name records explicitly to reconcile in batches",
                            graph.as_str()
                        ),
                    ));
                }
                held.grow_by((s.as_str().len() as u64).saturating_add(96))
                    .map_err(|e| classify(&label, e))?;
                found.insert(s);
            }
        }
    }
    held.grow_by((found.len() * std::mem::size_of::<NamedNode>()) as u64)
        .map_err(|e| classify(&label, e))?;
    let out: Vec<NamedNode> = found.into_iter().collect();
    Ok((out, held))
}

/// Valid `xsd:dateTime` with an explicit timezone offset.
fn valid_date_time(lit: &Literal) -> bool {
    if lit.datatype() != xsd::DATE_TIME {
        return false;
    }
    lit.value()
        .parse::<DateTime>()
        .ok()
        .is_some_and(|dt| dt.timezone_offset().is_some())
}

/// Estimate the heap footprint of a decoded term so retained immutable triples stay
/// charged to the preparation budget.
fn term_estimate(t: &Term) -> u64 {
    match t {
        Term::NamedNode(n) => (n.as_str().len() as u64).saturating_add(48),
        Term::BlankNode(b) => (b.as_str().len() as u64).saturating_add(48),
        Term::Literal(l) => (l.value().len() as u64)
            .saturating_add(l.datatype().as_str().len() as u64)
            .saturating_add(l.language().map_or(0, str::len) as u64)
            .saturating_add(96),
        Term::Triple(tr) => {
            let s = match &tr.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str().len() as u64,
                NamedOrBlankNode::BlankNode(b) => b.as_str().len() as u64,
            };
            s.saturating_add(tr.predicate.as_str().len() as u64)
                .saturating_add(term_estimate(&tr.object))
                .saturating_add(96)
        }
    }
}

type SubjectPairs = (Vec<(Id, Id)>, Reservation, bool);

/// Scan all `(predicate, object)` id pairs of `subject` in `graph`, bounded by `limit`.
/// Looks up `subject` without inserting absent or one-shot IRIs into `Reads::ids`, and
/// returns the [`Reservation`] keeping the id vector charged until the caller drops it.
fn scan_subject_pairs(
    reads: &mut Reads<'_>,
    subject: &str,
    graph: &str,
    limit: usize,
) -> Result<SubjectPairs> {
    let mut held = reads.preparation().reserve(0)?;
    let Some(g_id) = reads.id(graph)? else {
        return Ok((Vec::new(), held, false));
    };
    let Some(s_id) = reads.preparation().lookup_iri(reads.snap(), subject)? else {
        return Ok((Vec::new(), held, false));
    };
    let preparation = reads.preparation();
    let snap = reads.snap();
    let mut pairs = Vec::new();
    let mut more = false;
    preparation.scan(snap, Perm::Gspo, &[g_id.0, s_id.0], |q| {
        if pairs.len() == limit {
            more = true;
            return Ok(false);
        }
        if pairs.len() == pairs.capacity() {
            const CHUNK: usize = 256;
            held.grow_by((CHUNK * std::mem::size_of::<(Id, Id)>()) as u64)?;
            pairs.reserve_exact(CHUNK);
        }
        pairs.push((q[1], q[2]));
        Ok(true)
    })?;
    Ok((pairs, held, more))
}

fn decode_predicate(reads: &Reads<'_>, p_id: Id) -> Result<Option<NamedNode>> {
    reads
        .preparation()
        .with_term(reads.snap(), p_id, |t| match t {
            Some(Term::NamedNode(n)) => Ok(Some(n.clone())),
            _ => Ok(None),
        })
}

fn decode_term(reads: &Reads<'_>, o_id: Id) -> Result<Option<Term>> {
    reads
        .preparation()
        .with_term(reads.snap(), o_id, |t| Ok(t.cloned()))
}

/// Parse a canonical lowercase hyphenated UUID literal (`xsd:string`). Refuses
/// noncanonical spellings (such as uppercase hex) so `Uuid::to_string()` delete quads
/// always match the exact stored triple.
fn parse_canonical_uuid_str(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s).ok().filter(|u| u.to_string() == s)
}

fn parse_canonical_uuid_term(t: &Term) -> Option<Uuid> {
    match t {
        Term::Literal(l) if l.datatype().as_str() == XSD_STRING => {
            parse_canonical_uuid_str(l.value())
        }
        _ => None,
    }
}

fn parse_digest_term(t: &Term) -> Option<Sha256Digest> {
    match t {
        Term::Literal(l) if l.datatype().as_str() == XSD_STRING => l.value().parse().ok(),
        _ => None,
    }
}

/// Read the single canonical `ev:graphProtection` UUID of `s` in `g`.
fn canonical_nonce_of(reads: &mut Reads<'_>, s: &str, g: &str) -> Result<Option<Uuid>> {
    Ok(reads
        .one_string(s, &format!("{EV}graphProtection"), g)?
        .and_then(|v| parse_canonical_uuid_str(&v)))
}

/// Collect a subject's immutable triples (and server-owned fields) from `reads` in `graph`.
struct ScannedRecord {
    kind: RecordKind,
    receipt_iri: Option<NamedNode>,
    generated_by: Option<NamedNode>,
    record_nonce: Option<Uuid>,
    recorded_digest: Option<Sha256Digest>,
    first_available_at: Option<Literal>,
    invalidated_at: Option<Literal>,
    content_digest: Option<Sha256Digest>,
    content_digest_count: usize,
    retained_bytes_id: Option<Id>,
    normalized_text_id: Option<Id>,
    has_blank_expected_triple: bool,
    immutable_triples: Vec<Triple>,
    triples_held: Reservation,
}

fn scan_record(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    iri: &NamedNode,
    label: &str,
) -> Result<ScannedRecord> {
    let (pairs, _pairs_held, more) =
        scan_subject_pairs(reads, iri.as_str(), graph.as_str(), MAX_RECORD_TRIPLES)
            .map_err(|e| classify(label, e))?;
    if more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            format!("{label} exceeds {MAX_RECORD_TRIPLES} triples"),
        ));
    }
    if pairs.is_empty() {
        return Err(managed(
            ManagedErrorKind::EvidenceUnavailable,
            format!("{label} was not found in graph <{}>", graph.as_str()),
        ));
    }
    let mut total_triples = pairs.len();

    // Determine the single profile RecordKind of the subject in `graph`.
    let mut found_kind: Option<RecordKind> = None;
    for kind in KINDS {
        if reads
            .contains(
                iri.as_str(),
                RDF_TYPE,
                &Term::NamedNode(NamedNode::new_unchecked(kind_iri(kind))),
                graph.as_str(),
            )
            .map_err(|e| classify(label, e))?
        {
            if found_kind.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one evidence record kind"),
                ));
            }
            found_kind = Some(kind);
        }
    }
    let Some(kind) = found_kind else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label} has no evidence record kind in graph <{}>",
                graph.as_str()
            ),
        ));
    };

    let p_receipt = format!("{EV}availabilityReceipt");
    let p_generated_by = format!("{PROV}wasGeneratedBy");
    let p_graph_protection = format!("{EV}graphProtection");
    let p_record_digest = format!("{EV}recordDigest");
    let p_first_available = format!("{EV}firstAvailableAt");
    let p_invalidated_at = format!("{PROV}invalidatedAtTime");
    let p_retained_bytes = format!("{EV}retainedBytes");
    let p_normalized_text = format!("{EV}normalizedText");
    let p_content_digest = format!("{EV}contentDigest");
    let p_expected_triple = format!("{EV}expectedTriple");

    let mut receipt_iri: Option<NamedNode> = None;
    let mut generated_by: Option<NamedNode> = None;
    let mut record_nonce: Option<Uuid> = None;
    let mut recorded_digest: Option<Sha256Digest> = None;
    let mut first_available_at: Option<Literal> = None;
    let mut invalidated_at: Option<Literal> = None;
    let mut content_digest: Option<Sha256Digest> = None;
    let mut content_digest_count: usize = 0;
    let mut retained_bytes_id: Option<Id> = None;
    let mut normalized_text_id: Option<Id> = None;
    let mut expected_triple_count: usize = 0;
    let mut has_blank_expected_triple = false;

    let mut immutable_triples: Vec<Triple> = Vec::new();
    let mut triples_held: Reservation = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify(label, e))?;

    for (p_id, o_id) in pairs {
        reads.preparation().check()?;
        let Some(pred) = decode_predicate(reads, p_id).map_err(|e| classify(label, e))? else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label} has a non-IRI predicate"),
            ));
        };
        let p = pred.as_str();

        // Large retained payloads are verified against `ev:contentDigest` without
        // copying them into `immutable_triples`; only predicate presence enters the
        // record digest (see `write::record_digest`).
        if p == p_retained_bytes {
            if !matches!(kind, RecordKind::SourceRevision | RecordKind::Rendition) {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} carries retainedBytes for an unsupported record kind"),
                ));
            }
            if retained_bytes_id.is_some() || normalized_text_id.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one retained payload"),
                ));
            }
            retained_bytes_id = Some(o_id);
            triples_held.grow_by(128).map_err(|e| classify(label, e))?;
            immutable_triples.push(Triple::new(iri.clone(), pred, string("")));
            continue;
        }
        if p == p_normalized_text {
            if kind != RecordKind::Rendition {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} carries normalizedText for a non-rendition record"),
                ));
            }
            if normalized_text_id.is_some() || retained_bytes_id.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one retained payload"),
                ));
            }
            normalized_text_id = Some(o_id);
            triples_held.grow_by(128).map_err(|e| classify(label, e))?;
            immutable_triples.push(Triple::new(iri.clone(), pred, string("")));
            continue;
        }

        let Some(obj) = decode_term(reads, o_id).map_err(|e| classify(label, e))? else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label} has an unreadable object term"),
            ));
        };

        if p == p_receipt {
            if receipt_iri.is_some() {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!("{label} has more than one availabilityReceipt"),
                ));
            }
            let Term::NamedNode(op) = obj else {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!("{label} has a non-IRI availabilityReceipt"),
                ));
            };
            receipt_iri = Some(op);
            continue;
        }
        if p == p_generated_by {
            if generated_by.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one wasGeneratedBy"),
                ));
            }
            let Term::NamedNode(op) = obj else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has a non-IRI wasGeneratedBy"),
                ));
            };
            generated_by = Some(op);
            continue;
        }
        if p == p_graph_protection {
            if record_nonce.is_some() {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!("{label} has more than one graphProtection nonce"),
                ));
            }
            let Some(n) = parse_canonical_uuid_term(&obj) else {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!("{label} has an invalid or noncanonical graphProtection nonce"),
                ));
            };
            record_nonce = Some(n);
            continue;
        }
        if p == p_record_digest {
            if recorded_digest.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one recordDigest"),
                ));
            }
            let Some(d) = parse_digest_term(&obj) else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has an invalid recordDigest"),
                ));
            };
            recorded_digest = Some(d);
            continue;
        }
        if p == p_first_available && kind == RecordKind::SourceRevision {
            if first_available_at.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one firstAvailableAt"),
                ));
            }
            let Term::Literal(l) = obj else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has a non-literal firstAvailableAt"),
                ));
            };
            if !valid_date_time(&l) {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has an invalid firstAvailableAt timestamp"),
                ));
            }
            first_available_at = Some(l);
            continue;
        }
        if p == p_invalidated_at && kind == RecordKind::Withdrawal {
            if invalidated_at.is_some() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one invalidatedAtTime"),
                ));
            }
            let Term::Literal(l) = obj else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has a non-literal invalidatedAtTime"),
                ));
            };
            if !valid_date_time(&l) {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has an invalid invalidatedAtTime timestamp"),
                ));
            }
            invalidated_at = Some(l);
            continue;
        }

        if p == p_content_digest {
            content_digest_count += 1;
            content_digest = parse_digest_term(&obj);
        }

        // Blank nodes are refused in immutable record content except inside a stored
        // fact's single `ev:expectedTriple` triple term (C25 proposal decision 4).
        let is_expected_triple = kind == RecordKind::EvidenceRef && p == p_expected_triple;
        if is_expected_triple {
            expected_triple_count += 1;
            if expected_triple_count > 1 {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has more than one ev:expectedTriple"),
                ));
            }
            let Term::Triple(inner) = &obj else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} has a non-triple ev:expectedTriple"),
                ));
            };
            if has_blank(inner) {
                has_blank_expected_triple = true;
            }
        } else if no_blank(label, &obj).is_err() {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label} contains a blank node outside a stored-fact expectedTriple"),
            ));
        }

        let est = (iri.as_str().len() as u64)
            .saturating_add(p.len() as u64)
            .saturating_add(term_estimate(&obj))
            .saturating_add(64);
        triples_held.grow_by(est).map_err(|e| classify(label, e))?;
        immutable_triples.push(Triple::new(iri.clone(), pred, obj));
    }
    drop(_pairs_held);

    // Collect triples of all fragment sub-nodes owned by this record, counting all direct
    // and fragment triples against `MAX_RECORD_TRIPLES`.
    for frag in FRAGMENTS {
        reads.preparation().check()?;
        let sub_str = format!("{}#{frag}", iri.as_str());
        let (sub_pairs, _sub_pairs_held, sub_more) = scan_subject_pairs(
            reads,
            &sub_str,
            graph.as_str(),
            MAX_RECORD_TRIPLES.saturating_sub(total_triples),
        )
        .map_err(|e| classify(label, e))?;
        if sub_more {
            return Err(managed(
                ManagedErrorKind::IncompleteEvidence,
                format!("{label} and its sub-nodes exceed {MAX_RECORD_TRIPLES} triples"),
            ));
        }
        total_triples = total_triples.saturating_add(sub_pairs.len());
        if sub_pairs.is_empty() {
            continue;
        }
        let sub_node = NamedNode::new_unchecked(sub_str);
        for (p_id, o_id) in sub_pairs {
            reads.preparation().check()?;
            let Some(pred) = decode_predicate(reads, p_id).map_err(|e| classify(label, e))? else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} sub-node has a non-IRI predicate"),
                ));
            };
            if FORBIDDEN_FRAGMENT_PREDICATES.contains(&pred.as_str()) {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "{label} sub-node carries forbidden predicate <{}>",
                        pred.as_str()
                    ),
                ));
            }
            let Some(obj) = decode_term(reads, o_id).map_err(|e| classify(label, e))? else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} sub-node has an unreadable object term"),
                ));
            };
            if no_blank(label, &obj).is_err() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} sub-node contains a blank node"),
                ));
            }
            let est = (sub_node.as_str().len() as u64)
                .saturating_add(pred.as_str().len() as u64)
                .saturating_add(term_estimate(&obj))
                .saturating_add(64);
            triples_held.grow_by(est).map_err(|e| classify(label, e))?;
            immutable_triples.push(Triple::new(sub_node.clone(), pred, obj));
        }
    }

    Ok(ScannedRecord {
        kind,
        receipt_iri,
        generated_by,
        record_nonce,
        recorded_digest,
        first_available_at,
        invalidated_at,
        content_digest,
        content_digest_count,
        retained_bytes_id,
        normalized_text_id,
        has_blank_expected_triple,
        immutable_triples,
        triples_held,
    })
}

/// Verify a single candidate record `check` in `graph` on `logical` against its
/// independently authenticated publication view (`BoundEvidenceView`), its recorded
/// `ev:recordDigest`, its retained content and passage spans, and its receipt and audit
/// lineage.
///
/// `other_record_graphs` are the other configured record-role graphs of the dataset
/// (excluding `graph`) where the record must not also exist. `all_record_graphs` are
/// all configured record-role graphs, searched when verifying a passage's rendition or
/// a structured reference's retained revision.
#[allow(clippy::too_many_arguments)]
pub fn verify_candidate(
    reads: &mut Reads<'_>,
    views: &mut PublicationViews,
    main: &Store,
    logical: &Store,
    graph: &NamedNode,
    current_nonce: Uuid,
    other_record_graphs: &[NamedNode],
    all_record_graphs: &[NamedNode],
    check: &RecordCheck,
) -> Result<VerifiedCandidate> {
    let iri = &check.iri;
    let label = format!("record <{}>", iri.as_str());
    reads.preparation().check()?;

    // A record identity must not be duplicated in any other record graph.
    for other in other_record_graphs {
        if reads
            .has_subject_in(iri.as_str(), other.as_str())
            .map_err(|e| classify(&label, e))?
        {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label} is also stored in graph <{}>", other.as_str()),
            ));
        }
    }

    let scanned = scan_record(reads, graph, iri, &label)?;
    let kind = scanned.kind;

    // Verify mandatory server-owned fields on the record.
    let Some(receipt) = scanned.receipt_iri else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label} has no availabilityReceipt"),
        ));
    };
    if scanned.generated_by.as_ref() != Some(&receipt) {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: prov:wasGeneratedBy does not match ev:availabilityReceipt"),
        ));
    }
    let Some(record_nonce) = scanned.record_nonce else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label} has no graphProtection nonce"),
        ));
    };
    let Some(recorded_digest) = scanned.recorded_digest else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label} has no recorded ev:recordDigest"),
        ));
    };

    // Recompute the canonical record digest and check against the recorded digest.
    // A legacy pre-fix `EvidenceRef` with blank nodes in `ev:expectedTriple` may carry
    // a digest computed over caller blank labels before storage relabeling; we allow it
    // only if verified against its original publication view below.
    let control = reads.preparation().control();
    let computed_digest = record_digest(kind, &scanned.immutable_triples, control)
        .map_err(|e| classify(&label, e))?;
    let needs_legacy_blank_check = if computed_digest != recorded_digest {
        if scanned.has_blank_expected_triple {
            true
        } else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: stored content digest {computed_digest} does not match recorded digest {recorded_digest}"
                ),
            ));
        }
    } else {
        false
    };

    if let Some(expected) = check.expected_digest
        && recorded_digest != expected
    {
        return Err(managed(
            ManagedErrorKind::TargetChanged,
            format!(
                "{label}: recorded digest {recorded_digest} differs from expected digest {expected}"
            ),
        ));
    }

    // Verify retained content payloads against `ev:contentDigest`.
    if let Some(o_id) = scanned.retained_bytes_id {
        if scanned.content_digest_count != 1 {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: retainedBytes requires a single ev:contentDigest"),
            ));
        }
        let Some(expected_content) = scanned.content_digest else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: invalid ev:contentDigest"),
            ));
        };
        verify_retained_bytes(reads, o_id, expected_content, &label)?;
    }
    if let Some(o_id) = scanned.normalized_text_id {
        if scanned.content_digest_count != 1 {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: normalizedText requires a single ev:contentDigest"),
            ));
        }
        let Some(expected_content) = scanned.content_digest else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: invalid ev:contentDigest"),
            ));
        };
        verify_normalized_text(reads, o_id, expected_content, &label)?;
    }

    // Drop the retained immutable triples buffer before further lookups.
    drop(scanned.immutable_triples);
    drop(scanned.triples_held);

    // Verify the operation receipt in `graph`.
    let op = verify_receipt_operation(reads, main, graph, &receipt, &label)?;

    // Server-stamped creation/invalidation timestamps must still match the operation's
    // recorded end time.
    if kind == RecordKind::SourceRevision {
        let Some(fa) = &scanned.first_available_at else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: SourceRevision has no ev:firstAvailableAt"),
            ));
        };
        if *fa != op.ended_at {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: ev:firstAvailableAt does not match the receipt operation's prov:endedAtTime"
                ),
            ));
        }
    }
    if kind == RecordKind::Withdrawal {
        let Some(inv) = &scanned.invalidated_at else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: Withdrawal has no prov:invalidatedAtTime"),
            ));
        };
        if *inv != op.ended_at {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: prov:invalidatedAtTime does not match the receipt operation's prov:endedAtTime"
                ),
            ));
        }
    }

    let already_authenticated = record_nonce == current_nonce && op.nonce == current_nonce;
    let logical_head = reads.snap().commit;

    // Independently establish from the retained, configuration-bound publication view
    // that `iri` and `receipt` were genuinely published together at `op.commit` while
    // `graph` was protected, with exact matching digest, complete non-nonce receipt
    // triple set, receipt metadata and commit timestamp. This stops detached edits that
    // replace content + digest together, plant records citing an earlier receipt, plant
    // extra triples onto a receipt IRI, forge a receipt at a detached commit, or modify
    // both timestamps together.
    if !already_authenticated || needs_legacy_blank_check {
        let orig_nonce = verify_publication_view(
            reads,
            views,
            main,
            logical,
            logical_head,
            graph,
            iri,
            kind,
            &receipt,
            &op,
            recorded_digest,
            computed_digest,
            needs_legacy_blank_check,
            scanned.first_available_at.as_ref(),
            scanned.invalidated_at.as_ref(),
            &label,
        )?;

        if !already_authenticated {
            // Verify protection-nonce lineage for both the record and its receipt across
            // one or more detach / partial-reconciliation cycles.
            let record_lineage_ok = record_nonce == orig_nonce
                || audit_moved_subject(
                    reads,
                    views,
                    main,
                    logical,
                    logical_head,
                    graph,
                    other_record_graphs,
                    &receipt,
                    Some(iri),
                    record_nonce,
                    current_nonce,
                    &label,
                )?;
            if !record_lineage_ok {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!(
                        "{label}: record protection nonce does not match its original protection or an authenticated reconciliation"
                    ),
                ));
            }
            let op_lineage_ok = op.nonce == orig_nonce
                || audit_moved_subject(
                    reads,
                    views,
                    main,
                    logical,
                    logical_head,
                    graph,
                    other_record_graphs,
                    &receipt,
                    None,
                    op.nonce,
                    current_nonce,
                    &label,
                )?;
            if !op_lineage_ok {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!(
                        "{label}: receipt operation protection nonce does not match its original protection or an authenticated reconciliation"
                    ),
                ));
            }
        }
    }

    // Kind-specific retained evidence checks.
    if kind == RecordKind::Passage {
        verify_passage_content(reads, graph, iri, all_record_graphs, &label)?;
    } else if kind == RecordKind::EvidenceRef {
        verify_structured_reference(reads, graph, iri, all_record_graphs, &label)?;
    }

    Ok(VerifiedCandidate {
        iri: iri.clone(),
        kind,
        digest: recorded_digest,
        receipt,
        record_nonce,
        operation_nonce: op.nonce,
        already_authenticated,
    })
}

struct VerifiedOp {
    branch_id: Uuid,
    commit: u64,
    agent: NamedNode,
    payload_digest: Sha256Digest,
    nonce: Uuid,
    ended_at: Literal,
}

fn verify_receipt_operation(
    reads: &mut Reads<'_>,
    main: &Store,
    graph: &NamedNode,
    receipt: &NamedNode,
    label: &str,
) -> Result<VerifiedOp> {
    let g = graph.as_str();
    let op = receipt.as_str();
    if !reads.is_operation(op, g).map_err(|e| classify(label, e))?
        || !reads
            .contains(op, RDF_TYPE, &Term::NamedNode(prov("Activity")), g)
            .map_err(|e| classify(label, e))?
    {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> is not an ev:Operation in graph <{g}>"),
        ));
    }
    let ds_id = reads
        .one_string(op, &format!("{EV}datasetId"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| Uuid::parse_str(&v).ok());
    if ds_id != Some(main.dataset_id()) {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> does not name this dataset"),
        ));
    }
    let branch_id = reads
        .one_string(op, &format!("{EV}branchId"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| Uuid::parse_str(&v).ok());
    let Some(branch_id) = branch_id else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid branchId"),
        ));
    };
    if !main
        .branch_known_prepared(branch_id, reads.preparation())
        .map_err(|e| classify(label, e))?
    {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> names an unknown branch"),
        ));
    }
    let commit = match reads
        .single(op, &format!("{EV}commit"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::Literal(l)) => integer(&l),
        _ => None,
    };
    let Some(commit) = commit else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid commit"),
        ));
    };
    let Some(Term::NamedNode(agent)) = reads
        .single(op, &format!("{PROV}wasAssociatedWith"), g)
        .map_err(|e| classify(label, e))?
    else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no associated principal"),
        ));
    };
    let ended_at = match reads
        .single(op, &format!("{PROV}endedAtTime"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::Literal(l)) if valid_date_time(&l) => l,
        _ => {
            return Err(managed(
                ManagedErrorKind::OriginNotReproduced,
                format!("{label}: receipt <{op}> has no valid endedAtTime"),
            ));
        }
    };
    let payload_digest = reads
        .one_string(op, &format!("{EV}payloadDigest"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| v.parse::<Sha256Digest>().ok());
    let Some(payload_digest) = payload_digest else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid payloadDigest"),
        ));
    };
    let Some(nonce) = canonical_nonce_of(reads, op, g).map_err(|e| classify(label, e))? else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid canonical graphProtection nonce"),
        ));
    };
    Ok(VerifiedOp {
        branch_id,
        commit,
        agent,
        payload_digest,
        nonce,
        ended_at,
    })
}

/// Immutable identity and stamp of a verified reconciliation audit `ev:Operation`, shared
/// between idempotency replay and multi-detach lineage verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAuditOp {
    pub branch_id: Uuid,
    pub commit: u64,
    pub agent: NamedNode,
    pub payload_digest: Sha256Digest,
    pub nonce: Uuid,
    pub ended_at: Literal,
}

/// Validate a reconciliation audit `ev:Operation`'s immutable stamp and cross-graph
/// subject uniqueness in `graph`.
pub fn verify_audit_operation(
    reads: &mut Reads<'_>,
    main: &Store,
    graph: &NamedNode,
    op_iri: &NamedNode,
    other_record_graphs: &[NamedNode],
    expected_principal: Option<&NamedNode>,
    label: &str,
) -> Result<VerifiedAuditOp> {
    let g = graph.as_str();
    let op = op_iri.as_str();
    for other in other_record_graphs {
        if reads
            .has_subject_in(op, other.as_str())
            .map_err(|e| classify(label, e))?
        {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: audit operation <{op}> also has triples in graph <{}>",
                    other.as_str()
                ),
            ));
        }
    }
    if !reads.is_operation(op, g).map_err(|e| classify(label, e))?
        || !reads
            .contains(op, RDF_TYPE, &Term::NamedNode(prov("Activity")), g)
            .map_err(|e| classify(label, e))?
    {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> is not an ev:Operation in graph <{g}>"),
        ));
    }
    let kind_ok = match reads
        .single(op, EV_OPERATION_KIND, g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::NamedNode(n)) => n.as_str() == OPERATION_KIND_REAUTH,
        _ => false,
    };
    let graph_ok = match reads
        .single(op, EV_REAUTHENTICATED_GRAPH, g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::NamedNode(n)) => &n == graph,
        _ => false,
    };
    if !kind_ok || !graph_ok {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: operation <{op}> does not carry valid reconciliation audit provenance"
            ),
        ));
    }
    let ds_id = reads
        .one_string(op, &format!("{EV}datasetId"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| Uuid::parse_str(&v).ok());
    if ds_id != Some(main.dataset_id()) {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> does not name this dataset"),
        ));
    }
    let branch_id = reads
        .one_string(op, &format!("{EV}branchId"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| Uuid::parse_str(&v).ok());
    let Some(branch_id) = branch_id else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> has no valid branchId"),
        ));
    };
    if !main
        .branch_known_prepared(branch_id, reads.preparation())
        .map_err(|e| classify(label, e))?
    {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> names an unknown branch"),
        ));
    }
    let commit = match reads
        .single(op, &format!("{EV}commit"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::Literal(l)) => integer(&l),
        _ => None,
    };
    let Some(commit) = commit else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> has no valid commit"),
        ));
    };
    let Some(Term::NamedNode(agent)) = reads
        .single(op, &format!("{PROV}wasAssociatedWith"), g)
        .map_err(|e| classify(label, e))?
    else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> has no associated principal"),
        ));
    };
    if let Some(expected) = expected_principal
        && &agent != expected
    {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> principal does not match"),
        ));
    }
    let ended_at = match reads
        .single(op, &format!("{PROV}endedAtTime"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::Literal(l)) if valid_date_time(&l) => l,
        _ => {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: audit operation <{op}> has no valid endedAtTime"),
            ));
        }
    };
    let payload_digest = reads
        .one_string(op, &format!("{EV}payloadDigest"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| v.parse::<Sha256Digest>().ok());
    let Some(payload_digest) = payload_digest else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> has no valid payloadDigest"),
        ));
    };
    let Some(nonce) = canonical_nonce_of(reads, op, g).map_err(|e| classify(label, e))? else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: audit operation <{op}> has no valid canonical graphProtection nonce"),
        ));
    };
    Ok(VerifiedAuditOp {
        branch_id,
        commit,
        agent,
        payload_digest,
        nonce,
        ended_at,
    })
}

type ReceiptTriples = (HashSet<(NamedNode, Term)>, Reservation);

/// Scan the complete non-`ev:graphProtection` triple set of `receipt` in `graph`,
/// bounded by `MAX_RECORD_TRIPLES`, charging all retained terms to the preparation
/// allowance.
fn scan_receipt_triples(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    receipt: &NamedNode,
    label: &str,
) -> Result<ReceiptTriples> {
    let p_graph_protection = format!("{EV}graphProtection");
    let (pairs, pairs_held, more) =
        scan_subject_pairs(reads, receipt.as_str(), graph.as_str(), MAX_RECORD_TRIPLES)
            .map_err(|e| classify(label, e))?;
    if more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            format!(
                "{label}: receipt <{}> exceeds {MAX_RECORD_TRIPLES} triples",
                receipt.as_str()
            ),
        ));
    }
    let mut held = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify(label, e))?;
    let mut out = HashSet::new();
    for (p_id, o_id) in pairs {
        reads.preparation().check()?;
        let Some(pred) = decode_predicate(reads, p_id).map_err(|e| classify(label, e))? else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: receipt <{}> has a non-IRI predicate",
                    receipt.as_str()
                ),
            ));
        };
        if pred.as_str() == p_graph_protection {
            continue;
        }
        let Some(obj) = decode_term(reads, o_id).map_err(|e| classify(label, e))? else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: receipt <{}> has an unreadable object term",
                    receipt.as_str()
                ),
            ));
        };
        if no_blank(label, &obj).is_err() {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: receipt <{}> contains a blank node",
                    receipt.as_str()
                ),
            ));
        }
        let est = (pred.as_str().len() as u64)
            .saturating_add(term_estimate(&obj))
            .saturating_add(96);
        held.grow_by(est).map_err(|e| classify(label, e))?;
        out.insert((pred, obj));
    }
    drop(pairs_held);
    Ok((out, held))
}

/// Verify `iri` and `receipt` against the retained, independently authenticated
/// `BoundEvidenceView` at `op.commit`, returning the graph's original protection nonce
/// at that commit.
#[allow(clippy::too_many_arguments)]
fn verify_publication_view(
    reads: &mut Reads<'_>,
    views: &mut PublicationViews,
    main: &Store,
    logical: &Store,
    logical_head: u64,
    graph: &NamedNode,
    iri: &NamedNode,
    kind: RecordKind,
    receipt: &NamedNode,
    op: &VerifiedOp,
    recorded_digest: Sha256Digest,
    computed_digest: Sha256Digest,
    needs_legacy_blank_check: bool,
    first_available_at: Option<&Literal>,
    invalidated_at: Option<&Literal>,
    label: &str,
) -> Result<Uuid> {
    let preparation = reads.preparation();
    let pub_view = views.get_or_open(
        main,
        logical,
        logical_head,
        op.branch_id,
        op.commit,
        preparation,
        label,
    )?;
    let g = graph.as_str();

    // 1. At `op.commit`, `graph` must have been protected by the evidence owner under a
    //    specific protection nonce. A detached write commit fails here.
    let orig_nonce = pub_view
        .owner(OwnerKind::Evidence)
        .filter(|e| e.graphs.iter().any(|managed_g| managed_g == g))
        .and_then(|e| e.protections.get(g).copied())
        .ok_or_else(|| {
            managed(
                ManagedErrorKind::OriginNotReproduced,
                format!(
                    "{label}: graph <{g}> was not protected by the evidence profile at commit {}",
                    op.commit
                ),
            )
        })?;

    // 2. The receipt's recorded timestamp must match the immutable commit timestamp.
    let commit_time = date_time(pub_view.commit().timestamp_ms);
    if op.ended_at != commit_time {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: receipt endedAtTime does not match commit {} timestamp",
                op.commit
            ),
        ));
    }

    // 3. Verify `receipt` existed in `graph` at `op.commit` under `orig_nonce` with
    //    identical immutable metadata AND identical complete non-`ev:graphProtection`
    //    triple set so detached writes planted onto a receipt IRI can never gain
    //    `current_nonce`.
    let mut hist_reads = Reads::from_evidence(preparation, pub_view);
    let hist_op = verify_receipt_operation(&mut hist_reads, main, graph, receipt, label)?;
    if hist_op.nonce != orig_nonce {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!(
                "{label}: receipt <{}> did not carry graph protection nonce at commit {}",
                receipt.as_str(),
                op.commit
            ),
        ));
    }
    if hist_op.branch_id != op.branch_id
        || hist_op.commit != op.commit
        || hist_op.agent != op.agent
        || hist_op.payload_digest != op.payload_digest
        || hist_op.ended_at != op.ended_at
    {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: receipt <{}> metadata differs from publication commit {}",
                receipt.as_str(),
                op.commit
            ),
        ));
    }
    let (current_receipt_triples, current_receipt_held) =
        scan_receipt_triples(reads, graph, receipt, label)?;
    let (hist_receipt_triples, hist_receipt_held) =
        scan_receipt_triples(&mut hist_reads, graph, receipt, label)?;
    if current_receipt_triples != hist_receipt_triples {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: receipt <{}> triples differ from publication commit {}",
                receipt.as_str(),
                op.commit
            ),
        ));
    }
    drop(current_receipt_triples);
    drop(current_receipt_held);
    drop(hist_receipt_triples);
    drop(hist_receipt_held);

    // 4. Verify `iri` existed in `graph` at `op.commit` under `orig_nonce` with matching
    //    kind, receipt, `ev:recordDigest`, and server-owned timestamps.
    if !hist_reads
        .has_subject_in(iri.as_str(), g)
        .map_err(|e| classify(label, e))?
    {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!(
                "{label}: record did not exist at publication commit {}",
                op.commit
            ),
        ));
    }
    let hist_kind_ok = hist_reads
        .contains(
            iri.as_str(),
            RDF_TYPE,
            &Term::NamedNode(NamedNode::new_unchecked(kind_iri(kind))),
            g,
        )
        .map_err(|e| classify(label, e))?;
    if !hist_kind_ok {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: record kind differs from publication commit {}",
                op.commit
            ),
        ));
    }
    let hist_receipt = hist_reads
        .receipt_of(iri, graph)
        .map_err(|e| classify(label, e))?;
    if hist_receipt.as_ref() != Some(receipt) {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!(
                "{label}: availabilityReceipt differs from publication commit {}",
                op.commit
            ),
        ));
    }
    let hist_gen = match hist_reads
        .single(iri.as_str(), &format!("{PROV}wasGeneratedBy"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::NamedNode(n)) => Some(n),
        _ => None,
    };
    if hist_gen.as_ref() != Some(receipt) {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!(
                "{label}: prov:wasGeneratedBy differs from publication commit {}",
                op.commit
            ),
        ));
    }
    let hist_nonce =
        canonical_nonce_of(&mut hist_reads, iri.as_str(), g).map_err(|e| classify(label, e))?;
    if hist_nonce != Some(orig_nonce) {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!(
                "{label}: record did not carry graph protection nonce at publication commit {}",
                op.commit
            ),
        ));
    }
    let hist_digest = hist_reads
        .one_string(iri.as_str(), &format!("{EV}recordDigest"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| v.parse::<Sha256Digest>().ok());
    if hist_digest != Some(recorded_digest) {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: ev:recordDigest differs from publication commit {}",
                op.commit
            ),
        ));
    }
    if kind == RecordKind::SourceRevision {
        let hist_fa = match hist_reads
            .single(iri.as_str(), &format!("{EV}firstAvailableAt"), g)
            .map_err(|e| classify(label, e))?
        {
            Some(Term::Literal(l)) => Some(l),
            _ => None,
        };
        if hist_fa.as_ref() != first_available_at {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: ev:firstAvailableAt differs from publication commit {}",
                    op.commit
                ),
            ));
        }
    }
    if kind == RecordKind::Withdrawal {
        let hist_inv = match hist_reads
            .single(iri.as_str(), &format!("{PROV}invalidatedAtTime"), g)
            .map_err(|e| classify(label, e))?
        {
            Some(Term::Literal(l)) => Some(l),
            _ => None,
        };
        if hist_inv.as_ref() != invalidated_at {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: prov:invalidatedAtTime differs from publication commit {}",
                    op.commit
                ),
            ));
        }
    }

    // 5. Legacy blank-node `ev:expectedTriple` fallback: a record published before
    //    canonical blank-node digesting hashed raw caller labels into `ev:recordDigest`.
    //    Verify that the current canonicalized immutable triples match the historical
    //    canonicalized immutable triples from `pub_view` byte-for-byte.
    if needs_legacy_blank_check {
        let hist_scanned = scan_record(&mut hist_reads, graph, iri, label)?;
        let hist_computed =
            record_digest(kind, &hist_scanned.immutable_triples, preparation.control())
                .map_err(|e| classify(label, e))?;
        if hist_computed != computed_digest {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: stored blank-node EvidenceRef content differs from publication commit {}",
                    op.commit
                ),
            ));
        }
    }

    Ok(orig_nonce)
}

/// Check whether an authenticated reconciliation audit operation in `graph` moved
/// `receipt` (and `target_record` when `Some`) to `target_nonce`.
///
/// When `target_nonce == current_nonce`, the audit operation carries `current_nonce` in
/// `graph`, which only a typed reconciliation under the current protection can write.
/// When `target_nonce != current_nonce` (an earlier detach/re-protect epoch), the audit
/// operation is independently verified against its own retained, configuration-bound
/// publication view at `audit_commit`.
#[allow(clippy::too_many_arguments)]
fn audit_moved_subject(
    reads: &mut Reads<'_>,
    views: &mut PublicationViews,
    main: &Store,
    logical: &Store,
    logical_head: u64,
    graph: &NamedNode,
    other_record_graphs: &[NamedNode],
    receipt: &NamedNode,
    target_record: Option<&NamedNode>,
    target_nonce: Uuid,
    current_nonce: Uuid,
    label: &str,
) -> Result<bool> {
    let g = graph.as_str();
    let mut scan_held = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify(label, e))?;
    let (subjects, more) = reads
        .subjects_up_to_held(
            &format!("{PROV}wasInformedBy"),
            &Term::NamedNode(receipt.clone()),
            g,
            MAX_REAUTHENTICATE,
            &mut scan_held,
        )
        .map_err(|e| classify(label, e))?;
    if more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            format!("{label}: too many reconciliation links on receipt"),
        ));
    }

    let mut candidates: Vec<(NamedNode, VerifiedAuditOp)> = Vec::new();
    let mut cand_held = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify(label, e))?;

    for s in subjects {
        let s_str = s.as_str();
        let audit = match verify_audit_operation(
            reads,
            main,
            graph,
            &s,
            other_record_graphs,
            None,
            label,
        ) {
            Ok(audit) => audit,
            Err(Error::Managed(m)) if m.kind == ManagedErrorKind::ImmutableRecordConflict => {
                continue;
            }
            Err(e) => return Err(e),
        };
        if audit.nonce != target_nonce {
            continue;
        }
        if let Some(rec) = target_record
            && !reads
                .contains(
                    s_str,
                    &format!("{PROV}used"),
                    &Term::NamedNode(rec.clone()),
                    g,
                )
                .map_err(|e| classify(label, e))?
        {
            continue;
        }
        if target_nonce == current_nonce {
            return Ok(true);
        }
        let est = (s_str.len() as u64)
            .saturating_add(audit.agent.as_str().len() as u64)
            .saturating_add(audit.ended_at.value().len() as u64)
            .saturating_add(160);
        cand_held.grow_by(est).map_err(|e| classify(label, e))?;
        candidates.push((s, audit));
    }
    drop(scan_held);

    // For prior epochs (`target_nonce != current_nonce`), verify the audit operation
    // against its own retained publication view so a detached write cannot fabricate or
    // alter an audit operation to bless an unrelated nonce.
    let preparation = reads.preparation();
    for (audit_iri, audit) in candidates {
        let audit_view = match views.get_or_open(
            main,
            logical,
            logical_head,
            audit.branch_id,
            audit.commit,
            preparation,
            label,
        ) {
            Ok(v) => v,
            Err(Error::Managed(m)) if m.kind == ManagedErrorKind::OriginNotReproduced => continue,
            Err(e) => return Err(e),
        };
        let hist_nonce = audit_view
            .owner(OwnerKind::Evidence)
            .filter(|e| e.graphs.iter().any(|mg| mg == g))
            .and_then(|e| e.protections.get(g).copied());
        if hist_nonce != Some(target_nonce) {
            continue;
        }
        if date_time(audit_view.commit().timestamp_ms) != audit.ended_at {
            continue;
        }
        let mut hist_reads = Reads::from_evidence(preparation, audit_view);
        let hist_audit = match verify_audit_operation(
            &mut hist_reads,
            main,
            graph,
            &audit_iri,
            other_record_graphs,
            Some(&audit.agent),
            label,
        ) {
            Ok(ha) => ha,
            Err(Error::Managed(m)) if m.kind == ManagedErrorKind::ImmutableRecordConflict => {
                continue;
            }
            Err(e) => return Err(e),
        };
        if hist_audit.branch_id != audit.branch_id
            || hist_audit.commit != audit.commit
            || hist_audit.ended_at != audit.ended_at
            || hist_audit.payload_digest != audit.payload_digest
            || hist_audit.nonce != target_nonce
        {
            continue;
        }
        let a = audit_iri.as_str();
        let links_ok = hist_reads
            .contains(
                a,
                &format!("{PROV}wasInformedBy"),
                &Term::NamedNode(receipt.clone()),
                g,
            )
            .map_err(|e| classify(label, e))?
            && match target_record {
                Some(rec) => hist_reads
                    .contains(a, &format!("{PROV}used"), &Term::NamedNode(rec.clone()), g)
                    .map_err(|e| classify(label, e))?,
                None => true,
            };
        if links_ok {
            return Ok(true);
        }
    }
    Ok(false)
}

fn verify_retained_bytes(
    reads: &Reads<'_>,
    o_id: Id,
    expected: Sha256Digest,
    label: &str,
) -> Result<()> {
    let preparation = reads.preparation();
    let control = preparation.control();
    let ok = preparation
        .with_typed_literal(reads.snap(), o_id, XSD_BASE64, |raw| {
            let Some(raw) = raw else {
                return Ok(Err("retainedBytes is not an xsd:base64Binary literal"));
            };
            // Stream-decode base64 into SHA-256 using a fixed 48 KiB scratch buffer so
            // payloads up to C25's 16 MiB limit stay well within the 64 MiB allowance.
            let mut decoder = base64::read::DecoderReader::new(
                std::io::Cursor::new(raw.as_bytes()),
                &base64::engine::general_purpose::STANDARD,
            );
            let mut hasher = Sha256::new();
            let mut buf = [0u8; 48 * 1024];
            loop {
                control.check()?;
                match decoder.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => hasher.update(&buf[..n]),
                    Err(_) => return Ok(Err("retainedBytes is not valid base64")),
                }
            }
            let actual = Sha256Digest::from_digest_bytes(hasher.finalize().into());
            if actual != expected {
                return Ok(Err("retainedBytes digest does not match ev:contentDigest"));
            }
            Ok(Ok(()))
        })
        .map_err(|e| classify(label, e))?;
    if let Err(why) = ok {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: {why}"),
        ));
    }
    Ok(())
}

fn verify_normalized_text(
    reads: &Reads<'_>,
    o_id: Id,
    expected: Sha256Digest,
    label: &str,
) -> Result<()> {
    let preparation = reads.preparation();
    let control = preparation.control();
    let ok = preparation
        .with_simple_literal(reads.snap(), o_id, |text| {
            let Some(text) = text else {
                return Ok(Err("normalizedText is not a simple literal"));
            };
            let actual = Sha256Digest::of_bytes(text.as_bytes(), control)?;
            if actual != expected {
                return Ok(Err("normalizedText digest does not match ev:contentDigest"));
            }
            Ok(Ok(()))
        })
        .map_err(|e| classify(label, e))?;
    if let Err(why) = ok {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: {why}"),
        ));
    }
    Ok(())
}

/// Find the unique graph in `all_record_graphs` that holds `subject` and verify it has
/// only `expected_kind`. Refuses missing subjects with `evidence-unavailable` and
/// duplicates or wrong kinds with `immutable-record-conflict`.
fn find_unique_dependency<'a>(
    reads: &mut Reads<'_>,
    subject: &NamedNode,
    expected_kind: RecordKind,
    all_record_graphs: &'a [NamedNode],
    label: &str,
    what: &str,
) -> Result<&'a NamedNode> {
    let mut found: Option<&'a NamedNode> = None;
    for rg in all_record_graphs {
        let rg_str = rg.as_str();
        if !reads
            .has_subject_in(subject.as_str(), rg_str)
            .map_err(|e| classify(label, e))?
        {
            continue;
        }
        if found.is_some() {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: {what} <{}> is duplicated across record graphs",
                    subject.as_str()
                ),
            ));
        }
        let mut matched_kind = false;
        for k in KINDS {
            if reads
                .contains(
                    subject.as_str(),
                    RDF_TYPE,
                    &Term::NamedNode(NamedNode::new_unchecked(kind_iri(k))),
                    rg_str,
                )
                .map_err(|e| classify(label, e))?
            {
                if k != expected_kind {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "{label}: {what} <{}> has unexpected record kind",
                            subject.as_str()
                        ),
                    ));
                }
                matched_kind = true;
            }
        }
        if !matched_kind {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: {what} <{}> is not typed as {what}",
                    subject.as_str()
                ),
            ));
        }
        found = Some(rg);
    }
    found.ok_or_else(|| {
        managed(
            ManagedErrorKind::EvidenceUnavailable,
            format!(
                "{label}: {what} <{}> was not found in any record graph",
                subject.as_str()
            ),
        )
    })
}

/// Verify a passage's `ev:textDigest`, optional `oa:exact` quote, selector bounds, and
/// its rendition's retained `ev:normalizedText` and selected span.
fn verify_passage_content(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    iri: &NamedNode,
    all_record_graphs: &[NamedNode],
    label: &str,
) -> Result<()> {
    let g = graph.as_str();
    let s = iri.as_str();
    let text_digest = reads
        .one_string(s, &format!("{EV}textDigest"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| v.parse::<Sha256Digest>().ok());
    let Some(text_digest) = text_digest else {
        return Err(managed(
            ManagedErrorKind::SpanMismatch,
            format!("{label}: passage has no valid ev:textDigest"),
        ));
    };
    let quote_objs = reads
        .pair_objects(s, &format!("{OA}exact"), g)
        .map_err(|e| classify(label, e))?;
    let quote: Option<String> = match quote_objs.as_slice() {
        [] => None,
        [Term::Literal(l)] if l.datatype().as_str() == XSD_STRING => {
            let actual =
                Sha256Digest::of_bytes(l.value().as_bytes(), reads.preparation().control())
                    .map_err(|e| classify(label, e))?;
            if actual != text_digest {
                return Err(managed(
                    ManagedErrorKind::SpanMismatch,
                    format!("{label}: passage quote does not match ev:textDigest"),
                ));
            }
            Some(l.value().to_string())
        }
        _ => {
            return Err(managed(
                ManagedErrorKind::SpanMismatch,
                format!("{label}: passage quote is missing, non-string or duplicated"),
            ));
        }
    };
    let Some(Term::NamedNode(selector)) = reads
        .single(s, &format!("{OA}hasSelector"), g)
        .map_err(|e| classify(label, e))?
    else {
        return Err(managed(
            ManagedErrorKind::SpanMismatch,
            format!("{label}: passage has no IRI selector"),
        ));
    };
    let start = match reads
        .single(selector.as_str(), &format!("{OA}start"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::Literal(l)) => integer(&l),
        _ => None,
    };
    let end = match reads
        .single(selector.as_str(), &format!("{OA}end"), g)
        .map_err(|e| classify(label, e))?
    {
        Some(Term::Literal(l)) => integer(&l),
        _ => None,
    };
    let (Some(start), Some(end)) = (start, end) else {
        return Err(managed(
            ManagedErrorKind::SpanMismatch,
            format!("{label}: passage selector has no single start and end"),
        ));
    };
    if end < start {
        return Err(managed(
            ManagedErrorKind::SpanMismatch,
            format!("{label}: passage selector ends before it starts"),
        ));
    }
    let Some(Term::NamedNode(rendition)) = reads
        .single(s, &format!("{OA}hasSource"), g)
        .map_err(|e| classify(label, e))?
    else {
        return Err(managed(
            ManagedErrorKind::SpanMismatch,
            format!("{label}: passage has no oa:hasSource rendition"),
        ));
    };

    // The rendition must exist uniquely across all configured record graphs and retain
    // a single `ev:normalizedText`, matching `resolve::verify_passage`.
    let rg = find_unique_dependency(
        reads,
        &rendition,
        RecordKind::Rendition,
        all_record_graphs,
        label,
        "rendition",
    )?;
    let rg_str = rg.as_str();
    let (Some(r_id), Some(p_id), Some(g_id)) = (
        reads
            .preparation()
            .lookup_iri(reads.snap(), rendition.as_str())
            .map_err(|e| classify(label, e))?,
        reads
            .id(&format!("{EV}normalizedText"))
            .map_err(|e| classify(label, e))?,
        reads.id(rg_str).map_err(|e| classify(label, e))?,
    ) else {
        return Err(managed(
            ManagedErrorKind::EvidenceUnavailable,
            format!(
                "{label}: rendition <{}> has no retained normalizedText",
                rendition.as_str()
            ),
        ));
    };
    let mut text_ids = Vec::new();
    reads
        .preparation()
        .scan(reads.snap(), Perm::Gspo, &[g_id.0, r_id.0, p_id.0], |q| {
            text_ids.push(q[2]);
            Ok(text_ids.len() < 2)
        })
        .map_err(|e| classify(label, e))?;
    match text_ids.as_slice() {
        [] => {
            return Err(managed(
                ManagedErrorKind::EvidenceUnavailable,
                format!(
                    "{label}: rendition <{}> has no retained normalizedText",
                    rendition.as_str()
                ),
            ));
        }
        [text_id] => {
            let rend_digest = reads
                .one_string(rendition.as_str(), &format!("{EV}contentDigest"), rg_str)
                .map_err(|e| classify(label, e))?
                .and_then(|v| v.parse::<Sha256Digest>().ok());
            let Some(rend_digest) = rend_digest else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "{label}: rendition <{}> has no valid ev:contentDigest",
                        rendition.as_str()
                    ),
                ));
            };
            let preparation = reads.preparation();
            let control = preparation.control();
            let checked = preparation
                .with_simple_literal(reads.snap(), *text_id, |text| {
                    let Some(text) = text else {
                        return Ok(Err((
                            ManagedErrorKind::ImmutableRecordConflict,
                            "rendition normalizedText is not a simple literal",
                        )));
                    };
                    if Sha256Digest::of_bytes(text.as_bytes(), control)? != rend_digest {
                        return Ok(Err((
                            ManagedErrorKind::ImmutableRecordConflict,
                            "rendition normalizedText does not match its contentDigest",
                        )));
                    }
                    let offsets: &[u64] = if start == end {
                        &[start]
                    } else {
                        &[start, end]
                    };
                    let positions = match locate_boundaries(text, offsets, control) {
                        Ok(positions) => positions,
                        Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
                        Err(_) => {
                            return Ok(Err((
                                ManagedErrorKind::SpanMismatch,
                                "passage selector exceeds rendition normalizedText",
                            )));
                        }
                    };
                    let selected = &text[positions[0]..positions[positions.len() - 1]];
                    if Sha256Digest::of_bytes(selected.as_bytes(), control)? != text_digest {
                        return Ok(Err((
                            ManagedErrorKind::SpanMismatch,
                            "passage textDigest does not match rendition normalizedText",
                        )));
                    }
                    if let Some(q) = &quote
                        && selected != q.as_str()
                    {
                        return Ok(Err((
                            ManagedErrorKind::SpanMismatch,
                            "passage quote does not match rendition normalizedText",
                        )));
                    }
                    Ok(Ok(()))
                })
                .map_err(|e| classify(label, e))?;
            if let Err((err_kind, why)) = checked {
                return Err(managed(err_kind, format!("{label}: {why}")));
            }
        }
        _ => {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: rendition <{}> has more than one normalizedText",
                    rendition.as_str()
                ),
            ));
        }
    }
    Ok(())
}

/// If `iri` is a structured `ev:EvidenceRef`, validate its required fields and, when its
/// `ev:sourceRevision` retains bytes under `JSON_LEXICAL_UTF8_V1`, verify the JSON pointer
/// selection against `ev:valueDigest` under the shared `Preparation` allowance.
fn verify_structured_reference(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    iri: &NamedNode,
    all_record_graphs: &[NamedNode],
    label: &str,
) -> Result<()> {
    let g = graph.as_str();
    let s = iri.as_str();
    let pointers = reads
        .pair_objects(s, &format!("{EV}jsonPointer"), g)
        .map_err(|e| classify(label, e))?;
    let value_digests = reads
        .pair_objects(s, &format!("{EV}valueDigest"), g)
        .map_err(|e| classify(label, e))?;
    if pointers.is_empty() && value_digests.is_empty() {
        // Passage reference or stored-fact reference, not a structured reference.
        return Ok(());
    }
    let pointer = match pointers.as_slice() {
        [Term::Literal(l)] if l.datatype().as_str() == XSD_STRING => l.value().to_string(),
        _ => {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: structured reference must have a single string ev:jsonPointer"),
            ));
        }
    };
    let value_digest = match value_digests.as_slice() {
        [t] => parse_digest_term(t),
        _ => None,
    };
    let Some(value_digest) = value_digest else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: structured reference has no single valid ev:valueDigest"),
        ));
    };
    let methods = reads
        .pair_objects(s, &format!("{EV}method"), g)
        .map_err(|e| classify(label, e))?;
    let method = match methods.as_slice() {
        [Term::NamedNode(n)] => n.clone(),
        _ => {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: structured reference has no single IRI ev:method"),
            ));
        }
    };
    let versions = reads
        .pair_objects(s, &format!("{EV}methodVersion"), g)
        .map_err(|e| classify(label, e))?;
    let method_version = match versions.as_slice() {
        [Term::Literal(l)] if l.datatype().as_str() == XSD_STRING => l.value().to_string(),
        _ => {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: structured reference has no single string ev:methodVersion"),
            ));
        }
    };
    let revisions = reads
        .pair_objects(s, &format!("{EV}sourceRevision"), g)
        .map_err(|e| classify(label, e))?;
    let revision = match revisions.as_slice() {
        [Term::NamedNode(n)] => n.clone(),
        _ => {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: structured reference has no single IRI ev:sourceRevision"),
            ));
        }
    };
    if method.as_str() != JSON_LEXICAL_UTF8_V1 || method_version != JSON_LEXICAL_UTF8_VERSION {
        return Ok(());
    }

    let rg = find_unique_dependency(
        reads,
        &revision,
        RecordKind::SourceRevision,
        all_record_graphs,
        label,
        "source revision",
    )?;
    let rg_str = rg.as_str();
    let (Some(r_id), Some(p_id), Some(g_id)) = (
        reads
            .preparation()
            .lookup_iri(reads.snap(), revision.as_str())
            .map_err(|e| classify(label, e))?,
        reads
            .id(&format!("{EV}retainedBytes"))
            .map_err(|e| classify(label, e))?,
        reads.id(rg_str).map_err(|e| classify(label, e))?,
    ) else {
        // Metadata-only revision without retainedBytes.
        return Ok(());
    };
    let mut ids = Vec::new();
    reads
        .preparation()
        .scan(reads.snap(), Perm::Gspo, &[g_id.0, r_id.0, p_id.0], |q| {
            ids.push(q[2]);
            Ok(ids.len() < 2)
        })
        .map_err(|e| classify(label, e))?;
    match ids.as_slice() {
        [] => Ok(()),
        [bytes_id] => {
            let rev_digest = reads
                .one_string(revision.as_str(), &format!("{EV}contentDigest"), rg_str)
                .map_err(|e| classify(label, e))?
                .and_then(|v| v.parse::<Sha256Digest>().ok());
            let Some(rev_digest) = rev_digest else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "{label}: revision <{}> has no valid ev:contentDigest",
                        revision.as_str()
                    ),
                ));
            };
            let preparation = reads.preparation();
            let control = preparation.control();
            // Decode base64 inside `with_typed_literal`, then return `(decoded, held)`
            // so the vocabulary block workspace drops before JSON selection runs.
            let decoded_res = preparation
                .with_typed_literal(reads.snap(), *bytes_id, XSD_BASE64, |raw| {
                    let Some(raw) = raw else {
                        return Ok(Err("revision retainedBytes is not xsd:base64Binary"));
                    };
                    let max_decoded = (raw.len() / 4 + 1).saturating_mul(3);
                    let mut held = preparation.reserve(max_decoded as u64)?;
                    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw) else {
                        return Ok(Err("revision retainedBytes is not valid base64"));
                    };
                    held.release_to(decoded.capacity() as u64);
                    Ok(Ok((decoded, held)))
                })
                .map_err(|e| classify(label, e))?;
            let (decoded, _decoded_held) = match decoded_res {
                Ok(v) => v,
                Err(why) => {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!("{label}: {why}"),
                    ));
                }
            };
            if Sha256Digest::of_bytes(&decoded, control).map_err(|e| classify(label, e))?
                != rev_digest
            {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label}: revision retainedBytes does not match its contentDigest"),
                ));
            }
            let Ok(json_str) = std::str::from_utf8(&decoded) else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label}: revision retainedBytes is not valid UTF-8 JSON"),
                ));
            };
            let selected = match select_json_lexical_utf8_prepared(json_str, &pointer, preparation)
            {
                Ok(sel) => sel,
                Err(
                    e @ (Error::Cancelled
                    | Error::Timeout
                    | Error::BudgetExceeded(_)
                    | Error::Unsupported(_)),
                ) => return Err(classify(label, e)),
                Err(_) => {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "{label}: structured reference jsonPointer does not resolve against retained revision"
                        ),
                    ));
                }
            };
            if selected.digest != value_digest {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "{label}: structured reference valueDigest does not match retained revision selection"
                    ),
                ));
            }
            Ok(())
        }
        _ => Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: revision <{}> has more than one ev:retainedBytes",
                revision.as_str()
            ),
        )),
    }
}

/// Per-quad allocator alignment/metadata margin across the four owned heap terms.
const QUAD_ALLOC_OVERHEAD: u64 = 64;
/// Upper bound for constant predicate, literal and datatype IRI strings across the 11
/// (or 12 with `idempotencyKey`) fixed audit operation quads plus local `new_nonce` and
/// `time` values (exact byte sum is under 1,000 bytes).
const AUDIT_FIXED_STRINGS_BYTES: u64 = 1280;
/// Conservative B-tree node bytes per entry in `BTreeMap<&NamedNode, Uuid>`.
const RECEIPT_MAP_ENTRY_BYTES: u64 = 128;
/// Conservative B-tree node bytes per entry in `BTreeSet<&NamedNode>`.
const RECEIPT_SET_ENTRY_BYTES: u64 = 64;

struct QuadIndex<'a> {
    ops_to_update: BTreeMap<&'a NamedNode, Uuid>,
    informed_by: BTreeSet<&'a NamedNode>,
    renewed_count: usize,
    already_count: usize,
}

fn index_verified_receipts<'a>(
    verified: &'a [VerifiedCandidate],
    current_nonce: Uuid,
) -> QuadIndex<'a> {
    let mut ops_to_update: BTreeMap<&'a NamedNode, Uuid> = BTreeMap::new();
    let mut informed_by: BTreeSet<&'a NamedNode> = BTreeSet::new();
    let mut renewed_count = 0usize;
    let mut already_count = 0usize;
    for item in verified {
        if item.already_authenticated {
            already_count += 1;
        } else {
            renewed_count += 1;
            informed_by.insert(&item.receipt);
            if item.operation_nonce != current_nonce {
                ops_to_update
                    .entry(&item.receipt)
                    .or_insert(item.operation_nonce);
            }
        }
    }
    QuadIndex {
        ops_to_update,
        informed_by,
        renewed_count,
        already_count,
    }
}

fn estimate_quad_index_bytes(verified: &[VerifiedCandidate], current_nonce: Uuid) -> u64 {
    let mut total = 0u64;
    for item in verified {
        if !item.already_authenticated {
            total = total.saturating_add(RECEIPT_SET_ENTRY_BYTES);
            if item.operation_nonce != current_nonce {
                total = total.saturating_add(RECEIPT_MAP_ENTRY_BYTES);
            }
        }
    }
    total
}

fn estimate_quad_output_bytes(
    operation_iri: &NamedNode,
    principal: &NamedNode,
    idempotency_key: Option<&str>,
    graph: &NamedNode,
    verified: &[VerifiedCandidate],
    index: &QuadIndex<'_>,
) -> u64 {
    let quad_slot = std::mem::size_of::<Quad>() as u64;
    let g_len = graph.as_str().len() as u64;
    let op_len = operation_iri.as_str().len() as u64;
    let audit_quads = 11usize + usize::from(idempotency_key.is_some());
    let delete_len = index.renewed_count + index.ops_to_update.len();
    let insert_len = audit_quads
        + index.already_count
        + 2 * index.renewed_count
        + index.ops_to_update.len()
        + index.informed_by.len();

    // Backing `Vec<Quad>` arrays (pre-allocated to exact capacities) plus fixed audit
    // quads, the extra graph IRI object on `ev:reauthenticatedGraph`, local `gn`,
    // `principal`, optional `idempotency_key`, and constant predicate/literal IRIs.
    let mut total = ((delete_len + insert_len) as u64).saturating_mul(quad_slot);
    total = total
        .saturating_add(
            (audit_quads as u64).saturating_mul(
                op_len
                    .saturating_add(g_len)
                    .saturating_add(QUAD_ALLOC_OVERHEAD),
            ),
        )
        .saturating_add(g_len.saturating_mul(2))
        .saturating_add(principal.as_str().len() as u64)
        .saturating_add(idempotency_key.map_or(0, |k| k.len() as u64))
        .saturating_add(AUDIT_FIXED_STRINGS_BYTES);

    // Per-candidate quads: 1 insert for already-authenticated records, or 3 quads
    // (nonce delete, nonce insert, `prov:used` insert) for renewed records.
    let ev_already_len = EV_ALREADY_AUTHENTICATED.len() as u64;
    let record_fixed_bytes = 39u64 + 36 + 39 + 36 + 31 + 3 * QUAD_ALLOC_OVERHEAD;
    for item in verified {
        let iri_len = item.iri.as_str().len() as u64;
        if item.already_authenticated {
            total = total.saturating_add(
                op_len
                    .saturating_add(ev_already_len)
                    .saturating_add(iri_len)
                    .saturating_add(g_len)
                    .saturating_add(QUAD_ALLOC_OVERHEAD),
            );
        } else {
            total = total.saturating_add(
                iri_len
                    .saturating_mul(3)
                    .saturating_add(op_len)
                    .saturating_add(g_len.saturating_mul(3))
                    .saturating_add(record_fixed_bytes),
            );
        }
    }

    // Distinct stale receipts: 2 quads each (nonce delete + nonce insert).
    let receipt_nonce_fixed_bytes = 39u64 + 36 + 39 + 36 + 2 * QUAD_ALLOC_OVERHEAD;
    for op in index.ops_to_update.keys() {
        let r_len = op.as_str().len() as u64;
        total = total.saturating_add(
            r_len
                .saturating_mul(2)
                .saturating_add(g_len.saturating_mul(2))
                .saturating_add(receipt_nonce_fixed_bytes),
        );
    }

    // Distinct informed receipts: 1 `prov:wasInformedBy` insert each.
    let informed_fixed_bytes = 40u64 + QUAD_ALLOC_OVERHEAD;
    for op in &index.informed_by {
        let r_len = op.as_str().len() as u64;
        total = total.saturating_add(
            op_len
                .saturating_add(r_len)
                .saturating_add(g_len)
                .saturating_add(informed_fixed_bytes),
        );
    }

    total
}

#[allow(clippy::too_many_arguments)]
fn emit_quads(
    operation_iri: &NamedNode,
    principal: &NamedNode,
    payload_digest: Sha256Digest,
    idempotency_key: Option<&str>,
    graph: &NamedNode,
    current_nonce: Uuid,
    stamp: &Stamp,
    verified: &[VerifiedCandidate],
    index: QuadIndex<'_>,
) -> (Vec<Quad>, Vec<Quad>) {
    let gn = GraphName::NamedNode(graph.clone());
    let new_nonce = string(&current_nonce.to_string());
    let time = date_time(stamp.time_ms);
    let audit_quads = 11usize + usize::from(idempotency_key.is_some());
    let delete_len = index.renewed_count + index.ops_to_update.len();
    let insert_len = audit_quads
        + index.already_count
        + 2 * index.renewed_count
        + index.ops_to_update.len()
        + index.informed_by.len();

    let mut deletes = Vec::with_capacity(delete_len);
    let mut inserts = Vec::with_capacity(insert_len);

    // Audit operation record in `graph`, explicitly marked as a reconciliation of `graph`.
    inserts.extend([
        Quad::new(
            operation_iri.clone(),
            rdf_type(),
            ev("Operation"),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            rdf_type(),
            prov("Activity"),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            NamedNode::new_unchecked(EV_OPERATION_KIND),
            NamedNode::new_unchecked(OPERATION_KIND_REAUTH),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            NamedNode::new_unchecked(EV_REAUTHENTICATED_GRAPH),
            graph.clone(),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            prov("wasAssociatedWith"),
            principal.clone(),
            gn.clone(),
        ),
        Quad::new(operation_iri.clone(), prov("endedAtTime"), time, gn.clone()),
        Quad::new(
            operation_iri.clone(),
            ev("datasetId"),
            string(&stamp.dataset.to_string()),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            ev("branchId"),
            string(&stamp.branch.to_string()),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            ev("commit"),
            Literal::new_typed_literal(
                stamp.commit.to_string(),
                NamedNode::new_unchecked(XSD_INTEGER),
            ),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            ev("payloadDigest"),
            string(&payload_digest.to_string()),
            gn.clone(),
        ),
        Quad::new(
            operation_iri.clone(),
            ev("graphProtection"),
            new_nonce.clone(),
            gn.clone(),
        ),
    ]);
    if let Some(k) = idempotency_key {
        inserts.push(Quad::new(
            operation_iri.clone(),
            ev("idempotencyKey"),
            string(k),
            gn.clone(),
        ));
    }

    for item in verified {
        if item.already_authenticated {
            // Record already-authenticated candidates separately so a keyed whole-graph
            // replay returns the exact same record list and verifies their authentication.
            inserts.push(Quad::new(
                operation_iri.clone(),
                NamedNode::new_unchecked(EV_ALREADY_AUTHENTICATED),
                item.iri.clone(),
                gn.clone(),
            ));
            continue;
        }
        deletes.push(Quad::new(
            item.iri.clone(),
            ev("graphProtection"),
            string(&item.record_nonce.to_string()),
            gn.clone(),
        ));
        inserts.push(Quad::new(
            item.iri.clone(),
            ev("graphProtection"),
            new_nonce.clone(),
            gn.clone(),
        ));
        inserts.push(Quad::new(
            operation_iri.clone(),
            prov("used"),
            item.iri.clone(),
            gn.clone(),
        ));
    }
    for (op, old_nonce) in index.ops_to_update {
        deletes.push(Quad::new(
            op.clone(),
            ev("graphProtection"),
            string(&old_nonce.to_string()),
            gn.clone(),
        ));
        inserts.push(Quad::new(
            op.clone(),
            ev("graphProtection"),
            new_nonce.clone(),
            gn.clone(),
        ));
    }
    for op in index.informed_by {
        inserts.push(Quad::new(
            operation_iri.clone(),
            prov("wasInformedBy"),
            op.clone(),
            gn.clone(),
        ));
    }
    debug_assert_eq!(deletes.len(), delete_len);
    debug_assert_eq!(inserts.len(), insert_len);
    (deletes, inserts)
}

/// Charge `preparation` for the retained receipt indexes, `Vec<Quad>` backing buffers and
/// all owned quad terms before allocating them, then build `(deletes, inserts)` for one
/// re-authentication commit in `graph` under `current_nonce`.
#[allow(clippy::too_many_arguments)]
pub fn build_quads_charged(
    preparation: &Preparation,
    label: &str,
    operation_iri: &NamedNode,
    principal: &NamedNode,
    payload_digest: Sha256Digest,
    idempotency_key: Option<&str>,
    graph: &NamedNode,
    current_nonce: Uuid,
    stamp: &Stamp,
    verified: &[VerifiedCandidate],
) -> Result<(Vec<Quad>, Vec<Quad>, Reservation)> {
    let index_bytes = estimate_quad_index_bytes(verified, current_nonce);
    let mut held = preparation
        .reserve(index_bytes)
        .map_err(|e| classify_error(label, e))?;
    let index = index_verified_receipts(verified, current_nonce);
    let output_bytes = estimate_quad_output_bytes(
        operation_iri,
        principal,
        idempotency_key,
        graph,
        verified,
        &index,
    );
    held.grow_by(output_bytes)
        .map_err(|e| classify_error(label, e))?;
    let (deletes, inserts) = emit_quads(
        operation_iri,
        principal,
        payload_digest,
        idempotency_key,
        graph,
        current_nonce,
        stamp,
        verified,
        index,
    );
    Ok((deletes, inserts, held))
}

/// Build `(deletes, inserts)` for one re-authentication commit in `graph` under
/// `current_nonce`.
#[allow(clippy::too_many_arguments)]
pub fn build_quads(
    operation_iri: &NamedNode,
    principal: &NamedNode,
    payload_digest: Sha256Digest,
    idempotency_key: Option<&str>,
    graph: &NamedNode,
    current_nonce: Uuid,
    stamp: &Stamp,
    verified: &[VerifiedCandidate],
) -> (Vec<Quad>, Vec<Quad>) {
    let index = index_verified_receipts(verified, current_nonce);
    emit_quads(
        operation_iri,
        principal,
        payload_digest,
        idempotency_key,
        graph,
        current_nonce,
        stamp,
        verified,
        index,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    #[test]
    fn discover_records_charges_long_iris_to_preparation() {
        let store = Store::in_memory(StoreOptions::default());
        let g = NamedNode::new_unchecked("urn:ev:claims");
        let claim_class = NamedNode::new_unchecked(kind_iri(RecordKind::Claim));
        let mut t = store.write();
        let mut labels = std::collections::HashMap::new();
        for i in 0..40 {
            let long_iri = NamedNode::new_unchecked(format!("urn:long:{i}:{}", "a".repeat(2048)));
            let q = Quad::new(
                long_iri,
                rdf_type(),
                claim_class.clone(),
                GraphName::NamedNode(g.clone()),
            );
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
        let snap = store.snapshot();
        // A tight 16 KiB budget must reject 40 x 2 KiB IRIs during charged discovery.
        let tight = Preparation::new(Control::none(), 16 << 10).unwrap();
        let mut reads = Reads::new(&tight, &snap);
        let err = match discover_records(&mut reads, &g, MAX_REAUTHENTICATE) {
            Err(e) => e,
            Ok((found, _)) => {
                panic!("expected incomplete-evidence, got {} records", found.len())
            }
        };
        assert!(
            matches!(
                err,
                Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence
            ),
            "got {err:?}"
        );
        // A generous budget succeeds and keeps its reservation alive.
        let roomy = Preparation::new(Control::none(), 4 << 20).unwrap();
        let mut reads = Reads::new(&roomy, &snap);
        let (found, held) = discover_records(&mut reads, &g, MAX_REAUTHENTICATE).unwrap();
        assert_eq!(found.len(), 40);
        assert!(held.bytes() >= 40 * 2048);
    }

    #[test]
    fn scan_subject_pairs_keeps_reservation_alive_while_pairs_live() {
        let store = Store::in_memory(StoreOptions::default());
        let g = NamedNode::new_unchecked("urn:ev:claims");
        let s = NamedNode::new_unchecked("urn:ev:subject:pairs");
        let mut t = store.write();
        let mut labels = std::collections::HashMap::new();
        for i in 0..300 {
            let q = Quad::new(
                s.clone(),
                NamedNode::new_unchecked(format!("urn:p:{i}")),
                Literal::new_simple_literal(format!("v{i}")),
                GraphName::NamedNode(g.clone()),
            );
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
        let snap = store.snapshot();
        let prep = Preparation::new(Control::none(), 4 << 20).unwrap();
        let mut reads = Reads::new(&prep, &snap);
        let (pairs, held, more) =
            scan_subject_pairs(&mut reads, s.as_str(), g.as_str(), MAX_RECORD_TRIPLES).unwrap();
        assert!(!more);
        assert_eq!(pairs.len(), 300);
        assert!(
            held.bytes() >= (pairs.len() * std::mem::size_of::<(Id, Id)>()) as u64,
            "expected held bytes to cover live pairs vector, got {}",
            held.bytes()
        );
        let used_with_pairs = prep.inner_used();
        drop(pairs);
        drop(held);
        assert!(prep.inner_used() < used_with_pairs);
    }

    #[test]
    fn structured_reference_json_budget_exhaustion_classifies_as_incomplete_evidence() {
        let store = Store::in_memory(StoreOptions::default());
        let g = NamedNode::new_unchecked("urn:ev:claims");
        let rev = NamedNode::new_unchecked("urn:rev:big");
        let sref = NamedNode::new_unchecked("urn:sref:big");
        // Build a valid JSON object with many unique keys (~40 KiB) so base64 decoding
        // plus JSON key tracking exceeds a tight 80 KiB shared preparation allowance.
        let mut json = String::from("{");
        for i in 0..1200 {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"key_{i:04}_padding_padding\":{i}"));
        }
        json.push('}');
        let rev_digest = Sha256Digest::of_bytes(json.as_bytes(), &Control::none()).unwrap();
        let val_digest = Sha256Digest::of_bytes(b"0", &Control::none()).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        let mut t = store.write();
        let mut labels = std::collections::HashMap::new();
        let gn = GraphName::NamedNode(g.clone());
        let quads = vec![
            Quad::new(
                rev.clone(),
                rdf_type(),
                NamedNode::new_unchecked(kind_iri(RecordKind::SourceRevision)),
                gn.clone(),
            ),
            Quad::new(
                rev.clone(),
                ev("contentDigest"),
                string(&rev_digest.to_string()),
                gn.clone(),
            ),
            Quad::new(
                rev.clone(),
                ev("retainedBytes"),
                Literal::new_typed_literal(b64, NamedNode::new_unchecked(XSD_BASE64)),
                gn.clone(),
            ),
            Quad::new(
                sref.clone(),
                rdf_type(),
                NamedNode::new_unchecked(kind_iri(RecordKind::EvidenceRef)),
                gn.clone(),
            ),
            Quad::new(
                sref.clone(),
                ev("jsonPointer"),
                string("/key_0000_padding_padding"),
                gn.clone(),
            ),
            Quad::new(
                sref.clone(),
                ev("valueDigest"),
                string(&val_digest.to_string()),
                gn.clone(),
            ),
            Quad::new(
                sref.clone(),
                ev("method"),
                NamedNode::new_unchecked(JSON_LEXICAL_UTF8_V1),
                gn.clone(),
            ),
            Quad::new(
                sref.clone(),
                ev("methodVersion"),
                string(JSON_LEXICAL_UTF8_VERSION),
                gn.clone(),
            ),
            Quad::new(sref.clone(), ev("sourceRevision"), rev.clone(), gn),
        ];
        for q in &quads {
            let ids = t.encode_quad(q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
        let snap = store.snapshot();
        // Tight allowance: fits the base64 key + decoded JSON, then exhausts during JSON
        // key tracking under the shared Preparation -> must be `incomplete-evidence`,
        // never `immutable-record-conflict`.
        let tight = Preparation::new(Control::none(), 120 << 10).unwrap();
        let mut reads = Reads::new(&tight, &snap);
        let err =
            verify_structured_reference(&mut reads, &g, &sref, std::slice::from_ref(&g), "sref")
                .unwrap_err();
        match err {
            Error::Managed(m) => assert_eq!(m.code, "incomplete-evidence", "{m:?}"),
            other => panic!("expected incomplete-evidence, got {other:?}"),
        }
        // Generous allowance verifies cleanly.
        let roomy = Preparation::new(Control::none(), 8 << 20).unwrap();
        let mut reads = Reads::new(&roomy, &snap);
        verify_structured_reference(&mut reads, &g, &sref, std::slice::from_ref(&g), "sref")
            .unwrap();
    }

    #[test]
    fn absent_fragment_scans_do_not_grow_preparation_cache() {
        let store = Store::in_memory(StoreOptions::default());
        let g = NamedNode::new_unchecked("urn:ev:claims");
        let snap = store.snapshot();
        let prep = Preparation::new(Control::none(), 64 << 10).unwrap();
        let mut reads = Reads::new(&prep, &snap);
        // Scanning thousands of absent subject/fragment IRIs must not cache `None` in
        // `Reads::ids` or exhaust a 64 KiB budget.
        for i in 0..500 {
            for frag in FRAGMENTS {
                let sub = format!("urn:absent:record:{i:04}:{}#{frag}", "x".repeat(128));
                let (pairs, _held, more) =
                    scan_subject_pairs(&mut reads, &sub, g.as_str(), 100).unwrap();
                assert!(pairs.is_empty());
                assert!(!more);
            }
        }
    }

    fn measured_quad_vec_bytes(quads: &[Quad], capacity: usize) -> u64 {
        let mut total = (capacity * std::mem::size_of::<Quad>()) as u64;
        for q in quads {
            total += match &q.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str().len() as u64,
                NamedOrBlankNode::BlankNode(b) => b.as_str().len() as u64,
            };
            total += q.predicate.as_str().len() as u64;
            total += match &q.object {
                Term::NamedNode(n) => n.as_str().len() as u64,
                Term::BlankNode(b) => b.as_str().len() as u64,
                Term::Literal(l) => {
                    l.value().len() as u64
                        + l.language().map_or(0, |s| s.len() as u64)
                        + if l.datatype().as_str() == XSD_STRING {
                            0
                        } else {
                            l.datatype().as_str().len() as u64
                        }
                }
                Term::Triple(_) => 0,
            };
            total += match &q.graph_name {
                GraphName::NamedNode(n) => n.as_str().len() as u64,
                GraphName::BlankNode(b) => b.as_str().len() as u64,
                GraphName::DefaultGraph => 0,
            };
        }
        total
    }

    #[test]
    fn build_quads_charged_bounds_long_graph_iri_with_distinct_receipts_within_64mib() {
        let long_graph = NamedNode::new_unchecked(format!("urn:ev:claims:{}", "g".repeat(4082)));
        assert_eq!(long_graph.as_str().len(), 4096);
        let op_iri = NamedNode::new_unchecked(format!("urn:uuid:{}", Uuid::new_v4()));
        let principal = NamedNode::new_unchecked("urn:x-sparkles:principal:admin");
        let digest = Sha256Digest::of_bytes(b"payload", &Control::none()).unwrap();
        let current_nonce = Uuid::new_v4();
        let stamp = Stamp {
            dataset: Uuid::new_v4(),
            branch: Uuid::new_v4(),
            commit: 42,
            time_ms: 1_700_000_000_000,
        };

        // 2,800 renewed records with distinct stale receipts and a 4,096-byte graph IRI:
        // six output quads per record own 68.8+ MiB of graph IRI strings alone, so the
        // shared 64 MiB preparation allowance MUST refuse before allocating output quads.
        let verified: Vec<VerifiedCandidate> = (0..2800)
            .map(|i| VerifiedCandidate {
                iri: NamedNode::new_unchecked(format!("urn:ev:claim:{i:04}")),
                kind: RecordKind::Claim,
                digest,
                receipt: NamedNode::new_unchecked(format!("urn:uuid:{}", Uuid::new_v4())),
                record_nonce: Uuid::new_v4(),
                operation_nonce: Uuid::new_v4(),
                already_authenticated: false,
            })
            .collect();
        let prep_64m =
            Preparation::new(Control::none(), super::super::resolve::MEMORY_BYTES).unwrap();
        let err = match build_quads_charged(
            &prep_64m,
            "graph",
            &op_iri,
            &principal,
            digest,
            Some("idempotency-key-1"),
            &long_graph,
            current_nonce,
            &stamp,
            &verified,
        ) {
            Err(e) => e,
            Ok((d, i, _)) => panic!(
                "expected incomplete-evidence, got {} deletes and {} inserts",
                d.len(),
                i.len()
            ),
        };
        assert!(
            matches!(
                err,
                Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence
            ),
            "expected incomplete-evidence for 2,800 distinct stale receipts x 4 KiB graph IRI, got {err:?}"
        );

        // On a smaller mixed batch (distinct stale receipts, shared receipt, and an
        // already-authenticated record), the charged reservation must strictly cover both
        // pre-sized output vectors and every owned term string inside them.
        let shared_receipt = NamedNode::new_unchecked(format!("urn:uuid:{}", Uuid::new_v4()));
        let shared_old_nonce = Uuid::new_v4();
        let mut small = verified[..20].to_vec();
        for i in 20..25 {
            small.push(VerifiedCandidate {
                iri: NamedNode::new_unchecked(format!("urn:ev:shared:{i:04}")),
                kind: RecordKind::Claim,
                digest,
                receipt: shared_receipt.clone(),
                record_nonce: shared_old_nonce,
                operation_nonce: shared_old_nonce,
                already_authenticated: false,
            });
        }
        small.push(VerifiedCandidate {
            iri: NamedNode::new_unchecked("urn:ev:already:0001"),
            kind: RecordKind::Claim,
            digest,
            receipt: NamedNode::new_unchecked(format!("urn:uuid:{}", Uuid::new_v4())),
            record_nonce: current_nonce,
            operation_nonce: current_nonce,
            already_authenticated: true,
        });
        let prep_roomy =
            Preparation::new(Control::none(), super::super::resolve::MEMORY_BYTES).unwrap();
        let (deletes, inserts, held) = build_quads_charged(
            &prep_roomy,
            "graph",
            &op_iri,
            &principal,
            digest,
            Some("idempotency-key-small"),
            &long_graph,
            current_nonce,
            &stamp,
            &small,
        )
        .unwrap();
        assert_eq!(deletes.len(), deletes.capacity());
        assert_eq!(inserts.len(), inserts.capacity());
        let actual_bytes = measured_quad_vec_bytes(&deletes, deletes.capacity())
            + measured_quad_vec_bytes(&inserts, inserts.capacity());
        assert!(
            held.bytes() >= actual_bytes,
            "charged {} bytes < actual {} bytes",
            held.bytes(),
            actual_bytes
        );
    }
}
