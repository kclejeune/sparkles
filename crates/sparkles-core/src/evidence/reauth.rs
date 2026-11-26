//! Verification and quad planning for administered re-authentication after a detach
//! (spec C25, roadmap item 552).
//!
//! When an evidence graph is detached and protected again, the managed-graph registry
//! assigns it a new protection nonce. Records written under an earlier protection then
//! answer `origin-not-reproduced` and their identities refuse new proposals. This module
//! verifies candidate records in a protected record graph against their recorded
//! `ev:recordDigest`, their retained content and passage spans, and their publication
//! receipts, and plans the managed-write changes that mark verified records under the
//! current protection nonce together with an audit `ev:Operation`.
//!
//! As in [`super::write`] and [`super::review`], the functions here hold no lock and
//! authorize nothing. The runtime that calls them obtains the managed-write capability,
//! checks administrator authority, drift and preconditions, and makes the one commit.

use super::canonical::FrameHash;
use super::prepare::locate_boundaries;
use super::resolve::{KINDS, integer, kind_iri};
use super::structured::select_json_lexical_utf8;
use super::write::{Reads, Stamp, date_time, ev, no_blank, prov, rdf_type, record_digest, string};
use super::{JSON_LEXICAL_UTF8_V1, JSON_LEXICAL_UTF8_VERSION, RecordKind, Sha256Digest};
use crate::access::Reservation;
use crate::id::Id;
use crate::index::Perm;
use crate::managed::{ManagedError, ManagedErrorKind};
use crate::store::Store;
use crate::task::Control;
use crate::{Error, Result};
use base64::Engine as _;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple, vocab::xsd};
use oxsdatatypes::DateTime;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const EV: &str = "urn:x-sparkles:evidence:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const OA: &str = "http://www.w3.org/ns/oa#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_BASE64: &str = "http://www.w3.org/2001/XMLSchema#base64Binary";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

/// The most records one re-authentication operation reconciles.
pub const MAX_REAUTHENTICATE: usize = 10_000;

/// The most stored triples read for a single record and its fragment sub-nodes.
pub const MAX_RECORD_TRIPLES: usize = 10_000;

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

/// One candidate record an administrator asks to verify and re-authenticate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordCheck {
    pub iri: NamedNode,
    /// Optional expected `ev:recordDigest` that the record must match in addition to
    /// matching its own stored `ev:recordDigest`.
    pub expected_digest: Option<Sha256Digest>,
}

/// A candidate record that passed digest, content, span and receipt verification.
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

fn managed(kind: ManagedErrorKind, message: impl Into<String>) -> Error {
    ManagedError::error(kind, message)
}

fn classify(context: &str, e: Error) -> Error {
    match e {
        Error::Managed(_) | Error::Cancelled | Error::Timeout => e,
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
/// profile records.
pub fn discover_records(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    limit: usize,
) -> Result<Vec<NamedNode>> {
    let mut found = BTreeSet::new();
    let mut held = reads.preparation().reserve(0)?;
    for kind in KINDS {
        reads.preparation().check()?;
        let class = Term::NamedNode(NamedNode::new_unchecked(kind_iri(kind)));
        let (subjects, more) = reads.subjects_up_to(RDF_TYPE, &class, graph.as_str(), limit)?;
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
                held.grow_by((s.as_str().len() as u64).saturating_add(96))?;
                found.insert(s);
            }
        }
    }
    Ok(found.into_iter().collect())
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

/// Scan all `(predicate, object)` id pairs of `subject` in `graph`, bounded by `limit`.
fn scan_subject_pairs(
    reads: &mut Reads<'_>,
    subject: &str,
    graph: &str,
    limit: usize,
) -> Result<(Vec<(Id, Id)>, bool)> {
    let (Some(g_id), Some(s_id)) = (reads.id(graph)?, reads.id(subject)?) else {
        return Ok((Vec::new(), false));
    };
    let preparation = reads.preparation();
    let snap = reads.snap();
    let mut pairs = Vec::new();
    let mut held = preparation.reserve(0)?;
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
    Ok((pairs, more))
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

fn parse_uuid_term(t: &Term) -> Option<Uuid> {
    match t {
        Term::Literal(l) if l.datatype().as_str() == XSD_STRING => Uuid::parse_str(l.value()).ok(),
        _ => None,
    }
}

fn parse_digest_term(t: &Term) -> Option<Sha256Digest> {
    match t {
        Term::Literal(l) if l.datatype().as_str() == XSD_STRING => l.value().parse().ok(),
        _ => None,
    }
}

/// Verify a single candidate record `check` in `graph` against its recorded
/// `ev:recordDigest`, its retained content and passage spans, and its publication
/// receipt.
///
/// `other_record_graphs` are the other configured record-role graphs of the dataset
/// (excluding `graph`) where the record must not also exist. `all_record_graphs` are
/// all configured record-role graphs, searched when verifying a passage's rendition or
/// a structured reference's retained revision.
pub fn verify_candidate(
    reads: &mut Reads<'_>,
    main: &Store,
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
            .has_subject(iri.as_str(), other.as_str())
            .map_err(|e| classify(&label, e))?
        {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label} is also stored in graph <{}>", other.as_str()),
            ));
        }
    }

    let (pairs, more) = scan_subject_pairs(reads, iri.as_str(), graph.as_str(), MAX_RECORD_TRIPLES)
        .map_err(|e| classify(&label, e))?;
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
            .map_err(|e| classify(&label, e))?
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

    let mut immutable_triples: Vec<Triple> = Vec::new();
    let mut triples_held: Reservation = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify(&label, e))?;

    for (p_id, o_id) in pairs {
        reads.preparation().check()?;
        let Some(pred) = decode_predicate(reads, p_id).map_err(|e| classify(&label, e))? else {
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
            triples_held.grow_by(128).map_err(|e| classify(&label, e))?;
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
            triples_held.grow_by(128).map_err(|e| classify(&label, e))?;
            immutable_triples.push(Triple::new(iri.clone(), pred, string("")));
            continue;
        }

        let Some(obj) = decode_term(reads, o_id).map_err(|e| classify(&label, e))? else {
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
            let Some(n) = parse_uuid_term(&obj) else {
                return Err(managed(
                    ManagedErrorKind::OriginNotReproduced,
                    format!("{label} has an invalid graphProtection nonce"),
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
        // fact's `ev:expectedTriple` (C25 proposal decision 4).
        let allow_blank = kind == RecordKind::EvidenceRef && p == p_expected_triple;
        if !allow_blank && no_blank(&label, &obj).is_err() {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label} contains a blank node outside a stored-fact expectedTriple"),
            ));
        }

        let est = (iri.as_str().len() as u64)
            .saturating_add(p.len() as u64)
            .saturating_add(term_estimate(&obj))
            .saturating_add(64);
        triples_held.grow_by(est).map_err(|e| classify(&label, e))?;
        immutable_triples.push(Triple::new(iri.clone(), pred, obj));
    }

    // Collect triples of all fragment sub-nodes owned by this record.
    for frag in FRAGMENTS {
        reads.preparation().check()?;
        let sub_str = format!("{}#{frag}", iri.as_str());
        let (sub_pairs, sub_more) = scan_subject_pairs(
            reads,
            &sub_str,
            graph.as_str(),
            MAX_RECORD_TRIPLES.saturating_sub(immutable_triples.len()),
        )
        .map_err(|e| classify(&label, e))?;
        if sub_more {
            return Err(managed(
                ManagedErrorKind::IncompleteEvidence,
                format!("{label} and its sub-nodes exceed {MAX_RECORD_TRIPLES} triples"),
            ));
        }
        if sub_pairs.is_empty() {
            continue;
        }
        let sub_node = NamedNode::new_unchecked(sub_str);
        for (p_id, o_id) in sub_pairs {
            reads.preparation().check()?;
            let Some(pred) = decode_predicate(reads, p_id).map_err(|e| classify(&label, e))? else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} sub-node has a non-IRI predicate"),
                ));
            };
            let Some(obj) = decode_term(reads, o_id).map_err(|e| classify(&label, e))? else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} sub-node has an unreadable object term"),
                ));
            };
            if no_blank(&label, &obj).is_err() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label} sub-node contains a blank node"),
                ));
            }
            let est = (sub_node.as_str().len() as u64)
                .saturating_add(pred.as_str().len() as u64)
                .saturating_add(term_estimate(&obj))
                .saturating_add(64);
            triples_held.grow_by(est).map_err(|e| classify(&label, e))?;
            immutable_triples.push(Triple::new(sub_node.clone(), pred, obj));
        }
    }

    // Verify mandatory server-owned fields on the record.
    let Some(receipt) = receipt_iri else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label} has no availabilityReceipt"),
        ));
    };
    if generated_by.as_ref() != Some(&receipt) {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: prov:wasGeneratedBy does not match ev:availabilityReceipt"),
        ));
    }
    let Some(record_nonce) = record_nonce else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label} has no graphProtection nonce"),
        ));
    };
    let Some(recorded_digest) = recorded_digest else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label} has no recorded ev:recordDigest"),
        ));
    };

    // Recompute the canonical record digest and check against the recorded digest.
    let control = reads.preparation().control();
    let computed_digest =
        record_digest(kind, &immutable_triples, control).map_err(|e| classify(&label, e))?;
    if computed_digest != recorded_digest {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "{label}: stored content digest {computed_digest} does not match recorded digest {recorded_digest}"
            ),
        ));
    }
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
    if let Some(o_id) = retained_bytes_id {
        if content_digest_count != 1 {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: retainedBytes requires a single ev:contentDigest"),
            ));
        }
        let Some(expected_content) = content_digest else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: invalid ev:contentDigest"),
            ));
        };
        verify_retained_bytes(reads, o_id, expected_content, &label)?;
    }
    if let Some(o_id) = normalized_text_id {
        if content_digest_count != 1 {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: normalizedText requires a single ev:contentDigest"),
            ));
        }
        let Some(expected_content) = content_digest else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: invalid ev:contentDigest"),
            ));
        };
        verify_normalized_text(reads, o_id, expected_content, &label)?;
    }

    // Drop the retained immutable triples buffer before further lookups.
    drop(immutable_triples);
    drop(triples_held);

    // Verify the operation receipt in `graph`.
    let op = verify_receipt_operation(reads, main, graph, &receipt, &label)?;

    // Server-stamped creation/invalidation timestamps must still match the operation's
    // recorded end time.
    if kind == RecordKind::SourceRevision {
        let Some(fa) = first_available_at else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: SourceRevision has no ev:firstAvailableAt"),
            ));
        };
        if fa != op.ended_at {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: ev:firstAvailableAt does not match the receipt operation's prov:endedAtTime"
                ),
            ));
        }
    }
    if kind == RecordKind::Withdrawal {
        let Some(inv) = invalidated_at else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{label}: Withdrawal has no prov:invalidatedAtTime"),
            ));
        };
        if inv != op.ended_at {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{label}: prov:invalidatedAtTime does not match the receipt operation's prov:endedAtTime"
                ),
            ));
        }
    }

    // Verify protection-nonce lineage.
    let already_authenticated = if record_nonce == current_nonce && op.nonce == current_nonce {
        true
    } else if record_nonce != current_nonce && op.nonce == record_nonce {
        false
    } else if record_nonce != current_nonce && op.nonce == current_nonce {
        // The operation already moved to `current_nonce` in an earlier partial
        // reconciliation under this protection. Allow this only when an authenticated
        // reconciliation operation in `graph` explicitly links to `<receipt>` via
        // `prov:wasInformedBy`, preventing a detached write from borrowing a later
        // operation's predictable IRI.
        if !operation_was_reauthenticated(reads, graph, &receipt, current_nonce, &label)? {
            return Err(managed(
                ManagedErrorKind::OriginNotReproduced,
                format!("{label}: record protection nonce does not match its receipt operation"),
            ));
        }
        false
    } else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: record protection nonce does not match its receipt operation"),
        ));
    };

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
    if commit.is_none() {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid commit"),
        ));
    }
    let agent = reads
        .single(op, &format!("{PROV}wasAssociatedWith"), g)
        .map_err(|e| classify(label, e))?;
    if !matches!(agent, Some(Term::NamedNode(_))) {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no associated principal"),
        ));
    }
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
    if payload_digest.is_none() {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid payloadDigest"),
        ));
    }
    let Some(nonce) = reads.nonce_of(op, g).map_err(|e| classify(label, e))? else {
        return Err(managed(
            ManagedErrorKind::OriginNotReproduced,
            format!("{label}: receipt <{op}> has no valid graphProtection nonce"),
        ));
    };
    Ok(VerifiedOp { nonce, ended_at })
}

fn operation_was_reauthenticated(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    receipt: &NamedNode,
    current_nonce: Uuid,
    label: &str,
) -> Result<bool> {
    let (subjects, more) = reads
        .subjects_up_to(
            &format!("{PROV}wasInformedBy"),
            &Term::NamedNode(receipt.clone()),
            graph.as_str(),
            MAX_REAUTHENTICATE,
        )
        .map_err(|e| classify(label, e))?;
    if more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            format!("{label}: too many reconciliation links on receipt"),
        ));
    }
    for s in subjects {
        if reads
            .marked(s.as_str(), graph.as_str(), Some(current_nonce))
            .map_err(|e| classify(label, e))?
        {
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
        .with_term(reads.snap(), o_id, |term| {
            let Some(Term::Literal(l)) = term else {
                return Ok(Err("retainedBytes is not a literal"));
            };
            if l.datatype().as_str() != XSD_BASE64 {
                return Ok(Err("retainedBytes is not xsd:base64Binary"));
            }
            let raw = l.value();
            let max_decoded = (raw.len() / 4 + 1).saturating_mul(3);
            let _held = preparation.reserve(max_decoded as u64)?;
            let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw) else {
                return Ok(Err("retainedBytes is not valid base64"));
            };
            let actual = Sha256Digest::of_bytes(&decoded, control)?;
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

/// Verify a passage's `ev:textDigest`, optional `oa:exact` quote, selector bounds, and
/// (when its rendition retains `ev:normalizedText` in a record graph) the selected text.
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
    if let Some(quote_term) = reads
        .single(s, &format!("{OA}exact"), g)
        .map_err(|e| classify(label, e))?
    {
        let Term::Literal(l) = quote_term else {
            return Err(managed(
                ManagedErrorKind::SpanMismatch,
                format!("{label}: passage quote is not a literal"),
            ));
        };
        if l.datatype().as_str() != XSD_STRING {
            return Err(managed(
                ManagedErrorKind::SpanMismatch,
                format!("{label}: passage quote is not a string literal"),
            ));
        }
        let actual = Sha256Digest::of_bytes(l.value().as_bytes(), reads.preparation().control())
            .map_err(|e| classify(label, e))?;
        if actual != text_digest {
            return Err(managed(
                ManagedErrorKind::SpanMismatch,
                format!("{label}: passage quote does not match ev:textDigest"),
            ));
        }
    }
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
    // If the rendition retains normalized text in a record graph of this branch, check
    // both the rendition's own content digest and the selected span.
    for rg in all_record_graphs {
        let rg_str = rg.as_str();
        if !reads
            .contains(
                rendition.as_str(),
                RDF_TYPE,
                &Term::NamedNode(NamedNode::new_unchecked(kind_iri(RecordKind::Rendition))),
                rg_str,
            )
            .map_err(|e| classify(label, e))?
        {
            continue;
        }
        let (Some(r_id), Some(p_id), Some(g_id)) = (
            reads
                .id(rendition.as_str())
                .map_err(|e| classify(label, e))?,
            reads
                .id(&format!("{EV}normalizedText"))
                .map_err(|e| classify(label, e))?,
            reads.id(rg_str).map_err(|e| classify(label, e))?,
        ) else {
            continue;
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
            [] => {}
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
        break;
    }
    Ok(())
}

/// If `iri` is a structured `ev:EvidenceRef` whose `ev:sourceRevision` retains JSON
/// bytes in a record graph under `JSON_LEXICAL_UTF8_V1`, verify the JSON pointer
/// selection against `ev:valueDigest`.
fn verify_structured_reference(
    reads: &mut Reads<'_>,
    graph: &NamedNode,
    iri: &NamedNode,
    all_record_graphs: &[NamedNode],
    label: &str,
) -> Result<()> {
    let g = graph.as_str();
    let s = iri.as_str();
    let Some(pointer) = reads
        .one_string(s, &format!("{EV}jsonPointer"), g)
        .map_err(|e| classify(label, e))?
    else {
        return Ok(());
    };
    let method = reads
        .single(s, &format!("{EV}method"), g)
        .map_err(|e| classify(label, e))?;
    let method_version = reads
        .one_string(s, &format!("{EV}methodVersion"), g)
        .map_err(|e| classify(label, e))?;
    if !matches!(method, Some(Term::NamedNode(ref n)) if n.as_str() == JSON_LEXICAL_UTF8_V1)
        || method_version.as_deref() != Some(JSON_LEXICAL_UTF8_VERSION)
    {
        return Ok(());
    }
    let value_digest = reads
        .one_string(s, &format!("{EV}valueDigest"), g)
        .map_err(|e| classify(label, e))?
        .and_then(|v| v.parse::<Sha256Digest>().ok());
    let Some(value_digest) = value_digest else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: structured reference has no valid ev:valueDigest"),
        ));
    };
    let Some(Term::NamedNode(revision)) = reads
        .single(s, &format!("{EV}sourceRevision"), g)
        .map_err(|e| classify(label, e))?
    else {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("{label}: structured reference has no ev:sourceRevision"),
        ));
    };
    for rg in all_record_graphs {
        let rg_str = rg.as_str();
        if !reads
            .contains(
                revision.as_str(),
                RDF_TYPE,
                &Term::NamedNode(NamedNode::new_unchecked(kind_iri(
                    RecordKind::SourceRevision,
                ))),
                rg_str,
            )
            .map_err(|e| classify(label, e))?
        {
            continue;
        }
        let (Some(r_id), Some(p_id), Some(g_id)) = (
            reads
                .id(revision.as_str())
                .map_err(|e| classify(label, e))?,
            reads
                .id(&format!("{EV}retainedBytes"))
                .map_err(|e| classify(label, e))?,
            reads.id(rg_str).map_err(|e| classify(label, e))?,
        ) else {
            continue;
        };
        let mut ids = Vec::new();
        reads
            .preparation()
            .scan(reads.snap(), Perm::Gspo, &[g_id.0, r_id.0, p_id.0], |q| {
                ids.push(q[2]);
                Ok(ids.len() < 2)
            })
            .map_err(|e| classify(label, e))?;
        if let [bytes_id] = ids.as_slice() {
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
            let checked = preparation
                .with_term(reads.snap(), *bytes_id, |term| {
                    let Some(Term::Literal(l)) = term else {
                        return Ok(Err("revision retainedBytes is not a literal"));
                    };
                    if l.datatype().as_str() != XSD_BASE64 {
                        return Ok(Err("revision retainedBytes is not xsd:base64Binary"));
                    }
                    let raw = l.value();
                    let max_decoded = (raw.len() / 4 + 1).saturating_mul(3);
                    let _held = preparation.reserve(max_decoded as u64)?;
                    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw) else {
                        return Ok(Err("revision retainedBytes is not valid base64"));
                    };
                    if Sha256Digest::of_bytes(&decoded, control)? != rev_digest {
                        return Ok(Err(
                            "revision retainedBytes does not match its contentDigest",
                        ));
                    }
                    let Ok(json_str) = std::str::from_utf8(&decoded) else {
                        return Ok(Err("revision retainedBytes is not valid UTF-8 JSON"));
                    };
                    let selected = match select_json_lexical_utf8(json_str, &pointer, control) {
                        Ok(sel) => sel,
                        Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
                        Err(_) => {
                            return Ok(Err(
                                "structured reference jsonPointer does not resolve against retained revision",
                            ));
                        }
                    };
                    if selected.digest != value_digest {
                        return Ok(Err(
                            "structured reference valueDigest does not match retained revision selection",
                        ));
                    }
                    Ok(Ok(()))
                })
                .map_err(|e| classify(label, e))?;
            if let Err(why) = checked {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("{label}: {why}"),
                ));
            }
        }
        break;
    }
    Ok(())
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
    let gn = GraphName::NamedNode(graph.clone());
    let new_nonce = string(&current_nonce.to_string());
    let time = date_time(stamp.time_ms);
    let mut deletes = Vec::new();
    // Audit operation record in `graph`.
    let mut inserts = vec![
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
    ];
    if let Some(k) = idempotency_key {
        inserts.push(Quad::new(
            operation_iri.clone(),
            ev("idempotencyKey"),
            string(k),
            gn.clone(),
        ));
    }

    // Track distinct original receipt operations whose nonce needs updating, and all
    // original receipt operations informed by this reconciliation.
    let mut ops_to_update: BTreeMap<NamedNode, Uuid> = BTreeMap::new();
    let mut informed_by: BTreeSet<NamedNode> = BTreeSet::new();
    for item in verified {
        if item.already_authenticated {
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
        informed_by.insert(item.receipt.clone());
        if item.operation_nonce != current_nonce {
            ops_to_update
                .entry(item.receipt.clone())
                .or_insert(item.operation_nonce);
        }
    }
    for (op, old_nonce) in ops_to_update {
        deletes.push(Quad::new(
            op.clone(),
            ev("graphProtection"),
            string(&old_nonce.to_string()),
            gn.clone(),
        ));
        inserts.push(Quad::new(
            op,
            ev("graphProtection"),
            new_nonce.clone(),
            gn.clone(),
        ));
    }
    for op in informed_by {
        inserts.push(Quad::new(
            operation_iri.clone(),
            prov("wasInformedBy"),
            op,
            gn.clone(),
        ));
    }
    (deletes, inserts)
}
