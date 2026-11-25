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

enum CachedSnapshot {
    Empty,
    Data {
        snap: Arc<Snapshot>,
        candidates: Vec<(u64, u64)>,
        local_keys: FxHashMap<u64, Vec<u8>>,
        // Declared last so `candidates` and `local_keys` drop before credit returns.
        _held: Held,
    },
    Missing,
}

/// Extract the candidate literal-typing ids and any needed `Tag::Local` keys from `cache`
/// without holding `cache.kept` across any `Budget` acquisition, release, or term decode.
fn snapshot_cached_typings(
    store: &Store,
    commit: u64,
    cache: &Cache,
    controls: &DiagnosticControls,
    budget: &Arc<Budget>,
) -> anyhow::Result<CachedSnapshot> {
    let ty_key = sparkles_core::id::iri_key(rdf::TYPE.as_str());
    loop {
        check(controls)?;
        // Phase 1: size candidates and unique `Tag::Local` keys under `cache.kept.lock()`
        // without allocating or touching `budget`.
        let sizing = {
            let slot = cache.kept.lock();
            match slot.kept.as_ref() {
                Some(kept) if kept.dataset_id == store.dataset_id() && kept.commit == commit => {
                    let terms = &kept.closure.terms;
                    let ty_id = terms.snap.lookup_iri(rdf::TYPE.as_str()).map(|i| i.0);
                    let mut count = 0usize;
                    let mut local_entries = 0usize;
                    let mut local_bytes = 0usize;
                    // Small stack cache to avoid over-counting repeated local datatype IRIs
                    // without heap allocation while the cache mutex is held.
                    let mut seen_obj_locals = [0u64; 32];
                    let mut seen_obj_len = 0usize;
                    for (idx, triple) in kept.generalized.iter().enumerate() {
                        if idx.is_multiple_of(1024) {
                            check(controls)?;
                        }
                        let is_type = if Id(triple[1]).tag() == sparkles_core::id::Tag::Local {
                            terms.local_key(triple[1]) == Some(ty_key.as_slice())
                        } else {
                            ty_id == Some(triple[1])
                        };
                        if !is_type
                            || terms.kind(triple[0]) != crate::terms::Kind::Literal
                            || terms.kind(triple[2]) != crate::terms::Kind::Iri
                        {
                            continue;
                        }
                        count += 1;
                        if Id(triple[0]).tag() == sparkles_core::id::Tag::Local
                            && let Some(key) = terms.local_key(triple[0])
                        {
                            local_entries += 1;
                            local_bytes = local_bytes.saturating_add(key.len());
                        }
                        if Id(triple[2]).tag() == sparkles_core::id::Tag::Local
                            && let Some(key) = terms.local_key(triple[2])
                            && !seen_obj_locals[..seen_obj_len].contains(&triple[2])
                        {
                            if seen_obj_len < seen_obj_locals.len() {
                                seen_obj_locals[seen_obj_len] = triple[2];
                                seen_obj_len += 1;
                            }
                            local_entries += 1;
                            local_bytes = local_bytes.saturating_add(key.len());
                        }
                    }
                    Some((slot.generation, count, local_entries, local_bytes))
                }
                _ => None,
            }
        };
        let Some((generation, count, local_entries, local_bytes)) = sizing else {
            return Ok(CachedSnapshot::Missing);
        };
        if count == 0 {
            return Ok(CachedSnapshot::Empty);
        }
        // Phase 2: pre-admit the snapshot buffers outside `cache.kept.lock()`.
        let map_bytes = if local_entries == 0 {
            0
        } else {
            table_bytes(table_capacity(local_entries), size_of::<(u64, Vec<u8>)>())
        };
        let need_bytes = count
            .saturating_mul(size_of::<(u64, u64)>())
            .saturating_add(map_bytes)
            .saturating_add(local_bytes);
        let held = Held::try_new(budget, need_bytes)?;
        // Phase 3: re-lock briefly to copy out raw ids and local keys while generation matches.
        enum CopyStep {
            Copied(Arc<Snapshot>, Vec<(u64, u64)>, FxHashMap<u64, Vec<u8>>),
            Retry,
            Missing,
        }
        let copied = {
            let slot = cache.kept.lock();
            match slot.kept.as_ref() {
                Some(kept)
                    if slot.generation == generation
                        && kept.dataset_id == store.dataset_id()
                        && kept.commit == commit =>
                {
                    let terms = &kept.closure.terms;
                    let ty_id = terms.snap.lookup_iri(rdf::TYPE.as_str()).map(|i| i.0);
                    let mut candidates = Vec::with_capacity(count);
                    let mut local_keys = FxHashMap::default();
                    if local_entries > 0 {
                        local_keys.reserve(local_entries);
                    }
                    for (idx, triple) in kept.generalized.iter().enumerate() {
                        if idx.is_multiple_of(1024) {
                            check(controls)?;
                        }
                        let is_type = if Id(triple[1]).tag() == sparkles_core::id::Tag::Local {
                            terms.local_key(triple[1]) == Some(ty_key.as_slice())
                        } else {
                            ty_id == Some(triple[1])
                        };
                        if !is_type
                            || terms.kind(triple[0]) != crate::terms::Kind::Literal
                            || terms.kind(triple[2]) != crate::terms::Kind::Iri
                        {
                            continue;
                        }
                        candidates.push((triple[0], triple[2]));
                        if Id(triple[0]).tag() == sparkles_core::id::Tag::Local
                            && !local_keys.contains_key(&triple[0])
                            && let Some(key) = terms.local_key(triple[0])
                        {
                            local_keys.insert(triple[0], key.to_vec());
                        }
                        if Id(triple[2]).tag() == sparkles_core::id::Tag::Local
                            && !local_keys.contains_key(&triple[2])
                            && let Some(key) = terms.local_key(triple[2])
                        {
                            local_keys.insert(triple[2], key.to_vec());
                        }
                    }
                    CopyStep::Copied(terms.snap.clone(), candidates, local_keys)
                }
                Some(kept) if kept.dataset_id == store.dataset_id() && kept.commit == commit => {
                    CopyStep::Retry
                }
                _ => CopyStep::Missing,
            }
        };
        match copied {
            CopyStep::Copied(snap, candidates, local_keys) => {
                return Ok(CachedSnapshot::Data {
                    snap,
                    candidates,
                    local_keys,
                    _held: held,
                });
            }
            CopyStep::Retry => {
                drop(held);
            }
            CopyStep::Missing => {
                drop(held);
                return Ok(CachedSnapshot::Missing);
            }
        }
    }
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
        match snapshot_cached_typings(store, commit, cache, controls, &budget)? {
            CachedSnapshot::Empty => {
                return Ok(Some(GuardedLiteralTypings::new(
                    LiteralTypings {
                        commit,
                        facts: Vec::new(),
                    },
                    None,
                )));
            }
            CachedSnapshot::Data {
                snap,
                candidates,
                local_keys,
                _held,
            } => {
                let mut found_held = Held::new(&budget);
                let mut found = Vec::new();
                let predicate = Term::NamedNode(rdf::TYPE.into_owned());
                for (subject_id, object_id) in &candidates {
                    check(controls)?;
                    let (subject, _subject_memory) =
                        if Id(*subject_id).tag() == sparkles_core::id::Tag::Local {
                            match local_keys.get(subject_id) {
                                Some(key) => {
                                    let (term, held) =
                                        crate::terms::decode_key_controlled(key, &budget)?;
                                    (Some(term), held)
                                }
                                None => (None, Held::new(&budget)),
                            }
                        } else {
                            crate::terms::decode_store_term_controlled(&snap, *subject_id, &budget)?
                        };
                    let (object, _object_memory) =
                        if Id(*object_id).tag() == sparkles_core::id::Tag::Local {
                            match local_keys.get(object_id) {
                                Some(key) => {
                                    let (term, held) =
                                        crate::terms::decode_key_controlled(key, &budget)?;
                                    (Some(term), held)
                                }
                                None => (None, Held::new(&budget)),
                            }
                        } else {
                            crate::terms::decode_store_term_controlled(&snap, *object_id, &budget)?
                        };
                    if let (Some(subject), Some(object)) = (subject, object)
                        && let Some((literal, datatype)) =
                            LiteralTypings::violation(&subject, &predicate, &object)
                    {
                        found_held.try_grow(fact_bytes(&literal, &datatype))?;
                        found.push((literal, datatype));
                    }
                }
                drop(candidates);
                drop(local_keys);
                drop(_held);
                let summary_held = (!found.is_empty()).then(|| Arc::new(found_held));
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
    let ty_key = sparkles_core::id::iri_key(rdf::TYPE.as_str());
    let ty_term = Term::NamedNode(rdf::TYPE.into_owned());
    let mut found: FxHashMap<Keys, ((Literal, NamedNode), Held)> = FxHashMap::default();
    let consistent = scan_controlled(root, &meta, controls, &budget, |added, keys, mut held| {
        check(controls)?;
        if !added {
            found.remove(&keys);
        } else if keys[1] == ty_key && keys[0].first() == Some(&b'"') {
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
        Some(Arc::new(summary_held)),
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
