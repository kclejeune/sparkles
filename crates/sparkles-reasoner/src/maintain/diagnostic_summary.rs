//! Controlled reads of exact generalized typings from the selected materialization.

use super::*;
use crate::diagnostics::{DiagnosticControls, check_control};
use std::io::Read as _;

fn check(controls: &DiagnosticControls) -> anyhow::Result<()> {
    check_control(controls.deadline, controls.cancel.as_deref())?;
    Ok(())
}

fn fact_bytes(literal: &Literal, datatype: &NamedNode) -> usize {
    256 + literal.value().len() * 2
        + literal.datatype().as_str().len() * 2
        + datatype.as_str().len() * 2
}

/// Read actual materialized terms under a separate diagnostic memory share. Missing
/// or inconsistent optional state returns None, never a recomputed volatile summary.
pub fn literal_typings_controlled(
    store: &Store,
    commit: u64,
    cache: Option<&Cache>,
    controls: &DiagnosticControls,
) -> anyhow::Result<Option<LiteralTypings>> {
    check(controls)?;
    let budget = Budget::new(
        controls
            .max_memory
            .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX)),
    );
    if let Some(cache) = cache
        && let Some(kept) = cache.kept.try_lock()
        && let Some(kept) = kept
            .as_ref()
            .filter(|kept| kept.dataset_id == store.dataset_id() && kept.commit == commit)
    {
        let mut found = Vec::new();
        let terms = &kept.closure.terms;
        let predicate = Term::NamedNode(rdf::TYPE.into_owned());
        for triple in &kept.generalized {
            check(controls)?;
            let (actual_predicate, _predicate_memory) =
                terms.term_controlled(triple[1], &budget)?;
            if actual_predicate.as_ref() != Some(&predicate) {
                continue;
            }
            let (subject, _subject_memory) = terms.term_controlled(triple[0], &budget)?;
            let (object, _object_memory) = terms.term_controlled(triple[2], &budget)?;
            if let (Some(subject), Some(object)) = (subject, object)
                && let Some((literal, datatype)) =
                    LiteralTypings::violation(&subject, &predicate, &object)
            {
                budget.try_charge(fact_bytes(&literal, &datatype))?;
                found.push((literal, datatype));
            }
        }
        return Ok(Some(LiteralTypings {
            commit,
            facts: found,
        }));
    }
    let Some(root) = store.root() else {
        return Ok(None);
    };
    if let Some(limit) = controls.max_memory
        && std::fs::metadata(root.join(SAVED_META))
            .ok()
            .is_some_and(|meta| meta.len() > limit)
    {
        return Err(MemoryExceeded {
            limit: limit.try_into().unwrap_or(usize::MAX),
            context: " while reading generalized diagnostic metadata".into(),
        }
        .into());
    }
    let Some(meta) = saved_meta(root, store.dataset_id(), commit) else {
        return Ok(None);
    };
    // Two bounded file buffers and map bookkeeping overlap while replaying the log.
    budget.try_charge(32 * 1024)?;
    let ty_key = sparkles_core::id::iri_key(rdf::TYPE.as_str());
    let ty_term = Term::NamedNode(rdf::TYPE.into_owned());
    let mut found: FxHashMap<Keys, ((Literal, NamedNode), usize)> = FxHashMap::default();
    let consistent = scan_controlled(root, &meta, controls, &budget, |added, keys, held| {
        check(controls)?;
        if !added {
            if let Some((_, bytes)) = found.remove(&keys) {
                budget.release(bytes);
            }
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
                budget.try_charge(extra)?;
                if let Some((_, bytes)) = found.insert(keys, ((literal, datatype), extra + held)) {
                    budget.release(bytes);
                }
                return Ok(());
            }
        }
        budget.release(held);
        Ok(())
    })?;
    Ok(consistent.then(|| LiteralTypings {
        commit,
        facts: found.into_values().map(|(fact, _)| fact).collect(),
    }))
}

fn keys<R: std::io::Read>(
    reader: &mut Hashing<R>,
    end: u64,
    controls: &DiagnosticControls,
    budget: &Arc<Budget>,
) -> anyhow::Result<Option<(Keys, usize)>> {
    let mut triple: Keys = Default::default();
    let mut held = 0;
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
        budget.try_charge(charge)?;
        held += charge;
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
    mut visit: impl FnMut(bool, Keys, usize) -> anyhow::Result<()>,
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
