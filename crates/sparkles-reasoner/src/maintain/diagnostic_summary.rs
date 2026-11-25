//! Controlled reads of exact generalized typings from the selected materialization.

use super::*;
use crate::diagnostics::{DiagnosticControls, GuardedLiteralTypings, check_control};
use std::io::Read as _;
use std::mem::size_of;

fn check(controls: &DiagnosticControls) -> anyhow::Result<()> {
    check_control(controls.deadline, controls.cancel.as_deref())?;
    Ok(())
}

fn fact_bytes(literal: &Literal, datatype: &NamedNode) -> usize {
    256 + literal.value().len() * 2
        + literal.datatype().as_str().len() * 2
        + literal.language().map_or(0, str::len) * 2
        + datatype.as_str().len() * 2
}

struct CachedData {
    candidates: Vec<Candidate>,
    local_keys: FxHashMap<u64, Vec<u8>>,
    datatypes: Option<Datatypes>,
    snap: Arc<Snapshot>,
    // Declared last so `candidates`, `local_keys`, and `snap` drop before credit returns
    // on both normal and early-error exits.
    _held: Held,
}

impl CachedData {
    fn term(&self, raw: u64) -> Option<Term> {
        if Id(raw).tag() == sparkles_core::id::Tag::Local {
            self.local_keys
                .get(&raw)
                .map(|k| sparkles_core::id::key_to_term(k))
        } else {
            self.snap.term(Id(raw))
        }
    }

    fn term_controlled(
        &self,
        raw: u64,
        budget: &Arc<Budget>,
    ) -> Result<(Held, Option<Term>), BudgetFailure> {
        if Id(raw).tag() == sparkles_core::id::Tag::Local {
            if let Some(key) = self.local_keys.get(&raw) {
                let (term, held) = crate::terms::decode_key_controlled(key, budget)?;
                return Ok((held, Some(term)));
            }
            return Ok((Held::new(budget), None));
        }
        let (term, held) = crate::terms::decode_store_term_controlled(&self.snap, raw, budget)?;
        Ok((held, term))
    }
}

enum CachedSnapshot {
    Empty,
    Data(CachedData),
    Missing,
}

/// Resolve a vocabulary key in `snap` outside `cache.kept.lock()`, admitting any base
/// vocabulary reconstruction scratch to `budget` and polling `controls`.
fn lookup_key_controlled_in_snap(
    snap: &Snapshot,
    key: &[u8],
    budget: &Arc<Budget>,
    controls: &DiagnosticControls,
) -> anyhow::Result<Option<Id>> {
    let scratch = std::cell::RefCell::new(Held::new(budget));
    snap.lookup_key_controlled(
        key,
        |old, new| -> anyhow::Result<()> {
            if let Err(err) = scratch.borrow_mut().try_set(old.saturating_add(new)) {
                check(controls)?;
                return Err(err.into());
            }
            Ok(())
        },
        |capacity| -> anyhow::Result<()> {
            if let Err(err) = scratch.borrow_mut().try_set(capacity) {
                check(controls)?;
                return Err(err.into());
            }
            Ok(())
        },
        || check(controls),
    )
}

// The lexical key encoding for an IRI is '<' followed by its UTF-8 bytes.
const RDF_TYPE_KEY: &[u8] = b"<http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

type Candidate = (u64, u64);
type Datatypes = FxHashMap<u64, Term>;

fn visit_candidates(
    kept: &Kept,
    ty_id: Option<Id>,
    controls: &DiagnosticControls,
    datatypes: Option<&Datatypes>,
    mut visit: impl FnMut(Candidate),
) -> anyhow::Result<()> {
    use crate::terms::Kind;
    use sparkles_core::id::Tag;

    let terms = &kept.closure.terms;
    for (index, &[subject, predicate, object]) in kept.generalized.iter().enumerate() {
        if index.is_multiple_of(1024) {
            check(controls)?;
        }
        let is_type = if Id(predicate).tag() == Tag::Local {
            terms.local_key(predicate) == Some(RDF_TYPE_KEY)
        } else {
            ty_id == Some(Id(predicate))
        };
        if is_type
            && terms.kind(subject) == Kind::Literal
            && terms.kind(object) == Kind::Iri
            && datatypes.is_none_or(|datatypes| datatypes.contains_key(&object))
        {
            visit((subject, object));
        }
    }
    Ok(())
}

fn local_keys(
    terms: &Terms,
    (subject, object): Candidate,
    include_objects: bool,
) -> impl Iterator<Item = (u64, &[u8])> {
    std::iter::once(subject)
        .chain(include_objects.then_some(object))
        .filter_map(|id| terms.local_key(id).map(|key| (id, key)))
}

/// Copy each supported local datatype key once. Resolve store objects outside
/// the lock before deciding which subjects to copy for the public reader.
fn supported_datatypes(
    cache: &Cache,
    generation: u64,
    snap: &Snapshot,
    ty_id: Option<Id>,
    controls: &DiagnosticControls,
) -> anyhow::Result<Option<Datatypes>> {
    let mut objects = FxHashMap::default();
    {
        let slot = cache.kept.lock();
        let Some(kept) = slot.kept.as_ref().filter(|_| slot.generation == generation) else {
            return Ok(None);
        };
        visit_candidates(kept, ty_id, controls, None, |(_, object)| {
            let key = kept.closure.terms.local_key(object);
            if key.is_none_or(crate::diagnostics::decidable_range_key) {
                objects
                    .entry(object)
                    .or_insert_with(|| key.map(<[u8]>::to_vec));
            }
        })?;
    }
    let datatypes = objects
        .into_iter()
        .filter_map(|(object, key)| {
            let term = match key {
                Some(key) => sparkles_core::id::key_to_term(&key),
                None => snap.term(Id(object))?,
            };
            matches!(&term, Term::NamedNode(n) if crate::diagnostics::decidable_range(n.as_str()))
                .then_some((object, term))
        })
        .collect();
    Ok(Some(datatypes))
}

#[derive(Default)]
struct KeySize {
    count: usize,
    bytes: usize,
}

impl KeySize {
    fn add(&mut self, key: &[u8]) {
        self.count += 1;
        self.bytes = self.bytes.saturating_add(key.len());
    }

    fn allocation_bytes(&self) -> usize {
        let map_bytes = if self.count == 0 {
            0
        } else {
            table_bytes(table_capacity(self.count), size_of::<(u64, Vec<u8>)>())
        };
        map_bytes.saturating_add(self.bytes)
    }
}

/// Copy candidate ids and local keys from one cache generation. Vocabulary lookup and
/// budget callbacks run outside the cache lock. The controlled reader admits all
/// candidate storage before copying. The public reader resolves ranges before
/// selecting literal subjects.
fn snapshot_cached_typings(
    store: &Store,
    commit: u64,
    cache: &Cache,
    controls: &DiagnosticControls,
    budget: &Arc<Budget>,
    supported_ranges_only: bool,
) -> anyhow::Result<CachedSnapshot> {
    let mut resolved = None;
    let matches_commit =
        |kept: &&Kept| kept.dataset_id == store.dataset_id() && kept.commit == commit;
    loop {
        check(controls)?;
        let (generation, snap) = {
            let slot = cache.kept.lock();
            let Some(kept) = slot.kept.as_ref().filter(matches_commit) else {
                return Ok(CachedSnapshot::Missing);
            };
            if kept.generalized.is_empty() {
                return Ok(CachedSnapshot::Empty);
            }
            (slot.generation, Arc::clone(&kept.closure.terms.snap))
        };
        if !resolved
            .as_ref()
            .is_some_and(|(previous, _)| Arc::ptr_eq(previous, &snap))
        {
            let ty_id = lookup_key_controlled_in_snap(&snap, RDF_TYPE_KEY, budget, controls)?;
            resolved = Some((Arc::clone(&snap), ty_id));
        }
        let ty_id = resolved.as_ref().and_then(|(_, id)| *id);
        let current = |slot: &CacheSlot| slot.generation == generation;
        let datatypes = if supported_ranges_only {
            let Some(datatypes) = supported_datatypes(cache, generation, &snap, ty_id, controls)?
            else {
                continue;
            };
            Some(datatypes)
        } else {
            None
        };
        let include_objects = datatypes.is_none();
        let mut candidate_count = 0usize;
        let mut key_size = KeySize::default();
        let mut seen = [0; 64];
        let mut overflowed = false;
        let mut local_refs = 0usize;
        let mut max_payload = 0;
        {
            let slot = cache.kept.lock();
            let Some(kept) = slot.kept.as_ref().filter(|_| current(&slot)) else {
                continue;
            };
            if datatypes.as_ref().is_some_and(Datatypes::is_empty) {
                return Ok(CachedSnapshot::Empty);
            }
            visit_candidates(kept, ty_id, controls, datatypes.as_ref(), |candidate| {
                candidate_count += 1;
                for (id, key) in local_keys(&kept.closure.terms, candidate, include_objects) {
                    local_refs += 1;
                    max_payload = max_payload.max(Id(id).payload() as usize);
                    if !overflowed && !seen[..key_size.count].contains(&id) {
                        if let Some(slot) = seen.get_mut(key_size.count) {
                            *slot = id;
                            key_size.add(key);
                        } else {
                            overflowed = true;
                        }
                    }
                }
            })?;
        }
        if candidate_count == 0 {
            return Ok(CachedSnapshot::Empty);
        }

        // A full stack table might have missed later ids. Use the smaller of a bitset
        // indexed by local id and a list of references, with either allocated before locking.
        if overflowed {
            key_size = KeySize::default();
            let words = max_payload.saturating_add(1).div_ceil(u64::BITS as usize);
            if words <= local_refs {
                let _scratch = Held::try_new(budget, words.saturating_mul(size_of::<u64>()))?;
                let mut seen = vec![0u64; words];
                let slot = cache.kept.lock();
                let Some(kept) = slot.kept.as_ref().filter(|_| current(&slot)) else {
                    continue;
                };
                visit_candidates(kept, ty_id, controls, datatypes.as_ref(), |candidate| {
                    for (id, key) in local_keys(&kept.closure.terms, candidate, include_objects) {
                        let payload = Id(id).payload() as usize;
                        let word = &mut seen[payload / u64::BITS as usize];
                        let mask = 1u64 << (payload % u64::BITS as usize);
                        if *word & mask == 0 {
                            *word |= mask;
                            key_size.add(key);
                        }
                    }
                })?;
            } else {
                let _scratch = Held::try_new(budget, local_refs.saturating_mul(size_of::<u64>()))?;
                let mut ids = Vec::with_capacity(local_refs);
                {
                    let slot = cache.kept.lock();
                    let Some(kept) = slot.kept.as_ref().filter(|_| current(&slot)) else {
                        continue;
                    };
                    visit_candidates(kept, ty_id, controls, datatypes.as_ref(), |candidate| {
                        ids.extend(
                            local_keys(&kept.closure.terms, candidate, include_objects)
                                .map(|(id, _)| id),
                        );
                    })?;
                }
                ids.sort_unstable();
                ids.dedup();
                let slot = cache.kept.lock();
                let Some(kept) = slot.kept.as_ref().filter(|_| current(&slot)) else {
                    continue;
                };
                for (index, id) in ids.into_iter().enumerate() {
                    if index.is_multiple_of(1024) {
                        check(controls)?;
                    }
                    if let Some(key) = kept.closure.terms.local_key(id) {
                        key_size.add(key);
                    }
                }
            }
        }

        let bytes = candidate_count
            .saturating_mul(size_of::<Candidate>())
            .saturating_add(key_size.allocation_bytes());
        let held = Held::try_new(budget, bytes)?;
        let mut data = CachedData {
            candidates: Vec::with_capacity(candidate_count),
            local_keys: FxHashMap::with_capacity_and_hasher(key_size.count, Default::default()),
            datatypes,
            snap,
            _held: held,
        };
        {
            let slot = cache.kept.lock();
            let Some(kept) = slot.kept.as_ref().filter(|_| current(&slot)) else {
                continue;
            };
            visit_candidates(
                kept,
                ty_id,
                controls,
                data.datatypes.as_ref(),
                |candidate| {
                    data.candidates.push(candidate);
                    for (id, key) in local_keys(&kept.closure.terms, candidate, include_objects) {
                        data.local_keys.entry(id).or_insert_with(|| key.to_vec());
                    }
                },
            )?;
        }
        return Ok(CachedSnapshot::Data(data));
    }
}

/// Read the in-memory summary for the public reader without holding the cache lock
/// across any term decoding or datatype validation.
pub(super) fn literal_typings_cached(
    store: &Store,
    commit: u64,
    cache: &Cache,
) -> anyhow::Result<Option<LiteralTypings>> {
    let mut data = match snapshot_cached_typings(
        store,
        commit,
        cache,
        &DiagnosticControls::default(),
        &Budget::unlimited(),
        true,
    )? {
        CachedSnapshot::Missing => return Ok(None),
        CachedSnapshot::Empty => {
            return Ok(Some(LiteralTypings {
                commit,
                facts: Vec::new(),
            }));
        }
        CachedSnapshot::Data(data) => data,
    };
    let predicate = Term::NamedNode(rdf::TYPE.into_owned());
    let mut facts = Vec::new();
    let datatypes = data
        .datatypes
        .as_ref()
        .expect("public snapshot resolves ranges");
    data.candidates.sort_unstable();
    for &(subject, object) in &data.candidates {
        if let Some(subject) = data.term(subject) {
            facts.extend(LiteralTypings::violation(
                &subject,
                &predicate,
                &datatypes[&object],
            ));
        }
    }
    Ok(Some(LiteralTypings { commit, facts }))
}

/// Read actual materialized terms under a separate diagnostic memory share. Missing
/// or inconsistent optional state returns None, never a recomputed volatile summary.
pub fn literal_typings_controlled(
    store: &Store,
    commit: u64,
    cache: Option<&Cache>,
    controls: &DiagnosticControls,
) -> anyhow::Result<Option<LiteralTypings>> {
    Ok(
        literal_typings_controlled_guarded(store, commit, cache, controls)?
            .map(GuardedLiteralTypings::into_typings),
    )
}

/// [`literal_typings_controlled`], returning a [`GuardedLiteralTypings`] that keeps
/// pooled summary credit charged until the caller drops the summary.
pub fn literal_typings_controlled_guarded(
    store: &Store,
    commit: u64,
    cache: Option<&Cache>,
    controls: &DiagnosticControls,
) -> anyhow::Result<Option<GuardedLiteralTypings>> {
    check(controls)?;
    let budget = Budget::with_parent(
        controls
            .max_memory
            .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX)),
        controls.memory_parent.clone(),
    );
    if let Some(cache) = cache {
        match snapshot_cached_typings(store, commit, cache, controls, &budget, false)? {
            CachedSnapshot::Empty => {
                return Ok(Some(GuardedLiteralTypings::new(
                    LiteralTypings {
                        commit,
                        facts: Vec::new(),
                    },
                    None,
                )));
            }
            CachedSnapshot::Data(data) => {
                let mut found_held = Held::new(&budget);
                let mut found = Vec::new();
                let predicate = Term::NamedNode(rdf::TYPE.into_owned());
                for &(subject_id, object_id) in &data.candidates {
                    check(controls)?;
                    // Bind each guard before its term so error exits release the term first.
                    let (_subject_memory, subject) = data.term_controlled(subject_id, &budget)?;
                    let (_object_memory, object) = data.term_controlled(object_id, &budget)?;
                    if let (Some(subject), Some(object)) = (subject, object)
                        && let Some((literal, datatype)) =
                            LiteralTypings::violation(&subject, &predicate, &object)
                    {
                        found_held.try_grow(fact_bytes(&literal, &datatype))?;
                        found.push((literal, datatype));
                    }
                }
                drop(data);
                let summary_held = (!found.is_empty()).then_some(found_held);
                return Ok(Some(GuardedLiteralTypings::new(
                    LiteralTypings {
                        commit,
                        facts: found,
                    },
                    summary_held,
                )));
            }
            CachedSnapshot::Missing => {}
        }
    }
    let Some(root) = store.root() else {
        return Ok(None);
    };
    if let Some(limit) = controls.max_memory
        && std::fs::metadata(root.join(SAVED_META))
            .ok()
            .is_some_and(|meta| meta.len() > limit)
    {
        return Err(BudgetFailure::local(
            limit.try_into().unwrap_or(usize::MAX),
            " while reading generalized diagnostic metadata".into(),
        )
        .into());
    }
    let Some(meta) = saved_meta(root, store.dataset_id(), commit) else {
        return Ok(None);
    };
    // Two bounded file buffers and map bookkeeping overlap while replaying the log.
    let _buf_held = Held::try_new(&budget, 32 * 1024)?;
    let ty_term = Term::NamedNode(rdf::TYPE.into_owned());
    let mut found: FxHashMap<Keys, ((Literal, NamedNode), Held)> = FxHashMap::default();
    let consistent = scan_controlled(root, &meta, controls, &budget, |added, keys, mut held| {
        check(controls)?;
        if !added {
            found.remove(&keys);
        } else if keys[1] == RDF_TYPE_KEY && keys[0].first() == Some(&b'"') {
            let _scratch = Held::try_new(
                &budget,
                keys[0]
                    .len()
                    .saturating_add(keys[2].len())
                    .saturating_mul(8)
                    .saturating_add(512),
            )?;
            let subject = sparkles_core::id::key_to_term(&keys[0]);
            let object = sparkles_core::id::key_to_term(&keys[2]);
            if let Some((literal, datatype)) =
                LiteralTypings::violation(&subject, &ty_term, &object)
            {
                let extra = fact_bytes(&literal, &datatype);
                held.try_grow(extra)?;
                found.insert(keys, ((literal, datatype), held));
                return Ok(());
            }
        }
        Ok(())
    })?;
    if !consistent {
        return Ok(None);
    }
    if found.is_empty() {
        return Ok(Some(GuardedLiteralTypings::new(
            LiteralTypings {
                commit,
                facts: Vec::new(),
            },
            None,
        )));
    }
    let mut summary_held = Held::new(&budget);
    summary_held.try_grow(
        found
            .len()
            .saturating_mul(std::mem::size_of::<(Literal, NamedNode)>()),
    )?;
    let mut facts = Vec::with_capacity(found.len());
    for ((literal, datatype), mut entry_held) in found.into_values() {
        let mut fact_held = entry_held.split(fact_bytes(&literal, &datatype));
        summary_held.try_merge(&mut fact_held)?;
        facts.push((literal, datatype));
        drop(entry_held);
    }
    drop(_buf_held);
    Ok(Some(GuardedLiteralTypings::new(
        LiteralTypings { commit, facts },
        Some(summary_held),
    )))
}

fn keys<R: std::io::Read>(
    reader: &mut Hashing<R>,
    end: u64,
    controls: &DiagnosticControls,
    budget: &Arc<Budget>,
) -> anyhow::Result<Option<(Keys, Held)>> {
    let mut held = Held::new(budget);
    let mut triple: Keys = Default::default();
    for key in &mut triple {
        check(controls)?;
        let mut length = [0u8; 4];
        if reader.read_exact(&mut length).is_err() {
            return Ok(None);
        }
        let length = u32::from_le_bytes(length) as u64;
        if length > end.saturating_sub(reader.n) {
            return Ok(None);
        }
        let length = length as usize;
        let charge = length.max(8);
        held.try_grow(charge)?;
        key.resize(length, 0);
        if reader.read_exact(key).is_err() {
            return Ok(None);
        }
    }
    Ok(Some((triple, held)))
}

fn scan_controlled(
    root: &Path,
    meta: &SavedMeta,
    controls: &DiagnosticControls,
    budget: &Arc<Budget>,
    mut visit: impl FnMut(bool, Keys, Held) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let Ok(file) = std::fs::File::open(root.join(SAVED_BIN)) else {
        return Ok(false);
    };
    if file.metadata().map(|meta| meta.len()).ok() != Some(meta.bytes) {
        return Ok(false);
    }
    let mut reader = Hashing::new(std::io::BufReader::new(file.take(meta.bytes)));
    let mut head = [0u8; 16];
    if reader.read_exact(&mut head).is_err()
        || &head[..8] != MAGIC
        || u64::from_le_bytes(head[8..16].try_into().expect("eight bytes")) != meta.triples
    {
        return Ok(false);
    }
    for _ in 0..meta.triples {
        let Some((keys, held)) = keys(&mut reader, meta.bytes, controls, budget)? else {
            return Ok(false);
        };
        visit(true, keys, held)?;
    }
    let mut rest = [0u8; 1];
    if !matches!(reader.read(&mut rest), Ok(0))
        || reader.n != meta.bytes
        || reader.checksum() != meta.checksum
    {
        return Ok(false);
    }
    if meta.log_bytes == 0 {
        return Ok(true);
    }
    let Ok(file) = std::fs::File::open(root.join(SAVED_LOG)) else {
        return Ok(false);
    };
    let mut reader = Hashing::new(std::io::BufReader::new(file.take(meta.log_bytes)));
    while reader.n < meta.log_bytes {
        check(controls)?;
        let mut operation = [0u8; 1];
        if reader.read_exact(&mut operation).is_err() {
            return Ok(false);
        }
        let Some((keys, held)) = keys(&mut reader, meta.log_bytes, controls, budget)? else {
            return Ok(false);
        };
        match operation[0] {
            b'+' => visit(true, keys, held)?,
            b'-' => visit(false, keys, held)?,
            _ => return Ok(false),
        }
    }
    Ok(reader.n == meta.log_bytes && reader.checksum() == meta.log_checksum)
}
