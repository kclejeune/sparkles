//! Administered re-authentication after an evidence graph is detached and protected
//! again (spec C25, roadmap item 552).
//!
//! Detaching a protected evidence graph drops its protection nonce, and protecting it
//! again chooses a new nonce. Records written under the earlier protection then answer
//! `origin-not-reproduced` and their identities refuse new proposals. [`reauthenticate`]
//! requires activated configuration history, verifies candidate records in a protected
//! record graph against their retained, configuration-bound publication views, recorded
//! `ev:recordDigest`, retained content, passage spans and receipt lineage, and
//! atomically marks the vouched records and their receipt operations under the graph's
//! current protection nonce together with an audit `ev:Operation`.

use super::propose::{AuthorityFence, managed, publish_hook, record_graphs, settings_of};
use crate::access::GraphAccess;
use crate::managed::{ManagedErrorKind, OwnerKind};
use crate::task::Control;
use crate::{Dataset, Error, Result};
use oxrdf::{NamedNode, Term};
use sparkles_core::evidence::reauth::{
    self, EV_ALREADY_AUTHENTICATED, MAX_REAUTHENTICATE, PublicationViews, RecordCheck,
    VerifiedCandidate, build_quads_charged, classify_error, discover_records, payload_digest,
    require_configuration_history, verify_audit_operation, verify_candidate,
};
use sparkles_core::evidence::resolve::{MEMORY_BYTES, kind_iri};
use sparkles_core::evidence::write::{
    self, ProtectedGraph, Reads, Stamp, operation_id, operation_iri,
};
use sparkles_core::evidence::{RecordKind, Sha256Digest};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uuid::Uuid;

pub use sparkles_core::evidence::reauth::{
    MAX_REAUTHENTICATE as MAX_REAUTHENTICATE_RECORDS, RecordCheck as ReauthenticateRecord,
};

const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const EV_RECORD_DIGEST: &str = "urn:x-sparkles:evidence:recordDigest";

/// Maximum retries if a concurrent commit or registry change advances state between
/// lock-free historical verification and acquiring the branch writer lock.
const MAX_HEAD_RETRIES: usize = 3;

#[cfg(test)]
thread_local! {
    #[allow(clippy::type_complexity)]
    static REAUTH_TEST_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&'static str)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn fire_test_hook(point: &'static str) {
    REAUTH_TEST_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(point);
        }
    });
}

/// How [`reauthenticate`] runs.
#[derive(Clone)]
pub struct ReauthenticateOptions {
    /// The authenticated administrator principal recorded as the reconciliation
    /// operation's agent.
    pub principal: NamedNode,
    /// Specific records to verify and vouch for, or `None` to verify and reconcile
    /// every profile record in the target graph (up to [`MAX_REAUTHENTICATE`]).
    pub records: Option<Vec<RecordCheck>>,
    /// Makes a retry with the same payload return the first operation's receipt.
    pub idempotency_key: Option<String>,
    /// Commit only while this commit is the branch head.
    pub if_head: Option<u64>,
    /// Verify and preview the changes without committing.
    pub dry_run: bool,
    /// The commit message.
    pub message: Option<Arc<str>>,
    /// Who makes the write, recorded in the change log.
    pub author: Option<Arc<str>>,
    /// What the caller may write on the branch, `None` for every graph.
    pub write: Option<Arc<GraphAccess>>,
    /// The caller's authority and settings-drift fence, checked right before publication.
    pub authority: Option<Arc<dyn AuthorityFence>>,
}

impl ReauthenticateOptions {
    /// Options for `principal` reconciling every record in the graph, with no key,
    /// head precondition, dry run or graph restriction.
    pub fn new(principal: NamedNode) -> Self {
        Self {
            principal,
            records: None,
            idempotency_key: None,
            if_head: None,
            dry_run: false,
            message: None,
            author: None,
            write: None,
            authority: None,
        }
    }
}

/// One record verified by [`reauthenticate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReauthenticatedRecord {
    pub iri: NamedNode,
    pub kind: RecordKind,
    pub digest: Sha256Digest,
    pub receipt: NamedNode,
    /// Whether the record already carried the graph's current protection nonce, so no
    /// nonce triple was rewritten for it.
    pub already_authenticated: bool,
}

/// The outcome of [`reauthenticate`].
#[derive(Debug)]
pub struct Reauthenticated {
    /// The audit operation IRI in `graph`.
    pub operation: NamedNode,
    /// The reconciled protected record graph.
    pub graph: NamedNode,
    /// Whether this call published a commit.
    pub committed: bool,
    /// Whether the idempotency key had already published the same payload, whose
    /// receipt this is. Nothing was written.
    pub replayed: bool,
    /// The publishing commit, the first one for a replay, or the head for a dry run or
    /// no-op reconciliation.
    pub commit: u64,
    /// The branch the commit published on.
    pub branch: Uuid,
    /// Every verified record, in request order when named or sorted IRI order for a
    /// whole-graph reconciliation.
    pub records: Vec<ReauthenticatedRecord>,
    /// A dry run's preview.
    pub preview: Option<Box<crate::preview::Preview>>,
}

fn into_records(verified: Vec<VerifiedCandidate>) -> Vec<ReauthenticatedRecord> {
    verified
        .into_iter()
        .map(|v| ReauthenticatedRecord {
            iri: v.iri,
            kind: v.kind,
            digest: v.digest,
            receipt: v.receipt,
            already_authenticated: v.already_authenticated,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn replay(
    main: &Dataset,
    operation_iri: &NamedNode,
    principal: &NamedNode,
    graph: &NamedNode,
    other_record_graphs: &[NamedNode],
    current_nonce: Uuid,
    payload_digest: Sha256Digest,
    requested: Option<&[RecordCheck]>,
    found: write::RecordedOperation,
    reads: &mut Reads<'_>,
) -> Result<Reauthenticated> {
    let g = graph.as_str();
    let op = operation_iri.as_str();
    let label = format!("operation <{op}>");
    let in_target = reads
        .is_operation_in(op, g)
        .map_err(|e| classify_error(&label, e))?;
    let mut in_other_op = false;
    let mut in_other_subject = false;
    for other in other_record_graphs {
        if reads
            .has_subject_in(op, other.as_str())
            .map_err(|e| classify_error(&label, e))?
        {
            in_other_subject = true;
            if reads
                .is_operation_in(op, other.as_str())
                .map_err(|e| classify_error(&label, e))?
            {
                in_other_op = true;
            }
        }
    }
    if !in_target || in_other_op {
        // The idempotency key already belongs to an operation in another record graph
        // (such as another reconciliation or a proposal/review operation).
        if found.authenticated || found.payload_digest.is_some_and(|d| d != payload_digest) {
            return Err(managed(
                ManagedErrorKind::IdempotencyConflict,
                "this idempotency key was used with a different payload",
            ));
        }
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("operation <{op}> was recorded outside the evidence profile's protection"),
        ));
    }
    if in_other_subject {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("operation <{op}> also has triples in another record graph"),
        ));
    }
    if !found.authenticated {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("operation <{op}> was recorded outside the evidence profile's protection"),
        ));
    }
    if found.payload_digest != Some(payload_digest) {
        return Err(managed(
            ManagedErrorKind::IdempotencyConflict,
            "this idempotency key was used with a different payload",
        ));
    }
    // Validate the complete immutable audit stamp and cross-graph subject uniqueness
    // using the same verifier as multi-detach lineage checks (no branch/commit fallback).
    let audit = verify_audit_operation(
        reads,
        main.store(),
        graph,
        operation_iri,
        other_record_graphs,
        Some(principal),
        &label,
    )?;
    if audit.nonce != current_nonce {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("operation <{op}> was recorded outside the evidence profile's protection"),
        ));
    }
    if audit.payload_digest != payload_digest {
        return Err(managed(
            ManagedErrorKind::IdempotencyConflict,
            "this idempotency key was used with a different payload",
        ));
    }

    let mut held = reads
        .preparation()
        .reserve(0)
        .map_err(|e| classify_error(&label, e))?;
    let (used_terms, used_more) = reads
        .objects_up_to_held(op, PROV_USED, g, MAX_REAUTHENTICATE, &mut held)
        .map_err(|e| classify_error(&label, e))?;
    if used_more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            "replayed reconciliation operation exceeds record limit",
        ));
    }
    let (already_terms, already_more) = reads
        .objects_up_to_held(
            op,
            EV_ALREADY_AUTHENTICATED,
            g,
            MAX_REAUTHENTICATE,
            &mut held,
        )
        .map_err(|e| classify_error(&label, e))?;
    if already_more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            "replayed reconciliation operation exceeds record limit",
        ));
    }
    // Move IRIs directly into a single sorted map (iri -> already_authenticated) without
    // duplicating names into an uncharged intermediate collection.
    let mut records_map: BTreeMap<NamedNode, bool> = BTreeMap::new();
    for t in used_terms {
        let Term::NamedNode(n) = t else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("operation <{op}> has a non-IRI prov:used target"),
            ));
        };
        if let std::collections::btree_map::Entry::Vacant(e) = records_map.entry(n) {
            held.grow_by(96)
                .map_err(|err| classify_error(&label, err))?;
            e.insert(false);
        }
    }
    for t in already_terms {
        let Term::NamedNode(n) = t else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("operation <{op}> has a non-IRI ev:alreadyAuthenticated target"),
            ));
        };
        match records_map.entry(n) {
            std::collections::btree_map::Entry::Occupied(e) => {
                if !*e.get() {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "operation <{op}> lists <{}> as both renewed and already authenticated",
                            e.key().as_str()
                        ),
                    ));
                }
            }
            std::collections::btree_map::Entry::Vacant(e) => {
                held.grow_by(96)
                    .map_err(|err| classify_error(&label, err))?;
                e.insert(true);
            }
        }
    }
    if records_map.len() > MAX_REAUTHENTICATE {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            "replayed reconciliation operation exceeds record limit",
        ));
    }

    // Every record the original operation verified (both renewed and already
    // authenticated) must still be authenticated under `current_nonce`.
    for iri in records_map.keys() {
        if !reads
            .authenticated(iri.as_str(), g, Some(current_nonce))
            .map_err(|e| classify_error(&label, e))?
        {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "record <{}> from replayed operation <{op}> is no longer authenticated",
                    iri.as_str(),
                ),
            ));
        }
    }

    let read_meta = |reads: &mut Reads<'_>,
                     iri: &NamedNode,
                     held: &mut sparkles_core::access::Reservation|
     -> Result<(RecordKind, Sha256Digest, NamedNode)> {
        let kind = find_kind(reads, iri, graph)
            .map_err(|e| classify_error(&label, e))?
            .ok_or_else(|| {
                managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "record <{}> from replayed operation <{op}> is missing its kind",
                        iri.as_str(),
                    ),
                )
            })?;
        let digest = match reads
            .single(iri.as_str(), EV_RECORD_DIGEST, g)
            .map_err(|e| classify_error(&label, e))?
        {
            Some(Term::Literal(l)) => l.value().parse::<Sha256Digest>().ok(),
            _ => None,
        }
        .ok_or_else(|| {
            managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "record <{}> from replayed operation <{op}> has no valid recordDigest",
                    iri.as_str(),
                ),
            )
        })?;
        let receipt = reads
            .receipt_of(iri, graph)
            .map_err(|e| classify_error(&label, e))?
            .ok_or_else(|| {
                managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "record <{}> from replayed operation <{op}> has no receipt",
                        iri.as_str(),
                    ),
                )
            })?;
        held.grow_by((receipt.as_str().len() as u64).saturating_add(64))
            .map_err(|e| classify_error(&label, e))?;
        Ok((kind, digest, receipt))
    };

    let out = match requested {
        Some(list) => {
            if records_map.len() != list.len() {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!("replayed operation <{op}> record set does not match the request"),
                ));
            }
            for item in list {
                if !records_map.contains_key(&item.iri) {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "record <{}> was not recorded by replayed operation <{op}>",
                            item.iri.as_str(),
                        ),
                    ));
                }
            }
            held.grow_by((list.len() * std::mem::size_of::<ReauthenticatedRecord>()) as u64)
                .map_err(|e| classify_error(&label, e))?;
            let mut out = Vec::with_capacity(list.len());
            for item in list {
                let already_authenticated = records_map[&item.iri];
                held.grow_by((item.iri.as_str().len() as u64).saturating_add(48))
                    .map_err(|e| classify_error(&label, e))?;
                let (kind, digest, receipt) = read_meta(reads, &item.iri, &mut held)?;
                out.push(ReauthenticatedRecord {
                    iri: item.iri.clone(),
                    kind,
                    digest,
                    receipt,
                    already_authenticated,
                });
            }
            out
        }
        None => {
            held.grow_by((records_map.len() * std::mem::size_of::<ReauthenticatedRecord>()) as u64)
                .map_err(|e| classify_error(&label, e))?;
            let mut out = Vec::with_capacity(records_map.len());
            for (iri, already_authenticated) in records_map {
                let (kind, digest, receipt) = read_meta(reads, &iri, &mut held)?;
                out.push(ReauthenticatedRecord {
                    iri,
                    kind,
                    digest,
                    receipt,
                    already_authenticated,
                });
            }
            out
        }
    };
    drop(held);
    Ok(Reauthenticated {
        operation: operation_iri.clone(),
        graph: graph.clone(),
        committed: false,
        replayed: true,
        commit: audit.commit,
        branch: audit.branch_id,
        records: out,
        preview: None,
    })
}

fn find_kind(
    reads: &mut Reads<'_>,
    iri: &NamedNode,
    graph: &NamedNode,
) -> Result<Option<RecordKind>> {
    use RecordKind::*;
    const ALL: [RecordKind; 16] = [
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
        ResolutionMap,
    ];
    for k in ALL {
        if reads.contains(
            iri.as_str(),
            RDF_TYPE,
            &Term::NamedNode(NamedNode::new_unchecked(kind_iri(k))),
            graph.as_str(),
        )? {
            return Ok(Some(k));
        }
    }
    Ok(None)
}

/// Verify records written under an earlier protection in `graph` on `logical` (which is
/// `main` or one of its branches) against their retained, configuration-bound
/// publication views, recorded digests, retained content, passage spans and receipt
/// lineage, and atomically mark them under `graph`'s current protection nonce.
///
/// `graph` must be a currently protected record-role graph (`sourceRecords`,
/// `proposals`, `decisions`, `supports` or `identity`), never an accepted projection
/// graph. Configuration history must be activated on the dataset (returns
/// `configuration-history-inactive` otherwise). If any candidate fails verification,
/// nothing is written.
pub fn reauthenticate(
    main: &Dataset,
    logical: &Dataset,
    graph: &NamedNode,
    options: &ReauthenticateOptions,
    control: &Control,
) -> Result<Reauthenticated> {
    reauthenticate_with_budget(main, logical, graph, options, control, MEMORY_BYTES)
}

fn reauthenticate_with_budget(
    main: &Dataset,
    logical: &Dataset,
    graph: &NamedNode,
    options: &ReauthenticateOptions,
    control: &Control,
    memory_bytes: u64,
) -> Result<Reauthenticated> {
    control.check()?;
    if main.store().is_branch() || logical.store().owner_dataset_id() != main.dataset_id() {
        return Err(Error::invalid(
            "re-authentication names the dataset's main store and one of its branches",
        ));
    }
    if let Some(k) = &options.idempotency_key
        && (k.is_empty() || k.len() > 256)
    {
        return Err(Error::invalid("idempotencyKey has 1 to 256 bytes"));
    }
    if let Some(list) = &options.records {
        if list.is_empty() {
            return Err(Error::invalid(
                "records must not be empty when supplied; omit records to reconcile every record in the graph",
            ));
        }
        if list.len() > MAX_REAUTHENTICATE {
            return Err(Error::invalid(format!(
                "records: at most {MAX_REAUTHENTICATE} records per reconciliation"
            )));
        }
        let mut seen = BTreeSet::new();
        for item in list {
            if !seen.insert(item.iri.as_str()) {
                return Err(Error::invalid(format!(
                    "records: duplicate record <{}>",
                    item.iri.as_str()
                )));
            }
        }
    }

    let (settings, settings_bytes) = settings_of(main)?;
    if !settings.enabled {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    if settings.projections.iter().any(|g| g == graph.as_str()) {
        return Err(Error::invalid(format!(
            "graph <{}> is an accepted projection graph and cannot be re-authenticated directly",
            graph.as_str()
        )));
    }
    let all_record_graphs = record_graphs(&settings);
    if !all_record_graphs.iter().any(|g| g == graph) {
        return Err(Error::invalid(format!(
            "graph <{}> is not a record graph of the evidence settings",
            graph.as_str()
        )));
    }
    require_configuration_history(main.store())?;

    if let Some(access) = &options.write {
        let term = Term::NamedNode(graph.clone());
        if !access.writable(Some(&term)) {
            return Err(GraphAccess::refused(Some(&term)));
        }
    }

    let operation = match options.idempotency_key.as_deref() {
        Some(k) => operation_id(main.dataset_id(), &options.principal, k),
        None => Uuid::new_v4(),
    };
    let op_iri = operation_iri(operation);
    let op_label = format!("graph <{}>", graph.as_str());
    let p_digest = payload_digest(graph, options.records.as_deref(), control)?;

    let store = logical.store();
    let branch = store.branch_id();
    let other_record_graphs: Vec<NamedNode> = all_record_graphs
        .iter()
        .filter(|g| *g != graph)
        .cloned()
        .collect();

    for attempt in 0..=MAX_HEAD_RETRIES {
        control.check()?;
        let capability = store.managed_writer(OwnerKind::Evidence, operation)?;
        if capability.settings().as_deref() != Some(&settings_bytes[..]) {
            return Err(Error::Conflict(
                "the evidence settings changed while re-authentication was prepared; retry".into(),
            ));
        }
        let Some(current_nonce) = capability.protection(graph.as_str()) else {
            return Err(write::unprotected(graph));
        };
        let all_protected: Vec<ProtectedGraph> = all_record_graphs
            .iter()
            .map(|g| (g.clone(), capability.protection(g.as_str())))
            .collect();

        #[cfg(test)]
        fire_test_hook("after_capability");

        // Pin a branch snapshot BEFORE acquiring the branch writer lock so historical
        // `BoundEvidenceView` reads never contend with `self.writer`, then immediately
        // revalidate the registry identity so a stale capability can never pair with a
        // newer RDF head.
        let head = store.snapshot();
        if !store.managed_writer_current(&capability) {
            if attempt < MAX_HEAD_RETRIES {
                continue;
            }
            return Err(Error::Conflict(
                "the managed-graph registry changed while re-authentication was prepared; retry"
                    .into(),
            ));
        }
        let preparation = crate::access::Preparation::new(control.clone(), memory_bytes)
            .map_err(|e| classify_error(&op_label, e))?;
        let mut reads = Reads::new(&preparation, &head);

        // Replay check always runs BEFORE `ifHead` on both fast and locked paths.
        if options.idempotency_key.is_some()
            && let Some(found) = reads
                .operation(&op_iri, &all_protected)
                .map_err(|e| classify_error(&op_label, e))?
        {
            let res = replay(
                main,
                &op_iri,
                &options.principal,
                graph,
                &other_record_graphs,
                current_nonce,
                p_digest,
                options.records.as_deref(),
                found,
                &mut reads,
            )?;
            #[cfg(test)]
            fire_test_hook("before_fast_replay_return");
            if !store.managed_writer_current(&capability) {
                if attempt < MAX_HEAD_RETRIES {
                    continue;
                }
                return Err(Error::Conflict(
                    "the managed-graph registry changed while re-authentication was prepared; retry"
                        .into(),
                ));
            }
            return Ok(res);
        }
        if let Some(expected) = options.if_head
            && head.commit != expected
        {
            return Err(managed(
                ManagedErrorKind::HeadChanged,
                format!(
                    "ifHead: the head of the branch is commit {}, not {expected}",
                    head.commit
                ),
            ));
        }

        // A new audit operation subject must not carry any pre-existing triples in any
        // record graph (prevents detached untyped triples from gaining authority).
        for rg in &all_record_graphs {
            if reads
                .has_subject_in(op_iri.as_str(), rg.as_str())
                .map_err(|e| classify_error(&op_label, e))?
            {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "audit operation <{}> already has triples in graph <{}>",
                        op_iri.as_str(),
                        rg.as_str()
                    ),
                ));
            }
        }

        let (checks, _checks_held) = match &options.records {
            Some(list) => {
                let mut held = preparation
                    .reserve((list.len() * std::mem::size_of::<RecordCheck>()) as u64)
                    .map_err(|e| classify_error(&op_label, e))?;
                for item in list {
                    held.grow_by((item.iri.as_str().len() as u64).saturating_add(96))
                        .map_err(|e| classify_error(&op_label, e))?;
                }
                (list.clone(), held)
            }
            None => {
                let (iris, mut held) = discover_records(&mut reads, graph, MAX_REAUTHENTICATE)?;
                held.grow_by((iris.len() * std::mem::size_of::<RecordCheck>()) as u64)
                    .map_err(|e| classify_error(&op_label, e))?;
                let checks = iris
                    .into_iter()
                    .map(|iri| RecordCheck {
                        iri,
                        expected_digest: None,
                    })
                    .collect();
                (checks, held)
            }
        };

        let mut views = PublicationViews::new();
        let mut verified_held = preparation
            .reserve((checks.len() * std::mem::size_of::<VerifiedCandidate>()) as u64)
            .map_err(|e| classify_error(&op_label, e))?;
        let mut verified = Vec::with_capacity(checks.len());
        for check in &checks {
            control.check()?;
            let v = verify_candidate(
                &mut reads,
                &mut views,
                main.store(),
                store,
                graph,
                current_nonce,
                &other_record_graphs,
                &all_record_graphs,
                check,
            )?;
            verified_held
                .grow_by(
                    (v.iri.as_str().len() as u64)
                        .saturating_add(v.receipt.as_str().len() as u64)
                        .saturating_add(160),
                )
                .map_err(|e| classify_error(&op_label, e))?;
            verified.push(v);
        }
        drop(views);
        drop(reads);
        drop(checks);
        drop(_checks_held);

        let needs_write = verified.iter().any(|v| !v.already_authenticated);
        if !needs_write && options.idempotency_key.is_none() {
            #[cfg(test)]
            fire_test_hook("before_noop_check");
            // Confirm both the branch head and the managed-graph registry did not change
            // before returning a no-op answer.
            if store.snapshot().commit != head.commit || !store.managed_writer_current(&capability)
            {
                if attempt < MAX_HEAD_RETRIES {
                    continue;
                }
                return Err(Error::Conflict(
                    "the branch head or registry changed while re-authentication was prepared; retry"
                        .into(),
                ));
            }
            let records = into_records(verified);
            drop(verified_held);
            drop(preparation);
            return Ok(Reauthenticated {
                operation: op_iri,
                graph: graph.clone(),
                committed: false,
                replayed: false,
                commit: head.commit,
                branch,
                records,
                preview: None,
            });
        }

        let (release, before_publish) = publish_hook(options.authority.clone(), || Ok(None));
        let opts = crate::guard::WriteOptions {
            deadline: control.deadline,
            cancel: Some(control.cancel.flag()),
            message: options.message.clone(),
            author: options.author.clone(),
            graphs: options.write.clone(),
            dry_run: options.dry_run.then(crate::preview::DryRun::default),
            before_publish: Some(before_publish),
            ..Default::default()
        };
        #[cfg(test)]
        fire_test_hook("before_writer_lock");

        let mut t = store.try_write_with(crate::commit::CommitKind::Transaction, opts)?;
        t.set_managed(capability)?;
        if !store.managed_writer_current(t.managed().expect("capability set")) {
            drop(t);
            drop(release);
            if attempt < MAX_HEAD_RETRIES {
                continue;
            }
            return Err(Error::Conflict(
                "the managed-graph registry changed while re-authentication was prepared; retry"
                    .into(),
            ));
        }

        if t.base().commit != head.commit {
            // Release prior verification allocations before locked-path replay.
            drop(verified);
            drop(verified_held);
            drop(preparation);
            // Another commit landed while we verified. First check locked-path replay
            // (before `ifHead`, matching the fast path).
            if options.idempotency_key.is_some() {
                let locked_head = t.base().clone();
                let locked_prep = crate::access::Preparation::new(control.clone(), memory_bytes)
                    .map_err(|e| classify_error(&op_label, e))?;
                let mut locked_reads = Reads::new(&locked_prep, &locked_head);
                if let Some(found) = locked_reads
                    .operation(&op_iri, &all_protected)
                    .map_err(|e| classify_error(&op_label, e))?
                {
                    let res = replay(
                        main,
                        &op_iri,
                        &options.principal,
                        graph,
                        &other_record_graphs,
                        current_nonce,
                        p_digest,
                        options.records.as_deref(),
                        found,
                        &mut locked_reads,
                    )?;
                    #[cfg(test)]
                    fire_test_hook("before_locked_replay_return");
                    if !store.managed_writer_current(t.managed().expect("capability set")) {
                        drop(locked_reads);
                        drop(locked_prep);
                        drop(t);
                        drop(release);
                        if attempt < MAX_HEAD_RETRIES {
                            continue;
                        }
                        return Err(Error::Conflict(
                            "the managed-graph registry changed while re-authentication was prepared; retry"
                                .into(),
                        ));
                    }
                    return Ok(res);
                }
            }
            if let Some(expected) = options.if_head
                && t.base().commit != expected
            {
                return Err(managed(
                    ManagedErrorKind::HeadChanged,
                    format!(
                        "ifHead: the head of the branch is commit {}, not {expected}",
                        t.base().commit
                    ),
                ));
            }
            drop(t);
            drop(release);
            if attempt < MAX_HEAD_RETRIES {
                continue;
            }
            return Err(Error::Conflict(
                "the branch head changed while re-authentication was prepared; retry".into(),
            ));
        }

        // `t.base().commit == head.commit` and `managed_writer_current` confirmed the
        // registry did not change, so all verification results and audit-subject checks
        // are exact for `t.base()`.
        let (seq, time_ms) = t.managed_stamp()?;
        let stamp = Stamp {
            dataset: main.dataset_id(),
            branch,
            commit: seq,
            time_ms,
        };
        let (deletes, inserts, quads_held) = build_quads_charged(
            &preparation,
            &op_label,
            &op_iri,
            &options.principal,
            p_digest,
            options.idempotency_key.as_deref(),
            graph,
            current_nonce,
            &stamp,
            &verified,
        )?;

        let mut labels = std::collections::HashMap::new();
        for q in &deletes {
            control.check()?;
            let ids = t.encode_quad(q, &mut labels)?;
            t.delete(ids)?;
        }
        for q in &inserts {
            control.check()?;
            let ids = t.encode_quad(q, &mut labels)?;
            t.insert(ids)?;
        }
        drop(deletes);
        drop(inserts);
        drop(quads_held);
        let records = into_records(verified);
        drop(verified_held);
        drop(preparation);
        let committed = t.commit();
        drop(release);
        return match committed {
            Err(Error::DryRun(preview)) => Ok(Reauthenticated {
                operation: op_iri,
                graph: graph.clone(),
                committed: false,
                replayed: false,
                commit: head.commit,
                branch,
                records,
                preview: Some(preview),
            }),
            Err(e) => Err(e),
            Ok(receipt) => Ok(Reauthenticated {
                operation: op_iri,
                graph: graph.clone(),
                committed: receipt.committed,
                replayed: false,
                commit: receipt.commit.seq,
                branch,
                records,
                preview: None,
            }),
        };
    }
    Err(Error::Conflict(
        "the branch head or registry changed while re-authentication was prepared; retry".into(),
    ))
}

// Re-export module for callers that want constants.
pub use reauth::MAX_RECORD_TRIPLES;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{
        self, ClaimInput, ConfidenceFacets, EvidenceInput, Polarity, ProposeOptions, RecordKey,
        RecordVersion, Scope, Settings, SourceContent, SourceLineage, SourceMetadata,
        SourceRevisionInput, SourceTimes,
    };
    use oxrdf::{Literal, Triple};
    use std::sync::atomic::{AtomicBool, Ordering};

    const SOURCES: &str = "urn:ev:sources";
    const CLAIMS: &str = "urn:ev:claims";

    struct HookGuard;
    impl Drop for HookGuard {
        fn drop(&mut self) {
            REAUTH_TEST_HOOK.with(|slot| {
                *slot.borrow_mut() = None;
            });
        }
    }

    fn set_hook(f: impl FnMut(&'static str) + 'static) -> HookGuard {
        REAUTH_TEST_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(f));
        });
        HookGuard
    }

    fn setup_ds() -> (Dataset, Settings, NamedNode) {
        let ds = Dataset::memory();
        let prep = crate::access::Preparation::new(Control::none(), 16 << 20).unwrap();
        ds.store()
            .activate_memory_configuration_history(&prep)
            .unwrap();
        ds.store()
            .set_retention(sparkles_core::history::Retention {
                keep_commits: Some(256),
                ..Default::default()
            })
            .unwrap();
        let settings = Settings {
            enabled: true,
            source_records: vec![SOURCES.into()],
            proposals: vec![CLAIMS.into()],
            ..Settings::default()
        };
        evidence::configure(&ds, &settings, &Control::none()).unwrap();
        let mut batch = EvidenceInput::default();
        batch.revisions.push(SourceRevisionInput {
            was_revision_of: None,
            version: RecordVersion::V1,
            key: RecordKey::new("rev").unwrap(),
            source: NamedNode::new_unchecked("https://example.org/src"),
            source_key: RecordKey::new("src-1").unwrap(),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: Sha256Digest::of_bytes(b"rev", &Control::none()).unwrap(),
            },
            metadata: SourceMetadata::default(),
            times: SourceTimes::default(),
            lineage: SourceLineage::default(),
        });
        batch.claims.push(ClaimInput {
            version: RecordVersion::V1,
            key: RecordKey::new("c1").unwrap(),
            occurrence_key: RecordKey::new("c1").unwrap(),
            revision: sparkles_core::evidence::RecordRef::Local(RecordKey::new("rev").unwrap()),
            proposition: Triple::new(
                NamedNode::new_unchecked("https://example.org/s"),
                NamedNode::new_unchecked("https://example.org/p"),
                Literal::new_simple_literal("v"),
            ),
            kind: sparkles_core::evidence::ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: NamedNode::new_unchecked("https://example.org/author"),
            scope: Scope::new(),
            confidence: ConfidenceFacets::default(),
            validity: None,
        });
        let popts = ProposeOptions::new(NamedNode::new_unchecked("urn:x-sparkles:principal:imp"));
        let p = evidence::propose(
            None,
            &ds,
            &ds,
            &batch,
            &popts,
            &mut |_, _| None,
            &Control::none(),
        )
        .unwrap();
        let claim = p
            .records
            .iter()
            .find(|r| r.key == "c1")
            .unwrap()
            .iri
            .clone();
        (ds, settings, claim)
    }

    #[test]
    fn no_write_returns_revalidate_registry_with_selected_head() {
        let admin = NamedNode::new_unchecked("urn:x-sparkles:principal:admin");
        let graph = NamedNode::new_unchecked(CLAIMS);

        // 1. Unkeyed no-op with a detach + re-protect + RDF commit interleaved right
        //    between `managed_writer` and `store.snapshot()`. Without revalidation, the
        //    no-op would combine the stale capability with the newer RDF commit and return
        //    `committed: false` claiming `claim` is already authenticated at the new head.
        {
            let (ds, settings, claim) = setup_ds();
            let fired = Arc::new(AtomicBool::new(false));
            let fired_c = fired.clone();
            let ds_c = ds.clone();
            let settings_c = settings.clone();
            let _guard = set_hook(move |point| {
                if point == "after_capability" && !fired_c.swap(true, Ordering::SeqCst) {
                    evidence::detach(&ds_c, CLAIMS).unwrap();
                    evidence::configure(&ds_c, &settings_c, &Control::none()).unwrap();
                    ds_c.update("INSERT DATA { GRAPH <urn:other> { <urn:a> <urn:b> \"1\" } }")
                        .unwrap();
                }
            });
            let res = reauthenticate(
                &ds,
                &ds,
                &graph,
                &ReauthenticateOptions::new(admin.clone()),
                &Control::none(),
            )
            .unwrap();
            assert!(fired.load(Ordering::SeqCst));
            // Must have retried under the new registry and actually committed!
            assert!(res.committed, "stale capability returned a false no-op");
            assert!(!res.records[0].already_authenticated);
            assert_eq!(res.records[0].iri, claim);
        }

        // 2. Unkeyed no-op where detach + re-protect occurs right before the no-op return
        //    (without advancing the RDF commit). Must retry and commit under the new nonce.
        {
            let (ds, settings, _claim) = setup_ds();
            let fired = Arc::new(AtomicBool::new(false));
            let fired_c = fired.clone();
            let ds_c = ds.clone();
            let settings_c = settings.clone();
            let _guard = set_hook(move |point| {
                if point == "before_noop_check" && !fired_c.swap(true, Ordering::SeqCst) {
                    evidence::detach(&ds_c, CLAIMS).unwrap();
                    evidence::configure(&ds_c, &settings_c, &Control::none()).unwrap();
                }
            });
            let res = reauthenticate(
                &ds,
                &ds,
                &graph,
                &ReauthenticateOptions::new(admin.clone()),
                &Control::none(),
            )
            .unwrap();
            assert!(fired.load(Ordering::SeqCst));
            assert!(
                res.committed,
                "registry change before no-op check must retry and commit"
            );
        }

        // 3. Fast-path keyed replay where detach + re-protect + RDF commit occurs between
        //    `managed_writer` and `store.snapshot()`. Under the new registry the old audit
        //    operation is no longer authenticated, so replay MUST NOT succeed using the
        //    stale capability; it retries and refuses with `immutable-record-conflict`.
        {
            let (ds, settings, _claim) = setup_ds();
            evidence::detach(&ds, CLAIMS).unwrap();
            evidence::configure(&ds, &settings, &Control::none()).unwrap();
            let mut opts = ReauthenticateOptions::new(admin.clone());
            opts.idempotency_key = Some("reauth-race-1".into());
            let first = reauthenticate(&ds, &ds, &graph, &opts, &Control::none()).unwrap();
            assert!(first.committed);

            let fired = Arc::new(AtomicBool::new(false));
            let fired_c = fired.clone();
            let ds_c = ds.clone();
            let settings_c = settings.clone();
            let _guard = set_hook(move |point| {
                if point == "after_capability" && !fired_c.swap(true, Ordering::SeqCst) {
                    evidence::detach(&ds_c, CLAIMS).unwrap();
                    evidence::configure(&ds_c, &settings_c, &Control::none()).unwrap();
                    ds_c.update("INSERT DATA { GRAPH <urn:other> { <urn:a> <urn:b> \"2\" } }")
                        .unwrap();
                }
            });
            let err = reauthenticate(&ds, &ds, &graph, &opts, &Control::none()).unwrap_err();
            assert!(fired.load(Ordering::SeqCst));
            assert!(
                matches!(
                    err,
                    Error::Managed(ref m) if m.kind == ManagedErrorKind::ImmutableRecordConflict
                ),
                "expected immutable-record-conflict after registry change, got {err:?}"
            );
        }

        // 4. Fast-path keyed replay where detach + re-protect occurs AFTER replay lookup
        //    succeeds, right before returning `Ok(res)` (`before_fast_replay_return`).
        //    Must not return the stale replay result; it retries and refuses with
        //    `immutable-record-conflict`.
        {
            let (ds, settings, _claim) = setup_ds();
            evidence::detach(&ds, CLAIMS).unwrap();
            evidence::configure(&ds, &settings, &Control::none()).unwrap();
            let mut opts = ReauthenticateOptions::new(admin.clone());
            opts.idempotency_key = Some("reauth-race-fast-return".into());
            let first = reauthenticate(&ds, &ds, &graph, &opts, &Control::none()).unwrap();
            assert!(first.committed);

            let fired = Arc::new(AtomicBool::new(false));
            let fired_c = fired.clone();
            let ds_c = ds.clone();
            let settings_c = settings.clone();
            let _guard = set_hook(move |point| {
                if point == "before_fast_replay_return" && !fired_c.swap(true, Ordering::SeqCst) {
                    evidence::detach(&ds_c, CLAIMS).unwrap();
                    evidence::configure(&ds_c, &settings_c, &Control::none()).unwrap();
                }
            });
            let err = reauthenticate(&ds, &ds, &graph, &opts, &Control::none()).unwrap_err();
            assert!(
                fired.load(Ordering::SeqCst),
                "expected before_fast_replay_return hook to fire"
            );
            assert!(
                matches!(
                    err,
                    Error::Managed(ref m) if m.kind == ManagedErrorKind::ImmutableRecordConflict
                ),
                "expected immutable-record-conflict after fast replay registry change, got {err:?}"
            );
        }

        // 5. Locked-path keyed replay on a non-main branch: an intervening peer commit
        //    lands the keyed operation before we acquire the branch writer lock so we
        //    enter locked-path replay, and then detach mutates the registry through
        //    `main` on a concurrent thread right at `before_locked_replay_return` (waiting
        //    until the new registry is installed before returning from the hook, so
        //    `managed_barrier` releases as soon as the retry drops `t`). Must not return
        //    the stale locked replay; it retries and refuses under the updated registry.
        {
            let (ds, settings, _claim) = setup_ds();
            evidence::detach(&ds, CLAIMS).unwrap();
            evidence::configure(&ds, &settings, &Control::none()).unwrap();
            ds.create_branch("dev", &Default::default()).unwrap();
            let dev = ds.branch("dev").unwrap();

            let mut opts = ReauthenticateOptions::new(admin.clone());
            opts.idempotency_key = Some("reauth-race-locked-return".into());

            let peer_fired = Arc::new(AtomicBool::new(false));
            let locked_fired = Arc::new(AtomicBool::new(false));
            let detach_handle: Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>> =
                Arc::new(std::sync::Mutex::new(None));
            let peer_fired_c = peer_fired.clone();
            let locked_fired_c = locked_fired.clone();
            let detach_handle_c = detach_handle.clone();
            let ds_c = ds.clone();
            let dev_c = dev.clone();
            let graph_c = graph.clone();
            let opts_c = opts.clone();
            let _guard = set_hook(move |point| {
                if point == "before_writer_lock" && !peer_fired_c.swap(true, Ordering::SeqCst) {
                    let main_t = ds_c.clone();
                    let dev_t = dev_c.clone();
                    let graph_t = graph_c.clone();
                    let opts_t = opts_c.clone();
                    let peer = std::thread::spawn(move || {
                        reauthenticate(&main_t, &dev_t, &graph_t, &opts_t, &Control::none())
                            .unwrap()
                    })
                    .join()
                    .unwrap();
                    assert!(peer.committed);
                } else if point == "before_locked_replay_return"
                    && !locked_fired_c.swap(true, Ordering::SeqCst)
                {
                    let rev_before = ds_c
                        .store()
                        .managed_snapshot()
                        .unwrap()
                        .unwrap()
                        .graphs
                        .revision;
                    let main_t = ds_c.clone();
                    let handle = std::thread::spawn(move || {
                        evidence::detach(&main_t, CLAIMS).unwrap();
                    });
                    *detach_handle_c.lock().unwrap() = Some(handle);
                    // `change_managed` publishes the new registry into `family.managed`
                    // before entering `managed_barrier()` to wait for `t` to drop.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    loop {
                        let rev_now = ds_c
                            .store()
                            .managed_snapshot()
                            .unwrap()
                            .unwrap()
                            .graphs
                            .revision;
                        if rev_now > rev_before {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "timed out waiting for detached registry to install"
                        );
                        std::thread::yield_now();
                    }
                }
            });
            let err = reauthenticate(&ds, &dev, &graph, &opts, &Control::none()).unwrap_err();
            if let Some(handle) = detach_handle.lock().unwrap().take() {
                handle.join().unwrap();
            }
            assert!(
                peer_fired.load(Ordering::SeqCst),
                "expected before_writer_lock hook to fire"
            );
            assert!(
                locked_fired.load(Ordering::SeqCst),
                "expected before_locked_replay_return hook to fire"
            );
            assert!(
                matches!(
                    err,
                    Error::Managed(ref m) if m.kind == ManagedErrorKind::UnsupportedProfile
                ),
                "expected unprotected-graph refusal after locked replay registry change, got {err:?}"
            );
        }
    }

    #[test]
    fn reauthenticate_classifies_tight_preparation_budget_across_overlapping_allocations() {
        let (ds, settings, claim) = setup_ds();
        let admin = NamedNode::new_unchecked("urn:x-sparkles:principal:admin");
        let graph = NamedNode::new_unchecked(CLAIMS);

        evidence::detach(&ds, CLAIMS).unwrap();
        evidence::configure(&ds, &settings, &Control::none()).unwrap();

        // A tiny local preparation budget (e.g. 2 KiB) fails during candidate/reads/quad
        // reservation and MUST map to `422 incomplete-evidence`, never raw `BudgetExceeded`.
        let mut opts = ReauthenticateOptions::new(admin.clone());
        opts.records = Some(vec![RecordCheck {
            iri: claim.clone(),
            expected_digest: None,
        }]);
        let err = reauthenticate_with_budget(&ds, &ds, &graph, &opts, &Control::none(), 2048)
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence
            ),
            "expected incomplete-evidence for tight explicit budget, got {err:?}"
        );

        // Whole-graph discovery under a tight budget also maps to `422 incomplete-evidence`.
        let whole_opts = ReauthenticateOptions::new(admin.clone());
        let err = reauthenticate_with_budget(&ds, &ds, &graph, &whole_opts, &Control::none(), 2048)
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence
            ),
            "expected incomplete-evidence for tight discovery budget, got {err:?}"
        );

        // Succeed with normal budget under a key, then replay under a tiny budget -> must
        // also classify as `422 incomplete-evidence`.
        let mut keyed = ReauthenticateOptions::new(admin);
        keyed.idempotency_key = Some("budget-replay-key".into());
        assert!(
            reauthenticate(&ds, &ds, &graph, &keyed, &Control::none())
                .unwrap()
                .committed
        );
        let err = reauthenticate_with_budget(&ds, &ds, &graph, &keyed, &Control::none(), 1024)
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence
            ),
            "expected incomplete-evidence for tight replay budget, got {err:?}"
        );
    }

    #[test]
    fn reauthenticate_long_graph_iri_distinct_receipts_exhausts_at_quad_planning() {
        let dir = tempfile::tempdir().unwrap();
        {
            let init = Dataset::open(dir.path()).unwrap();
            drop(init);
            let prep = crate::access::Preparation::new(Control::none(), 64 << 20).unwrap();
            sparkles_core::migrate::activate_configuration_history(dir.path(), false, &prep)
                .unwrap();
        }
        let ds = Dataset::from_store(
            sparkles_core::store::Store::open(
                dir.path(),
                sparkles_core::store::StoreOptions {
                    history_max_generations: 80,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        ds.store()
            .set_retention(sparkles_core::history::Retention {
                keep_commits: Some(512),
                ..Default::default()
            })
            .unwrap();

        let long_claims = format!("urn:ev:claims:{}", "g".repeat(3986));
        assert_eq!(long_claims.len(), 4000);
        let settings = Settings {
            enabled: true,
            source_records: vec![SOURCES.into()],
            proposals: vec![long_claims.clone()],
            ..Settings::default()
        };
        evidence::configure(&ds, &settings, &Control::none()).unwrap();

        let popts = ProposeOptions::new(NamedNode::new_unchecked("urn:x-sparkles:principal:imp"));
        let mut rev_batch = EvidenceInput::default();
        rev_batch.revisions.push(SourceRevisionInput {
            was_revision_of: None,
            version: RecordVersion::V1,
            key: RecordKey::new("rev-shared").unwrap(),
            source: NamedNode::new_unchecked("https://example.org/src/shared"),
            source_key: RecordKey::new("src-shared").unwrap(),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: Sha256Digest::of_bytes(b"rev-shared", &Control::none()).unwrap(),
            },
            metadata: SourceMetadata::default(),
            times: SourceTimes::default(),
            lineage: SourceLineage::default(),
        });
        let rev_prop = evidence::propose(
            None,
            &ds,
            &ds,
            &rev_batch,
            &popts,
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
        )
        .unwrap();
        let rev_iri = rev_prop.records[0].iri.clone();

        // Publish 300 claims in 300 distinct operations (compacting every 6 commits so
        // WAL replay stays bounded while all 50 generations remain retained) so each
        // renewed record has its own stale receipt and contributes 6 output quads owning
        // clones of `long_claims`.
        for i in 0..300 {
            let mut batch = EvidenceInput::default();
            let claim_key = RecordKey::new(format!("c-{i}")).unwrap();
            batch.claims.push(ClaimInput {
                version: RecordVersion::V1,
                key: claim_key.clone(),
                occurrence_key: claim_key,
                revision: sparkles_core::evidence::RecordRef::Existing(rev_iri.clone()),
                proposition: Triple::new(
                    NamedNode::new_unchecked(format!("https://example.org/s/{i}")),
                    NamedNode::new_unchecked("https://example.org/p"),
                    Literal::new_simple_literal(format!("v-{i}")),
                ),
                kind: sparkles_core::evidence::ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: NamedNode::new_unchecked("https://example.org/author"),
                scope: Scope::new(),
                confidence: ConfidenceFacets::default(),
                validity: None,
            });
            evidence::propose(
                None,
                &ds,
                &ds,
                &batch,
                &popts,
                &mut |_, _| Some(Arc::new(GraphAccess::all())),
                &Control::none(),
            )
            .unwrap();
            if (i + 1) % 6 == 0 {
                ds.store().compact().unwrap();
            }
        }

        evidence::detach(&ds, &long_claims).unwrap();
        evidence::configure(&ds, &settings, &Control::none()).unwrap();

        let admin = NamedNode::new_unchecked("urn:x-sparkles:principal:admin");
        let graph = NamedNode::new_unchecked(long_claims);
        let head_before = ds.store().head_commit().seq;

        // 7,100 KiB clears discovery and verification of all 300 records (bounded by
        // `MAX_CACHED_VIEWS = 8`), exceeds the old under-counted 5-graph-copy estimate
        // (~6,380 KiB), and falls below the actual 6-quad-per-record + Vec<Quad> +
        // receipt-index requirement (~7,940 KiB). It must reach quad planning, return
        // `422 incomplete-evidence`, and leave head unchanged.
        let reached_quad_planning = Arc::new(AtomicBool::new(false));
        let reached_c = reached_quad_planning.clone();
        let guard = set_hook(move |point| {
            if point == "before_writer_lock" {
                reached_c.store(true, Ordering::SeqCst);
            }
        });
        let err = reauthenticate_with_budget(
            &ds,
            &ds,
            &graph,
            &ReauthenticateOptions::new(admin.clone()),
            &Control::none(),
            7100 << 10,
        )
        .unwrap_err();
        drop(guard);
        assert!(
            reached_quad_planning.load(Ordering::SeqCst),
            "expected discovery and verification to finish and reach quad planning, got early error: {err:?}"
        );
        assert!(
            matches!(
                err,
                Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence
            ),
            "expected incomplete-evidence at quad planning, got {err:?}"
        );
        assert_eq!(
            ds.store().head_commit().seq,
            head_before,
            "quad-planning exhaustion must leave branch head unchanged"
        );

        // With enough allowance to cover all 6 quads per record and backing allocations,
        // the same reconciliation succeeds and commits all 300 records.
        let res = reauthenticate_with_budget(
            &ds,
            &ds,
            &graph,
            &ReauthenticateOptions::new(admin),
            &Control::none(),
            9000 << 10,
        )
        .unwrap();
        assert!(res.committed);
        assert_eq!(res.records.len(), 300);
        assert!(res.records.iter().all(|r| !r.already_authenticated));
    }
}
