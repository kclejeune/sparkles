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
use crate::access::{GraphAccess, Preparation};
use crate::evidence::review::{MAX_EVIDENCE, NONCE_PREDICATES, policy_iri};
use crate::evidence::{Policy, Settings};
use crate::history::ConfigurationIdentity;
use crate::id::{Id, Tag};
use crate::index::{Key, Perm};
use crate::managed::{Installed, OwnerEntry, OwnerKind};
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
    /// Derive the active projection visibility configuration from an installed managed
    /// registry. Returns `None` when the evidence owner is absent, disabled, unreadable,
    /// or declares no review policies.
    pub(crate) fn from_installed(installed: &Installed) -> Option<Arc<Self>> {
        let entry = installed.graphs.owners.get(&OwnerKind::Evidence)?;
        let bytes = installed.settings.get(&OwnerKind::Evidence)?;
        let tag = format!("r{}", installed.graphs.revision);
        Self::from_owner(&tag, entry, bytes, &Control::none()).ok()?
    }

    /// Derive the projection visibility configuration from a resolved historical
    /// configuration generation, charging JSON decoding to `preparation`.
    pub(crate) fn from_generation(
        identity: ConfigurationIdentity,
        owners: &BTreeMap<OwnerKind, OwnerEntry>,
        owner_settings: &BTreeMap<OwnerKind, String>,
        preparation: &Preparation,
    ) -> Result<Option<Arc<Self>>> {
        preparation.check()?;
        let Some(entry) = owners.get(&OwnerKind::Evidence) else {
            return Ok(None);
        };
        let Some(text) = owner_settings.get(&OwnerKind::Evidence) else {
            return Ok(None);
        };
        let bytes = text.as_bytes();
        let _decoding = preparation.reserve(
            (bytes.len() as u64)
                .saturating_mul(4)
                .saturating_add(64 << 10),
        )?;
        let tag = format!("cfg:{}:{}", identity.lineage_id, identity.generation);
        Self::from_owner(&tag, entry, bytes, preparation.control())
    }

    fn from_owner(
        tag: &str,
        entry: &OwnerEntry,
        settings_bytes: &[u8],
        control: &Control,
    ) -> Result<Option<Arc<Self>>> {
        let settings = match Settings::from_slice(settings_bytes, control) {
            Ok(settings) => settings,
            Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
            Err(_) => return Ok(None),
        };
        if !settings.enabled || settings.policies.is_empty() {
            return Ok(None);
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
        Ok(Some(Arc::new(Self {
            fingerprint,
            settings,
            protections,
        })))
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
            if !access.read.allows_named_iri(&policy.output) {
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

/// Append every quad in any readable review-policy output graph that lacks a live,
/// authenticated, caller-readable support to `hidden`.
///
/// `base_hidden` is the sorted, deduplicated SPO key slice already hidden by ordinary
/// triple protections (`access.triples`), used to check whether any record or quad is
/// hidden by triple rules without building an intermediate masked snapshot.
pub(crate) fn append_hidden(
    access: &GraphAccess,
    base_hidden: &[Key],
    config: &ProjectionConfig,
    snap: &Snapshot,
    limits: &Limits,
    preparation: &Preparation,
    hidden: &mut Buffer<Key>,
) -> Result<()> {
    if access.reads_everything() {
        return Ok(());
    }
    preparation.stage("preparing projection support visibility")?;
    let mut eval = Evaluator::new(access, base_hidden, config, snap, preparation)?;
    for (policy_name, policy) in &config.settings.policies {
        preparation.check()?;
        if !access.read.allows_named_iri(&policy.output) {
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
                if (hidden.len() + to_hide.len()) as u64 >= limits.max_hidden {
                    return Err(too_many(limits, limits.max_hidden.saturating_add(1)));
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
        if hidden.len() as u64 >= limits.max_hidden {
            return Err(too_many(limits, limits.max_hidden.saturating_add(1)));
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

/// Whether `base_hidden` (sorted SPO keys) hides any triple of subject `subj` in `graph`.
fn subject_masked(base_hidden: &[Key], subj: Id, graph: Id) -> bool {
    if base_hidden.is_empty() {
        return false;
    }
    let start = base_hidden.partition_point(|k| k[0] < subj.0);
    for k in &base_hidden[start..] {
        if k[0] != subj.0 {
            break;
        }
        if k[3] == graph.0 {
            return true;
        }
    }
    false
}

/// Vocabulary term IDs looked up once per mask build.
struct Vocab {
    rdf_type: Option<Id>,
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
    ev_commit: Option<Id>,
    ev_graph_kind: Option<Id>,
    ev_default_graph: Option<Id>,
    ev_named_graph: Option<Id>,
    ev_graph: Option<Id>,
    ev_expected_triple: Option<Id>,
    ev_reifier: Option<Id>,
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
        Ok(Self {
            rdf_type: iri(RDF_TYPE)?,
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
            prov_was_derived_from: iri(&format!("{PROV}wasDerivedFrom"))?,
            ev_target: ev("target")?,
            ev_relation: ev("relation")?,
            ev_supports_rel: ev("Supports")?,
            ev_evidence: ev("evidence")?,
            ev_source_revision_pred: ev("sourceRevision")?,
            ev_rendition_pred: ev("rendition")?,
            ev_passage_pred: ev("passage")?,
            oa_has_source: iri(&format!("{OA}hasSource"))?,
            ev_dataset_id: ev("datasetId")?,
            ev_commit: ev("commit")?,
            ev_graph_kind: ev("graphKind")?,
            ev_default_graph: ev("DefaultGraph")?,
            ev_named_graph: ev("NamedGraph")?,
            ev_graph: ev("graph")?,
            ev_expected_triple: ev("expectedTriple")?,
            ev_reifier: ev("reifier")?,
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
    /// Boundary proposal graphs (subset of `settings.proposals` present in `policy.boundary`).
    boundary_proposals: Buffer<GraphRef>,
}

struct Evaluator<'a> {
    access: &'a GraphAccess,
    base_hidden: &'a [Key],
    config: &'a ProjectionConfig,
    snap: &'a Snapshot,
    prep: &'a Preparation,
    vocab: Vocab,
    /// All configured proposal graphs present in `snap`.
    proposals: Buffer<GraphRef>,
    /// All configured source-record graphs present in `snap`.
    source_records: Buffer<GraphRef>,
    /// Memoized `(decisions_graph_id, decision_id) -> live & readable`.
    decision_cache: Map<(u64, u64), bool>,
    /// Memoized `(policy_iri_id, claim_id) -> live & readable with evidence`.
    claim_cache: Map<(u64, u64), bool>,
    /// Memoized `(source_graph_id, record_id, kind_tag) -> authenticated & readable`.
    source_cache: Map<(u64, u64, u8), bool>,
}

impl<'a> Evaluator<'a> {
    fn new(
        access: &'a GraphAccess,
        base_hidden: &'a [Key],
        config: &'a ProjectionConfig,
        snap: &'a Snapshot,
        prep: &'a Preparation,
    ) -> Result<Self> {
        let vocab = Vocab::lookup(snap, prep)?;
        let proposals = resolve_graphs(&config.settings.proposals, config, access, snap, prep)?;
        let source_records =
            resolve_graphs(&config.settings.source_records, config, access, snap, prep)?;
        Ok(Self {
            access,
            base_hidden,
            config,
            snap,
            prep,
            vocab,
            proposals,
            source_records,
            decision_cache: Map::new(prep)?,
            claim_cache: Map::new(prep)?,
            source_cache: Map::new(prep)?,
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
        if !self.access.read.allows_named_iri(&policy.decisions)
            || !self.access.read.allows_named_iri(&policy.supports)
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
        let mut boundary_proposals = Buffer::new(self.prep)?;
        for g in &policy.boundary {
            self.prep.check()?;
            if !self.config.settings.proposals.iter().any(|p| p == g) {
                continue;
            }
            if let Some(gr) = resolve_graph(g, self.config, self.access, self.snap, self.prep)? {
                boundary_proposals.push(gr)?;
            }
        }
        Ok(Some(PolicyCtx {
            policy_iri_id,
            output_id,
            decisions,
            supports,
            boundary_proposals,
        }))
    }

    /// Match C25's `State::check_projection`: at least one authenticated `ev:Projection`
    /// record for `policy` must exist in `supports`, and every `ev:Projection` record for
    /// `policy` in `supports` must authenticate under `supports`'s nonce and record the
    /// current nonces of decisions, supports and output.
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
        let mut records = Buffer::new(self.prep)?;
        let mut more = false;
        self.prep
            .scan(self.snap, Perm::Pos, &[rdf_type.0, proj_class.0], |q| {
                self.prep.check()?;
                if q[3] != supports.id {
                    return Ok(true);
                }
                if records.len() == MAX_EVIDENCE {
                    more = true;
                    return Ok(false);
                }
                records.push(q[0])?;
                Ok(true)
            })?;
        if more {
            return Ok(false);
        }
        let nonce_preds = [dec_p, sup_p, out_p];
        let mut found = false;
        for &rec in records.iter() {
            self.prep.check()?;
            if !self.contains_quad(rec, review_policy, policy_iri_id, supports.id)? {
                continue;
            }
            found = true;
            if !self.is_authenticated(rec, supports)? {
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
        if subject_masked(self.base_hidden, support_id, policy_ctx.supports.id) {
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
        if !decisions.readable || subject_masked(self.base_hidden, decision_id, decisions.id) {
            return Ok(false);
        }
        if !self.is_typed_authenticated(decision_id, decisions, decision_class)? {
            return Ok(false);
        }
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
        // Locate the authenticated claim across all configured proposal graphs.
        let mut claim_graph = None;
        for &g in self.proposals.iter() {
            self.prep.check()?;
            if self.is_typed_authenticated(claim_id, g, claim_class)? {
                claim_graph = Some(g);
                break;
            }
        }
        let Some(claim_graph) = claim_graph else {
            return Ok(false);
        };
        if !claim_graph.readable || self.claim_record_masked(claim_id, claim_graph.id)? {
            return Ok(false);
        }
        // Check withdrawals across every configured proposal graph, even unreadable ones,
        // because a withdrawn claim is no longer live.
        if let (Some(withdraws_pred), Some(withdrawal_class)) =
            (self.vocab.ev_withdraws, self.vocab.ev_withdrawal)
        {
            for &wg in self.proposals.iter() {
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
        // the policy's boundary proposal graphs (and the claim's own graph if it left
        // the boundary in a later policy version).
        if let (Some(target_pred), Some(link_class), Some(rel_pred), Some(supports_rel)) = (
            self.vocab.ev_target,
            self.vocab.ev_evidence_link,
            self.vocab.ev_relation,
            self.vocab.ev_supports_rel,
        ) {
            let mut link_graphs = Buffer::new(self.prep)?;
            let mut has_claim_graph = false;
            for &g in policy_ctx.boundary_proposals.iter() {
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
                    if !lg.readable || subject_masked(self.base_hidden, link_id, lg.id) {
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

    /// Whether `claim_id` or any of its `#...` fragment sub-nodes (confidence or
    /// validity records) in `graph_id` has a triple hidden by `base_hidden`.
    fn claim_record_masked(&self, claim_id: Id, graph_id: Id) -> Result<bool> {
        if self.base_hidden.is_empty() {
            return Ok(false);
        }
        if subject_masked(self.base_hidden, claim_id, graph_id) {
            return Ok(true);
        }
        let prefix = self
            .prep
            .with_term(self.snap, claim_id, |term| match term {
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
            .scan(self.snap, Perm::Gspo, &[graph_id.0, claim_id.0], |q| {
                self.prep.check()?;
                let o = q[2];
                let is_sub = self.prep.with_term(self.snap, o, |term| {
                    Ok(matches!(
                        term,
                        Some(Term::NamedNode(n)) if n.as_str().starts_with(prefix.value.as_str())
                    ))
                })?;
                if is_sub {
                    if subject_masked(self.base_hidden, o, graph_id) {
                        masked = true;
                        return Ok(false);
                    }
                    if level1.len() < 32 {
                        level1.push(o)?;
                    }
                }
                Ok(true)
            })?;
        if masked {
            return Ok(true);
        }
        // Also check depth-1 children (e.g. #validity -> #validity-start / #validity-end).
        for &sub in level1.iter() {
            if subject_masked(self.base_hidden, sub, graph_id) {
                return Ok(true);
            }
            let mut masked = false;
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
                    if is_sub && subject_masked(self.base_hidden, o, graph_id) {
                        masked = true;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
            if masked {
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
        for &g in self.proposals.iter() {
            self.prep.check()?;
            if self.is_typed_authenticated(ref_id, g, ref_class)? {
                ref_graph = Some(g);
                break;
            }
        }
        let Some(ref_graph) = ref_graph else {
            return Ok(false);
        };
        if !ref_graph.readable || subject_masked(self.base_hidden, ref_id, ref_graph.id) {
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
        let ok = if !sg.readable || subject_masked(self.base_hidden, rec_id, sg.id) {
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

    fn stored_fact_ref_readable(&self, ref_id: Id, ref_graph_id: Id) -> Result<bool> {
        let (Some(ds_pred), Some(gk_pred)) = (self.vocab.ev_dataset_id, self.vocab.ev_graph_kind)
        else {
            return Ok(false);
        };
        // Cross-dataset references cannot be proven readable by this dataset's GraphAccess.
        if self.single_uuid(ref_id, ds_pred, ref_graph_id)? != Some(self.snap.dataset_id) {
            return Ok(false);
        }
        let Some(gk) = self.single_iri_object(ref_id, gk_pred, ref_graph_id)? else {
            return Ok(false);
        };
        let (graph_id, graph_term) = if Some(gk) == self.vocab.ev_default_graph {
            if !self.access.read.default_graph() {
                return Ok(false);
            }
            (Some(Id::DEFAULT_GRAPH), None)
        } else if Some(gk) == self.vocab.ev_named_graph {
            let Some(g_pred) = self.vocab.ev_graph else {
                return Ok(false);
            };
            let Some(g_id) = self.single_iri_object(ref_id, g_pred, ref_graph_id)? else {
                return Ok(false);
            };
            let allowed = self.prep.with_term(self.snap, g_id, |term| match term {
                Some(Term::NamedNode(n)) => Ok(self
                    .access
                    .read
                    .allows_named_iri(n.as_str())
                    .then(|| Term::NamedNode(n.clone()))),
                _ => Ok(None),
            })?;
            let Some(term) = allowed else {
                return Ok(false);
            };
            (Some(g_id), Some(term))
        } else {
            return Ok(false);
        };
        let Some(rules) = self.access.triples.as_ref().filter(|t| t.hides()) else {
            return Ok(true);
        };
        let Some(expected_pred) = self.vocab.ev_expected_triple else {
            return Ok(false);
        };
        let Some(expected_id) = self.single_object(ref_id, expected_pred, ref_graph_id)? else {
            return Ok(false);
        };
        let commit = match self.vocab.ev_commit {
            Some(cp) => self.single_u64(ref_id, cp, ref_graph_id)?,
            None => None,
        };
        let reifier = match self.vocab.ev_reifier {
            Some(rp) => self.single_iri_object(ref_id, rp, ref_graph_id)?,
            None => None,
        };
        if commit == Some(self.snap.commit)
            && let Some(g_id) = graph_id
        {
            if let Some(r_id) = reifier
                && subject_masked(self.base_hidden, r_id, g_id)
            {
                return Ok(false);
            }
            let hidden = self.prep.with_term(self.snap, expected_id, |term| {
                let Some(Term::Triple(t)) = term else {
                    return Ok(true);
                };
                let s_term = match &t.subject {
                    NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
                    NamedOrBlankNode::BlankNode(_) => {
                        return Ok(reifier.is_none());
                    }
                };
                let p_term = Term::NamedNode(t.predicate.clone());
                let Some(s_id) = self.prep.lookup_term(self.snap, &s_term)? else {
                    return Ok(false);
                };
                let Some(p_id) = self.prep.lookup_term(self.snap, &p_term)? else {
                    return Ok(false);
                };
                let Some(o_id) = self.prep.lookup_term(self.snap, &t.object)? else {
                    return Ok(false);
                };
                Ok(self
                    .base_hidden
                    .binary_search(&[s_id.0, p_id.0, o_id.0, g_id.0])
                    .is_ok())
            })?;
            return Ok(!hidden);
        }
        if rules.state_independent() {
            let hides = self.prep.with_term(self.snap, expected_id, |term| {
                let Some(Term::Triple(t)) = term else {
                    return Ok(true);
                };
                if rules.hides_quad(t.predicate.as_str(), graph_term.as_ref()) {
                    return Ok(true);
                }
                if reifier.is_some() && rules.hides_quad(RDF_REIFIES, graph_term.as_ref()) {
                    return Ok(true);
                }
                Ok(false)
            })?;
            return Ok(!hides);
        }
        // State-dependent protection on a different commit cannot be proven from this
        // snapshot alone; fail closed.
        Ok(false)
    }

    fn lookup_triple_term(&self, triple: [Id; 3]) -> Result<Option<Id>> {
        let s = self
            .prep
            .with_term(self.snap, triple[0], |term| match term {
                Some(Term::NamedNode(n)) => Ok(Some(NamedOrBlankNode::NamedNode(n.clone()))),
                _ => Ok(None),
            })?;
        let Some(s) = s else {
            return Ok(None);
        };
        let p = self
            .prep
            .with_term(self.snap, triple[1], |term| match term {
                Some(Term::NamedNode(n)) => Ok(Some(n.clone())),
                _ => Ok(None),
            })?;
        let Some(p) = p else {
            return Ok(None);
        };
        let o = self
            .prep
            .with_term(self.snap, triple[2], |term| match term {
                Some(t) if !contains_blank(t) => Ok(Some(t.clone())),
                _ => Ok(None),
            })?;
        let Some(o) = o else {
            return Ok(None);
        };
        let tt = Term::Triple(Box::new(Triple::new(s, p, o)));
        self.prep.lookup_term(self.snap, &tt)
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
        Term::Triple(t) => {
            matches!(t.subject, NamedOrBlankNode::BlankNode(_)) || contains_blank(&t.object)
        }
        _ => false,
    }
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
    let readable = access.read.allows_named_iri(iri);
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
