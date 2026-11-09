//! The record encoding of the authenticated typed evidence writer (spec C25 §2, §5 and
//! §7).
//!
//! [`plan`] turns a prepared input into profile records with deterministic identities
//! and their immutable RDF content. [`Plan::quads`] adds the fields only the server
//! sets: the creation activity, the availability receipt, the first availability of a
//! source revision, each record's digest and the operation marker. The functions here
//! hold no lock and authorize nothing. The runtime that publishes a plan obtains the
//! managed-write capability, checks the caller's grants and preconditions, and makes
//! the one commit.
//!
//! # Identities
//!
//! Record IRIs are `urn:uuid:` UUIDs derived from the dataset, the authenticated
//! principal, a scope key and the record's occurrence key, through
//! [`IdentityNamespace`]. The scope key of a source revision is its source key, and its
//! occurrence is its content digest and upstream version, so the same observed revision
//! has one identity. A rendition is scoped by its revision, a passage by its rendition,
//! a claim by its revision, an evidence link and an assessment by their target, and an
//! evidence reference by what it references. The operation IRI is derived from the
//! dataset, the principal and the idempotency key, or is random without a key.
//!
//! # Immutable content and digests
//!
//! A record's immutable content is every triple the input determines: its class, its
//! fields and those of its selector and confidence nodes, which are named
//! `<record>#selector`, `<record>#sourceConfidence`, `<record>#extractionConfidence` and
//! `<record>#analystConfidence`. Its `ev:recordDigest` is the SHA-256 digest of the
//! sorted digests of those triples. The retained bytes and normalized text count only
//! by their presence, because the record's `ev:contentDigest` already covers them. The
//! server-owned fields are not part of it, so a record published again by another
//! operation with equal content has the same digest.
//!
//! # The operation marker
//!
//! Every graph an operation writes gets the operation's description, whose
//! `ev:graphProtection` is that graph's current protection nonce (see
//! [`crate::managed`]). Every record names the operation as its
//! `ev:availabilityReceipt` and carries the same `ev:graphProtection` itself. A
//! resolver trusts a record only when both the record and its receipt carry the nonce
//! of the protection in force, which a write made while the graph was detached cannot
//! know. The nonce on the record matters because operation IRIs derived from an
//! idempotency key are predictable, so a planted record could otherwise name an
//! operation that a later typed write describes with the current nonce.

use super::canonical::FrameHash;
use super::prepare::PreparedInput;
use super::{
    BlankAnchor, ClaimKind, Confidence, EvidenceRefTarget, EvidenceRelation, GraphIdentity,
    IdentityNamespace, MethodRef, Polarity, RecordKey, RecordKind, RecordRef, RenditionContent,
    Settings, Sha256Digest, SourceContent,
};
use crate::access::{Preparation, Reservation};
use crate::id::Id;
use crate::index::Perm;
use crate::store::Snapshot;
use crate::task::Control;
use crate::{Error, Result};
use base64::Engine as _;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use sha1::{Digest as _, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const EV: &str = "urn:x-sparkles:evidence:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const OA: &str = "http://www.w3.org/ns/oa#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// The predicates whose large values a record's `ev:contentDigest` already covers, and
/// which its record digest leaves out.
const LARGE: [&str; 2] = [
    "urn:x-sparkles:evidence:retainedBytes",
    "urn:x-sparkles:evidence:normalizedText",
];

pub(super) fn ev(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EV}{local}"))
}

pub(super) fn prov(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{PROV}{local}"))
}

fn oa(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{OA}{local}"))
}

pub(super) fn rdf_type() -> NamedNode {
    NamedNode::new_unchecked(RDF_TYPE)
}

pub(super) fn string(value: &str) -> Term {
    Literal::new_simple_literal(value).into()
}

pub(super) fn integer(value: u64) -> Term {
    Literal::new_typed_literal(
        value.to_string(),
        NamedNode::new_unchecked(format!("{XSD}integer")),
    )
    .into()
}

/// An `xsd:dateTime` in UTC with millisecond precision.
pub fn date_time(ms: i64) -> Literal {
    Literal::new_typed_literal(
        crate::commit::rfc3339_ms(ms),
        NamedNode::new_unchecked(format!("{XSD}dateTime")),
    )
}

/// The operation identity of an idempotency key: a UUID version 5 over a versioned,
/// length-framed tuple of the dataset, the authenticated principal and the key.
pub fn operation_id(dataset: Uuid, principal: &NamedNode, key: &str) -> Uuid {
    let mut hash = Sha1::new();
    hash.update(dataset.as_bytes());
    hash.update(b"sparkles-evidence-operation-v1\0");
    for field in [principal.as_str(), key] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let hash = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Builder::from_sha1_bytes(bytes).into_uuid()
}

/// The IRI of an operation id.
pub fn operation_iri(id: Uuid) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:uuid:{id}"))
}

/// The choices a proposal makes beyond its records.
#[derive(Clone, Debug)]
pub struct ProposalOptions<'a> {
    /// The authenticated principal, the creation activity's agent and the importer
    /// whose identity scopes the record identities.
    pub principal: &'a NamedNode,
    pub idempotency_key: Option<&'a str>,
    /// The source-record graph, required when the settings name more than one.
    pub source_graph: Option<&'a NamedNode>,
    /// The proposal graph, required when the settings name more than one.
    pub proposal_graph: Option<&'a NamedNode>,
    /// The identity graph of identity proposals, required when the settings name more
    /// than one.
    pub identity_graph: Option<&'a NamedNode>,
}

/// One record of a plan.
#[derive(Clone, Debug)]
pub struct PlannedRecord {
    /// The batch alias of the record.
    pub key: String,
    pub kind: RecordKind,
    pub iri: NamedNode,
    pub graph: NamedNode,
    /// The digest of the record's immutable content.
    pub digest: Sha256Digest,
    /// The immutable content.
    pub(super) triples: Vec<Triple>,
}

/// A stored fact of the plan's dataset that the operation keeps readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HeldCommit {
    pub branch: Uuid,
    pub commit: u64,
}

/// A proposal's records with their identities and immutable content.
#[derive(Clone, Debug)]
pub struct Plan {
    pub operation: Uuid,
    pub operation_iri: NamedNode,
    /// The digest that identifies the request's payload for idempotency: the prepared
    /// input's body digest and the chosen graphs.
    pub payload_digest: Sha256Digest,
    pub records: Vec<PlannedRecord>,
    /// The distinct graphs the operation writes, sorted.
    pub graphs: Vec<NamedNode>,
    /// The stored facts of this dataset that the operation's references name.
    pub held: Vec<HeldCommit>,
    principal: NamedNode,
    idempotency_key: Option<String>,
    /// `<source> a ev:Source` in the source-record graph.
    sources: Vec<NamedNode>,
    source_graph: Option<NamedNode>,
}

/// The fields of one publication that the server fixes under the writer.
#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    pub dataset: Uuid,
    /// The branch the commit publishes on.
    pub branch: Uuid,
    pub commit: u64,
    pub time_ms: i64,
}

fn choose(role: &str, configured: &[String], requested: Option<&NamedNode>) -> Result<NamedNode> {
    match requested {
        Some(g) if configured.iter().any(|c| c == g.as_str()) => Ok(g.clone()),
        Some(g) => Err(Error::invalid(format!(
            "{role}Graph: <{}> is not a {role} graph of the evidence settings",
            g.as_str()
        ))),
        None => match configured {
            [one] => NamedNode::new(one.as_str())
                .map_err(|e| Error::invalid(format!("{role} graph: {e}"))),
            [] => Err(Error::invalid(format!(
                "the evidence settings name no {role} graph"
            ))),
            _ => Err(Error::invalid(format!(
                "{role}Graph: the evidence settings name more than one {role} graph; name one"
            ))),
        },
    }
}

pub(super) fn no_blank(field: &str, term: &Term) -> Result<()> {
    fn blank(term: &Term) -> bool {
        match term {
            Term::BlankNode(_) => true,
            Term::Triple(t) => {
                matches!(t.subject, NamedOrBlankNode::BlankNode(_)) || blank(&t.object)
            }
            _ => false,
        }
    }
    if blank(term) {
        return Err(Error::invalid(format!(
            "{field}: a blank node is not durable record content; use an IRI"
        )));
    }
    Ok(())
}

fn key(value: impl Into<String>) -> Result<RecordKey> {
    RecordKey::new(value)
}

struct Builder<'i, 'c> {
    ns: IdentityNamespace,
    control: &'c Control,
    local: BTreeMap<&'i str, NamedNode>,
}

impl Builder<'_, '_> {
    fn mint(&self, kind: RecordKind, scope: &str, occurrence: &RecordKey) -> Result<NamedNode> {
        self.ns
            .record_id(kind, &key(scope)?, occurrence, self.control)
    }

    fn resolve(&self, reference: &RecordRef) -> Result<NamedNode> {
        match reference {
            RecordRef::Existing(iri) => Ok(iri.clone()),
            RecordRef::Local(k) => self
                .local
                .get(k.as_str())
                .cloned()
                .ok_or_else(|| Error::invalid(format!("unknown batch record {}", k.as_str()))),
        }
    }
}

pub(super) struct Record {
    pub(super) subject: NamedNode,
    pub(super) triples: Vec<Triple>,
}

impl Record {
    pub(super) fn new(subject: &NamedNode, kind: RecordKind) -> Record {
        let mut r = Record {
            subject: subject.clone(),
            triples: Vec::new(),
        };
        r.add(
            rdf_type(),
            NamedNode::new_unchecked(super::resolve::kind_iri(kind)),
        );
        r
    }

    pub(super) fn add(&mut self, p: NamedNode, o: impl Into<Term>) {
        self.triples
            .push(Triple::new(self.subject.clone(), p, o.into()));
    }

    fn on(&mut self, s: &NamedNode, p: NamedNode, o: impl Into<Term>) {
        self.triples.push(Triple::new(s.clone(), p, o.into()));
    }

    fn method(&mut self, method: &MethodRef) {
        self.add(ev("method"), method.method.clone());
        self.add(ev("methodVersion"), string(method.version.as_str()));
    }

    fn scope(&mut self, field: &str, scope: &super::Scope) -> Result<()> {
        for (predicate, values) in scope {
            let p = NamedNode::new(predicate.as_str())
                .map_err(|e| Error::invalid(format!("{field}.scope: {e}")))?;
            for value in values {
                no_blank(&format!("{field}.scope"), value)?;
                self.add(p.clone(), value.clone());
            }
        }
        Ok(())
    }

    fn confidence(&mut self, local: &str, value: Option<&Confidence>) {
        let Some(value) = value else {
            return;
        };
        let node = NamedNode::new_unchecked(format!("{}#{local}", self.subject.as_str()));
        self.add(ev(local), node.clone());
        self.on(&node, rdf_type(), ev("Confidence"));
        self.on(&node, ev("value"), value.value.clone());
        self.on(&node, ev("scale"), value.scale.clone());
        self.on(&node, prov("wasAttributedTo"), value.evaluator.clone());
        if let Some(method) = &value.method {
            self.on(&node, ev("method"), method.method.clone());
            self.on(&node, ev("methodVersion"), string(method.version.as_str()));
        }
    }
}

/// The digest of a record's immutable content: the sorted digests of its triples,
/// framed under the record kind, without the large values listed in [`LARGE`].
pub(super) fn record_digest(
    kind: RecordKind,
    triples: &[Triple],
    control: &Control,
) -> Result<Sha256Digest> {
    let mut each = Vec::with_capacity(triples.len());
    for t in triples {
        if LARGE.contains(&t.predicate.as_str()) {
            // which form of retained content the record has is part of its identity,
            // so that a metadata-only rendition never stands for one with text
            let mut frame = FrameHash::new(b"sparkles-evidence-record-retained-v1", control)?;
            frame.field(1, t.predicate.as_str().as_bytes())?;
            each.push(frame.finish()?);
            continue;
        }
        let mut frame = FrameHash::new(b"sparkles-evidence-record-triple-v1", control)?;
        frame.triple(t)?;
        each.push(frame.finish()?);
    }
    each.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    each.dedup();
    let mut frame = FrameHash::new(b"sparkles-evidence-record-v1", control)?;
    frame.field(20, &[kind.tag()])?;
    for d in &each {
        frame.field(12, d.as_bytes())?;
    }
    frame.finish()
}

/// Mint the records of `prepared` for the dataset `dataset` under `settings`, with
/// their immutable content and the graphs they go to. Source revisions, renditions and
/// passages go to the source-record graph, and evidence references, claims, evidence
/// links and assessments to the proposal graph. Refuses a batch without records, a
/// graph the settings do not give the role, blank nodes in record content other than
/// a stored fact's expected triple, and an assessment cycle.
pub fn plan<'i>(
    prepared: &PreparedInput<'i>,
    dataset: Uuid,
    settings: &Settings,
    options: &ProposalOptions<'_>,
    control: &Control,
) -> Result<Plan> {
    control.check()?;
    let input = prepared.input();
    if !settings.enabled {
        return Err(crate::managed::ManagedError::error(
            crate::managed::ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    let sources_used =
        !(input.revisions.is_empty() && input.renditions.is_empty() && input.passages.is_empty());
    let proposals_used = !(input.references.is_empty()
        && input.claims.is_empty()
        && input.links.is_empty()
        && input.assessments.is_empty());
    let identities_used = !input.identities.is_empty();
    if !sources_used && !proposals_used && !identities_used {
        return Err(Error::invalid("a proposal holds at least one record"));
    }
    if options.identity_graph.is_some() && !identities_used {
        return Err(Error::invalid(
            "identityGraph: the proposal has no identity proposal",
        ));
    }
    if options.source_graph.is_some() && !sources_used {
        return Err(Error::invalid(
            "sourceGraph: the proposal has no source revision, rendition or passage",
        ));
    }
    if options.proposal_graph.is_some() && !proposals_used {
        return Err(Error::invalid(
            "proposalGraph: the proposal has no reference, claim, link or assessment",
        ));
    }
    let source_graph = sources_used
        .then(|| choose("source", &settings.source_records, options.source_graph))
        .transpose()?;
    let proposal_graph = proposals_used
        .then(|| choose("proposal", &settings.proposals, options.proposal_graph))
        .transpose()?;
    let identity_graph = identities_used
        .then(|| choose("identity", &settings.identity, options.identity_graph))
        .transpose()?;
    if let Some(k) = options.idempotency_key
        && (k.is_empty() || k.len() > 256)
    {
        return Err(Error::invalid("idempotencyKey has 1 to 256 bytes"));
    }
    let operation = match options.idempotency_key {
        Some(k) => operation_id(dataset, options.principal, k),
        None => Uuid::new_v4(),
    };
    let mut payload = FrameHash::new(b"sparkles-evidence-proposal-v1", control)?;
    payload.field(12, prepared.body_digest().as_bytes())?;
    for g in [&source_graph, &proposal_graph] {
        payload.field(13, g.as_ref().map_or("", |g| g.as_str()).as_bytes())?;
    }
    // only a batch with identity proposals frames its identity graph, so that the
    // payload digests of other batches stay as they were
    if let Some(g) = &identity_graph {
        payload.field(14, g.as_str().as_bytes())?;
    }
    let payload_digest = payload.finish()?;

    let mut b = Builder {
        ns: IdentityNamespace::new(dataset, options.principal.clone())?,
        control,
        local: BTreeMap::new(),
    };
    let mut records: Vec<PlannedRecord> = Vec::new();
    let mut sources: Vec<NamedNode> = Vec::new();
    let push = |records: &mut Vec<PlannedRecord>,
                key: &str,
                kind: RecordKind,
                r: Record,
                graph: &NamedNode|
     -> Result<()> {
        let digest = record_digest(kind, &r.triples, control)?;
        records.push(PlannedRecord {
            key: key.to_string(),
            kind,
            iri: r.subject,
            graph: graph.clone(),
            digest,
            triples: r.triples,
        });
        Ok(())
    };
    // Source revisions are identified by their own content, so every revision IRI is
    // known before a lineage reference to another revision of the batch is encoded.
    for source in &input.revisions {
        let content = &prepared.content()[source.key.as_str()];
        let mut occurrence = content.digest.to_string();
        if let Some(v) = &source.upstream_version {
            occurrence.push(' ');
            occurrence.push_str(v.as_str());
        }
        let iri = b.mint(
            RecordKind::SourceRevision,
            source.source_key.as_str(),
            &key(occurrence)?,
        )?;
        b.local.insert(source.key.as_str(), iri);
    }
    let sg = source_graph.as_ref();
    let pg = proposal_graph.as_ref();
    for source in &input.revisions {
        control.check()?;
        let iri = b.local[source.key.as_str()].clone();
        let content = &prepared.content()[source.key.as_str()];
        let mut r = Record::new(&iri, RecordKind::SourceRevision);
        r.add(prov("specializationOf"), source.source.clone());
        if !sources.contains(&source.source) {
            sources.push(source.source.clone());
        }
        r.add(ev("contentDigest"), string(&content.digest.to_string()));
        r.add(ev("sourceKey"), string(source.source_key.as_str()));
        if let Some(v) = &source.upstream_version {
            r.add(ev("upstreamVersion"), string(v.as_str()));
        }
        if let SourceContent::Retained { bytes, .. } = &source.content {
            r.add(ev("retainedBytes"), base64_literal(bytes));
        }
        for locator in &source.metadata.locators {
            r.add(ev("locator"), locator.clone());
        }
        for rights in &source.metadata.reported_rights {
            no_blank("revisions.reportedRights", rights)?;
            r.add(ev("reportedRights"), rights.clone());
        }
        if let Some(t) = &source.times.published_at {
            r.add(ev("publishedAt"), t.clone());
        }
        if let Some(t) = &source.times.reported_available_at {
            r.add(ev("reportedAvailableAt"), t.clone());
        }
        for upstream in &source.lineage.upstream_revisions {
            r.add(ev("upstreamSource"), b.resolve(upstream)?);
        }
        for family in &source.lineage.families {
            r.add(ev("sourceFamily"), family.clone());
        }
        push(
            &mut records,
            source.key.as_str(),
            RecordKind::SourceRevision,
            r,
            sg.expect("source graph"),
        )?;
    }
    for rendition in &input.renditions {
        control.check()?;
        let revision = b.resolve(&rendition.revision)?;
        let iri = b.mint(
            RecordKind::Rendition,
            revision.as_str(),
            &rendition.occurrence_key,
        )?;
        let content = &prepared.content()[rendition.key.as_str()];
        let mut r = Record::new(&iri, RecordKind::Rendition);
        r.add(prov("wasDerivedFrom"), revision);
        r.method(&rendition.conversion);
        r.add(ev("contentDigest"), string(&content.digest.to_string()));
        r.add(ev("mediaType"), string(&rendition.media_type));
        if let Some(l) = &rendition.language {
            r.add(ev("language"), string(l));
        }
        match &rendition.content {
            RenditionContent::NormalizedText(text) => r.add(ev("normalizedText"), string(text)),
            RenditionContent::RetainedBytes(bytes) => {
                r.add(ev("retainedBytes"), base64_literal(bytes))
            }
            RenditionContent::MetadataOnly(_) => {}
        }
        b.local.insert(rendition.key.as_str(), iri);
        push(
            &mut records,
            rendition.key.as_str(),
            RecordKind::Rendition,
            r,
            sg.expect("source graph"),
        )?;
    }
    for passage in &input.passages {
        control.check()?;
        let revision = b.resolve(&passage.revision)?;
        let rendition = b.resolve(&passage.rendition)?;
        let iri = b.mint(
            RecordKind::Passage,
            rendition.as_str(),
            &passage.occurrence_key,
        )?;
        let mut r = Record::new(&iri, RecordKind::Passage);
        r.add(oa("hasSource"), rendition);
        r.add(ev("sourceRevision"), revision);
        let selector = NamedNode::new_unchecked(format!("{}#selector", iri.as_str()));
        r.add(oa("hasSelector"), selector.clone());
        r.on(&selector, rdf_type(), oa("TextPositionSelector"));
        r.on(&selector, oa("start"), integer(passage.start));
        r.on(&selector, oa("end"), integer(passage.end));
        r.add(ev("textDigest"), string(&passage.text_digest.to_string()));
        if let Some(q) = &passage.quote {
            r.add(oa("exact"), string(q));
        }
        b.local.insert(passage.key.as_str(), iri);
        push(
            &mut records,
            passage.key.as_str(),
            RecordKind::Passage,
            r,
            sg.expect("source graph"),
        )?;
    }
    let mut held = Vec::new();
    for reference in &input.references {
        control.check()?;
        let (scope, mut r_fields): (String, Vec<(NamedNode, Term)>) = match &reference.target {
            EvidenceRefTarget::StoredFact(fact) => {
                if fact.dataset == dataset {
                    held.push(HeldCommit {
                        branch: fact.branch,
                        commit: fact.commit,
                    });
                }
                let mut fields = vec![
                    (ev("datasetId"), string(&fact.dataset.to_string())),
                    (ev("branchId"), string(&fact.branch.to_string())),
                    (ev("commit"), integer(fact.commit)),
                    (
                        ev("expectedTriple"),
                        Term::Triple(Box::new(fact.expected.clone())),
                    ),
                ];
                match &fact.graph {
                    GraphIdentity::Default => {
                        fields.push((ev("graphKind"), ev("DefaultGraph").into()));
                    }
                    GraphIdentity::Named(g) => {
                        fields.push((ev("graphKind"), ev("NamedGraph").into()));
                        fields.push((ev("graph"), g.clone().into()));
                    }
                }
                match &fact.anchor {
                    None => {}
                    Some(BlankAnchor::Reifier(n)) => fields.push((ev("reifier"), n.clone().into())),
                    Some(BlankAnchor::ImportMap(n)) => {
                        fields.push((ev("skolemMap"), n.clone().into()))
                    }
                }
                (
                    format!(
                        "{EV}storedFact:{}:{}:{}",
                        fact.dataset, fact.branch, fact.commit
                    ),
                    fields,
                )
            }
            EvidenceRefTarget::Passage {
                revision,
                rendition,
                passage,
            } => {
                let passage = b.resolve(passage)?;
                (
                    passage.as_str().to_string(),
                    vec![
                        (ev("sourceRevision"), b.resolve(revision)?.into()),
                        (ev("rendition"), b.resolve(rendition)?.into()),
                        (ev("passage"), passage.into()),
                    ],
                )
            }
            EvidenceRefTarget::Structured {
                revision,
                pointer,
                method,
                value_digest,
            } => {
                let revision = b.resolve(revision)?;
                (
                    revision.as_str().to_string(),
                    vec![
                        (ev("sourceRevision"), revision.into()),
                        (ev("jsonPointer"), string(pointer)),
                        (ev("valueDigest"), string(&value_digest.to_string())),
                        (ev("method"), method.method.clone().into()),
                        (ev("methodVersion"), string(method.version.as_str())),
                    ],
                )
            }
        };
        let iri = b.mint(RecordKind::EvidenceRef, &scope, &reference.occurrence_key)?;
        let mut r = Record::new(&iri, RecordKind::EvidenceRef);
        for (p, o) in r_fields.drain(..) {
            r.add(p, o);
        }
        b.local.insert(reference.key.as_str(), iri);
        push(
            &mut records,
            reference.key.as_str(),
            RecordKind::EvidenceRef,
            r,
            pg.expect("proposal graph"),
        )?;
    }
    for claim in &input.claims {
        control.check()?;
        let revision = b.resolve(&claim.revision)?;
        no_blank(
            "claims.proposition",
            &Term::Triple(Box::new(claim.proposition.clone())),
        )?;
        let iri = b.mint(RecordKind::Claim, revision.as_str(), &claim.occurrence_key)?;
        let mut r = Record::new(&iri, RecordKind::Claim);
        r.add(
            NamedNode::new_unchecked(RDF_REIFIES),
            Term::Triple(Box::new(claim.proposition.clone())),
        );
        r.add(
            ev("claimKind"),
            ev(match claim.kind {
                ClaimKind::Reported => "Reported",
                ClaimKind::Observation => "Observation",
                ClaimKind::Hypothesis => "Hypothesis",
                ClaimKind::Preference => "Preference",
            }),
        );
        r.add(
            ev("polarity"),
            ev(match claim.polarity {
                Polarity::Positive => "Positive",
                Polarity::Negative => "Negative",
                Polarity::Uncertain => "Uncertain",
            }),
        );
        r.add(prov("wasDerivedFrom"), revision);
        r.add(prov("wasAttributedTo"), claim.attributed_author.clone());
        r.scope("claims", &claim.scope)?;
        r.confidence("sourceConfidence", claim.confidence.source.as_ref());
        r.confidence("extractionConfidence", claim.confidence.extraction.as_ref());
        r.confidence("analystConfidence", claim.confidence.analyst.as_ref());
        b.local.insert(claim.key.as_str(), iri);
        push(
            &mut records,
            claim.key.as_str(),
            RecordKind::Claim,
            r,
            pg.expect("proposal graph"),
        )?;
    }
    // Identity proposals are scoped by their set of entities.
    if let Some(ig) = identity_graph.as_ref() {
        for identity in &input.identities {
            control.check()?;
            let mut entities: Vec<&NamedNode> = identity.entities.iter().collect();
            entities.sort();
            let mut scope = FrameHash::new(b"sparkles-evidence-identity-scope-v1", control)?;
            for e in &entities {
                scope.field(13, e.as_str().as_bytes())?;
            }
            let scope = scope.finish()?.to_string();
            let iri = b.mint(
                RecordKind::IdentityProposal,
                &scope,
                &identity.occurrence_key,
            )?;
            let mut r = Record::new(&iri, RecordKind::IdentityProposal);
            for e in entities {
                r.add(ev("entity"), e.clone());
            }
            r.add(ev("relation"), ev(identity.relation.local()));
            if let Some(p) = &identity.preferred {
                r.add(ev("preferredEntity"), p.clone());
            }
            r.add(prov("wasAttributedTo"), identity.attributed_author.clone());
            if let Some(method) = &identity.method {
                r.method(method);
            }
            b.local.insert(identity.key.as_str(), iri);
            push(
                &mut records,
                identity.key.as_str(),
                RecordKind::IdentityProposal,
                r,
                ig,
            )?;
        }
    }
    // An assessment's identity is scoped by its target, which may be another
    // assessment of the batch, so they are minted once their targets are known.
    let mut pending: Vec<&super::AssessmentInput> = input.assessments.iter().collect();
    while !pending.is_empty() {
        control.check()?;
        let before = pending.len();
        let mut later = Vec::new();
        for assessment in pending {
            let Ok(target) = b.resolve(&assessment.target) else {
                later.push(assessment);
                continue;
            };
            let Ok(inputs) = assessment
                .inputs
                .iter()
                .map(|i| b.resolve(i))
                .collect::<Result<std::collections::BTreeSet<NamedNode>>>()
            else {
                later.push(assessment);
                continue;
            };
            no_blank("assessments.conclusion", &assessment.conclusion)?;
            let iri = b.mint(
                RecordKind::Assessment,
                target.as_str(),
                &assessment.occurrence_key,
            )?;
            let mut r = Record::new(&iri, RecordKind::Assessment);
            r.add(ev("target"), target);
            r.add(ev("conclusion"), assessment.conclusion.clone());
            r.method(&assessment.method);
            r.add(
                ev("configurationDigest"),
                string(&assessment.configuration_digest.to_string()),
            );
            r.add(ev("evaluatedAt"), assessment.evaluated_at.clone());
            r.add(
                prov("wasAttributedTo"),
                assessment.attributed_evaluator.clone(),
            );
            r.confidence("analystConfidence", assessment.confidence.as_ref());
            r.scope("assessments", &assessment.scope)?;
            for input in inputs {
                r.add(prov("used"), input);
            }
            b.local.insert(assessment.key.as_str(), iri);
            push(
                &mut records,
                assessment.key.as_str(),
                RecordKind::Assessment,
                r,
                pg.expect("proposal graph"),
            )?;
        }
        if later.len() == before {
            return Err(Error::invalid(
                "assessments: the batch's assessments target or use each other in a cycle",
            ));
        }
        pending = later;
    }
    for link in &input.links {
        control.check()?;
        let target = b.resolve(&link.target)?;
        let iri = b.mint(
            RecordKind::EvidenceLink,
            target.as_str(),
            &link.occurrence_key,
        )?;
        let mut r = Record::new(&iri, RecordKind::EvidenceLink);
        r.add(ev("target"), target);
        r.add(ev("evidence"), b.resolve(&link.evidence)?);
        r.add(
            ev("relation"),
            ev(match link.relation {
                EvidenceRelation::Supports => "Supports",
                EvidenceRelation::Contradicts => "Contradicts",
                EvidenceRelation::Mentions => "Mentions",
            }),
        );
        r.add(prov("wasAttributedTo"), link.attributed_author.clone());
        b.local.insert(link.key.as_str(), iri);
        push(
            &mut records,
            link.key.as_str(),
            RecordKind::EvidenceLink,
            r,
            pg.expect("proposal graph"),
        )?;
    }
    // Distinct aliases can mint one identity, as two occurrences with the same keys.
    let mut seen = std::collections::BTreeSet::new();
    for r in &records {
        if !seen.insert(r.iri.as_str()) {
            return Err(Error::invalid(format!(
                "{}: another record of the batch has the same identity; give it another occurrence key",
                r.key
            )));
        }
    }
    held.sort_unstable();
    held.dedup();
    let mut graphs: Vec<NamedNode> = source_graph
        .iter()
        .chain(&proposal_graph)
        .chain(&identity_graph)
        .cloned()
        .collect();
    graphs.sort();
    graphs.dedup();
    Ok(Plan {
        operation,
        operation_iri: operation_iri(operation),
        payload_digest,
        records,
        graphs,
        held,
        principal: options.principal.clone(),
        idempotency_key: options.idempotency_key.map(str::to_owned),
        sources,
        source_graph,
    })
}

fn base64_literal(bytes: &[u8]) -> Term {
    Literal::new_typed_literal(
        base64::engine::general_purpose::STANDARD.encode(bytes),
        NamedNode::new_unchecked(format!("{XSD}base64Binary")),
    )
    .into()
}

impl Plan {
    /// Every quad the operation publishes, given the commit's `stamp`, the protection
    /// nonce of each graph it writes, and the hold it acquired. `skip[i]` leaves out the
    /// record `records[i]`, which an earlier operation already published with equal
    /// content. The operation's description and receipt go to every graph it writes,
    /// each with that graph's nonce. A source revision's `ev:firstAvailableAt` is the
    /// commit's time.
    pub fn quads(
        &self,
        stamp: &Stamp,
        protection: impl Fn(&NamedNode) -> Option<Uuid>,
        hold: Option<Uuid>,
        skip: &[bool],
    ) -> Result<Vec<Quad>> {
        let mut out = publish_quads(
            &Operation {
                iri: &self.operation_iri,
                principal: &self.principal,
                payload_digest: self.payload_digest,
                idempotency_key: self.idempotency_key.as_deref(),
                hold,
            },
            &self.graphs,
            &self.records,
            stamp,
            &protection,
            skip,
        )?;
        if let Some(g) = &self.source_graph {
            for s in &self.sources {
                out.push(Quad::new(
                    s.clone(),
                    rdf_type(),
                    ev("Source"),
                    GraphName::NamedNode(g.clone()),
                ));
            }
        }
        Ok(out)
    }
}

/// The description of one typed operation.
pub(super) struct Operation<'a> {
    pub(super) iri: &'a NamedNode,
    pub(super) principal: &'a NamedNode,
    pub(super) payload_digest: Sha256Digest,
    pub(super) idempotency_key: Option<&'a str>,
    pub(super) hold: Option<Uuid>,
}

/// The operation's description in every graph of `graphs`, each with that graph's
/// protection nonce, and every record of `records` not left out by `skip`, with its
/// receipt, creation activity, nonce and record digest. A source revision's
/// `ev:firstAvailableAt` and a withdrawal's `prov:invalidatedAtTime` are the commit's
/// time. Every record graph must be among `graphs`.
pub(super) fn publish_quads(
    op: &Operation<'_>,
    graphs: &[NamedNode],
    records: &[PlannedRecord],
    stamp: &Stamp,
    protection: &dyn Fn(&NamedNode) -> Option<Uuid>,
    skip: &[bool],
) -> Result<Vec<Quad>> {
    let time = date_time(stamp.time_ms);
    let mut out = Vec::new();
    let mut quad = |s: &NamedNode, p: NamedNode, o: Term, g: &NamedNode| {
        out.push(Quad::new(s.clone(), p, o, GraphName::NamedNode(g.clone())));
    };
    let iri = op.iri;
    for g in graphs {
        let Some(nonce) = protection(g) else {
            return Err(unprotected(g));
        };
        quad(iri, rdf_type(), ev("Operation").into(), g);
        quad(iri, rdf_type(), prov("Activity").into(), g);
        quad(
            iri,
            prov("wasAssociatedWith"),
            op.principal.clone().into(),
            g,
        );
        quad(iri, prov("endedAtTime"), time.clone().into(), g);
        quad(iri, ev("datasetId"), string(&stamp.dataset.to_string()), g);
        quad(iri, ev("branchId"), string(&stamp.branch.to_string()), g);
        quad(iri, ev("commit"), integer(stamp.commit), g);
        quad(
            iri,
            ev("payloadDigest"),
            string(&op.payload_digest.to_string()),
            g,
        );
        quad(iri, ev("graphProtection"), string(&nonce.to_string()), g);
        if let Some(k) = op.idempotency_key {
            quad(iri, ev("idempotencyKey"), string(k), g);
        }
        if let Some(h) = op.hold {
            quad(iri, ev("sourceHold"), string(&h.to_string()), g);
        }
    }
    for (i, r) in records.iter().enumerate() {
        if skip.get(i).copied().unwrap_or(false) {
            continue;
        }
        for t in &r.triples {
            let NamedOrBlankNode::NamedNode(s) = &t.subject else {
                unreachable!("record subjects are IRIs");
            };
            quad(s, t.predicate.clone(), t.object.clone(), &r.graph);
        }
        quad(
            &r.iri,
            ev("availabilityReceipt"),
            iri.clone().into(),
            &r.graph,
        );
        quad(&r.iri, prov("wasGeneratedBy"), iri.clone().into(), &r.graph);
        // the record carries its graph's nonce itself, so that no record planted
        // while the graph was detached can borrow a later operation's marker
        let nonce = protection(&r.graph).ok_or_else(|| unprotected(&r.graph))?;
        quad(
            &r.iri,
            ev("graphProtection"),
            string(&nonce.to_string()),
            &r.graph,
        );
        quad(
            &r.iri,
            ev("recordDigest"),
            string(&r.digest.to_string()),
            &r.graph,
        );
        match r.kind {
            RecordKind::SourceRevision => quad(
                &r.iri,
                ev("firstAvailableAt"),
                time.clone().into(),
                &r.graph,
            ),
            RecordKind::Withdrawal => quad(
                &r.iri,
                prov("invalidatedAtTime"),
                time.clone().into(),
                &r.graph,
            ),
            _ => {}
        }
    }
    Ok(out)
}

/// The refusal of a write to a graph the evidence owner does not protect.
pub fn unprotected(g: &NamedNode) -> Error {
    crate::managed::ManagedError::error(
        crate::managed::ManagedErrorKind::UnsupportedProfile,
        format!(
            "graph <{}> is not protected by the evidence profile; record the evidence settings again",
            g.as_str()
        ),
    )
}

// ------------------------------------------------------- reads under the writer ---

/// A graph and its current protection nonce, `None` when it has none.
pub type ProtectedGraph = (NamedNode, Option<Uuid>);

/// An operation found in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedOperation {
    /// Whether every graph that describes the operation carries that graph's current
    /// protection nonce.
    pub authenticated: bool,
    pub payload_digest: Option<Sha256Digest>,
    pub branch: Option<Uuid>,
    pub commit: Option<u64>,
    /// When the operation published, as recorded.
    pub ended_at: Option<Literal>,
}

/// A record identity found in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExistingRecord {
    Absent,
    /// The identity has content in `graph`, and nowhere else among the graphs looked at.
    Present {
        graph: NamedNode,
        /// Its `ev:recordDigest`, when it has exactly one.
        digest: Option<Sha256Digest>,
        /// Whether its receipt carries the graph's current protection nonce.
        authenticated: bool,
    },
    /// The identity has content in more than one graph.
    Many,
}

/// Lookups in one unmasked snapshot under one preparation. The writer reads its head
/// with them, which no caller policy limits: what they find decides only whether the
/// operation repeats or conflicts with records of the caller's own identities.
pub struct Reads<'a> {
    preparation: &'a Preparation,
    snap: &'a Snapshot,
    ids: BTreeMap<String, Option<Id>>,
}

impl<'a> Reads<'a> {
    /// The preparation the reads are charged to.
    pub fn preparation(&self) -> &'a Preparation {
        self.preparation
    }

    pub fn new(preparation: &'a Preparation, snap: &'a Snapshot) -> Self {
        Reads {
            preparation,
            snap,
            ids: BTreeMap::new(),
        }
    }

    fn id(&mut self, iri: &str) -> Result<Option<Id>> {
        if let Some(id) = self.ids.get(iri) {
            return Ok(*id);
        }
        let id = self.preparation.lookup_iri(self.snap, iri)?;
        self.ids.insert(iri.to_string(), id);
        Ok(id)
    }

    /// Up to two objects of `s p ?o` in `g`.
    fn objects(&mut self, s: &str, p: &str, g: &str) -> Result<Vec<Term>> {
        let (Some(s), Some(p), Some(g)) = (self.id(s)?, self.id(p)?, self.id(g)?) else {
            return Ok(Vec::new());
        };
        let mut ids = Vec::new();
        self.preparation
            .scan(self.snap, Perm::Gspo, &[g.0, s.0, p.0], |q| {
                ids.push(q[2]);
                Ok(ids.len() < 2)
            })?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(t) = self
                .preparation
                .with_term(self.snap, id, |t| Ok(t.cloned()))?
            {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// The objects of `s p ?o` in `g`, at most `limit` of them, and whether more exist.
    pub fn objects_up_to(
        &mut self,
        s: &str,
        p: &str,
        g: &str,
        limit: usize,
    ) -> Result<(Vec<Term>, bool)> {
        let (Some(s), Some(p), Some(g)) = (self.id(s)?, self.id(p)?, self.id(g)?) else {
            return Ok((Vec::new(), false));
        };
        let mut ids = Vec::new();
        let mut more = false;
        self.preparation
            .scan(self.snap, Perm::Gspo, &[g.0, s.0, p.0], |q| {
                if ids.len() == limit {
                    more = true;
                    return Ok(false);
                }
                ids.push(q[2]);
                Ok(true)
            })?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self
                .preparation
                .with_term(self.snap, id, |t| Ok(t.cloned()))?
            {
                out.push(t);
            }
        }
        Ok((out, more))
    }

    /// The IRI subjects of `?s p o` in `g`, at most `limit` of them, and whether more
    /// exist. `o` is a ground term.
    pub fn subjects_up_to(
        &mut self,
        p: &str,
        o: &Term,
        g: &str,
        limit: usize,
    ) -> Result<(Vec<NamedNode>, bool)> {
        let (Some(p), Some(g)) = (self.id(p)?, self.id(g)?) else {
            return Ok((Vec::new(), false));
        };
        let o = match o {
            Term::NamedNode(n) => self.id(n.as_str())?,
            t => self.preparation.lookup_term(self.snap, t)?,
        };
        let Some(o) = o else {
            return Ok((Vec::new(), false));
        };
        let mut ids = Vec::new();
        let mut more = false;
        self.preparation
            .scan(self.snap, Perm::Pos, &[p.0, o.0], |q| {
                if q[3] != g {
                    return Ok(true);
                }
                if ids.len() == limit {
                    more = true;
                    return Ok(false);
                }
                ids.push(q[0]);
                Ok(true)
            })?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(Term::NamedNode(n)) = self
                .preparation
                .with_term(self.snap, id, |t| Ok(t.cloned()))?
            {
                out.push(n);
            }
        }
        Ok((out, more))
    }

    /// The number of triples whose subject is `s` in `g`, counting at most `limit`.
    pub fn subject_triples(&mut self, s: &str, g: &str, limit: usize) -> Result<usize> {
        let (Some(s), Some(g)) = (self.id(s)?, self.id(g)?) else {
            return Ok(0);
        };
        let mut n = 0;
        self.preparation
            .scan(self.snap, Perm::Gspo, &[g.0, s.0], |_| {
                n += 1;
                Ok(n < limit)
            })?;
        Ok(n)
    }

    /// Whether `g` holds any quad.
    pub fn graph_nonempty(&mut self, g: &str) -> Result<bool> {
        let Some(g) = self.id(g)? else {
            return Ok(false);
        };
        let mut found = false;
        self.preparation.scan(self.snap, Perm::Gspo, &[g.0], |_| {
            found = true;
            Ok(false)
        })?;
        Ok(found)
    }

    /// Whether the quad `s p o g` is in the snapshot. `o` is a ground term.
    pub fn contains(&mut self, s: &str, p: &str, o: &Term, g: &str) -> Result<bool> {
        let (Some(s), Some(p), Some(g)) = (self.id(s)?, self.id(p)?, self.id(g)?) else {
            return Ok(false);
        };
        let o = match o {
            Term::NamedNode(n) => self.id(n.as_str())?,
            t => self.preparation.lookup_term(self.snap, t)?,
        };
        let Some(o) = o else {
            return Ok(false);
        };
        self.snap.contains(&[s, p, o, g])
    }

    /// Whether `s` is a typed operation's authenticated record in `g`: its single
    /// receipt is an operation of `g` carrying `nonce`, and the record carries `nonce`.
    pub fn authenticated(&mut self, s: &str, g: &str, nonce: Option<Uuid>) -> Result<bool> {
        if nonce.is_none() {
            return Ok(false);
        }
        Ok(match self.one(s, &format!("{EV}availabilityReceipt"), g)? {
            Some(Term::NamedNode(op)) => {
                self.marked(op.as_str(), g, nonce)? && self.nonce_of(s, g)? == nonce
            }
            _ => false,
        })
    }

    /// The single object of `s p ?o` in `g`, `None` when it has none or several.
    pub fn single(&mut self, s: &str, p: &str, g: &str) -> Result<Option<Term>> {
        self.one(s, p, g)
    }

    fn one(&mut self, s: &str, p: &str, g: &str) -> Result<Option<Term>> {
        let mut v = self.objects(s, p, g)?;
        Ok(if v.len() == 1 { v.pop() } else { None })
    }

    fn one_string(&mut self, s: &str, p: &str, g: &str) -> Result<Option<String>> {
        Ok(match self.one(s, p, g)? {
            Some(Term::Literal(l)) if l.datatype().as_str() == format!("{XSD}string") => {
                Some(l.value().to_string())
            }
            _ => None,
        })
    }

    fn has_subject(&mut self, s: &str, g: &str) -> Result<bool> {
        let (Some(s), Some(g)) = (self.id(s)?, self.id(g)?) else {
            return Ok(false);
        };
        let mut found = false;
        self.preparation
            .scan(self.snap, Perm::Gspo, &[g.0, s.0], |_| {
                found = true;
                Ok(false)
            })?;
        Ok(found)
    }

    fn is_operation(&mut self, op: &str, g: &str) -> Result<bool> {
        let (Some(s), Some(p), Some(o), Some(g)) = (
            self.id(op)?,
            self.id(RDF_TYPE)?,
            self.id(&format!("{EV}Operation"))?,
            self.id(g)?,
        ) else {
            return Ok(false);
        };
        self.snap.contains(&[s, p, o, g])
    }

    /// The single protection nonce that `s` carries in `g`.
    fn nonce_of(&mut self, s: &str, g: &str) -> Result<Option<Uuid>> {
        Ok(self
            .one_string(s, &format!("{EV}graphProtection"), g)?
            .and_then(|v| Uuid::parse_str(&v).ok()))
    }

    /// Whether `op` is an operation in `g` whose marker is `nonce`.
    fn marked(&mut self, op: &str, g: &str, nonce: Option<Uuid>) -> Result<bool> {
        let Some(nonce) = nonce else {
            return Ok(false);
        };
        Ok(self.is_operation(op, g)? && self.nonce_of(op, g)? == Some(nonce))
    }

    /// Whether some graph that holds `s p o` satisfies `accept`, which sees each graph's
    /// id, the default graph's included. `o` is a ground term.
    pub fn in_some_graph(
        &mut self,
        s: &str,
        p: &str,
        o: &Term,
        mut accept: impl FnMut(&Snapshot, Id) -> bool,
    ) -> Result<bool> {
        let (Some(s), Some(p)) = (self.id(s)?, self.id(p)?) else {
            return Ok(false);
        };
        let o = match o {
            Term::NamedNode(n) => self.id(n.as_str())?,
            t => self.preparation.lookup_term(self.snap, t)?,
        };
        let Some(o) = o else {
            return Ok(false);
        };
        let snap = self.snap;
        let mut found = false;
        self.preparation
            .scan(self.snap, Perm::Spo, &[s.0, p.0, o.0], |q| {
                if accept(snap, q[3]) {
                    found = true;
                    return Ok(false);
                }
                Ok(true)
            })?;
        Ok(found)
    }

    /// The IRI subjects in `g` that reify, through `p`, a triple term whose subject or
    /// object is one of `entities`, in subject order. At most `scan` quads of `g` are
    /// read. The rows read are charged to the preparation while the scan runs, and each
    /// identity returned is charged to `held` before it is copied, so the caller keeps
    /// it charged for as long as it keeps `held`. Returns whether `g` holds more than
    /// `scan` quads, in which case the list is incomplete.
    pub fn reifiers_about(
        &mut self,
        p: &str,
        g: &str,
        entities: &BTreeSet<String>,
        scan: usize,
        held: &mut Reservation,
    ) -> Result<(Vec<NamedNode>, bool)> {
        let (Some(p), Some(g)) = (self.id(p)?, self.id(g)?) else {
            return Ok((Vec::new(), false));
        };
        const ROW: u64 = std::mem::size_of::<(Id, Id)>() as u64;
        const CHUNK: usize = 4096;
        let mut rows_held = self.preparation.reserve(0)?;
        let mut rows: Vec<(Id, Id)> = Vec::new();
        let mut read = 0usize;
        let mut more = false;
        self.preparation.scan(self.snap, Perm::Gspo, &[g.0], |q| {
            if read == scan {
                more = true;
                return Ok(false);
            }
            read += 1;
            if q[1] == p {
                if rows.len() == rows.capacity() {
                    rows_held.grow_by(CHUNK as u64 * ROW)?;
                    rows.reserve_exact(CHUNK);
                }
                rows.push((q[0], q[2]));
            }
            Ok(true)
        })?;
        let mut out = Vec::new();
        for (s, o) in rows {
            let matches = self.preparation.with_term(self.snap, o, |t| {
                let Some(Term::Triple(t)) = t else {
                    return Ok(false);
                };
                let subject = match &t.subject {
                    NamedOrBlankNode::NamedNode(n) => entities.contains(n.as_str()),
                    _ => false,
                };
                let object = match &t.object {
                    Term::NamedNode(n) => entities.contains(n.as_str()),
                    _ => false,
                };
                Ok(subject || object)
            })?;
            if !matches {
                continue;
            }
            if let Some(n) = self.preparation.with_term(self.snap, s, |t| {
                Ok(match t {
                    Some(Term::NamedNode(n)) => {
                        held.grow_by(
                            n.as_str().len() as u64 + std::mem::size_of::<NamedNode>() as u64,
                        )?;
                        Some(n.as_str().to_string())
                    }
                    _ => None,
                })
            })? {
                out.push(NamedNode::new_unchecked(n));
            }
        }
        Ok((out, more))
    }

    /// Whether `op` is described as an `ev:Operation` in any graph, whatever its marker.
    /// A reconciliation keeps the hold of such an operation, since a changed protection
    /// does not show that it never published.
    pub fn described_anywhere(&mut self, op: &NamedNode) -> Result<bool> {
        let (Some(s), Some(p), Some(o)) = (
            self.id(op.as_str())?,
            self.id(RDF_TYPE)?,
            self.id(&format!("{EV}Operation"))?,
        ) else {
            return Ok(false);
        };
        let mut found = false;
        self.preparation
            .scan(self.snap, Perm::Spo, &[s.0, p.0, o.0], |_| {
                found = true;
                Ok(false)
            })?;
        Ok(found)
    }

    /// The operation `op` as `graphs` describe it, `None` when none does.
    pub fn operation(
        &mut self,
        op: &NamedNode,
        graphs: &[ProtectedGraph],
    ) -> Result<Option<RecordedOperation>> {
        let mut found: Option<RecordedOperation> = None;
        for (g, nonce) in graphs {
            self.preparation.check()?;
            if !self.is_operation(op.as_str(), g.as_str())? {
                continue;
            }
            let authenticated = self.marked(op.as_str(), g.as_str(), *nonce)?;
            let payload_digest = self
                .one_string(op.as_str(), &format!("{EV}payloadDigest"), g.as_str())?
                .and_then(|d| d.parse().ok());
            let branch = self
                .one_string(op.as_str(), &format!("{EV}branchId"), g.as_str())?
                .and_then(|b| Uuid::parse_str(&b).ok());
            let commit = match self.one(op.as_str(), &format!("{EV}commit"), g.as_str())? {
                Some(Term::Literal(l)) if l.datatype().as_str() == format!("{XSD}integer") => {
                    l.value().parse().ok()
                }
                _ => None,
            };
            let ended_at = match self.one(op.as_str(), &format!("{PROV}endedAtTime"), g.as_str())? {
                Some(Term::Literal(l)) => Some(l),
                _ => None,
            };
            let this = RecordedOperation {
                authenticated,
                payload_digest,
                branch,
                commit,
                ended_at,
            };
            found = Some(match found {
                None => this,
                Some(prev) => RecordedOperation {
                    authenticated: prev.authenticated && this.authenticated,
                    payload_digest: prev
                        .payload_digest
                        .filter(|d| Some(*d) == this.payload_digest),
                    branch: prev.branch.filter(|b| Some(*b) == this.branch),
                    commit: prev.commit.filter(|c| Some(*c) == this.commit),
                    ended_at: prev.ended_at,
                },
            });
        }
        Ok(found)
    }

    /// The single availability receipt of `iri` in `g`.
    pub fn receipt_of(&mut self, iri: &NamedNode, g: &NamedNode) -> Result<Option<NamedNode>> {
        Ok(
            match self.one(
                iri.as_str(),
                &format!("{EV}availabilityReceipt"),
                g.as_str(),
            )? {
                Some(Term::NamedNode(op)) => Some(op),
                _ => None,
            },
        )
    }

    /// Where the record identity `iri` has content among `graphs`.
    pub fn record(&mut self, iri: &NamedNode, graphs: &[ProtectedGraph]) -> Result<ExistingRecord> {
        let mut found = ExistingRecord::Absent;
        for (g, nonce) in graphs {
            self.preparation.check()?;
            if !self.has_subject(iri.as_str(), g.as_str())? {
                continue;
            }
            if !matches!(found, ExistingRecord::Absent) {
                return Ok(ExistingRecord::Many);
            }
            let digest = self
                .one_string(iri.as_str(), &format!("{EV}recordDigest"), g.as_str())?
                .and_then(|d| d.parse().ok());
            let authenticated = match self.one(
                iri.as_str(),
                &format!("{EV}availabilityReceipt"),
                g.as_str(),
            )? {
                Some(Term::NamedNode(op)) => {
                    self.marked(op.as_str(), g.as_str(), *nonce)?
                        && self.nonce_of(iri.as_str(), g.as_str())? == *nonce
                }
                _ => false,
            };
            found = ExistingRecord::Present {
                graph: g.clone(),
                digest,
                authenticated,
            };
        }
        Ok(found)
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
