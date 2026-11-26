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
    self, EV_ALREADY_AUTHENTICATED, EV_OPERATION_KIND, EV_REAUTHENTICATED_GRAPH,
    MAX_REAUTHENTICATE, OPERATION_KIND_REAUTH, PublicationViews, RecordCheck, VerifiedCandidate,
    build_quads, discover_records, payload_digest, require_configuration_history, verify_candidate,
};
use sparkles_core::evidence::resolve::{MEMORY_BYTES, kind_iri};
use sparkles_core::evidence::write::{
    self, ProtectedGraph, Reads, Stamp, operation_id, operation_iri,
};
use sparkles_core::evidence::{RecordKind, Sha256Digest};
use std::collections::BTreeSet;
use std::sync::Arc;
use uuid::Uuid;

pub use sparkles_core::evidence::reauth::{
    MAX_REAUTHENTICATE as MAX_REAUTHENTICATE_RECORDS, RecordCheck as ReauthenticateRecord,
};

const EV: &str = "urn:x-sparkles:evidence:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const EV_RECORD_DIGEST: &str = "urn:x-sparkles:evidence:recordDigest";

/// Maximum retries if a concurrent commit advances the branch head between
/// lock-free historical verification and acquiring the branch writer lock.
const MAX_HEAD_RETRIES: usize = 3;

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

fn to_records(verified: &[VerifiedCandidate]) -> Vec<ReauthenticatedRecord> {
    verified
        .iter()
        .map(|v| ReauthenticatedRecord {
            iri: v.iri.clone(),
            kind: v.kind,
            digest: v.digest,
            receipt: v.receipt.clone(),
            already_authenticated: v.already_authenticated,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn replay(
    main_dataset_id: Uuid,
    operation_iri: &NamedNode,
    principal: &NamedNode,
    graph: &NamedNode,
    other_record_graphs: &[NamedNode],
    current_nonce: Uuid,
    payload_digest: Sha256Digest,
    requested: Option<&[RecordCheck]>,
    found: write::RecordedOperation,
    reads: &mut Reads<'_>,
    branch: Uuid,
) -> Result<Reauthenticated> {
    let g = graph.as_str();
    let op = operation_iri.as_str();
    let in_target = reads.is_operation_in(op, g)?;
    let in_other = {
        let mut any = false;
        for other in other_record_graphs {
            if reads.is_operation_in(op, other.as_str())? {
                any = true;
                break;
            }
        }
        any
    };
    if !in_target || in_other {
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
    // Verify the replayed operation is genuinely a reconciliation audit operation for
    // `graph` in this dataset by `principal`.
    let valid_audit = reads.contains(
        op,
        RDF_TYPE,
        &Term::NamedNode(NamedNode::new_unchecked(format!("{PROV}Activity"))),
        g,
    )? && reads.contains(
        op,
        EV_OPERATION_KIND,
        &Term::NamedNode(NamedNode::new_unchecked(OPERATION_KIND_REAUTH)),
        g,
    )? && reads.contains(
        op,
        EV_REAUTHENTICATED_GRAPH,
        &Term::NamedNode(graph.clone()),
        g,
    )? && reads.contains(
        op,
        &format!("{PROV}wasAssociatedWith"),
        &Term::NamedNode(principal.clone()),
        g,
    )? && match reads.single(op, &format!("{EV}datasetId"), g)? {
        Some(Term::Literal(l)) => Uuid::parse_str(l.value()).ok() == Some(main_dataset_id),
        _ => false,
    };
    if !valid_audit {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!("operation <{op}> does not carry valid reconciliation audit provenance"),
        ));
    }

    let mut held = reads.preparation().reserve(0)?;
    let (used_terms, used_more) =
        reads.objects_up_to_held(op, PROV_USED, g, MAX_REAUTHENTICATE, &mut held)?;
    if used_more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            "replayed reconciliation operation exceeds record limit",
        ));
    }
    let (already_terms, already_more) = reads.objects_up_to_held(
        op,
        EV_ALREADY_AUTHENTICATED,
        g,
        MAX_REAUTHENTICATE,
        &mut held,
    )?;
    if already_more {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            "replayed reconciliation operation exceeds record limit",
        ));
    }
    let mut used = BTreeSet::new();
    for t in used_terms {
        let Term::NamedNode(n) = t else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("operation <{op}> has a non-IRI prov:used target"),
            ));
        };
        used.insert(n);
    }
    let mut already = BTreeSet::new();
    for t in already_terms {
        let Term::NamedNode(n) = t else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("operation <{op}> has a non-IRI ev:alreadyAuthenticated target"),
            ));
        };
        if used.contains(&n) {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "operation <{op}> lists <{}> as both renewed and already authenticated",
                    n.as_str()
                ),
            ));
        }
        already.insert(n);
    }
    if used.len().saturating_add(already.len()) > MAX_REAUTHENTICATE {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            "replayed reconciliation operation exceeds record limit",
        ));
    }

    // Every record the original operation verified (both renewed and already
    // authenticated) must still be authenticated under `current_nonce`.
    for iri in used.iter().chain(already.iter()) {
        if !reads.authenticated(iri.as_str(), g, Some(current_nonce))? {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "record <{}> from replayed operation <{op}> is no longer authenticated",
                    iri.as_str(),
                ),
            ));
        }
    }

    let iris: Vec<NamedNode> = match requested {
        Some(list) => {
            for item in list {
                if !used.contains(&item.iri) && !already.contains(&item.iri) {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "record <{}> was not recorded by replayed operation <{op}>",
                            item.iri.as_str(),
                        ),
                    ));
                }
            }
            list.iter().map(|r| r.iri.clone()).collect()
        }
        None => {
            let mut all: BTreeSet<NamedNode> = BTreeSet::new();
            all.extend(used.iter().cloned());
            all.extend(already.iter().cloned());
            all.into_iter().collect()
        }
    };
    let mut out = Vec::with_capacity(iris.len());
    for iri in iris {
        if !reads.authenticated(iri.as_str(), g, Some(current_nonce))? {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "record <{}> from replayed operation <{op}> is no longer authenticated",
                    iri.as_str(),
                ),
            ));
        }
        let kind = find_kind(reads, &iri, graph)?.ok_or_else(|| {
            managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "record <{}> from replayed operation <{op}> is missing its kind",
                    iri.as_str(),
                ),
            )
        })?;
        let digest = match reads.single(iri.as_str(), EV_RECORD_DIGEST, g)? {
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
        let receipt = reads.receipt_of(&iri, graph)?.ok_or_else(|| {
            managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "record <{}> from replayed operation <{op}> has no receipt",
                    iri.as_str(),
                ),
            )
        })?;
        out.push(ReauthenticatedRecord {
            already_authenticated: already.contains(&iri),
            iri,
            kind,
            digest,
            receipt,
        });
    }
    drop(held);
    Ok(Reauthenticated {
        operation: operation_iri.clone(),
        graph: graph.clone(),
        committed: false,
        replayed: true,
        commit: found.commit.unwrap_or_default(),
        branch: found.branch.unwrap_or(branch),
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

        // Pin a branch snapshot BEFORE acquiring the branch writer lock so historical
        // `BoundEvidenceView` reads never contend with `self.writer`.
        let head = store.snapshot();
        let preparation = crate::access::Preparation::new(control.clone(), MEMORY_BYTES)?;
        let mut reads = Reads::new(&preparation, &head);

        // Replay check always runs BEFORE `ifHead` on both fast and locked paths.
        if options.idempotency_key.is_some()
            && let Some(found) = reads.operation(&op_iri, &all_protected)?
        {
            return replay(
                main.dataset_id(),
                &op_iri,
                &options.principal,
                graph,
                &other_record_graphs,
                current_nonce,
                p_digest,
                options.records.as_deref(),
                found,
                &mut reads,
                branch,
            );
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
            if reads.has_subject_in(op_iri.as_str(), rg.as_str())? {
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
                let mut held = preparation.reserve(0)?;
                for item in list {
                    held.grow_by((item.iri.as_str().len() as u64).saturating_add(96))?;
                }
                (list.clone(), held)
            }
            None => {
                let (iris, held) = discover_records(&mut reads, graph, MAX_REAUTHENTICATE)?;
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
        let mut verified = Vec::with_capacity(checks.len());
        let mut verified_held = preparation.reserve(0)?;
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
            verified_held.grow_by(
                (v.iri.as_str().len() as u64)
                    .saturating_add(v.receipt.as_str().len() as u64)
                    .saturating_add(160),
            )?;
            verified.push(v);
        }
        drop(views);
        drop(reads);
        drop(_checks_held);
        drop(verified_held);
        drop(preparation);

        let needs_write = verified.iter().any(|v| !v.already_authenticated);
        if !needs_write && options.idempotency_key.is_none() {
            // Confirm the branch head and registry did not change before returning a
            // no-op answer.
            if store.snapshot().commit != head.commit {
                if attempt < MAX_HEAD_RETRIES {
                    continue;
                }
                return Err(Error::Conflict(
                    "the branch head changed while re-authentication was prepared; retry".into(),
                ));
            }
            return Ok(Reauthenticated {
                operation: op_iri,
                graph: graph.clone(),
                committed: false,
                replayed: false,
                commit: head.commit,
                branch,
                records: to_records(&verified),
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
        let mut t = store.try_write_with(crate::commit::CommitKind::Transaction, opts)?;
        t.set_managed(capability)?;

        if t.base().commit != head.commit {
            // Another commit landed while we verified. First check locked-path replay
            // (before `ifHead`, matching the fast path).
            if options.idempotency_key.is_some() {
                let locked_head = t.base().clone();
                let locked_prep = crate::access::Preparation::new(control.clone(), MEMORY_BYTES)?;
                let mut locked_reads = Reads::new(&locked_prep, &locked_head);
                if let Some(found) = locked_reads.operation(&op_iri, &all_protected)? {
                    return replay(
                        main.dataset_id(),
                        &op_iri,
                        &options.principal,
                        graph,
                        &other_record_graphs,
                        current_nonce,
                        p_digest,
                        options.records.as_deref(),
                        found,
                        &mut locked_reads,
                        branch,
                    );
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

        // `t.base().commit == head.commit` and `set_managed` confirmed the registry did
        // not change, so all verification results and audit-subject checks are exact for
        // `t.base()`.
        let (seq, time_ms) = t.managed_stamp()?;
        let stamp = Stamp {
            dataset: main.dataset_id(),
            branch,
            commit: seq,
            time_ms,
        };
        let (deletes, inserts) = build_quads(
            &op_iri,
            &options.principal,
            p_digest,
            options.idempotency_key.as_deref(),
            graph,
            current_nonce,
            &stamp,
            &verified,
        );

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
        let committed = t.commit();
        drop(release);
        let records = to_records(&verified);
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
        "the branch head changed while re-authentication was prepared; retry".into(),
    ))
}

// Re-export module for callers that want constants.
pub use reauth::MAX_RECORD_TRIPLES;
