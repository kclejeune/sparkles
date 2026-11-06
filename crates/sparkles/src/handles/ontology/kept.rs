//! Immutable classification publications and read-only bounded class relations.
//!
//! The retained estimate is a between-run policy. Old reader Arcs and a publication
//! visible during a new flight can extend peak lifetime beyond that estimate.
use super::classify::{CACHE_MAX_BYTES, CacheEntry, ClassifyRequest};
use super::{Ontologies, OntologyRequest, Opened, check};
use crate::dataset::DatasetState;
use crate::error::{Error, Result};
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use parking_lot::MutexGuard;
use serde::Serialize;
use serde_json::{Value as J, json};
use sha2::{Digest, Sha256};
use sparkles_owl::classify::{Classification, Excluded, Expansion, Gap};
use sparkles_reasoner::inputs::Import;
use std::io::{self, Write};
use std::sync::Arc;
use uuid::Uuid;

/// The exact bound input view, apart from its selected commit and producer policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputKey {
    pub bound_store_id: Uuid,
    pub family_id: Uuid,
    pub graphs: Vec<String>,
    pub imports: &'static str,
    pub mode: &'static str,
}
impl InputKey {
    pub(super) fn of(h: &Ontologies, req: &OntologyRequest, snap: &Snapshot) -> Self {
        Self {
            bound_store_id: h.ds.store().branch_id(),
            family_id: snap.dataset_id,
            graphs: req
                .graphs()
                .iter()
                .map(|g| g.as_str().to_string())
                .collect(),
            imports: match req.imports {
                sparkles_reasoner::inputs::ImportMode::None => "none",
                sparkles_reasoner::inputs::ImportMode::Dataset => "dataset",
                sparkles_reasoner::inputs::ImportMode::Fetch => "fetch",
            },
            mode: req.mode.as_str(),
        }
    }
    fn owned_bytes(&self) -> usize {
        self.graphs
            .capacity()
            .saturating_mul(size_of::<String>())
            .saturating_add(self.graphs.iter().map(String::capacity).sum::<usize>())
    }
}

/// Source data captured with a result, never read from later saturation inputs.
#[derive(Clone, Debug)]
pub(super) struct Provenance {
    pub input: InputKey,
    pub commit: u64,
    pub assertions: bool,
    pub el_only: bool,
    pub partial_requested: bool,
    pub expansion: &'static str,
    pub input_fingerprint: String,
    pub source_fingerprint: String,
    pub resolved: Vec<String>,
    pub requested: usize,
    pub imports: Vec<Import>,
    pub warnings: Vec<String>,
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn hash_json(hash: &mut HashWriter, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *hash, value)
        .map_err(|e| Error::invalid(format!("classification identity: {e}")))?;
    hash.write_all(b"\n").map_err(Error::Io)
}
fn finish_hash(hash: HashWriter) -> String {
    format!(
        "sha256:{}",
        hash.0
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn start_hash(domain: &[u8]) -> HashWriter {
    let mut h = Sha256::new();
    h.update(domain);
    HashWriter(h)
}
impl Provenance {
    pub(super) fn capture(
        h: &Ontologies,
        req: &OntologyRequest,
        c: &ClassifyRequest,
        o: &Opened,
        opts: &QueryOptions,
    ) -> Result<Self> {
        #[cfg(test)]
        assert!(
            h.ds.state().classify_cache.try_lock().is_some(),
            "source digest must run outside the slot lock"
        );
        check(opts)?;
        let input = InputKey::of(h, req, &o.snapshot);
        let mut hash = start_hash(b"sparkles-classification-input-v1\n");
        hash_json(
            &mut hash,
            &(&input, o.snapshot.commit, c.assertions, c.el_only),
        )?;
        let mut p = Self {
            input,
            commit: o.snapshot.commit,
            assertions: c.assertions,
            el_only: c.el_only,
            partial_requested: c.partial,
            expansion: match c.expansion {
                Expansion::Cautious => "cautious",
                Expansion::Eager => "eager",
            },
            input_fingerprint: finish_hash(hash),
            source_fingerprint: String::new(),
            resolved: o.graphs.iter().map(|g| g.as_str().to_string()).collect(),
            requested: o.requested,
            imports: o.imports.clone(),
            warnings: o.warnings.clone(),
        };
        let mut hash = start_hash(b"sparkles-classification-sources-v1\n");
        for i in 0..p.source_count() {
            check(opts)?;
            hash_json(&mut hash, &p.source_row(i).expect("source ordinal"))?;
        }
        p.source_fingerprint = finish_hash(hash);
        Ok(p)
    }
    pub fn source_count(&self) -> usize {
        self.input
            .graphs
            .len()
            .saturating_add(self.resolved.len())
            .saturating_add(self.imports.len())
            .saturating_add(self.warnings.len())
    }
    /// One canonical JSON row, with sorted object keys. Digest encoding is compact
    /// UTF-8 JSON plus LF for every row, following the v1 domain line.
    pub fn source_row(&self, mut i: usize) -> Option<J> {
        let ordinal = i;
        if let Some(graph) = self.input.graphs.get(i) {
            return Some(json!({"kind":"requestedGraph","ordinal":ordinal,"graph":graph}));
        }
        i = i.checked_sub(self.input.graphs.len())?;
        if let Some(graph) = self.resolved.get(i) {
            return Some(
                json!({"kind":"resolvedGraph","ordinal":ordinal,"graph":graph,"requested":i < self.requested}),
            );
        }
        i = i.checked_sub(self.resolved.len())?;
        if let Some(import) = self.imports.get(i) {
            return Some(
                json!({"kind":"import","ordinal":ordinal,"iri":import.iri,"location":import.location,"graph":import.graph}),
            );
        }
        i = i.checked_sub(self.imports.len())?;
        self.warnings
            .get(i)
            .map(|message| json!({"kind":"warning","ordinal":ordinal,"message":message}))
    }
    pub fn identity(&self, content: &str) -> J {
        json!({"boundStoreId":self.input.bound_store_id,"familyId":self.input.family_id,"commit":self.commit,
            "inputFingerprint":self.input_fingerprint,"contentFingerprint":content,
            "producer":{"assertions":self.assertions,"elOnly":self.el_only,"partialRequested":self.partial_requested,"expansion":self.expansion}})
    }
    fn owned_bytes(&self) -> usize {
        let strings = |v: &Vec<String>| {
            v.capacity()
                .saturating_mul(size_of::<String>())
                .saturating_add(v.iter().map(String::capacity).sum::<usize>())
        };
        size_of::<Self>()
            .saturating_add(self.input.owned_bytes())
            .saturating_add(strings(&self.resolved))
            .saturating_add(strings(&self.warnings))
            .saturating_add(self.imports.capacity().saturating_mul(size_of::<Import>()))
            .saturating_add(
                self.imports
                    .iter()
                    .map(|i| {
                        i.iri
                            .capacity()
                            .saturating_add(i.location.as_ref().map_or(0, String::capacity))
                            .saturating_add(i.graph.as_ref().map_or(0, String::capacity))
                    })
                    .sum::<usize>(),
            )
            .saturating_add(self.input_fingerprint.capacity())
            .saturating_add(self.source_fingerprint.capacity())
    }
}

struct ClassNames<'a>(&'a [sparkles_owl::Class]);
impl Serialize for ClassNames<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|c| c.as_str()))
    }
}
/// Fingerprint all paged taxonomy content and actual outcome, excluding timings,
/// labels, reuse flags and per-run IDs. No second taxonomy is materialized.
pub(super) fn content_fingerprint(
    cl: &Classification,
    excluded: &[Excluded],
    opts: &QueryOptions,
) -> Result<String> {
    let mut hash = start_hash(b"sparkles-classification-content-v1\n");
    hash_json(
        &mut hash,
        &(
            cl.stats.tier.as_str(),
            cl.consistent,
            consistency(cl),
            cl.complete,
            ClassNames(&cl.incomplete),
        ),
    )?;
    for (i, n) in cl.taxonomy.nodes().iter().enumerate() {
        check(opts)?;
        hash_json(
            &mut hash,
            &(i, ClassNames(&n.classes), &n.parents, &n.children),
        )?;
    }
    // A deterministic standard-prefix OFN axiom and typed coverage gap identify each retained exclusion.
    let r = sparkles_owl::ofn::Renderer::new(std::iter::empty::<(String, String)>());
    for e in excluded {
        check(opts)?;
        let ax = sparkles_owl::Axiom::new(e.axiom.clone());
        hash_json(&mut hash, &(r.axiom(&ax), gap_json(&e.gap)))?;
    }
    check(opts)?;
    Ok(finish_hash(hash))
}
pub(super) fn gap_json(gap: &Gap) -> J {
    match gap {
        Gap::Outside { rule, message } => json!({"kind":"omitted","rule":rule,"message":message}),
        Gap::Partial(reason) => json!({"kind":"partiallyUsed","reason":reason}),
    }
}
pub(super) fn consistency(cl: &Classification) -> &'static str {
    match cl.consistency {
        sparkles_owl::classify::Consistency::Consistent => "consistent",
        sparkles_owl::classify::Consistency::Inconsistent => "inconsistent",
        sparkles_owl::classify::Consistency::Unknown => "unknown",
    }
}

/// One immutable computation, published only when the retained policy permits it.
pub(super) struct KeptClassification {
    pub id: Uuid,
    pub source: Arc<Provenance>,
    pub classification: Arc<Classification>,
    pub excluded: Arc<Vec<Excluded>>,
    pub content_fingerprint: String,
    pub(super) auxiliary_bytes: usize,
}
impl KeptClassification {
    pub(super) fn capture(
        h: &Ontologies,
        req: &OntologyRequest,
        c: &ClassifyRequest,
        o: &Opened,
        classification: Arc<Classification>,
        excluded: Arc<Vec<Excluded>>,
        opts: &QueryOptions,
    ) -> Result<Arc<Self>> {
        let controls = super::classify::result_options(opts, classification.complete);
        let opts = &controls;
        let source = Arc::new(Provenance::capture(h, req, c, o, opts)?);
        let content_fingerprint = content_fingerprint(&classification, &excluded, opts)?;
        let auxiliary_bytes = size_of::<Self>()
            .saturating_add(source.owned_bytes())
            .saturating_add(sparkles_owl::classify::excluded_copy_bytes(&excluded))
            .saturating_add(content_fingerprint.capacity());
        Ok(Arc::new(Self {
            id: Uuid::new_v4(),
            source,
            classification,
            excluded,
            content_fingerprint,
            auxiliary_bytes,
        }))
    }
    pub fn identity(&self) -> J {
        self.source.identity(&self.content_fingerprint)
    }
}
#[derive(Clone)]
pub(super) struct Active {
    pub token: Uuid,
    pub key: InputKey,
    pub commit: u64,
    pub kind: &'static str,
}
/// The slot owns only references/status while a flight owns its mutable entry.
#[derive(Default)]
pub(crate) struct KeptCache {
    pub(super) entry: Option<CacheEntry>,
    pub(super) publication: Option<Arc<KeptClassification>>,
    pub(super) active: Option<Arc<Active>>,
}

pub(super) struct KeptFlight<'a> {
    state: &'a DatasetState,
    entry: Option<CacheEntry>,
    token: Uuid,
    finalized: bool,
    _flight: MutexGuard<'a, ()>,
}
impl<'a> KeptFlight<'a> {
    pub fn begin(
        state: &'a DatasetState,
        flight: MutexGuard<'a, ()>,
        key: InputKey,
        commit: u64,
        kind: &'static str,
    ) -> Self {
        let token = Uuid::new_v4();
        let active = Arc::new(Active {
            token,
            key: key.clone(),
            commit,
            kind,
        });
        let (entry, retired) = {
            let mut slot = state.classify_cache.lock();
            let entry = slot.entry.take();
            let same = entry.as_ref().is_some_and(|e| e.key == key);
            let retired = if same { None } else { slot.publication.take() };
            slot.active = Some(active);
            (entry, retired)
        };
        drop(retired);
        let mut entry = match entry {
            Some(e) if e.key == key => e,
            other => {
                drop(other);
                CacheEntry::new(key)
            }
        };
        entry.mutated = false;
        Self {
            state,
            entry: Some(entry),
            token,
            finalized: false,
            _flight: flight,
        }
    }
    pub fn entry(&mut self) -> &mut CacheEntry {
        self.entry.as_mut().expect("owned flight")
    }
    pub fn finish(mut self, keep: bool) {
        let entry = if keep { self.entry.take() } else { None };
        let publication = entry.as_ref().and_then(|e| e.result.clone());
        let retired = {
            let mut slot = self.state.classify_cache.lock();
            assert!(slot.active.as_ref().is_some_and(|a| a.token == self.token));
            let old_entry = std::mem::replace(&mut slot.entry, entry);
            let old_result = std::mem::replace(&mut slot.publication, publication);
            let old_active = slot.active.take();
            (old_entry, old_result, old_active)
        };
        self.finalized = true;
        drop(retired);
    }
}
impl Drop for KeptFlight<'_> {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }
        let retired = {
            let mut slot = self.state.classify_cache.lock();
            if slot.active.as_ref().is_some_and(|a| a.token == self.token) {
                (
                    slot.entry.take(),
                    slot.publication.take(),
                    slot.active.take(),
                )
            } else {
                (None, None, None)
            }
        };
        drop(retired);
        // The taken entry is dropped before the field's flight guard is released.
        drop(self.entry.take());
    }
}

pub(super) fn entry_aux_bytes(e: &CacheEntry) -> usize {
    size_of::<CacheEntry>().saturating_add(e.key.owned_bytes())
}
pub(super) fn classification_retained_bytes(e: &CacheEntry, p: &KeptClassification) -> usize {
    p.classification
        .stats
        .memory
        .saturating_add(entry_aux_bytes(e))
        .saturating_add(p.auxiliary_bytes)
}
pub(super) fn dl_retained_bytes(e: &CacheEntry, saturation: usize) -> usize {
    saturation
        .saturating_add(entry_aux_bytes(e))
        .saturating_add(e.taxonomy_memory())
        .saturating_add(e.result.as_ref().map_or(0, |p| p.auxiliary_bytes))
}
#[cfg(test)]
thread_local! { static TEST_RETENTION_CAP: std::cell::Cell<usize> = const { std::cell::Cell::new(CACHE_MAX_BYTES) }; }
pub(super) fn retention_cap() -> usize {
    #[cfg(test)]
    {
        TEST_RETENTION_CAP.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        CACHE_MAX_BYTES
    }
}
pub(super) fn retain_dl(e: &mut CacheEntry, saturation: usize) -> bool {
    if dl_retained_bytes(e, saturation) > retention_cap() {
        e.drop_taxonomy();
    }
    dl_retained_bytes(e, saturation) <= retention_cap()
}

/// Bounded portions of an immutable kept result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    #[default]
    Status,
    Parents,
    Children,
    Roots,
    Equivalents,
    Sources,
    Excluded,
}
#[derive(Clone, Debug)]
pub struct KeptQuery {
    pub part: Part,
    pub about: Option<String>,
    pub offset: usize,
    pub limit: usize,
    pub result_id: Option<Uuid>,
    pub languages: Vec<String>,
}
impl Default for KeptQuery {
    fn default() -> Self {
        Self {
            part: Part::Status,
            about: None,
            offset: 0,
            limit: 100,
            result_id: None,
            languages: Vec::new(),
        }
    }
}
impl KeptQuery {
    pub fn validate(&self, req: &OntologyRequest) -> Result<()> {
        req.validate()?;
        if req.inferences {
            return Err(Error::invalid(
                "a kept classification reads asserted inputs, not inferences",
            ));
        }
        if !(1..=1000).contains(&self.limit) {
            return Err(Error::invalid("limit must be from 1 to 1000"));
        }
        if let Some(iri) = &self.about {
            oxrdf::NamedNode::new(iri)
                .map_err(|_| Error::invalid("about must be an absolute named IRI"))?;
        }
        if matches!(
            self.part,
            Part::Parents | Part::Children | Part::Equivalents
        ) && self.about.is_none()
        {
            return Err(Error::invalid("parents/children/equivalents require about"));
        }
        if matches!(self.part, Part::Sources | Part::Excluded | Part::Roots) && self.about.is_some()
        {
            return Err(Error::invalid("sources/excluded/roots do not take about"));
        }
        if self.part == Part::Status && self.offset != 0 {
            return Err(Error::invalid("status has no offset"));
        }
        if self.offset != 0
            && (self.result_id.is_none() || !matches!(req.at, Some(crate::history::At::Commit(_))))
        {
            return Err(Error::invalid(
                "a continuation requires resultId and at=commit:N",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Available,
    Running,
    Mismatch,
    Unavailable,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCounts {
    pub requested_graphs: usize,
    pub resolved_graphs: usize,
    pub imports: usize,
    pub warnings: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultMetadata {
    pub result_id: Uuid,
    pub classified_commit: u64,
    pub bound_store_id: Uuid,
    pub family_id: Uuid,
    pub input_fingerprint: String,
    pub source_fingerprint: String,
    pub content_fingerprint: String,
    pub imports: &'static str,
    pub mode: &'static str,
    pub assertions: bool,
    pub el_only: bool,
    pub partial_requested: bool,
    pub expansion: &'static str,
    pub source_counts: SourceCounts,
    pub capabilities: [&'static str; 5],
    pub justifications: &'static str,
}
impl KeptClassification {
    pub fn metadata(&self) -> ResultMetadata {
        let p = &self.source;
        ResultMetadata {
            result_id: self.id,
            classified_commit: p.commit,
            bound_store_id: p.input.bound_store_id,
            family_id: p.input.family_id,
            input_fingerprint: p.input_fingerprint.clone(),
            source_fingerprint: p.source_fingerprint.clone(),
            content_fingerprint: self.content_fingerprint.clone(),
            imports: p.input.imports,
            mode: p.input.mode,
            assertions: p.assertions,
            el_only: p.el_only,
            partial_requested: p.partial_requested,
            expansion: p.expansion,
            source_counts: SourceCounts {
                requested_graphs: p.input.graphs.len(),
                resolved_graphs: p.resolved.len(),
                imports: p.imports.len(),
                warnings: p.warnings.len(),
            },
            capabilities: [
                "namedClassDirectParents",
                "namedClassDirectChildren",
                "namedClassRoots",
                "namedClassEquivalents",
                "namedClassUnsatisfiability",
            ],
            justifications: "unavailable",
        }
    }
    pub fn outcome(&self) -> Outcome {
        let c = &self.classification;
        Outcome {
            tier: c.stats.tier.as_str(),
            partial: !c.complete,
            complete: c.complete && self.excluded.is_empty(),
            consistency: consistency(c),
            excluded_count: self.excluded.len(),
            incomplete_count: c.incomplete.len(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub tier: &'static str,
    pub partial: bool,
    pub complete: bool,
    pub consistency: &'static str,
    pub excluded_count: usize,
    pub incomplete_count: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityStatus {
    pub iri: String,
    pub present: bool,
    pub unsatisfiable: Option<bool>,
    pub representative: Option<String>,
    pub equivalence_count: Option<usize>,
    pub children: Option<usize>,
    pub parents: Option<usize>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub kind: &'static str,
    pub commit: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub part: Part,
    pub about: Option<String>,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub items: Vec<J>,
    pub next_offset: Option<usize>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptRead {
    pub commit: u64,
    pub status: Status,
    pub reason: Option<&'static str>,
    pub result: Option<ResultMetadata>,
    pub outcome: Option<Outcome>,
    pub entity: Option<EntityStatus>,
    pub activity: Option<Activity>,
    pub page: Option<Page>,
    pub stale_commit: Option<u64>,
}
impl KeptRead {
    fn unavailable(commit: u64, reason: &'static str) -> Self {
        Self {
            commit,
            status: Status::Unavailable,
            reason: Some(reason),
            result: None,
            outcome: None,
            entity: None,
            activity: None,
            page: None,
            stale_commit: None,
        }
    }
}
impl Ontologies {
    /// Inspect an existing immutable classification without starting or waiting for
    /// classification, mapping the ontology or admitting algorithm workers.
    pub fn kept_classification(
        &self,
        req: &OntologyRequest,
        query: &KeptQuery,
        opts: &QueryOptions,
    ) -> Result<KeptRead> {
        query.validate(req)?;
        let (snapshot, opts) = self.state(req, opts)?;
        check(&opts)?;
        let mut reply = KeptRead::unavailable(snapshot.commit, "notKept");
        if opts.graphs.is_some() || !snapshot.cacheable {
            reply.reason = Some("unsupportedScope");
            return Ok(reply);
        }
        let key = InputKey::of(self, req, &snapshot);
        // Only clone pointers under the slot lock. Keys, names, counts and all
        // fingerprint/estimate work are inspected outside it.
        let (publication, active) = {
            let slot = self.ds.state().classify_cache.lock();
            (slot.publication.clone(), slot.active.clone())
        };
        let publication = publication.filter(|p| p.source.input == key);
        let active = active.filter(|a| a.key == key && a.commit == snapshot.commit);
        let selected = if let Some(id) = query.result_id {
            match publication {
                Some(p) if p.id == id && p.source.commit == snapshot.commit => Some(p),
                Some(p) => {
                    reply.status = Status::Mismatch;
                    reply.reason = Some(if p.id == id {
                        "commitMismatch"
                    } else {
                        "resultChanged"
                    });
                    return Ok(reply);
                }
                None => return Ok(reply),
            }
        } else if let Some(p) = publication
            .as_ref()
            .filter(|p| p.source.commit == snapshot.commit)
        {
            Some(p.clone())
        } else if let Some(a) = active.as_ref() {
            reply.status = Status::Running;
            reply.reason = None;
            reply.activity = Some(Activity {
                kind: a.kind,
                commit: a.commit,
            });
            return Ok(reply);
        } else if let Some(p) = publication {
            reply.status = Status::Mismatch;
            reply.reason = Some("commitMismatch");
            reply.stale_commit = Some(p.source.commit);
            return Ok(reply);
        } else {
            None
        };
        let Some(p) = selected else {
            return Ok(reply);
        };
        reply.status = Status::Available;
        reply.reason = None;
        reply.result = Some(p.metadata());
        reply.outcome = Some(p.outcome());
        reply.activity = active.map(|a| Activity {
            kind: a.kind,
            commit: a.commit,
        });
        if let Some(iri) = &query.about {
            let class = sparkles_owl::Class::new(iri.as_str());
            let taxonomy = &p.classification.taxonomy;
            let node = taxonomy.node_of(&class).map(|id| taxonomy.node(id));
            let present = node.is_some();
            reply.entity = Some(EntityStatus {
                iri: iri.clone(),
                present,
                unsatisfiable: present.then(|| taxonomy.is_unsatisfiable(&class)),
                representative: node
                    .and_then(|node| node.classes.first())
                    .map(|class| class.as_str().to_string()),
                equivalence_count: node.map(|node| node.classes.len()),
                children: node.map(|node| node.children.len()),
                parents: node.map(|node| node.parents.len()),
            });
        }
        if query.part != Part::Status {
            reply.page = Some(Self::kept_page(req, query, &snapshot, &p, &opts)?);
        }
        check(&opts)?;
        Ok(reply)
    }
    pub(super) fn kept_page(
        req: &OntologyRequest,
        q: &KeptQuery,
        snapshot: &Arc<Snapshot>,
        p: &KeptClassification,
        opts: &QueryOptions,
    ) -> Result<Page> {
        let mut page = Page {
            part: q.part,
            about: q.about.clone(),
            offset: q.offset,
            limit: q.limit,
            total: 0,
            items: Vec::new(),
            next_offset: None,
        };
        let end = q.offset.saturating_add(q.limit);
        match q.part {
            Part::Status => (),
            Part::Sources => {
                page.total = p.source.source_count();
                for i in q.offset..end.min(page.total) {
                    check(opts)?;
                    #[cfg(test)]
                    diagnostic::visit(diagnostic::Visit::Source);
                    page.items
                        .push(p.source.source_row(i).expect("source ordinal"));
                }
            }
            Part::Excluded => {
                page.total = p.excluded.len();
                let renderer =
                    sparkles_owl::ofn::Renderer::new(std::iter::empty::<(String, String)>());
                for e in p.excluded.iter().skip(q.offset).take(q.limit) {
                    check(opts)?;
                    #[cfg(test)]
                    diagnostic::visit(diagnostic::Visit::Excluded);
                    let ax = sparkles_owl::Axiom::new(e.axiom.clone());
                    page.items.push(json!({"type":ax.axiom_type().name(),"ofn":renderer.axiom(&ax),"axiom":sparkles_owl::json::axiom(&ax),"gap":gap_json(&e.gap)}));
                }
            }
            Part::Roots | Part::Children => {
                let t = &p.classification.taxonomy;
                let top_group = [0usize];
                let relatives = if q.part == Part::Roots {
                    // A nontrivial top-equivalence group is a root of its own.
                    // Otherwise expose its direct children without a redundant
                    // owl:Thing row. Bottom is a separate diagnostic root.
                    Some(if t.top().classes.len() > 1 {
                        top_group.as_slice()
                    } else {
                        t.top().children.as_slice()
                    })
                } else {
                    q.about
                        .as_deref()
                        .and_then(|iri| t.node_of(&sparkles_owl::Class::new(iri)))
                        .map(|node| t.node(node).children.as_slice())
                };
                let bottom =
                    (q.part == Part::Roots && t.bottom().classes.len() > 1).then_some(1usize);
                let relatives = relatives.unwrap_or_default();
                page.total = relatives.len() + usize::from(bottom.is_some());
                let ids: Vec<_> = relatives
                    .iter()
                    .copied()
                    .chain(bottom)
                    .skip(q.offset)
                    .take(q.limit)
                    .collect();
                let mut iris = Vec::with_capacity(ids.len());
                for &id in &ids {
                    check(opts)?;
                    if let Some(class) = t.node(id).classes.first() {
                        iris.push(class.as_str().to_string());
                    }
                }
                let names =
                    Self::kept_names_at(req, snapshot.clone(), &iris, &q.languages, opts.clone())?;
                for (id, name) in ids.into_iter().zip(names) {
                    let node = t.node(id);
                    page.items.push(json!({
                        "iri":name.iri,"label":name.label,"labelLang":name.label_lang,
                        "equivalenceCount":node.classes.len(),
                        "children":node.children.len(),"parents":node.parents.len(),
                    }));
                }
            }
            Part::Parents | Part::Equivalents => {
                let about = q.about.as_deref().expect("validated about");
                let t = &p.classification.taxonomy;
                if let Some(node) = t.node_of(&sparkles_owl::Class::new(about)) {
                    let mut iris = Vec::with_capacity(q.limit);
                    let mut counts = Vec::with_capacity(q.limit);
                    if q.part == Part::Parents {
                        // Parent IDs are sorted. Ordinary nodes have canonical IRI
                        // order; TOP is the sole parent when no ordinary parent exists.
                        page.total = t.node(node).parents.len();
                        for &parent in t.node(node).parents.iter().skip(q.offset).take(q.limit) {
                            check(opts)?;
                            #[cfg(test)]
                            diagnostic::visit(diagnostic::Visit::Parent);
                            let n = t.node(parent);
                            if let Some(c) = n.classes.first() {
                                iris.push(c.as_str().to_string());
                                counts.push((n.classes.len(), n.children.len(), n.parents.len()));
                            }
                        }
                    } else {
                        page.total = t.node(node).classes.len().saturating_sub(1);
                        let mut index = 0;
                        for c in ordered_classes(&t.node(node).classes) {
                            check(opts)?;
                            #[cfg(test)]
                            diagnostic::visit(diagnostic::Visit::Equivalent);
                            if c.as_str() == about {
                                continue;
                            }
                            if index >= q.offset && index < end {
                                iris.push(c.as_str().to_string());
                            }
                            index += 1;
                            if index >= end {
                                break;
                            }
                        }
                    }
                    let names = Self::kept_names_at(
                        req,
                        snapshot.clone(),
                        &iris,
                        &q.languages,
                        opts.clone(),
                    )?;
                    for (i, n) in names.into_iter().enumerate() {
                        let mut name =
                            json!({"iri":n.iri,"label":n.label,"labelLang":n.label_lang});
                        if q.part == Part::Parents {
                            name["equivalenceCount"] = json!(counts[i].0);
                            name["children"] = json!(counts[i].1);
                            name["parents"] = json!(counts[i].2);
                        }
                        page.items.push(name);
                    }
                }
            }
        }
        page.next_offset = (end < page.total).then_some(end);
        check(opts)?;
        Ok(page)
    }
}

/// Deterministic test accounting for taxonomy/source rows and per-page name
/// requests only. This is not a counter for underlying RDF/index work or latency.
#[cfg(test)]
pub(super) mod diagnostic {
    use std::cell::Cell;
    #[derive(Clone, Copy, Debug, Default, serde::Serialize)]
    pub struct Work {
        pub equivalence_candidates: usize,
        pub parent_candidates: usize,
        pub name_requests: usize,
        pub source_rows: usize,
        pub excluded_rows: usize,
    }
    thread_local! { static WORK: Cell<Work> = Cell::new(Work::default()); }
    pub enum Visit {
        Equivalent,
        Parent,
        Name,
        Source,
        Excluded,
    }
    pub fn visit(visit: Visit) {
        WORK.with(|cell| {
            let mut work = cell.get();
            match visit {
                Visit::Equivalent => work.equivalence_candidates += 1,
                Visit::Parent => work.parent_candidates += 1,
                Visit::Name => work.name_requests += 1,
                Visit::Source => work.source_rows += 1,
                Visit::Excluded => work.excluded_rows += 1,
            }
            cell.set(work);
        });
    }
    pub fn take() -> Work {
        WORK.with(|cell| cell.replace(Work::default()))
    }
}
/// TOP/BOTTOM have one or two built-ins first, followed by IRI-sorted named
/// members. Merge those short prefixes without copying a large equivalence node.
fn ordered_classes(classes: &[sparkles_owl::Class]) -> impl Iterator<Item = &sparkles_owl::Class> {
    let builtin = |c: &&sparkles_owl::Class| {
        matches!(
            c.as_str(),
            sparkles_owl::vocab::OWL_THING | sparkles_owl::vocab::OWL_NOTHING
        )
    };
    let prefix = classes.iter().take_while(builtin).count();
    let mut a = classes[..prefix].iter().peekable();
    let mut b = classes[prefix..].iter().peekable();
    std::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if x.as_str() <= y.as_str() => a.next(),
        (Some(_), Some(_)) | (None, Some(_)) => b.next(),
        (Some(_), None) => a.next(),
        (None, None) => None,
    })
}

#[cfg(test)]
mod tests;
