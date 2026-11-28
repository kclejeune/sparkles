//! Support-aware visibility of C25 accepted projections (spec C25 §5 and §7, F18 §7.1,
//! roadmap item 582).
//!
//! When a dataset has enabled evidence settings with review policies, each policy
//! projects accepted positive claims into its dedicated output graph. Under a restricted
//! read view, a quad in a policy's output graph is visible only when the caller can read
//! at least one live, authenticated acceptance support for it — the support record, its
//! current review decision, its non-withdrawn claim, the claim's source revision and any
//! supporting evidence links and targets — under the caller's current graph grants and
//! triple protections.
//!
//! Unsupported quads join the masked snapshot's deletions ([`super::Mask`]), so every
//! SPARQL query, aggregate, path, Graph Store read, Recall, Ask, export and count sees
//! only visible triples without disclosing hidden supports.

use super::{Limits, too_many};
use crate::access::preparation::{Buffer, Map, Text};
use crate::access::{GraphAccess, GraphGuard, GraphRule, Graphs, Preparation, Reservation};
use crate::evidence::review::{MAX_EVIDENCE, NONCE_PREDICATES, policy_iri};
use crate::evidence::write::Reads;
use crate::evidence::{Policy, Settings};
use crate::history::ConfigurationIdentity;
use crate::id::{Id, Tag};
use crate::index::{Key, Perm};
use crate::managed::{ManagedError, ManagedErrorKind, ManagedGraphs, OwnerEntry, OwnerKind};
use crate::store::Snapshot;
use crate::task::Control;
use crate::{Error, Result};
use oxrdf::{Literal, NamedOrBlankNode, Term, Triple};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

const EV: &str = "urn:x-sparkles:evidence:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const OA: &str = "http://www.w3.org/ns/oa#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";

/// Maximum nesting depth for stored-fact references that point into earlier commits or
/// other review policies' output graphs.
const MAX_STORED_FACT_DEPTH: u8 = 4;

/// Maximum number of `#...` fragment sub-nodes traversed per claim or passage before
/// failing closed.
const MAX_FRAGMENT_SUBNODES: usize = 32;

/// Retained header charge for cloning a snapshot to attach a pinned evidence config.
pub(crate) const PINNED_SNAPSHOT_HEADER_BYTES: u64 = 4096;

/// Resolved state of support-aware projection visibility configuration for a snapshot.
#[derive(Clone, Debug)]
pub(crate) enum EvidenceConfig {
    /// Verified configuration with no active review policies.
    None,
    /// Verified configuration with at least one active review policy.
    Active(Arc<ProjectionConfig>),
    /// Managed registry or historical configuration is unavailable, unreadable, or failed
    /// to resolve. Restricted reads must fail closed.
    Unavailable(String),
}

impl EvidenceConfig {
    pub(crate) fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// Generic fail-closed refusal when evidence visibility configuration cannot be verified.
pub(crate) fn unavailable_error(reason: &str) -> Error {
    ManagedError::error(ManagedErrorKind::Unavailable, reason.to_string())
}

/// Resolved review-policy configuration used to compute support-aware projection masks.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionConfig {
    /// Stable cache-key fingerprint covering the config identity/revision, settings
    /// digest and graph protection nonces.
    pub(crate) fingerprint: String,
    /// Validated, enabled evidence settings with at least one review policy.
    pub(crate) settings: Settings,
    /// Active protection nonces for graphs currently protected by `OwnerKind::Evidence`.
    pub(crate) protections: BTreeMap<String, Uuid>,
}

impl ProjectionConfig {
    /// Eagerly derive the active projection visibility configuration when an installed
    /// managed registry is constructed. Missing or unreadable evidence settings on an
    /// installed evidence owner fail closed as [`EvidenceConfig::Unavailable`].
    pub(crate) fn from_installed_parts(
        graphs: &ManagedGraphs,
        settings: &BTreeMap<OwnerKind, Arc<[u8]>>,
    ) -> EvidenceConfig {
        let Some(entry) = graphs.owners.get(&OwnerKind::Evidence) else {
            return EvidenceConfig::None;
        };
        let Some(bytes) = settings.get(&OwnerKind::Evidence) else {
            return EvidenceConfig::Unavailable(
                "evidence settings are missing from the installed managed registry".into(),
            );
        };
        let tag = format!("r{}", graphs.revision);
        match Self::from_owner(&tag, entry, bytes, &Control::none()) {
            Ok(cfg) => cfg,
            Err(e) => EvidenceConfig::Unavailable(format!(
                "installed evidence settings cannot be read: {e}"
            )),
        }
    }

    /// Derive the projection visibility configuration from a resolved historical
    /// configuration generation, charging decoding scratch and returning a retained
    /// [`Reservation`] covering the pinned [`ProjectionConfig`] and cloned snapshot header.
    pub(crate) fn from_generation(
        identity: ConfigurationIdentity,
        owners: &BTreeMap<OwnerKind, OwnerEntry>,
        owner_settings: &BTreeMap<OwnerKind, String>,
        preparation: &Preparation,
    ) -> Result<(EvidenceConfig, Reservation)> {
        preparation.check()?;
        let Some(entry) = owners.get(&OwnerKind::Evidence) else {
            return Ok((
                EvidenceConfig::None,
                preparation.reserve(PINNED_SNAPSHOT_HEADER_BYTES)?,
            ));
        };
        let Some(text) = owner_settings.get(&OwnerKind::Evidence) else {
            return Ok((
                EvidenceConfig::Unavailable(
                    "historical evidence settings are missing from the configuration generation"
                        .into(),
                ),
                preparation.reserve(PINNED_SNAPSHOT_HEADER_BYTES)?,
            ));
        };
        let bytes = text.as_bytes();
        let decoding = preparation.reserve(
            (bytes.len() as u64)
                .saturating_mul(4)
                .saturating_add(64 << 10),
        )?;
        let tag = format!("cfg:{}:{}", identity.lineage_id, identity.generation);
        let config = Self::from_owner(&tag, entry, bytes, preparation.control())?;
        let retained_bytes = match &config {
            EvidenceConfig::Active(cfg) => projection_config_bytes(cfg, preparation)?
                .saturating_add(PINNED_SNAPSHOT_HEADER_BYTES),
            EvidenceConfig::None => PINNED_SNAPSHOT_HEADER_BYTES,
            EvidenceConfig::Unavailable(reason) => {
                (reason.capacity() as u64).saturating_add(PINNED_SNAPSHOT_HEADER_BYTES)
            }
        };
        let held = preparation.reserve(retained_bytes)?;
        drop(decoding);
        preparation.check()?;
        Ok((config, held))
    }

    fn from_owner(
        tag: &str,
        entry: &OwnerEntry,
        settings_bytes: &[u8],
        control: &Control,
    ) -> Result<EvidenceConfig> {
        let settings = match Settings::from_slice(settings_bytes, control) {
            Ok(settings) => settings,
            Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(e);
            }
            Err(e) => {
                return Ok(EvidenceConfig::Unavailable(format!(
                    "evidence settings cannot be parsed: {e}"
                )));
            }
        };
        if !settings.enabled || settings.policies.is_empty() {
            return Ok(EvidenceConfig::None);
        }
        let mut protections = BTreeMap::new();
        for graph in &entry.graphs {
            if let Some(&nonce) = entry.protections.get(graph) {
                protections.insert(graph.clone(), nonce);
            }
        }
        let mut hasher = Sha256::new();
        hasher.update(tag.as_bytes());
        hasher.update([0]);
        hasher.update(entry.settings_digest.as_bytes());
        hasher.update([0]);
        for (graph, nonce) in &protections {
            hasher.update(graph.as_bytes());
            hasher.update([0]);
            hasher.update(nonce.as_bytes());
        }
        let digest = hasher.finalize();
        let mut fingerprint = String::with_capacity(tag.len() + 1 + 32);
        fingerprint.push_str(tag);
        fingerprint.push(':');
        for byte in &digest[..16] {
            let _ = write!(fingerprint, "{byte:02x}");
        }
        Ok(EvidenceConfig::Active(Arc::new(Self {
            fingerprint,
            settings,
            protections,
        })))
    }

    /// Whether `graph_iri` is the output graph of any active review policy.
    pub(crate) fn is_output_graph(&self, graph_iri: &str) -> bool {
        self.settings
            .policies
            .values()
            .any(|p| p.output == graph_iri)
    }

    /// Whether any review policy in this configuration has an output graph that `access`
    /// is permitted to read (independent of whether `snap` currently holds quads in it).
    /// Used to deterministically include `|ev:<fp>` in mask keys for idempotent re-entry.
    pub(crate) fn has_readable_policy_output(
        &self,
        access: &GraphAccess,
        preparation: &Preparation,
    ) -> Result<bool> {
        if access.reads_everything() {
            return Ok(false);
        }
        for policy in self.settings.policies.values() {
            preparation.check()?;
            if preparation.allows_iri(&access.read, &policy.output)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Legacy uncontrolled counterpart of [`Self::has_readable_policy_output`].
    pub(crate) fn has_readable_policy_output_uncontrolled(&self, access: &GraphAccess) -> bool {
        if access.reads_everything() {
            return false;
        }
        self.settings
            .policies
            .values()
            .any(|policy| access.read.allows_named_iri(&policy.output))
    }

    /// Whether any review policy in this configuration has a non-empty output graph in
    /// `snap` that `access` can read, requiring support-aware visibility masking.
    pub(crate) fn has_applicable_output(
        &self,
        access: &GraphAccess,
        snap: &Snapshot,
        preparation: &Preparation,
    ) -> Result<bool> {
        if access.reads_everything() {
            return Ok(false);
        }
        for policy in self.settings.policies.values() {
            preparation.check()?;
            if !preparation.allows_iri(&access.read, &policy.output)? {
                continue;
            }
            let Some(output_id) = preparation.lookup_iri(snap, &policy.output)? else {
                continue;
            };
            if graph_nonempty(snap, output_id, preparation)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Legacy check equivalent to [`Self::has_applicable_output`] without a [`Preparation`].
    pub(crate) fn has_applicable_output_uncontrolled(
        &self,
        access: &GraphAccess,
        snap: &Snapshot,
    ) -> Result<bool> {
        if access.reads_everything() {
            return Ok(false);
        }
        for policy in self.settings.policies.values() {
            if !access.read.allows_named_iri(&policy.output) {
                continue;
            }
            let Some(output_id) = snap.lookup_iri(&policy.output) else {
                continue;
            };
            if snap.count(Perm::Gspo, &[output_id.0])? > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn projection_config_bytes(cfg: &ProjectionConfig, prep: &Preparation) -> Result<u64> {
    let mut bytes = (std::mem::size_of::<ProjectionConfig>() as u64)
        .saturating_add(cfg.fingerprint.capacity() as u64)
        .saturating_add(512);
    for list in [
        &cfg.settings.source_records,
        &cfg.settings.proposals,
        &cfg.settings.decisions,
        &cfg.settings.supports,
        &cfg.settings.projections,
    ] {
        bytes = bytes.saturating_add(
            (list.capacity() as u64).saturating_mul(std::mem::size_of::<String>() as u64),
        );
        for s in list {
            prep.check()?;
            bytes = bytes.saturating_add(s.capacity() as u64).saturating_add(32);
        }
    }
    for (name, policy) in &cfg.settings.policies {
        prep.check()?;
        bytes = bytes
            .saturating_add(name.capacity() as u64)
            .saturating_add(std::mem::size_of::<Policy>() as u64)
            .saturating_add(policy.decisions.capacity() as u64)
            .saturating_add(policy.supports.capacity() as u64)
            .saturating_add(policy.output.capacity() as u64)
            .saturating_add(256);
        for s in &policy.boundary {
            prep.check()?;
            bytes = bytes.saturating_add(s.capacity() as u64).saturating_add(32);
        }
        for s in &policy.required_scope {
            prep.check()?;
            bytes = bytes.saturating_add(s.capacity() as u64).saturating_add(32);
        }
    }
    for graph in cfg.protections.keys() {
        prep.check()?;
        bytes = bytes
            .saturating_add(graph.capacity() as u64)
            .saturating_add(96);
    }
    Ok(bytes)
}

/// Append every quad in any readable review-policy output graph that lacks a live,
/// authenticated, caller-readable support to `hidden`.
pub(crate) fn append_hidden(
    access: &GraphAccess,
    base_hidden: &[Key],
    config: &ProjectionConfig,
    snap: &Snapshot,
    limits: &Limits,
    preparation: &Preparation,
    hidden: &mut Buffer<Key>,
) -> Result<()> {
    append_hidden_with_depth(
        access,
        base_hidden,
        config,
        snap,
        limits,
        preparation,
        hidden,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_hidden_with_depth(
    access: &GraphAccess,
    base_hidden: &[Key],
    config: &ProjectionConfig,
    snap: &Snapshot,
    limits: &Limits,
    preparation: &Preparation,
    hidden: &mut Buffer<Key>,
    depth: u8,
) -> Result<()> {
    if access.reads_everything() {
        return Ok(());
    }
    preparation.stage("preparing projection support visibility")?;
    let mut eval = Evaluator::new(access, base_hidden, config, snap, preparation, depth)?;
    for (policy_name, policy) in &config.settings.policies {
        preparation.check()?;
        if !preparation.allows_iri(&access.read, &policy.output)? {
            continue;
        }
        let Some(output_id) = preparation.lookup_iri(snap, &policy.output)? else {
            continue;
        };
        if !graph_nonempty(snap, output_id, preparation)? {
            continue;
        }
        let policy_ctx = eval.prepare_policy(policy_name, policy, output_id)?;
        let Some(policy_ctx) = policy_ctx else {
            // The caller cannot read the policy's decisions or supports graphs, or the
            // projection itself does not authenticate under the current nonces: fail
            // closed by hiding every quad in the output graph.
            hide_all_in_graph(snap, output_id, base_hidden, limits, preparation, hidden)?;
            continue;
        };
        let mut to_hide = Buffer::new(preparation)?;
        preparation.scan(snap, Perm::Gspo, &[output_id.0], |quad| {
            preparation.check()?;
            let key = [quad[0].0, quad[1].0, quad[2].0, output_id.0];
            if base_hidden.binary_search(&key).is_ok() {
                return Ok(true);
            }
            if !eval.triple_supported(&policy_ctx, [quad[0], quad[1], quad[2]])? {
                let total = base_hidden
                    .len()
                    .saturating_add(hidden.len())
                    .saturating_add(to_hide.len()) as u64;
                if total >= limits.max_hidden {
                    return Err(too_many(limits, total.saturating_add(1)));
                }
                to_hide.push(key)?;
            }
            Ok(true)
        })?;
        for &key in to_hide.iter() {
            preparation.check()?;
            hidden.push(key)?;
        }
    }
    preparation.check()
}

fn hide_all_in_graph(
    snap: &Snapshot,
    graph_id: Id,
    base_hidden: &[Key],
    limits: &Limits,
    preparation: &Preparation,
    hidden: &mut Buffer<Key>,
) -> Result<()> {
    preparation.scan(snap, Perm::Gspo, &[graph_id.0], |quad| {
        preparation.check()?;
        let key = [quad[0].0, quad[1].0, quad[2].0, graph_id.0];
        if base_hidden.binary_search(&key).is_ok() {
            return Ok(true);
        }
        let total = base_hidden.len().saturating_add(hidden.len()) as u64;
        if total >= limits.max_hidden {
            return Err(too_many(limits, total.saturating_add(1)));
        }
        hidden.push(key)?;
        Ok(true)
    })
}

fn graph_nonempty(snap: &Snapshot, graph_id: Id, preparation: &Preparation) -> Result<bool> {
    let mut any = false;
    preparation.scan(snap, Perm::Gspo, &[graph_id.0], |_| {
        any = true;
        Ok(false)
    })?;
    Ok(any)
}

/// Whether `base_hidden` (sorted SPO keys) hides any triple of subject `subj` in `graph`,
/// polling `prep` controls during the scan.
fn subject_masked_checked(
    base_hidden: &[Key],
    subj: Id,
    graph: Id,
    prep: &Preparation,
) -> Result<bool> {
    if base_hidden.is_empty() {
        return Ok(false);
    }
    let start = base_hidden.partition_point(|k| k[0] < subj.0);
    for (idx, k) in base_hidden[start..].iter().enumerate() {
        if idx.is_multiple_of(256) {
            prep.check()?;
        }
        if k[0] != subj.0 {
            break;
        }
        if k[3] == graph.0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Vocabulary term IDs looked up once per mask build.
struct Vocab {
    rdf_type: Option<Id>,
    rdf_reifies: Option<Id>,
    ev_operation: Option<Id>,
    ev_projection: Option<Id>,
    ev_acceptance_support: Option<Id>,
    ev_review_decision: Option<Id>,
    ev_claim: Option<Id>,
    ev_withdrawal: Option<Id>,
    ev_source_revision: Option<Id>,
    ev_rendition: Option<Id>,
    ev_passage: Option<Id>,
    ev_evidence_link: Option<Id>,
    ev_evidence_ref: Option<Id>,
    ev_availability_receipt: Option<Id>,
    ev_graph_protection: Option<Id>,
    ev_review_policy: Option<Id>,
    ev_output: Option<Id>,
    ev_supported_triple: Option<Id>,
    ev_decision: Option<Id>,
    ev_claim_pred: Option<Id>,
    ev_supersedes: Option<Id>,
    ev_withdraws: Option<Id>,
    ev_decisions_protection: Option<Id>,
    ev_supports_protection: Option<Id>,
    ev_output_protection: Option<Id>,
    prov_was_derived_from: Option<Id>,
    ev_target: Option<Id>,
    ev_relation: Option<Id>,
    ev_supports_rel: Option<Id>,
    ev_evidence: Option<Id>,
    ev_source_revision_pred: Option<Id>,
    ev_rendition_pred: Option<Id>,
    ev_passage_pred: Option<Id>,
    oa_has_source: Option<Id>,
    ev_dataset_id: Option<Id>,
    ev_branch_id: Option<Id>,
    ev_commit: Option<Id>,
    ev_graph_kind: Option<Id>,
    ev_default_graph: Option<Id>,
    ev_named_graph: Option<Id>,
    ev_graph: Option<Id>,
    ev_expected_triple: Option<Id>,
    ev_reifier: Option<Id>,
    ev_skolem_map: Option<Id>,
}

impl Vocab {
    fn lookup(snap: &Snapshot, prep: &Preparation) -> Result<Self> {
        let iri = |s: &str| prep.lookup_iri(snap, s);
        let ev = |local: &str| {
            let mut full = Text::new(prep)?;
            full.push_str(EV)?;
            full.push_str(local)?;
            iri(&full.value)
        };
        let pref = |ns: &str, local: &str| {
            let mut full = Text::new(prep)?;
            full.push_str(ns)?;
            full.push_str(local)?;
            iri(&full.value)
        };
        Ok(Self {
            rdf_type: iri(RDF_TYPE)?,
            rdf_reifies: iri(RDF_REIFIES)?,
            ev_operation: ev("Operation")?,
            ev_projection: ev("Projection")?,
            ev_acceptance_support: ev("AcceptanceSupport")?,
            ev_review_decision: ev("ReviewDecision")?,
            ev_claim: ev("Claim")?,
            ev_withdrawal: ev("Withdrawal")?,
            ev_source_revision: ev("SourceRevision")?,
            ev_rendition: ev("Rendition")?,
            ev_passage: ev("Passage")?,
            ev_evidence_link: ev("EvidenceLink")?,
            ev_evidence_ref: ev("EvidenceRef")?,
            ev_availability_receipt: ev("availabilityReceipt")?,
            ev_graph_protection: ev("graphProtection")?,
            ev_review_policy: ev("reviewPolicy")?,
            ev_output: ev("output")?,
            ev_supported_triple: ev("supportedTriple")?,
            ev_decision: ev("decision")?,
            ev_claim_pred: ev("claim")?,
            ev_supersedes: ev("supersedes")?,
            ev_withdraws: ev("withdraws")?,
            ev_decisions_protection: iri(NONCE_PREDICATES[0])?,
            ev_supports_protection: iri(NONCE_PREDICATES[1])?,
            ev_output_protection: iri(NONCE_PREDICATES[2])?,
            prov_was_derived_from: pref(PROV, "wasDerivedFrom")?,
            ev_target: ev("target")?,
            ev_relation: ev("relation")?,
            ev_supports_rel: ev("Supports")?,
            ev_evidence: ev("evidence")?,
            ev_source_revision_pred: ev("sourceRevision")?,
            ev_rendition_pred: ev("rendition")?,
            ev_passage_pred: ev("passage")?,
            oa_has_source: pref(OA, "hasSource")?,
            ev_dataset_id: ev("datasetId")?,
            ev_branch_id: ev("branchId")?,
            ev_commit: ev("commit")?,
            ev_graph_kind: ev("graphKind")?,
            ev_default_graph: ev("DefaultGraph")?,
            ev_named_graph: ev("NamedGraph")?,
            ev_graph: ev("graph")?,
            ev_expected_triple: ev("expectedTriple")?,
            ev_reifier: ev("reifier")?,
            ev_skolem_map: ev("skolemMap")?,
        })
    }
}

/// Pre-resolved graph metadata for a single configured evidence graph.
#[derive(Clone, Copy)]
struct GraphRef {
    id: Id,
    nonce: Option<Uuid>,
    nonce_id: Option<Id>,
    readable: bool,
}

/// Pre-resolved metadata for one review policy.
struct PolicyCtx {
    policy_iri_id: Id,
    output_id: Id,
    decisions: GraphRef,
    supports: GraphRef,
    /// All graphs in `policy.boundary` present in `snap`, including dual-role graphs that
    /// retained a protection nonce after leaving `settings.proposals`.
    boundary_graphs: Buffer<GraphRef>,
}

/// Cached historical masked snapshot for earlier-commit `ev:StoredFact` checks.
struct CachedFactSnapshot {
    snap: Option<Arc<Snapshot>>,
    _held: Reservation,
}

struct Evaluator<'a> {
    access: &'a GraphAccess,
    base_hidden: &'a [Key],
    config: &'a ProjectionConfig,
    snap: &'a Snapshot,
    prep: &'a Preparation,
    vocab: Vocab,
    depth: u8,
    /// All configured proposal graphs plus retained boundary record graphs present in `snap`.
    record_graphs: Buffer<GraphRef>,
    /// All configured source-record graphs present in `snap`.
    source_records: Buffer<GraphRef>,
    /// Memoized `(decisions_graph_id, decision_id) -> live & readable`.
    decision_cache: Map<(u64, u64), bool>,
    /// Memoized `(policy_iri_id, claim_id) -> live & readable with evidence`.
    claim_cache: Map<(u64, u64), bool>,
    /// Memoized `(source_graph_id, record_id, kind_tag) -> authenticated & readable`.
    source_cache: Map<(u64, u64, u8), bool>,
    /// Active `(output_graph_id, [s, p, o])` pairs under evaluation to break same-commit cycles.
    active_outputs: Buffer<(u64, [Id; 3])>,
    /// Cached masked snapshots for earlier-commit stored-fact proofs, keyed by commit seq.
    fact_views: BTreeMap<u64, CachedFactSnapshot>,
}

impl<'a> Evaluator<'a> {
    fn new(
        access: &'a GraphAccess,
        base_hidden: &'a [Key],
        config: &'a ProjectionConfig,
        snap: &'a Snapshot,
        prep: &'a Preparation,
        depth: u8,
    ) -> Result<Self> {
        let vocab = Vocab::lookup(snap, prep)?;
        let source_records =
            resolve_graphs(&config.settings.source_records, config, access, snap, prep)?;
        let mut record_graphs =
            resolve_graphs(&config.settings.proposals, config, access, snap, prep)?;
        // Also include retained boundary graphs and source-record graphs that still hold a
        // protection nonce so dual-role graphs that left `settings.proposals` remain visible
        // to claim, withdrawal, and EvidenceRef lookup (matching review/resolve traversal).
        for policy in config.settings.policies.values() {
            for iri in &policy.boundary {
                prep.check()?;
                if !config.protections.contains_key(iri) {
                    continue;
                }
                if let Some(gr) = resolve_graph(iri, config, access, snap, prep)?
                    && !record_graphs.iter().any(|existing| existing.id == gr.id)
                {
                    record_graphs.push(gr)?;
                }
            }
        }
        for &sg in source_records.iter() {
            prep.check()?;
            if sg.nonce.is_some() && !record_graphs.iter().any(|existing| existing.id == sg.id) {
                record_graphs.push(sg)?;
            }
        }
        Ok(Self {
            access,
            base_hidden,
            config,
            snap,
            prep,
            vocab,
            depth,
            record_graphs,
            source_records,
            decision_cache: Map::new(prep)?,
            claim_cache: Map::new(prep)?,
            source_cache: Map::new(prep)?,
            active_outputs: Buffer::new(prep)?,
            fact_views: BTreeMap::new(),
        })
    }

    /// Resolve a policy's metadata graphs and verify its `ev:Projection` records.
    /// Returns `None` if the caller cannot read the policy's decisions or supports
    /// graphs, or if the projection does not authenticate under the current nonces.
    fn prepare_policy(
        &mut self,
        policy_name: &str,
        policy: &Policy,
        output_id: Id,
    ) -> Result<Option<PolicyCtx>> {
        if !self.prep.allows_iri(&self.access.read, &policy.decisions)?
            || !self.prep.allows_iri(&self.access.read, &policy.supports)?
        {
            return Ok(None);
        }
        let Some(decisions) = resolve_graph(
            &policy.decisions,
            self.config,
            self.access,
            self.snap,
            self.prep,
        )?
        else {
            return Ok(None);
        };
        let Some(supports) = resolve_graph(
            &policy.supports,
            self.config,
            self.access,
            self.snap,
            self.prep,
        )?
        else {
            return Ok(None);
        };
        let Some(output_nonce) = self.config.protections.get(&policy.output).copied() else {
            return Ok(None);
        };
        let (Some(dec_nonce), Some(sup_nonce)) = (decisions.nonce, supports.nonce) else {
            return Ok(None);
        };
        let p_iri = policy_iri(policy_name);
        let Some(policy_iri_id) = self.prep.lookup_iri(self.snap, p_iri.as_str())? else {
            return Ok(None);
        };
        if !self.projection_authenticates(
            policy_iri_id,
            supports,
            [dec_nonce, sup_nonce, output_nonce],
        )? {
            return Ok(None);
        }
        let mut boundary_graphs: Buffer<GraphRef> = Buffer::new(self.prep)?;
        for g in &policy.boundary {
            self.prep.check()?;
            if let Some(gr) = resolve_graph(g, self.config, self.access, self.snap, self.prep)?
                && !boundary_graphs.iter().any(|existing| existing.id == gr.id)
            {
                boundary_graphs.push(gr)?;
            }
        }
        Ok(Some(PolicyCtx {
            policy_iri_id,
            output_id,
            decisions,
            supports,
            boundary_graphs,
        }))
    }

    /// Enforce caller visibility of each active `ev:Projection` record and its
    /// `ev:Operation` receipt in `supports`. Authenticated revisions end the
    /// activity of superseded records of this policy.
    fn projection_authenticates(
        &self,
        policy_iri_id: Id,
        supports: GraphRef,
        expected_nonces: [Uuid; 3],
    ) -> Result<bool> {
        let (
            Some(rdf_type),
            Some(proj_class),
            Some(review_policy),
            Some(dec_p),
            Some(sup_p),
            Some(out_p),
        ) = (
            self.vocab.rdf_type,
            self.vocab.ev_projection,
            self.vocab.ev_review_policy,
            self.vocab.ev_decisions_protection,
            self.vocab.ev_supports_protection,
            self.vocab.ev_output_protection,
        )
        else {
            return Ok(false);
        };
        let predecessor = self
            .prep
            .lookup_iri(self.snap, &format!("{PROV}wasRevisionOf"))?;
        let mut records = Map::new(self.prep)?;
        let mut superseded = Map::new(self.prep)?;
        let mut more = false;
        Reads::new(self.prep, self.snap).for_each_pos_intersection_in_graph(
            supports.id,
            &[(rdf_type, proj_class), (review_policy, policy_iri_id)],
            |rec| {
                if records.len() == MAX_EVIDENCE {
                    more = true;
                    return Ok(false);
                }
                let authenticated = self.is_authenticated(rec, supports)?;
                records.insert(rec, authenticated)?;
                if authenticated && let Some(pred) = predecessor {
                    let mut edges = 0;
                    self.prep.scan(
                        self.snap,
                        Perm::Gspo,
                        &[supports.id.0, rec.0, pred.0],
                        |q| {
                            if edges == MAX_EVIDENCE {
                                more = true;
                                return Ok(false);
                            }
                            edges += 1;
                            superseded.insert(q[2], ())?;
                            Ok(true)
                        },
                    )?;
                }
                Ok(!more)
            },
        )?;
        if more {
            return Ok(false);
        }
        let nonce_preds = [dec_p, sup_p, out_p];
        let mut found = false;
        for (&rec, &authenticated) in records.iter() {
            self.prep.check()?;
            if superseded.contains_key(&rec) {
                continue;
            }
            found = true;
            if !authenticated {
                return Ok(false);
            }
            if self.record_or_receipt_masked(rec, supports)? {
                return Ok(false);
            }
            for (&pred, &expected) in nonce_preds.iter().zip(&expected_nonces) {
                if self.single_uuid(rec, pred, supports.id)? != Some(expected) {
                    return Ok(false);
                }
            }
        }
        Ok(found)
    }

    /// Whether `triple = [s, p, o]` in `policy_ctx.output_id` has at least one live,
    /// authenticated, caller-readable acceptance support.
    fn triple_supported(&mut self, policy_ctx: &PolicyCtx, triple: [Id; 3]) -> Result<bool> {
        let active_key = (policy_ctx.output_id.0, triple);
        if self.active_outputs.contains(&active_key) {
            return Ok(false);
        }
        self.active_outputs.push(active_key)?;
        let res = self.triple_supported_inner(policy_ctx, triple);
        let _ = self.active_outputs.pop();
        res
    }

    fn triple_supported_inner(&mut self, policy_ctx: &PolicyCtx, triple: [Id; 3]) -> Result<bool> {
        let Some(supported_triple_pred) = self.vocab.ev_supported_triple else {
            return Ok(false);
        };
        if triple[0].tag() == Tag::BNode || triple[2].tag() == Tag::BNode {
            return Ok(false);
        }
        let Some(tt_id) = self.lookup_triple_term(triple)? else {
            return Ok(false);
        };
        let mut candidates = Buffer::new(self.prep)?;
        let mut more = false;
        self.prep.scan(
            self.snap,
            Perm::Pos,
            &[supported_triple_pred.0, tt_id.0],
            |q| {
                self.prep.check()?;
                if q[3] != policy_ctx.supports.id {
                    return Ok(true);
                }
                if candidates.len() == MAX_EVIDENCE {
                    more = true;
                    return Ok(false);
                }
                candidates.push(q[0])?;
                Ok(true)
            },
        )?;
        if more {
            return Ok(false);
        }
        for &support_id in candidates.iter() {
            self.prep.check()?;
            if self.support_live_and_readable(policy_ctx, support_id, tt_id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn support_live_and_readable(
        &mut self,
        policy_ctx: &PolicyCtx,
        support_id: Id,
        tt_id: Id,
    ) -> Result<bool> {
        let (
            Some(support_class),
            Some(review_policy),
            Some(output_pred),
            Some(supported_triple),
            Some(decision_pred),
            Some(claim_pred),
        ) = (
            self.vocab.ev_acceptance_support,
            self.vocab.ev_review_policy,
            self.vocab.ev_output,
            self.vocab.ev_supported_triple,
            self.vocab.ev_decision,
            self.vocab.ev_claim_pred,
        )
        else {
            return Ok(false);
        };
        if !self.is_typed_authenticated(support_id, policy_ctx.supports, support_class)? {
            return Ok(false);
        }
        if !self.contains_quad(
            support_id,
            review_policy,
            policy_ctx.policy_iri_id,
            policy_ctx.supports.id,
        )? {
            return Ok(false);
        }
        if !self.contains_quad(
            support_id,
            output_pred,
            policy_ctx.output_id,
            policy_ctx.supports.id,
        )? {
            return Ok(false);
        }
        if self.single_object(support_id, supported_triple, policy_ctx.supports.id)? != Some(tt_id)
        {
            return Ok(false);
        }
        if self.record_or_receipt_masked(support_id, policy_ctx.supports)? {
            return Ok(false);
        }
        let Some(decision_id) =
            self.single_iri_object(support_id, decision_pred, policy_ctx.supports.id)?
        else {
            return Ok(false);
        };
        if !self.decision_live_and_readable(policy_ctx.decisions, decision_id)? {
            return Ok(false);
        }
        let Some(claim_id) =
            self.single_iri_object(support_id, claim_pred, policy_ctx.supports.id)?
        else {
            return Ok(false);
        };
        self.claim_live_and_readable(policy_ctx, claim_id)
    }

    fn decision_live_and_readable(&mut self, decisions: GraphRef, decision_id: Id) -> Result<bool> {
        let key = (decisions.id.0, decision_id.0);
        if let Some(&cached) = self.decision_cache.get(&key) {
            return Ok(cached);
        }
        let ok = self.decision_live_and_readable_uncached(decisions, decision_id)?;
        self.decision_cache.insert(key, ok)?;
        Ok(ok)
    }

    fn decision_live_and_readable_uncached(
        &self,
        decisions: GraphRef,
        decision_id: Id,
    ) -> Result<bool> {
        let Some(decision_class) = self.vocab.ev_review_decision else {
            return Ok(false);
        };
        if !self.is_typed_authenticated(decision_id, decisions, decision_class)? {
            return Ok(false);
        }
        if !decisions.readable || self.record_or_receipt_masked(decision_id, decisions)? {
            return Ok(false);
        }
        // Superseding decisions are tested against authenticated raw state: a hidden
        // superseding decision still ends liveness.
        if let Some(supersedes_pred) = self.vocab.ev_supersedes {
            let mut superseding = Buffer::new(self.prep)?;
            let mut more = false;
            self.prep.scan(
                self.snap,
                Perm::Pos,
                &[supersedes_pred.0, decision_id.0],
                |q| {
                    self.prep.check()?;
                    if q[3] != decisions.id {
                        return Ok(true);
                    }
                    if superseding.len() == MAX_EVIDENCE {
                        more = true;
                        return Ok(false);
                    }
                    superseding.push(q[0])?;
                    Ok(true)
                },
            )?;
            if more {
                return Ok(false);
            }
            for &later in superseding.iter() {
                self.prep.check()?;
                if self.is_typed_authenticated(later, decisions, decision_class)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn claim_live_and_readable(&mut self, policy_ctx: &PolicyCtx, claim_id: Id) -> Result<bool> {
        let key = (policy_ctx.policy_iri_id.0, claim_id.0);
        if let Some(&cached) = self.claim_cache.get(&key) {
            return Ok(cached);
        }
        let ok = self.claim_live_and_readable_uncached(policy_ctx, claim_id)?;
        self.claim_cache.insert(key, ok)?;
        Ok(ok)
    }

    fn claim_live_and_readable_uncached(
        &mut self,
        policy_ctx: &PolicyCtx,
        claim_id: Id,
    ) -> Result<bool> {
        let Some(claim_class) = self.vocab.ev_claim else {
            return Ok(false);
        };
        // Locate the authenticated claim across all eligible record graphs.
        let mut claim_graph = None;
        for &g in self.record_graphs.iter() {
            self.prep.check()?;
            if self.is_typed_authenticated(claim_id, g, claim_class)? {
                claim_graph = Some(g);
                break;
            }
        }
        let Some(claim_graph) = claim_graph else {
            return Ok(false);
        };
        if !claim_graph.readable
            || self.record_or_receipt_masked(claim_id, claim_graph)?
            || self.record_with_fragments_masked(claim_id, claim_graph.id)?
        {
            return Ok(false);
        }
        // Check withdrawals across every eligible record graph in raw state, even
        // unreadable or triple-masked ones, because a withdrawn claim is no longer live.
        if let (Some(withdraws_pred), Some(withdrawal_class)) =
            (self.vocab.ev_withdraws, self.vocab.ev_withdrawal)
        {
            for &wg in self.record_graphs.iter() {
                self.prep.check()?;
                let mut withdrawals = Buffer::new(self.prep)?;
                let mut more = false;
                self.prep
                    .scan(self.snap, Perm::Pos, &[withdraws_pred.0, claim_id.0], |q| {
                        self.prep.check()?;
                        if q[3] != wg.id {
                            return Ok(true);
                        }
                        if withdrawals.len() == MAX_EVIDENCE {
                            more = true;
                            return Ok(false);
                        }
                        withdrawals.push(q[0])?;
                        Ok(true)
                    })?;
                if more {
                    return Ok(false);
                }
                for &w in withdrawals.iter() {
                    self.prep.check()?;
                    if self.is_typed_authenticated(w, wg, withdrawal_class)? {
                        return Ok(false);
                    }
                }
            }
        }
        // Check the claim's source revision (prov:wasDerivedFrom), if present.
        if let Some(derived_from) = self.vocab.prov_was_derived_from {
            let mut revs = Buffer::new(self.prep)?;
            self.prep.scan(
                self.snap,
                Perm::Gspo,
                &[claim_graph.id.0, claim_id.0, derived_from.0],
                |q| {
                    self.prep.check()?;
                    if revs.len() < 2 {
                        revs.push(q[2])?;
                    }
                    Ok(revs.len() < 2)
                },
            )?;
            if revs.len() > 1 {
                return Ok(false);
            }
            if let Some(&rev_id) = revs.first() {
                if !self.is_iri(rev_id)? {
                    return Ok(false);
                }
                if !self.source_record_readable(rev_id, SourceKind::Revision)? {
                    return Ok(false);
                }
            }
        }
        // Check every authenticated `ev:Supports` evidence link targeting this claim in
        // every graph in the policy's recorded boundary (plus the claim's own graph if it
        // left the boundary in a later policy version).
        if let (Some(target_pred), Some(link_class), Some(rel_pred), Some(supports_rel)) = (
            self.vocab.ev_target,
            self.vocab.ev_evidence_link,
            self.vocab.ev_relation,
            self.vocab.ev_supports_rel,
        ) {
            let mut link_graphs = Buffer::new(self.prep)?;
            let mut has_claim_graph = false;
            for &g in policy_ctx.boundary_graphs.iter() {
                if g.id == claim_graph.id {
                    has_claim_graph = true;
                }
                link_graphs.push(g)?;
            }
            if !has_claim_graph {
                link_graphs.push(claim_graph)?;
            }
            for &lg in link_graphs.iter() {
                self.prep.check()?;
                let mut links = Buffer::new(self.prep)?;
                let mut more = false;
                self.prep
                    .scan(self.snap, Perm::Pos, &[target_pred.0, claim_id.0], |q| {
                        self.prep.check()?;
                        if q[3] != lg.id {
                            return Ok(true);
                        }
                        if links.len() == MAX_EVIDENCE {
                            more = true;
                            return Ok(false);
                        }
                        links.push(q[0])?;
                        Ok(true)
                    })?;
                if more {
                    return Ok(false);
                }
                for &link_id in links.iter() {
                    self.prep.check()?;
                    if !self.is_typed_authenticated(link_id, lg, link_class)? {
                        continue;
                    }
                    if !self.contains_quad(link_id, rel_pred, supports_rel, lg.id)? {
                        continue;
                    }
                    if !lg.readable || self.record_or_receipt_masked(link_id, lg)? {
                        return Ok(false);
                    }
                    if !self.evidence_link_target_readable(link_id, lg)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Whether `root_id` or any of its `#...` fragment sub-nodes (such as a claim's
    /// `#confidence` / `#validity` nodes or a passage's `#selector` node) in `graph_id`
    /// has a triple hidden by `base_hidden`. Fails closed if the sub-node cap is exceeded.
    fn record_with_fragments_masked(&self, root_id: Id, graph_id: Id) -> Result<bool> {
        if self.base_hidden.is_empty() {
            return Ok(false);
        }
        if subject_masked_checked(self.base_hidden, root_id, graph_id, self.prep)? {
            return Ok(true);
        }
        let prefix = self.prep.with_term(self.snap, root_id, |term| match term {
            Some(Term::NamedNode(n)) => {
                let mut t = Text::new(self.prep)?;
                t.push_str(n.as_str())?;
                t.push_str("#")?;
                Ok(Some(t))
            }
            _ => Ok(None),
        })?;
        let Some(prefix) = prefix else {
            return Ok(false);
        };
        let mut level1 = Buffer::new(self.prep)?;
        let mut masked = false;
        self.prep
            .scan(self.snap, Perm::Gspo, &[graph_id.0, root_id.0], |q| {
                self.prep.check()?;
                let o = q[2];
                let is_sub = self.prep.with_term(self.snap, o, |term| {
                    Ok(matches!(
                        term,
                        Some(Term::NamedNode(n)) if n.as_str().starts_with(prefix.value.as_str())
                    ))
                })?;
                if is_sub {
                    if subject_masked_checked(self.base_hidden, o, graph_id, self.prep)? {
                        masked = true;
                        return Ok(false);
                    }
                    if level1.len() >= MAX_FRAGMENT_SUBNODES {
                        // Fail closed rather than silently truncating sub-node checks.
                        masked = true;
                        return Ok(false);
                    }
                    level1.push(o)?;
                }
                Ok(true)
            })?;
        if masked {
            return Ok(true);
        }
        // Also check depth-1 children (e.g. #validity -> #validity-start / #validity-end).
        for &sub in level1.iter() {
            self.prep.check()?;
            if subject_masked_checked(self.base_hidden, sub, graph_id, self.prep)? {
                return Ok(true);
            }
            let mut sub_masked = false;
            self.prep
                .scan(self.snap, Perm::Gspo, &[graph_id.0, sub.0], |q| {
                    self.prep.check()?;
                    let o = q[2];
                    let is_sub = self.prep.with_term(self.snap, o, |term| {
                        Ok(matches!(
                            term,
                            Some(Term::NamedNode(n)) if n.as_str().starts_with(prefix.value.as_str())
                        ))
                    })?;
                    if is_sub
                        && subject_masked_checked(self.base_hidden, o, graph_id, self.prep)?
                    {
                        sub_masked = true;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
            if sub_masked {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn evidence_link_target_readable(&mut self, link_id: Id, link_graph: GraphRef) -> Result<bool> {
        let (Some(ev_pred), Some(ref_class)) = (self.vocab.ev_evidence, self.vocab.ev_evidence_ref)
        else {
            return Ok(false);
        };
        let Some(ref_id) = self.single_iri_object(link_id, ev_pred, link_graph.id)? else {
            return Ok(false);
        };
        let mut ref_graph = None;
        for &g in self.record_graphs.iter() {
            self.prep.check()?;
            if self.is_typed_authenticated(ref_id, g, ref_class)? {
                ref_graph = Some(g);
                break;
            }
        }
        let Some(ref_graph) = ref_graph else {
            return Ok(false);
        };
        if !ref_graph.readable || self.record_or_receipt_masked(ref_id, ref_graph)? {
            return Ok(false);
        }
        if let Some(p) = self.vocab.ev_source_revision_pred
            && let Some(rev) = self.single_iri_object(ref_id, p, ref_graph.id)?
            && !self.source_record_readable(rev, SourceKind::Revision)?
        {
            return Ok(false);
        }
        if let Some(p) = self.vocab.ev_rendition_pred
            && let Some(rend) = self.single_iri_object(ref_id, p, ref_graph.id)?
            && !self.source_record_readable(rend, SourceKind::Rendition)?
        {
            return Ok(false);
        }
        if let Some(p) = self.vocab.ev_passage_pred
            && let Some(pass) = self.single_iri_object(ref_id, p, ref_graph.id)?
            && !self.source_record_readable(pass, SourceKind::Passage)?
        {
            return Ok(false);
        }
        if let Some(ds_pred) = self.vocab.ev_dataset_id
            && self.single_object(ref_id, ds_pred, ref_graph.id)?.is_some()
            && !self.stored_fact_ref_readable(ref_id, ref_graph.id)?
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn source_record_readable(&mut self, rec_id: Id, kind: SourceKind) -> Result<bool> {
        let class_id = match kind {
            SourceKind::Revision => self.vocab.ev_source_revision,
            SourceKind::Rendition => self.vocab.ev_rendition,
            SourceKind::Passage => self.vocab.ev_passage,
        };
        let Some(class_id) = class_id else {
            return Ok(false);
        };
        let mut home = None;
        for &sg in self.source_records.iter() {
            self.prep.check()?;
            if self.is_typed_authenticated(rec_id, sg, class_id)? {
                home = Some(sg);
                break;
            }
        }
        let Some(sg) = home else {
            return Ok(false);
        };
        let key = (sg.id.0, rec_id.0, kind as u8);
        if let Some(&cached) = self.source_cache.get(&key) {
            return Ok(cached);
        }
        let masked = !sg.readable
            || self.record_or_receipt_masked(rec_id, sg)?
            || (matches!(kind, SourceKind::Passage)
                && self.record_with_fragments_masked(rec_id, sg.id)?);
        let ok = if masked {
            false
        } else {
            match kind {
                SourceKind::Revision => true,
                SourceKind::Rendition => {
                    if let Some(p) = self.vocab.prov_was_derived_from
                        && let Some(rev) = self.single_iri_object(rec_id, p, sg.id)?
                    {
                        self.source_record_readable(rev, SourceKind::Revision)?
                    } else {
                        true
                    }
                }
                SourceKind::Passage => {
                    let mut ok = true;
                    if let Some(p) = self.vocab.ev_rendition_pred
                        && let Some(rend) = self.single_iri_object(rec_id, p, sg.id)?
                    {
                        ok &= self.source_record_readable(rend, SourceKind::Rendition)?;
                    }
                    if ok
                        && let Some(p) = self.vocab.oa_has_source
                        && let Some(rend) = self.single_iri_object(rec_id, p, sg.id)?
                    {
                        ok &= self.source_record_readable(rend, SourceKind::Rendition)?;
                    }
                    ok
                }
            }
        };
        self.source_cache.insert(key, ok)?;
        Ok(ok)
    }

    fn stored_fact_ref_readable(&mut self, ref_id: Id, ref_graph_id: Id) -> Result<bool> {
        let (
            Some(ds_pred),
            Some(branch_pred),
            Some(commit_pred),
            Some(gk_pred),
            Some(expected_pred),
        ) = (
            self.vocab.ev_dataset_id,
            self.vocab.ev_branch_id,
            self.vocab.ev_commit,
            self.vocab.ev_graph_kind,
            self.vocab.ev_expected_triple,
        )
        else {
            return Ok(false);
        };
        // Cross-dataset and cross-branch references cannot be proven readable by this
        // snapshot's single-branch GraphAccess: fail closed.
        if self.single_uuid(ref_id, ds_pred, ref_graph_id)? != Some(self.snap.dataset_id) {
            return Ok(false);
        }
        if self.single_uuid(ref_id, branch_pred, ref_graph_id)? != Some(self.snap.branch_id) {
            return Ok(false);
        }
        // Reject unanchored skolemMap references (matching evidence::resolve::check_anchor).
        if let Some(sk_pred) = self.vocab.ev_skolem_map
            && self.any_object(ref_id, sk_pred, ref_graph_id)?
        {
            return Ok(false);
        }
        let Some(commit) = self.single_u64(ref_id, commit_pred, ref_graph_id)? else {
            return Ok(false);
        };
        if commit > self.snap.commit {
            return Ok(false);
        }
        let Some(gk) = self.single_iri_object(ref_id, gk_pred, ref_graph_id)? else {
            return Ok(false);
        };
        let graph_iri: Option<Text> = if Some(gk) == self.vocab.ev_default_graph {
            if !self.access.read.default_graph() {
                return Ok(false);
            }
            None
        } else if Some(gk) == self.vocab.ev_named_graph {
            let Some(g_pred) = self.vocab.ev_graph else {
                return Ok(false);
            };
            let Some(g_id) = self.single_iri_object(ref_id, g_pred, ref_graph_id)? else {
                return Ok(false);
            };
            let text = self.prep.with_term(self.snap, g_id, |term| match term {
                Some(Term::NamedNode(n)) => {
                    if !self.prep.allows_iri(&self.access.read, n.as_str())? {
                        return Ok(None);
                    }
                    let mut t = Text::new(self.prep)?;
                    t.push_str(n.as_str())?;
                    Ok(Some(t))
                }
                _ => Ok(None),
            })?;
            let Some(text) = text else {
                return Ok(false);
            };
            Some(text)
        } else {
            return Ok(false);
        };
        let Some(expected_id) = self.single_object(ref_id, expected_pred, ref_graph_id)? else {
            return Ok(false);
        };
        let reifier_iri: Option<Text> = match self.vocab.ev_reifier {
            Some(rp) => {
                if self.any_object(ref_id, rp, ref_graph_id)? {
                    let Some(r_id) = self.single_iri_object(ref_id, rp, ref_graph_id)? else {
                        return Ok(false);
                    };
                    let text = self.prep.with_term(self.snap, r_id, |term| match term {
                        Some(Term::NamedNode(n)) => {
                            let mut t = Text::new(self.prep)?;
                            t.push_str(n.as_str())?;
                            Ok(Some(t))
                        }
                        _ => Ok(None),
                    })?;
                    let Some(text) = text else {
                        return Ok(false);
                    };
                    Some(text)
                } else {
                    None
                }
            }
            None => None,
        };
        // Charge and clone `expected` triple from `self.snap` under `self.prep`.
        let mut expected_held = self.prep.reserve(0)?;
        let expected = self.prep.with_term(self.snap, expected_id, |term| {
            let Some(Term::Triple(t)) = term else {
                return Ok(None);
            };
            let bytes = triple_bytes(t).saturating_add(128);
            expected_held.grow_by(bytes)?;
            Ok(Some((**t).clone()))
        })?;
        let Some(expected) = expected else {
            return Ok(false);
        };
        let blank = triple_contains_blank(&expected);
        if blank && reifier_iri.is_none() {
            return Ok(false);
        }
        if commit == self.snap.commit {
            let ok = self.check_stored_fact_same_commit(
                graph_iri.as_ref().map(|t| t.value.as_str()),
                &expected,
                expected_id,
                reifier_iri.as_ref().map(|t| t.value.as_str()),
                blank,
            )?;
            drop(expected_held);
            return Ok(ok);
        }
        // Earlier commit on the same dataset and branch: require an exact bounded proof on
        // the retained historical snapshot under the caller's current access policy.
        if self.depth >= MAX_STORED_FACT_DEPTH {
            return Ok(false);
        }
        // Bound GraphGuards were decided at `self.snap`, not at `commit`. Fail closed if a
        // bound guard matches the target graph or could influence a triple-rule pattern.
        if !can_reuse_access_at_earlier_commit(
            self.access,
            graph_iri.as_ref().map(|t| t.value.as_str()),
            self.prep,
        )? {
            return Ok(false);
        }
        let target_snap = self.load_masked_fact_snapshot(commit)?;
        let Some(target_snap) = target_snap else {
            return Ok(false);
        };
        let ok = check_fact_in_snapshot(
            &target_snap,
            &[],
            graph_iri.as_ref().map(|t| t.value.as_str()),
            &expected,
            reifier_iri.as_ref().map(|t| t.value.as_str()),
            blank,
            self.prep,
        )?;
        drop(expected_held);
        Ok(ok)
    }

    fn check_stored_fact_same_commit(
        &mut self,
        graph_iri: Option<&str>,
        expected: &Triple,
        expected_id: Id,
        reifier_iri: Option<&str>,
        blank: bool,
    ) -> Result<bool> {
        let Some(g_id) = (match graph_iri {
            None => Some(Id::DEFAULT_GRAPH),
            Some(iri) => self.prep.lookup_iri(self.snap, iri)?,
        }) else {
            return Ok(false);
        };
        if !blank {
            let s_term: Term = expected.subject.clone().into();
            let Some(s_id) = self.prep.lookup_term(self.snap, &s_term)? else {
                return Ok(false);
            };
            let Some(p_id) = self
                .prep
                .lookup_iri(self.snap, expected.predicate.as_str())?
            else {
                return Ok(false);
            };
            let Some(o_id) = self.prep.lookup_term(self.snap, &expected.object)? else {
                return Ok(false);
            };
            if !self.contains_quad(s_id, p_id, o_id, g_id)? {
                return Ok(false);
            }
            if self
                .base_hidden
                .binary_search(&[s_id.0, p_id.0, o_id.0, g_id.0])
                .is_ok()
            {
                return Ok(false);
            }
            if let Some(r_iri) = reifier_iri {
                let Some(rdf_reifies) = self.vocab.rdf_reifies else {
                    return Ok(false);
                };
                let Some(r_id) = self.prep.lookup_iri(self.snap, r_iri)? else {
                    return Ok(false);
                };
                if !self.contains_quad(r_id, rdf_reifies, expected_id, g_id)? {
                    return Ok(false);
                }
                if self
                    .base_hidden
                    .binary_search(&[r_id.0, rdf_reifies.0, expected_id.0, g_id.0])
                    .is_ok()
                {
                    return Ok(false);
                }
            }
            // If the target graph is itself a review policy's output graph, also prove
            // support-aware visibility of the cited triple under that policy (with cycle
            // detection via `self.active_outputs`).
            if let Some(g_iri) = graph_iri
                && let Some((pname, policy)) = self
                    .config
                    .settings
                    .policies
                    .iter()
                    .find(|(_, p)| p.output == g_iri)
            {
                let Some(pctx) = self.prepare_policy(pname, policy, g_id)? else {
                    return Ok(false);
                };
                if !self.triple_supported(&pctx, [s_id, p_id, o_id])? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        // Blank-node fact on the same commit: cannot be supported as an accepted projection
        // output, and must resolve through its single visible `rdf:reifies` triple term.
        if graph_iri.is_some_and(|iri| self.config.is_output_graph(iri)) {
            return Ok(false);
        }
        check_fact_in_snapshot(
            self.snap,
            self.base_hidden,
            graph_iri,
            expected,
            reifier_iri,
            true,
            self.prep,
        )
    }

    fn load_masked_fact_snapshot(&mut self, commit: u64) -> Result<Option<Arc<Snapshot>>> {
        if let Some(cached) = self.fact_views.get(&commit) {
            return Ok(cached.snap.clone());
        }
        let Some(origin) = self.snap.history_origin.as_ref().and_then(|w| w.upgrade()) else {
            return Ok(None);
        };
        let resolved = match origin.snapshot_at_commit(commit, self.prep) {
            Ok(opt) => opt,
            Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(e);
            }
            Err(_) => None,
        };
        let Some((raw_snap, mut held)) = resolved else {
            let entry_held = self.prep.reserve(128)?;
            self.fact_views.insert(
                commit,
                CachedFactSnapshot {
                    snap: None,
                    _held: entry_held,
                },
            );
            return Ok(None);
        };
        let (local_access, access_held) = clone_access_unbound(self.access, self.prep)?;
        let masked = match super::controlled::prepare_stored_fact_snapshot(
            &local_access,
            &raw_snap,
            self.prep,
            self.depth + 1,
        ) {
            Ok((masked_snap, mask_held)) => {
                held.merge(mask_held);
                drop(access_held);
                Some(masked_snap)
            }
            Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(e);
            }
            Err(_) => {
                drop(access_held);
                None
            }
        };
        let entry_held = self.prep.reserve(128)?;
        held.merge(entry_held);
        self.fact_views.insert(
            commit,
            CachedFactSnapshot {
                snap: masked.clone(),
                _held: held,
            },
        );
        Ok(masked)
    }

    fn lookup_triple_term(&self, triple: [Id; 3]) -> Result<Option<Id>> {
        let mut held = self
            .prep
            .reserve(std::mem::size_of::<Triple>() as u64 + 128)?;
        let s = self
            .prep
            .with_term(self.snap, triple[0], |term| match term {
                Some(Term::NamedNode(n)) => {
                    held.grow_by((n.as_str().len() as u64).saturating_add(64))?;
                    Ok(Some(NamedOrBlankNode::NamedNode(n.clone())))
                }
                _ => Ok(None),
            })?;
        let Some(s) = s else {
            return Ok(None);
        };
        let p = self
            .prep
            .with_term(self.snap, triple[1], |term| match term {
                Some(Term::NamedNode(n)) => {
                    held.grow_by((n.as_str().len() as u64).saturating_add(64))?;
                    Ok(Some(n.clone()))
                }
                _ => Ok(None),
            })?;
        let Some(p) = p else {
            return Ok(None);
        };
        let o = self
            .prep
            .with_term(self.snap, triple[2], |term| match term {
                Some(t) if !contains_blank(t) => {
                    held.grow_by(term_bytes(t))?;
                    Ok(Some(t.clone()))
                }
                _ => Ok(None),
            })?;
        let Some(o) = o else {
            return Ok(None);
        };
        let tt = Term::Triple(Box::new(Triple::new(s, p, o)));
        let id = self.prep.lookup_term(self.snap, &tt)?;
        drop(tt);
        drop(held);
        Ok(id)
    }

    fn is_typed_authenticated(&self, subj: Id, graph: GraphRef, class_id: Id) -> Result<bool> {
        let Some(rdf_type) = self.vocab.rdf_type else {
            return Ok(false);
        };
        if !self.contains_quad(subj, rdf_type, class_id, graph.id)? {
            return Ok(false);
        }
        self.is_authenticated(subj, graph)
    }

    fn is_authenticated(&self, subj: Id, graph: GraphRef) -> Result<bool> {
        let Some(nonce) = graph.nonce else {
            return Ok(false);
        };
        let (Some(receipt_pred), Some(op_class), Some(rdf_type)) = (
            self.vocab.ev_availability_receipt,
            self.vocab.ev_operation,
            self.vocab.rdf_type,
        ) else {
            return Ok(false);
        };
        if !self.has_nonce(subj, graph, nonce)? {
            return Ok(false);
        }
        let Some(op_id) = self.single_iri_object(subj, receipt_pred, graph.id)? else {
            return Ok(false);
        };
        if !self.contains_quad(op_id, rdf_type, op_class, graph.id)? {
            return Ok(false);
        }
        self.has_nonce(op_id, graph, nonce)
    }

    /// Whether `subj` or its `ev:Operation` receipt in `graph` has any triple hidden by
    /// `base_hidden`. Used for positive record dependencies (never for withdrawals or
    /// superseding decisions).
    fn record_or_receipt_masked(&self, subj: Id, graph: GraphRef) -> Result<bool> {
        if self.base_hidden.is_empty() {
            return Ok(false);
        }
        if subject_masked_checked(self.base_hidden, subj, graph.id, self.prep)? {
            return Ok(true);
        }
        let Some(receipt_pred) = self.vocab.ev_availability_receipt else {
            return Ok(true);
        };
        let Some(op_id) = self.single_iri_object(subj, receipt_pred, graph.id)? else {
            return Ok(true);
        };
        subject_masked_checked(self.base_hidden, op_id, graph.id, self.prep)
    }

    fn has_nonce(&self, subj: Id, graph: GraphRef, expected: Uuid) -> Result<bool> {
        let Some(prot_pred) = self.vocab.ev_graph_protection else {
            return Ok(false);
        };
        let Some(obj) = self.single_object(subj, prot_pred, graph.id)? else {
            return Ok(false);
        };
        if graph.nonce_id == Some(obj) {
            return Ok(true);
        }
        self.prep.with_simple_literal(self.snap, obj, |text| {
            Ok(text.and_then(|v| Uuid::parse_str(v).ok()) == Some(expected))
        })
    }

    fn contains_quad(&self, s: Id, p: Id, o: Id, g: Id) -> Result<bool> {
        let mut found = false;
        self.prep
            .scan(self.snap, Perm::Gspo, &[g.0, s.0, p.0, o.0], |_| {
                found = true;
                Ok(false)
            })?;
        Ok(found)
    }

    fn any_object(&self, s: Id, p: Id, g: Id) -> Result<bool> {
        let mut found = false;
        self.prep
            .scan(self.snap, Perm::Gspo, &[g.0, s.0, p.0], |_| {
                found = true;
                Ok(false)
            })?;
        Ok(found)
    }

    fn single_object(&self, s: Id, p: Id, g: Id) -> Result<Option<Id>> {
        let mut first = None;
        let mut many = false;
        self.prep
            .scan(self.snap, Perm::Gspo, &[g.0, s.0, p.0], |q| {
                self.prep.check()?;
                if first.is_some() {
                    many = true;
                    return Ok(false);
                }
                first = Some(q[2]);
                Ok(true)
            })?;
        Ok(if many { None } else { first })
    }

    fn single_iri_object(&self, s: Id, p: Id, g: Id) -> Result<Option<Id>> {
        let Some(o) = self.single_object(s, p, g)? else {
            return Ok(None);
        };
        Ok(self.is_iri(o)?.then_some(o))
    }

    fn single_uuid(&self, s: Id, p: Id, g: Id) -> Result<Option<Uuid>> {
        let Some(o) = self.single_object(s, p, g)? else {
            return Ok(None);
        };
        self.prep.with_simple_literal(self.snap, o, |text| {
            Ok(text.and_then(|v| Uuid::parse_str(v).ok()))
        })
    }

    fn single_u64(&self, s: Id, p: Id, g: Id) -> Result<Option<u64>> {
        let Some(o) = self.single_object(s, p, g)? else {
            return Ok(None);
        };
        self.prep.with_term(self.snap, o, |term| match term {
            Some(Term::Literal(l)) => Ok(l.value().parse().ok()),
            _ => Ok(None),
        })
    }

    fn is_iri(&self, id: Id) -> Result<bool> {
        self.prep.with_term(self.snap, id, |term| {
            Ok(matches!(term, Some(Term::NamedNode(_))))
        })
    }
}

/// Exact quad and blank-node/reifier verification matching `evidence::resolve::check_fact`.
/// `base_hidden` is empty when `snap` is already masked, or holds ordinary triple deletions
/// when checking against the unmasked current snapshot.
fn check_fact_in_snapshot(
    snap: &Snapshot,
    base_hidden: &[Key],
    graph_iri: Option<&str>,
    expected: &Triple,
    reifier_iri: Option<&str>,
    blank: bool,
    prep: &Preparation,
) -> Result<bool> {
    prep.check()?;
    let g_id = match graph_iri {
        None => Id::DEFAULT_GRAPH,
        Some(iri) => match prep.lookup_iri(snap, iri)? {
            Some(id) => id,
            None => return Ok(false),
        },
    };
    let contains = |s: Id, p: Id, o: Id, g: Id| -> Result<bool> {
        if base_hidden.binary_search(&[s.0, p.0, o.0, g.0]).is_ok() {
            return Ok(false);
        }
        let mut found = false;
        prep.scan(snap, Perm::Gspo, &[g.0, s.0, p.0, o.0], |_| {
            found = true;
            Ok(false)
        })?;
        Ok(found)
    };
    if !blank {
        let s_term: Term = expected.subject.clone().into();
        let Some(s_id) = prep.lookup_term(snap, &s_term)? else {
            return Ok(false);
        };
        let Some(p_id) = prep.lookup_iri(snap, expected.predicate.as_str())? else {
            return Ok(false);
        };
        let Some(o_id) = prep.lookup_term(snap, &expected.object)? else {
            return Ok(false);
        };
        if !contains(s_id, p_id, o_id, g_id)? {
            return Ok(false);
        }
        if let Some(r_iri) = reifier_iri {
            let Some(r_id) = prep.lookup_iri(snap, r_iri)? else {
                return Ok(false);
            };
            let Some(reifies) = prep.lookup_iri(snap, RDF_REIFIES)? else {
                return Ok(false);
            };
            let _held = prep.reserve(triple_bytes(expected).saturating_add(128))?;
            let tt_term = Term::Triple(Box::new(expected.clone()));
            let Some(tt_id) = prep.lookup_term(snap, &tt_term)? else {
                return Ok(false);
            };
            if !contains(r_id, reifies, tt_id, g_id)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    let Some(r_iri) = reifier_iri else {
        return Ok(false);
    };
    let Some(r_id) = prep.lookup_iri(snap, r_iri)? else {
        return Ok(false);
    };
    let Some(reifies) = prep.lookup_iri(snap, RDF_REIFIES)? else {
        return Ok(false);
    };
    let mut reified = Buffer::new(prep)?;
    prep.scan(snap, Perm::Gspo, &[g_id.0, r_id.0, reifies.0], |q| {
        prep.check()?;
        if base_hidden
            .binary_search(&[q[0].0, q[1].0, q[2].0, q[3].0])
            .is_ok()
        {
            return Ok(true);
        }
        reified.push(q[2])?;
        Ok(reified.len() < 2)
    })?;
    if reified.len() != 1 {
        return Ok(false);
    }
    let tt = reified[0];
    let mut stored_held = prep.reserve(0)?;
    let stored = prep.with_term(snap, tt, |term| match term {
        Some(Term::Triple(t)) => {
            stored_held.grow_by(triple_bytes(t).saturating_add(128))?;
            Ok(Some((**t).clone()))
        }
        _ => Ok(None),
    })?;
    let Some(stored) = stored else {
        return Ok(false);
    };
    if !isomorphic_triple(&stored, expected, prep)? {
        return Ok(false);
    }
    let s_term: Term = stored.subject.clone().into();
    let Some(s_id) = lookup_stored_term(snap, &s_term, prep)? else {
        return Ok(false);
    };
    let Some(p_id) = prep.lookup_iri(snap, stored.predicate.as_str())? else {
        return Ok(false);
    };
    let Some(o_id) = lookup_stored_term(snap, &stored.object, prep)? else {
        return Ok(false);
    };
    let ok = contains(s_id, p_id, o_id, g_id)?;
    drop(stored_held);
    Ok(ok)
}

fn lookup_stored_term(snap: &Snapshot, term: &Term, prep: &Preparation) -> Result<Option<Id>> {
    prep.check()?;
    match term {
        Term::NamedNode(n) => prep.lookup_iri(snap, n.as_str()),
        Term::BlankNode(_) | Term::Triple(_) => Ok(snap.lookup_term(term)),
        Term::Literal(_) => prep.lookup_term(snap, term),
    }
}

fn can_reuse_access_at_earlier_commit(
    access: &GraphAccess,
    graph_iri: Option<&str>,
    prep: &Preparation,
) -> Result<bool> {
    let Graphs::Only(rule) = &access.read else {
        return Ok(true);
    };
    let has_bound_guard = rule.guards.iter().any(|g| g.bound_to.is_some());
    if !has_bound_guard {
        return Ok(true);
    }
    if let Some(iri) = graph_iri {
        for guard in &rule.guards {
            prep.check()?;
            if guard.bound_to.is_some() && crate::access::glob(&guard.pattern, iri) {
                return Ok(false);
            }
        }
    }
    if let Some(rules) = &access.triples
        && rules
            .rules
            .iter()
            .any(|r| !r.read.is_all() && r.protection.pattern.is_some())
    {
        return Ok(false);
    }
    Ok(true)
}

fn clone_access_unbound(
    access: &GraphAccess,
    prep: &Preparation,
) -> Result<(GraphAccess, Reservation)> {
    let mut held = prep.reserve(1024)?;
    let charge_strings = |values: &[String], held: &mut Reservation| -> Result<()> {
        held.grow_by(64u64.saturating_add(
            (values.len() as u64).saturating_mul(std::mem::size_of::<String>() as u64),
        ))?;
        for s in values {
            prep.check()?;
            held.grow_by((s.len() as u64).saturating_add(64))?;
        }
        Ok(())
    };
    let strip = |graphs: &Graphs, held: &mut Reservation| -> Result<Graphs> {
        let Graphs::Only(rule) = graphs else {
            return Ok(Graphs::All);
        };
        charge_strings(&rule.iris, held)?;
        charge_strings(&rule.patterns, held)?;
        charge_strings(&rule.protected, held)?;
        charge_strings(&rule.excluded, held)?;
        let mut guards = Vec::new();
        held.grow_by(
            (rule.guards.len() as u64).saturating_mul(std::mem::size_of::<GraphGuard>() as u64),
        )?;
        guards
            .try_reserve_exact(rule.guards.len())
            .map_err(|_| prep.exhausted())?;
        for g in &rule.guards {
            prep.check()?;
            held.grow_by((g.pattern.len() as u64).saturating_add(64))?;
            // Snapshot-bound or undecided guards were decided at `self.snap`, not at the
            // earlier commit; fail closed by clearing their exemptions instead of
            // carrying commit-N exemptions onto earlier commits or transitive support graphs.
            let exempt = if g.bound_to.is_some() || g.decide.is_some() {
                Vec::new()
            } else {
                charge_strings(&g.exempt, held)?;
                g.exempt.clone()
            };
            guards.push(GraphGuard {
                pattern: g.pattern.clone(),
                exempt,
                decide: None,
                bound_to: None,
            });
        }
        Ok(Graphs::Only(GraphRule {
            default_graph: rule.default_graph,
            iris: rule.iris.clone(),
            patterns: rule.patterns.clone(),
            protected: rule.protected.clone(),
            blank_nodes: rule.blank_nodes,
            excluded: rule.excluded.clone(),
            guards,
        }))
    };
    let read = strip(&access.read, &mut held)?;
    let write = strip(&access.write, &mut held)?;
    Ok((
        GraphAccess {
            read,
            write,
            triples: access.triples.clone(),
            views: access.views.clone(),
        },
        held,
    ))
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum SourceKind {
    Revision = 1,
    Rendition = 2,
    Passage = 3,
}

fn contains_blank(term: &Term) -> bool {
    match term {
        Term::BlankNode(_) => true,
        Term::Triple(t) => triple_contains_blank(t),
        _ => false,
    }
}

fn triple_contains_blank(triple: &Triple) -> bool {
    matches!(triple.subject, NamedOrBlankNode::BlankNode(_)) || contains_blank(&triple.object)
}

fn term_bytes(term: &Term) -> u64 {
    match term {
        Term::NamedNode(n) => (n.as_str().len() as u64).saturating_add(64),
        Term::BlankNode(b) => (b.as_str().len() as u64).saturating_add(64),
        Term::Literal(l) => (l.value().len() as u64)
            .saturating_add(l.datatype().as_str().len() as u64)
            .saturating_add(l.language().map_or(0, |lang| lang.len() as u64))
            .saturating_add(96),
        Term::Triple(t) => triple_bytes(t).saturating_add(64),
    }
}

fn triple_bytes(triple: &Triple) -> u64 {
    let s = match &triple.subject {
        NamedOrBlankNode::NamedNode(n) => (n.as_str().len() as u64).saturating_add(64),
        NamedOrBlankNode::BlankNode(b) => (b.as_str().len() as u64).saturating_add(64),
    };
    let p = (triple.predicate.as_str().len() as u64).saturating_add(64);
    let o = term_bytes(&triple.object);
    s.saturating_add(p)
        .saturating_add(o)
        .saturating_add(std::mem::size_of::<Triple>() as u64)
}

/// Whether two triples are equal up to a consistent one-to-one renaming of blank nodes.
fn isomorphic_triple(a: &Triple, b: &Triple, prep: &Preparation) -> Result<bool> {
    let mut held = prep.reserve(512)?;
    let mut forward: BTreeMap<&str, &str> = BTreeMap::new();
    let mut backward: BTreeMap<&str, &str> = BTreeMap::new();
    fn blank<'a>(
        x: &'a str,
        y: &'a str,
        f: &mut BTreeMap<&'a str, &'a str>,
        r: &mut BTreeMap<&'a str, &'a str>,
        held: &mut Reservation,
    ) -> Result<bool> {
        match (f.get(x), r.get(y)) {
            (Some(&fy), Some(&rx)) => Ok(fy == y && rx == x),
            (None, None) => {
                held.grow_by(128)?;
                f.insert(x, y);
                r.insert(y, x);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    fn term<'a>(
        a: &'a Term,
        b: &'a Term,
        f: &mut BTreeMap<&'a str, &'a str>,
        r: &mut BTreeMap<&'a str, &'a str>,
        held: &mut Reservation,
        prep: &Preparation,
        depth: usize,
    ) -> Result<bool> {
        prep.check()?;
        if depth > crate::nesting::MAX_TRIPLE_TERMS {
            return Ok(false);
        }
        match (a, b) {
            (Term::BlankNode(x), Term::BlankNode(y)) => blank(x.as_str(), y.as_str(), f, r, held),
            (Term::Triple(x), Term::Triple(y)) => triple(x, y, f, r, held, prep, depth + 1),
            (Term::BlankNode(_), _) | (_, Term::BlankNode(_)) => Ok(false),
            (Term::Triple(_), _) | (_, Term::Triple(_)) => Ok(false),
            (x, y) => Ok(x == y),
        }
    }
    fn triple<'a>(
        a: &'a Triple,
        b: &'a Triple,
        f: &mut BTreeMap<&'a str, &'a str>,
        r: &mut BTreeMap<&'a str, &'a str>,
        held: &mut Reservation,
        prep: &Preparation,
        depth: usize,
    ) -> Result<bool> {
        prep.check()?;
        if a.predicate != b.predicate {
            return Ok(false);
        }
        let subj_ok = match (&a.subject, &b.subject) {
            (NamedOrBlankNode::BlankNode(x), NamedOrBlankNode::BlankNode(y)) => {
                blank(x.as_str(), y.as_str(), f, r, held)?
            }
            (NamedOrBlankNode::NamedNode(x), NamedOrBlankNode::NamedNode(y)) => x == y,
            _ => false,
        };
        if !subj_ok {
            return Ok(false);
        }
        term(&a.object, &b.object, f, r, held, prep, depth)
    }
    triple(a, b, &mut forward, &mut backward, &mut held, prep, 0)
}

fn resolve_graph(
    iri: &str,
    config: &ProjectionConfig,
    access: &GraphAccess,
    snap: &Snapshot,
    prep: &Preparation,
) -> Result<Option<GraphRef>> {
    let Some(id) = prep.lookup_iri(snap, iri)? else {
        return Ok(None);
    };
    let nonce = config.protections.get(iri).copied();
    let nonce_id = match nonce {
        Some(n) => prep.lookup_term(
            snap,
            &Term::Literal(Literal::new_simple_literal(n.to_string())),
        )?,
        None => None,
    };
    let readable = prep.allows_iri(&access.read, iri)?;
    Ok(Some(GraphRef {
        id,
        nonce,
        nonce_id,
        readable,
    }))
}

fn resolve_graphs(
    iris: &[String],
    config: &ProjectionConfig,
    access: &GraphAccess,
    snap: &Snapshot,
    prep: &Preparation,
) -> Result<Buffer<GraphRef>> {
    let mut out = Buffer::new(prep)?;
    for iri in iris {
        prep.check()?;
        if let Some(gr) = resolve_graph(iri, config, access, snap, prep)? {
            out.push(gr)?;
        }
    }
    Ok(out)
}
