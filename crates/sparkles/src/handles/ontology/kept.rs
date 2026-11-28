//! Immutable classification publications and read-only bounded class relations.
//!
//! The retained estimate is a between-run policy. Old reader Arcs and a publication
//! visible during a new flight can extend peak lifetime beyond that estimate.
use super::classify::{CACHE_MAX_BYTES, CacheEntry, ClassifiedExclusions, ClassifyRequest};
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
    /// the digest of the import map and vocabulary resources that the imports resolve
    /// through, which can change without a commit, or zero when imports are not read
    #[serde(skip)]
    pub import_targets: u64,
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
            import_targets: match req.imports {
                sparkles_reasoner::inputs::ImportMode::None => 0,
                _ => req.import_targets.digest(),
            },
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
    let mut hash = start_hash(b"sparkles-classification-content-v2\n");
    hash_json(
        &mut hash,
        &(
            cl.stats.tier.as_str(),
            cl.consistent,
            consistency(cl),
            cl.complete,
            cl.complete_for_taken,
            ClassNames(&cl.incomplete),
            cl.stats.unmapped,
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

pub(super) fn cache_parent(
    pool: &Arc<sparkles_core::sparql::pool::QueryPool>,
) -> sparkles_budget::ParentHandle {
    if pool.domain_stats().retained_capacity > 0 {
        pool.retained_parent()
    } else {
        pool.work_parent()
    }
}

/// Acquire independent dataset cache credit before returning the caller's credit.
pub(super) fn admit_cached_memory(
    opts: &QueryOptions,
    pool: &Arc<sparkles_core::sparql::pool::QueryPool>,
    memory: &mut crate::reasoning::memory::Memory,
) -> bool {
    if opts.grant.is_none()
        && opts.pool.as_ref().is_some_and(|p| Arc::ptr_eq(p, pool))
        && pool.domain_stats().retained_capacity == 0
    {
        return true;
    }
    let mut cached = crate::reasoning::memory::Memory::new(None, Some(cache_parent(pool)));
    if cached.grow(memory.bytes()).is_err() {
        return false;
    }
    *memory = cached;
    true
}

/// One immutable computation, published only when the retained policy permits it.
pub(super) struct KeptClassification {
    pub id: Uuid,
    pub source: Arc<Provenance>,
    pub classification: Arc<Classification>,
    pub excluded: Arc<ClassifiedExclusions>,
    pub content_fingerprint: String,
    pub(super) auxiliary_bytes: usize,
    memory: Option<crate::reasoning::memory::Memory>,
    _lease: Option<Arc<sparkles_budget::MemoryLease>>,
}
impl KeptClassification {
    pub(super) fn capture(
        h: &Ontologies,
        req: &OntologyRequest,
        c: &ClassifyRequest,
        o: &Opened,
        answer: (Classification, Vec<Excluded>),
        opts: &QueryOptions,
        working: &super::classify::Working,
    ) -> Result<Self> {
        let (classification, excluded) = answer;
        let taxonomy_bytes = classification.stats.taxonomy_memory;
        let excluded_bytes = sparkles_owl::classify::excluded_copy_bytes(&excluded);
        // Admit the source strings and the source rows written during hashing.
        working
            .lock()
            .grow((1024 + o.source_bytes().saturating_mul(4)) as u64)?;
        let controls = super::classify::result_options(
            opts,
            !super::classify::acknowledged_stop(&classification),
        );
        let opts = &controls;
        let source = Arc::new(Provenance::capture(h, req, c, o, opts)?);
        let content_fingerprint = content_fingerprint(&classification, &excluded, opts)?;
        let auxiliary_bytes = size_of::<Self>()
            .saturating_add(source.owned_bytes())
            .saturating_add(excluded_bytes)
            .saturating_add(classification.incomplete_bytes())
            .saturating_add(super::classify::unmapped_bytes(&classification.unmapped))
            .saturating_add(content_fingerprint.capacity());
        let memory = working
            .lock()
            .split((taxonomy_bytes + auxiliary_bytes) as u64)?;
        Ok(Self {
            id: Uuid::new_v4(),
            source,
            classification: Arc::new(classification),
            excluded: Arc::new(ClassifiedExclusions::new(excluded)),
            content_fingerprint,
            auxiliary_bytes,
            memory: Some(memory),
            _lease: None,
        })
    }

    pub(super) fn admit_retention(
        &mut self,
        opts: &QueryOptions,
        pool: Option<&Arc<sparkles_core::sparql::pool::QueryPool>>,
    ) -> bool {
        pool.is_none_or(|pool| {
            admit_cached_memory(
                opts,
                pool,
                self.memory.as_mut().expect("unpublished memory"),
            )
        })
    }

    pub(super) fn seal(mut self) -> Arc<Self> {
        let memory = self
            .memory
            .take()
            .and_then(|memory| memory.into_lease())
            .map(Arc::new);
        Arc::get_mut(&mut self.classification)
            .expect("unpublished classification")
            .taxonomy
            .keep_memory(memory.clone());
        Arc::get_mut(&mut self.excluded)
            .expect("unpublished exclusions")
            .memory = memory.clone();
        self._lease = memory;
        Arc::new(self)
    }

    pub fn identity(&self) -> J {
        self.source.identity(&self.content_fingerprint)
    }
}
#[derive(Clone)]
pub(super) struct Active {
    pub token: Uuid,
    pub key: InputKey,
    pub el_only: bool,
    pub commit: u64,
    pub kind: &'static str,
}
/// One mode slot (`el_only = false` or `el_only = true`) of the dataset's kept cache.
#[derive(Default)]
pub(super) struct KeptSlot {
    pub(super) entry: Option<CacheEntry>,
    pub(super) publication: Option<Arc<KeptClassification>>,
    pub(super) active: Option<Arc<Active>>,
}
/// The cache owns separate slots for default and EL-only runs so they never clobber
/// or mis-serve each other, while total retained bytes stay within [`retention_cap`].
#[derive(Default)]
pub(crate) struct KeptCache {
    slots: [KeptSlot; 2],
}
impl KeptCache {
    pub(super) fn slot(&self, el_only: bool) -> &KeptSlot {
        &self.slots[usize::from(el_only)]
    }
    pub(super) fn slot_mut(&mut self, el_only: bool) -> &mut KeptSlot {
        &mut self.slots[usize::from(el_only)]
    }
}

pub(super) struct KeptFlight<'a> {
    state: &'a DatasetState,
    el_only: bool,
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
        el_only: bool,
        commit: u64,
        kind: &'static str,
    ) -> Self {
        let token = Uuid::new_v4();
        let active = Arc::new(Active {
            token,
            key: key.clone(),
            el_only,
            commit,
            kind,
        });
        let (entry, retired) = {
            let mut cache = state.classify_cache.lock();
            let slot = cache.slot_mut(el_only);
            let entry = slot.entry.take();
            let same = entry
                .as_ref()
                .is_some_and(|e| e.key == key && e.el_only == el_only);
            let retired = if same { None } else { slot.publication.take() };
            slot.active = Some(active);
            (entry, retired)
        };
        drop(retired);
        let mut entry = match entry {
            Some(e) if e.key == key && e.el_only == el_only => e,
            other => {
                drop(other);
                CacheEntry::new(key, el_only)
            }
        };
        entry.mutated = false;
        Self {
            state,
            el_only,
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
            let mut cache = self.state.classify_cache.lock();
            let other_retired = if let Some(ref kept) = entry {
                let other = cache.slot_mut(!self.el_only);
                let other_bytes = other.entry.as_ref().map_or(0, |o| o.retained_bytes);
                if kept.retained_bytes.saturating_add(other_bytes) > retention_cap() {
                    (other.entry.take(), other.publication.take())
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };
            let slot = cache.slot_mut(self.el_only);
            assert!(slot.active.as_ref().is_some_and(|a| a.token == self.token));
            let old_entry = std::mem::replace(&mut slot.entry, entry);
            let old_result = std::mem::replace(&mut slot.publication, publication);
            let old_active = slot.active.take();
            (old_entry, old_result, old_active, other_retired)
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
            let mut cache = self.state.classify_cache.lock();
            let slot = cache.slot_mut(self.el_only);
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
        .saturating_sub(p.classification.incomplete_bytes())
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
    let bytes = dl_retained_bytes(e, saturation);
    let keep = bytes <= retention_cap();
    if keep {
        e.retained_bytes = bytes;
    }
    keep
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
    /// Read the kept EL-only classification instead of the default classification.
    pub el_only: bool,
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
            el_only: false,
        }
    }
}
impl KeptQuery {
    pub fn validate(&self, req: &OntologyRequest) -> Result<()> {
        self.validate_for(req, false)
    }
    /// [`validate`](Self::validate), where `pinned` says that the handle answers at a
    /// pinned snapshot, which names the commit of a continuation as `at=commit:N` does.
    pub(super) fn validate_for(&self, req: &OntologyRequest, pinned: bool) -> Result<()> {
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
        continuation(self.offset, self.result_id, req, pinned)
    }
}
/// A nonzero offset continues one immutable result at one exact commit.
fn continuation(
    offset: usize,
    result_id: Option<Uuid>,
    req: &OntologyRequest,
    pinned: bool,
) -> Result<()> {
    if offset != 0
        && (result_id.is_none()
            || !(pinned || matches!(req.at, Some(crate::history::At::Commit(_)))))
    {
        return Err(Error::invalid(
            "a continuation requires resultId and at=commit:N",
        ));
    }
    Ok(())
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
            partial: !c.complete_for_taken,
            complete: c.complete && self.excluded.is_empty(),
            consistency: consistency(c),
            excluded_count: self.excluded.len(),
            incomplete_count: c.incomplete.len(),
            unmapped_count: c.stats.unmapped,
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
    /// The triples of the input that map to no axiom, each of which leaves the answer
    /// incomplete although no `excluded` entry names it
    pub unmapped_count: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityStatus {
    pub iri: String,
    pub present: bool,
    pub unsatisfiable: Option<bool>,
    pub incomplete: Option<bool>,
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
        query.validate_for(req, self.pinned.is_some())?;
        let (snapshot, opts) = self.state(req, opts)?;
        check(&opts)?;
        let (mut reply, selected) =
            self.kept_select(req, &snapshot, &opts, query.result_id, query.el_only);
        let Some(p) = selected else {
            return Ok(reply);
        };
        if let Some(iri) = &query.about {
            reply.entity = Some(entity_status(&p, iri));
        }
        if query.part != Part::Status {
            reply.page = Some(Self::kept_page(req, query, &snapshot, &p, &opts)?);
        }
        check(&opts)?;
        Ok(reply)
    }
    /// The kept result that a read of `req` at `snapshot` selects, with the reply that
    /// describes the selection. Only pointers are cloned under the slot lock, and a
    /// restricted view or a synthetic snapshot never observes the shared slot.
    pub(super) fn kept_select(
        &self,
        req: &OntologyRequest,
        snapshot: &Arc<Snapshot>,
        opts: &QueryOptions,
        result_id: Option<Uuid>,
        el_only: bool,
    ) -> (KeptRead, Option<Arc<KeptClassification>>) {
        let mut reply = KeptRead::unavailable(snapshot.commit, "notKept");
        if opts.graphs.is_some() || !snapshot.cacheable {
            reply.reason = Some("unsupportedScope");
            return (reply, None);
        }
        let key = InputKey::of(self, req, snapshot);
        // Only clone pointers under the slot lock. Keys, names, counts and all
        // fingerprint/estimate work are inspected outside it.
        let (publication, active) = {
            let cache = self.ds.state().classify_cache.lock();
            let slot = cache.slot(el_only);
            (slot.publication.clone(), slot.active.clone())
        };
        let publication =
            publication.filter(|p| p.source.input == key && p.source.el_only == el_only);
        let active =
            active.filter(|a| a.key == key && a.el_only == el_only && a.commit == snapshot.commit);
        let selected = if let Some(id) = result_id {
            match publication {
                Some(p) if p.id == id && p.source.commit == snapshot.commit => Some(p),
                Some(p) => {
                    reply.status = Status::Mismatch;
                    reply.reason = Some(if p.id == id {
                        "commitMismatch"
                    } else {
                        "resultChanged"
                    });
                    return (reply, None);
                }
                None => return (reply, None),
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
            return (reply, None);
        } else if let Some(p) = publication {
            reply.status = Status::Mismatch;
            reply.reason = Some("commitMismatch");
            reply.stale_commit = Some(p.source.commit);
            return (reply, None);
        } else {
            None
        };
        let Some(p) = selected else {
            return (reply, None);
        };
        reply.status = Status::Available;
        reply.reason = None;
        reply.result = Some(p.metadata());
        reply.outcome = Some(p.outcome());
        reply.activity = active.map(|a| Activity {
            kind: a.kind,
            commit: a.commit,
        });
        (reply, Some(p))
    }
    /// What the kept classification says about one named class (item 588): its status
    /// and the first pages of its direct parents, direct children and equivalent named
    /// classes, with the selection's exact source and outcome. A miss classifies
    /// nothing and maps nothing, and answers the selection's status with no pages.
    pub fn kept_entity(
        &self,
        req: &OntologyRequest,
        query: &KeptEntityQuery,
        opts: &QueryOptions,
    ) -> Result<KeptEntityRead> {
        query.validate(req)?;
        let (snapshot, opts) = self.state(req, opts)?;
        self.kept_entity_at(req, &snapshot, query, &opts)
    }
    /// [`kept_entity`](Self::kept_entity) at the snapshot and caller options of an
    /// opened ontology, so that a read of the asserted entity and of its classification
    /// share one commit and one deadline.
    pub fn kept_entity_at_opened(
        &self,
        req: &OntologyRequest,
        o: &Opened,
        query: &KeptEntityQuery,
        opts: &QueryOptions,
    ) -> Result<KeptEntityRead> {
        query.validate(req)?;
        self.check_pinned_opened(o, req)?;
        self.kept_entity_at(req, &o.snapshot, query, opts)
    }
    fn kept_entity_at(
        &self,
        req: &OntologyRequest,
        snapshot: &Arc<Snapshot>,
        query: &KeptEntityQuery,
        opts: &QueryOptions,
    ) -> Result<KeptEntityRead> {
        check(opts)?;
        let (mut read, selected) =
            self.kept_select(req, snapshot, opts, query.result_id, query.el_only);
        let mut answer = KeptEntityRead {
            read: KeptRead::unavailable(snapshot.commit, "notKept"),
            parents: None,
            children: None,
            equivalents: None,
        };
        if let Some(p) = selected {
            let status = entity_status(&p, &query.iri);
            if status.present {
                let mut page = KeptQuery {
                    part: Part::Parents,
                    about: Some(query.iri.clone()),
                    offset: 0,
                    limit: query.limit,
                    result_id: Some(p.id),
                    languages: query.languages.clone(),
                    el_only: query.el_only,
                };
                answer.parents = Some(Self::kept_page(req, &page, snapshot, &p, opts)?);
                page.part = Part::Children;
                answer.children = Some(Self::kept_page(req, &page, snapshot, &p, opts)?);
                page.part = Part::Equivalents;
                answer.equivalents = Some(Self::kept_page(req, &page, snapshot, &p, opts)?);
            }
            read.entity = Some(status);
        }
        answer.read = read;
        check(opts)?;
        Ok(answer)
    }
    /// A page of the axioms that the kept classification infers (item 588): an
    /// `EquivalentClasses` axiom for each group of several equivalent classes and a
    /// `SubClassOf` axiom from the first class of each group to the first class of each
    /// direct parent group, in the order of the taxonomy's groups, as a classification
    /// written with `format=ofn` lists them. The page walks the retained taxonomy and
    /// builds only its own rows, whose estimated bytes it charges to the memory budget.
    /// A miss classifies nothing and maps nothing, and answers no axioms.
    pub fn kept_axioms(
        &self,
        req: &OntologyRequest,
        query: &KeptAxiomQuery,
        opts: &QueryOptions,
    ) -> Result<KeptAxioms> {
        query.validate_for(req, self.pinned.is_some())?;
        let (snapshot, opts) = self.state(req, opts)?;
        check(&opts)?;
        let (read, selected) =
            self.kept_select(req, &snapshot, &opts, query.result_id, query.el_only);
        let mut answer = KeptAxioms {
            read,
            axioms: None,
            total: None,
            offset: query.offset,
            limit: query.limit,
            next_offset: None,
            _charge: None,
        };
        let Some(p) = selected else {
            return Ok(answer);
        };
        // an absent class reads as `present: false`, not as an empty inference
        if let Some(iri) = &query.about {
            answer.read.entity = Some(entity_status(&p, iri));
        }
        let t = &p.classification.taxonomy;
        let wants =
            |ty: sparkles_owl::AxiomType| query.types.as_ref().is_none_or(|s| s.contains(&ty));
        let (eq, sub) = (
            wants(sparkles_owl::AxiomType::EquivalentClasses),
            wants(sparkles_owl::AxiomType::SubClassOf),
        );
        // the groups whose axioms can mention `about`, in the taxonomy's order
        let about = query.about.as_deref().map(sparkles_owl::Class::new);
        let about_node = about.as_ref().and_then(|c| t.node_of(c));
        // a subclass axiom names only the first class of each group
        let about_is_rep = matches!((&about, about_node), (Some(c), Some(n)) if t.node(n).classes.first() == Some(c));
        let end = query.offset.saturating_add(query.limit);
        let mut total = 0usize;
        let mut items: Vec<sparkles_owl::Axiom> = Vec::new();
        let mut charged = 0u64;
        let charge = Charge::new(&opts);
        let mut work = 0usize;
        // one group's axioms, polling the request's controls inside large groups
        let mut visit = |i: usize| -> Result<()> {
            poll(&opts, &mut work)?;
            let n = t.node(i);
            let own = about_node.is_none_or(|a| a == i);
            if eq && own && n.classes.len() > 1 {
                if total >= query.offset && total < end {
                    let mut bytes = 64u64;
                    for c in &n.classes {
                        poll(&opts, &mut work)?;
                        bytes = bytes.saturating_add(class_bytes(c));
                    }
                    charged += charge.take(bytes)?;
                    let mut members = Vec::with_capacity(n.classes.len());
                    for c in &n.classes {
                        poll(&opts, &mut work)?;
                        members.push(sparkles_owl::ClassExpression::Class(c.clone()));
                    }
                    items.push(sparkles_owl::Axiom::equivalent_classes(members));
                }
                total += 1;
            }
            // node 1 is the bottom group, below every class, with no parents listed
            if !sub || i == 1 {
                return Ok(());
            }
            let Some(rep) = n.classes.first() else {
                return Ok(());
            };
            for &parent in &n.parents {
                poll(&opts, &mut work)?;
                let mentions = about_node.is_none()
                    || (about_is_rep
                        && (i == about_node.unwrap_or(usize::MAX) || Some(parent) == about_node));
                if !mentions {
                    continue;
                }
                let Some(q) = t.node(parent).classes.first() else {
                    continue;
                };
                if total >= query.offset && total < end {
                    charged += charge.take(64 + class_bytes(rep) + class_bytes(q))?;
                    items.push(sparkles_owl::Axiom::sub_class_of(rep.clone(), q.clone()));
                }
                total += 1;
            }
            Ok(())
        };
        match about_node {
            // an absent class is in no axiom
            None if about.is_some() => {}
            // every group, in the taxonomy's order, without copying the node list
            None => {
                for i in 0..t.nodes().len() {
                    visit(i)?;
                }
            }
            // the class's own group, and for a first class the groups of its direct
            // children, which name it as their parent, merged in the taxonomy's order.
            // The children are in ascending node order, so the merge reads them in
            // place, and each visit polls the request's controls.
            Some(n) if about_is_rep => {
                let mut own = Some(n);
                let mut rest = t.node(n).children.iter().copied().peekable();
                loop {
                    let next = match (own, rest.peek().copied()) {
                        (Some(a), Some(b)) if b < a => rest.next(),
                        (Some(_), _) => own.take(),
                        (None, _) => rest.next(),
                    };
                    let Some(i) = next else { break };
                    visit(i)?;
                }
            }
            Some(n) => visit(n)?,
        }
        answer.axioms = Some(items);
        answer.total = Some(total);
        answer.next_offset = (end < total).then_some(end);
        answer._charge = charge.hold(charged);
        check(&opts)?;
        Ok(answer)
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

/// The status of one named class in a kept result.
fn entity_status(p: &KeptClassification, iri: &str) -> EntityStatus {
    let class = sparkles_owl::Class::new(iri);
    let taxonomy = &p.classification.taxonomy;
    let node = taxonomy.node_of(&class).map(|id| taxonomy.node(id));
    let present = node.is_some();
    EntityStatus {
        iri: iri.to_string(),
        present,
        unsatisfiable: present.then(|| taxonomy.is_unsatisfiable(&class)),
        incomplete: present.then(|| p.classification.incomplete.binary_search(&class).is_ok()),
        representative: node
            .and_then(|node| node.classes.first())
            .map(|class| class.as_str().to_string()),
        equivalence_count: node.map(|node| node.classes.len()),
        children: node.map(|node| node.children.len()),
        parents: node.map(|node| node.parents.len()),
    }
}

/// A read of one named class from the kept classification
/// ([`Ontologies::kept_entity`]).
#[derive(Clone, Debug)]
pub struct KeptEntityQuery {
    /// the absolute IRI of the named class
    pub iri: String,
    /// the most rows of each page, from 1 to 1,000
    pub limit: usize,
    /// the immutable result to read, which a miss refuses instead of switching results
    pub result_id: Option<Uuid>,
    /// the preferred label languages, most preferred first
    pub languages: Vec<String>,
    /// read the kept EL-only classification instead of the default classification
    pub el_only: bool,
}
impl KeptEntityQuery {
    pub fn new(iri: impl Into<String>) -> Self {
        Self {
            iri: iri.into(),
            limit: 100,
            result_id: None,
            languages: Vec::new(),
            el_only: false,
        }
    }
    pub fn validate(&self, req: &OntologyRequest) -> Result<()> {
        KeptQuery {
            part: Part::Status,
            about: Some(self.iri.clone()),
            offset: 0,
            limit: self.limit,
            result_id: self.result_id,
            languages: Vec::new(),
            el_only: self.el_only,
        }
        .validate(req)
    }
}
/// What [`Ontologies::kept_entity`] answers: the selection's status, source, outcome
/// and class status in `read`, and the first page of each relation of a present class.
#[derive(Clone, Debug)]
pub struct KeptEntityRead {
    pub read: KeptRead,
    pub parents: Option<Page>,
    pub children: Option<Page>,
    pub equivalents: Option<Page>,
}
/// A listing of the axioms a kept classification infers ([`Ontologies::kept_axioms`]).
#[derive(Clone, Debug)]
pub struct KeptAxiomQuery {
    /// keep only these axiom types, of which `SubClassOf` and `EquivalentClasses` occur
    pub types: Option<std::collections::BTreeSet<sparkles_owl::AxiomType>>,
    /// keep only the axioms that mention this named class
    pub about: Option<String>,
    pub offset: usize,
    /// the most axioms of the page, from 1 to 1,000
    pub limit: usize,
    /// the immutable result to read, required with `at=commit:N` for a nonzero offset
    pub result_id: Option<Uuid>,
    /// read the kept EL-only classification instead of the default classification
    pub el_only: bool,
}
impl Default for KeptAxiomQuery {
    fn default() -> Self {
        Self {
            types: None,
            about: None,
            offset: 0,
            limit: 100,
            result_id: None,
            el_only: false,
        }
    }
}
impl KeptAxiomQuery {
    pub fn validate(&self, req: &OntologyRequest) -> Result<()> {
        self.validate_for(req, false)
    }
    fn validate_for(&self, req: &OntologyRequest, pinned: bool) -> Result<()> {
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
        continuation(self.offset, self.result_id, req, pinned)
    }
}
/// What [`Ontologies::kept_axioms`] answers. `axioms` and `total` are `None` unless
/// `read.status` is available, so a miss never reads as an empty inference, and
/// `read.entity` gives the status of an `about` class in an available result.
#[derive(Debug)]
pub struct KeptAxioms {
    pub read: KeptRead,
    pub axioms: Option<Vec<sparkles_owl::Axiom>>,
    pub total: Option<usize>,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
    /// the page's bytes, charged to the memory pool's grant while the answer lives
    _charge: Option<super::dl_query::Charged>,
}
/// The estimated bytes of one class of an axiom, for the memory budget.
fn class_bytes(c: &sparkles_owl::Class) -> u64 {
    c.as_str().len() as u64 + 64
}

/// How many steps of a kept axiom scan run between polls of the request's controls.
const POLL_EVERY: usize = 1024;

/// Count one step of a kept axiom scan and poll the request's controls every
/// [`POLL_EVERY`] steps, so that a large group cannot outlast a cancellation.
/// Check the request's controls once every [`POLL_EVERY`] steps of a long read.
pub(super) fn poll(opts: &QueryOptions, work: &mut usize) -> Result<()> {
    #[cfg(test)]
    SCAN_STEPS.with(|n| {
        let steps = n.get() + 1;
        n.set(steps);
        if CANCEL_AT_STEP.with(|c| c.get()) == Some(steps)
            && let Some(flag) = &opts.cancel
        {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    if (*work).is_multiple_of(POLL_EVERY) {
        check(opts)?;
    }
    *work += 1;
    Ok(())
}
#[cfg(test)]
thread_local! {
    /// the steps of kept axiom scans on this thread
    static SCAN_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// the step at which a test's scan raises its cancellation flag
    static CANCEL_AT_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
/// The memory charge of a page of kept axioms: the request's own memory limit and the
/// server pool's grant, asked before each row is built.
struct Charge {
    grant: Option<Arc<sparkles_core::sparql::pool::PoolGrant>>,
    limit: Option<u64>,
    used: std::cell::Cell<u64>,
}
impl Charge {
    fn new(opts: &QueryOptions) -> Self {
        Self {
            grant: opts.grant.clone(),
            limit: opts.max_memory_bytes,
            used: std::cell::Cell::new(0),
        }
    }
    fn take(&self, bytes: u64) -> Result<u64> {
        let used = self.used.get().saturating_add(bytes);
        if let Some(limit) = self.limit
            && used > limit
        {
            self.release();
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Memory,
                limit,
                requested: used,
            }));
        }
        // the pool's own refusal says whether a retry can pass: more than the whole
        // pool, or memory that other requests hold for now
        if let Some(g) = &self.grant
            && let Err(err) = g.charge(bytes)
        {
            self.release();
            return Err(err);
        }
        self.used.set(used);
        Ok(bytes)
    }
    fn release(&self) {
        if let Some(g) = &self.grant {
            g.release(self.used.get());
        }
        self.used.set(0);
    }
    /// Hand the page's charge to the answer, which releases it when it drops.
    fn hold(self, bytes: u64) -> Option<super::dl_query::Charged> {
        debug_assert_eq!(bytes, self.used.get());
        let grant = self.grant.clone()?;
        self.used.set(0);
        Some(super::dl_query::Charged(grant, bytes))
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.release();
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
