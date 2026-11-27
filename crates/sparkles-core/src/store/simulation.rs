//! Isolated writable copies of immutable views for read-only reasoning analyses.

use super::*;
use crate::guard::WriteOptions;

fn reserve(id: Id, next: &mut u64, ordinals: &mut u64) -> Result<()> {
    if id.tag() != crate::id::Tag::BNode {
        return Ok(());
    }
    let payload = id.payload();
    if payload & Id::LOCAL_BNODE_BIT != 0 {
        return Err(Error::invalid(
            "a simulated view contains an unstored blank node",
        ));
    }
    *ordinals = (*ordinals).max(crate::branch::bnode_ordinal(payload) + 1);
    let own = crate::branch::bnode_ordinal(*next);
    if crate::branch::bnode_ordinal(payload) == own && payload >= *next {
        let following = payload + 1;
        if crate::branch::bnode_ordinal(following) != own {
            return Err(Error::invalid(
                "the simulated view exhausts its blank-node range",
            ));
        }
        *next = following;
    }
    Ok(())
}

pub(super) const APPEND_VOCAB_ENTRY_BYTES: u64 = 48;

fn split_triple_key(key: &[u8]) -> Result<(&[u8], &[u8], &[u8])> {
    let malformed = || Error::Corrupt("invalid vocabulary triple-term key".into());
    let mut pos = 1usize;
    let mut parts = [&[][..]; 3];
    for part in &mut parts {
        let mut len = 0u64;
        let mut shift = 0u32;
        loop {
            let next = *key.get(pos).ok_or_else(malformed)?;
            pos += 1;
            if shift == 63 && next > 1 {
                return Err(malformed());
            }
            len |= u64::from(next & 127)
                .checked_shl(shift)
                .ok_or_else(malformed)?;
            if next & 128 == 0 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return Err(malformed());
            }
        }
        let len = usize::try_from(len).map_err(|_| malformed())?;
        let end = pos.checked_add(len).ok_or_else(malformed)?;
        *part = key.get(pos..end).ok_or_else(malformed)?;
        pos = end;
    }
    if pos != key.len() {
        return Err(malformed());
    }
    Ok((parts[0], parts[1], parts[2]))
}

fn reserve_cdt_key(
    key: &[u8],
    next: &mut u64,
    ordinals: &mut u64,
    check: &mut dyn FnMut() -> Result<()>,
    allow_mem: &mut dyn FnMut(u64) -> bool,
) -> Result<()> {
    let Some(sep) = key.iter().rposition(|&b| b == id::KEY_SEP) else {
        return Ok(());
    };
    let Some(dt_bytes) = key[sep + 1..].strip_prefix(b"^") else {
        return Ok(());
    };
    let Ok(dt) = std::str::from_utf8(dt_bytes) else {
        return Ok(());
    };
    if !crate::sparql::cdt::is_cdt(dt) {
        return Ok(());
    }
    let raw_lex = &key[1..sep];
    if memchr::memmem::find(raw_lex, b"_:").is_none() && memchr::memchr(b'\\', raw_lex).is_none() {
        return Ok(());
    }
    let lex = std::str::from_utf8(raw_lex)
        .map_err(|_| Error::Corrupt("invalid UTF-8 in vocabulary composite literal key".into()))?;
    crate::sparql::cdt::scan_bnode_ids(lex, dt, check, allow_mem, |id| reserve(id, next, ordinals))
}

/// Reserve every stored blank node named inside `key` (RDF 1.2 triple terms and
/// CDT literals) directly from the vocabulary bytes in O(1) stack, using zero
/// heap for ordinary literals and preflighting wide-map duplicate indexes.
fn reserve_key(
    key: &[u8],
    next: &mut u64,
    ordinals: &mut u64,
    check: &mut dyn FnMut() -> Result<()>,
    allow_mem: &mut dyn FnMut(u64) -> bool,
) -> Result<()> {
    let mut current = key;
    let mut depth = 0usize;
    loop {
        match current.first() {
            Some(b'(') => {
                depth += 1;
                if depth > crate::nesting::MAX_TRIPLE_TERMS {
                    return Err(Error::invalid(format!(
                        "triple term nested deeper than {} levels",
                        crate::nesting::MAX_TRIPLE_TERMS
                    )));
                }
                let (subject, _predicate, object) = split_triple_key(current)?;
                if subject.first() == Some(&b'_') && subject.len() == 9 {
                    let payload =
                        u64::from_be_bytes(subject[1..9].try_into().unwrap()) & id::PAYLOAD_MASK;
                    reserve(Id::bnode(payload), next, ordinals)?;
                }
                current = object;
            }
            Some(b'_') if current.len() == 9 => {
                let payload =
                    u64::from_be_bytes(current[1..9].try_into().unwrap()) & id::PAYLOAD_MASK;
                return reserve(Id::bnode(payload), next, ordinals);
            }
            Some(b'"') => return reserve_cdt_key(current, next, ordinals, check, allow_mem),
            _ => return Ok(()),
        }
    }
}

fn copy_key_into(
    copied: &Store,
    transaction: &mut WriteTxn<'_>,
    key: &[u8],
    scratch_bytes: u64,
    next_ordinal: &mut u64,
    control: &WriteOptions,
) -> Result<Id> {
    if let Some(existing) = transaction.lookup_key(key) {
        return Ok(existing);
    }
    let mem_budget = if copied.opts.max_memory_bytes.is_some() || copied.private_memory.is_some() {
        let base = transaction.memory_size().saturating_add(scratch_bytes);
        let base_total = copied.memory_with_configuration(base)?;
        copied.check_memory_total(
            base_total
                .saturating_add(key.len() as u64)
                .saturating_add(APPEND_VOCAB_ENTRY_BYTES),
        )?;
        copied.opts.max_memory_bytes.map(|max| (base_total, max))
    } else {
        None
    };
    reserve_key(
        key,
        &mut transaction.guard.next_bnode,
        next_ordinal,
        &mut || control.check(),
        &mut |extra_bytes| {
            mem_budget.is_none_or(|(base_total, max)| base_total.saturating_add(extra_bytes) <= max)
        },
    )?;
    transaction.intern_key(key)
}

impl Store {
    /// Reserve an otherwise unused stored-blank range in an isolated memory family.
    /// Analysis callers share this range across their copies, without changing the
    /// source family's allocator or branch metadata.
    pub fn reserve_simulation_blank_range(&self, ordinal: Option<u16>) -> Result<u16> {
        if self.is_persistent() {
            return Err(Error::invalid(
                "simulation witnesses require an isolated memory store",
            ));
        }
        let set = self.owned_set()?;
        let ordinal = match ordinal {
            Some(ordinal) => ordinal,
            None => u16::try_from(set.next_ordinal()).map_err(|_| {
                Error::invalid("no blank-node range remains for simulation witnesses")
            })?,
        };
        let next = ordinal as u64 + 1;
        set.reserve_memory_ordinals(next)?;
        self.branching
            .next_ordinal
            .fetch_max(next, Ordering::Relaxed);
        Ok(ordinal)
    }

    /// Stream selected quads of `view` into an independent memory store, preserving
    /// stored blank-node identities, including inside RDF triple terms and CDT values.
    /// The view may be historical, masked or an unpublished write preview. Its dataset
    /// family must be this store's. Only the new store is written.
    ///
    /// The source allocator's counter prevents new nodes from colliding with copied
    /// nodes. Preview nodes advance that counter only within its own branch range;
    /// inherited nodes retain their separate ranges. The new store has no auxiliary
    /// indexes or write guard. `control` supplies cancellation, deadline and writer
    /// admission only, and `options.max_memory_bytes` bounds its estimated stored heap.
    pub fn copy_view_to_memory(
        &self,
        view: &Snapshot,
        options: StoreOptions,
        control: &WriteOptions,
        keep: impl FnMut(&[Id; 4]) -> Result<bool>,
    ) -> Result<Store> {
        self.copy_view_to_memory_with_parent(view, options, control, None, keep)
    }

    /// Copy a request-local snapshot under its operation's storage admission.
    pub fn copy_view_to_memory_with_parent(
        &self,
        view: &Snapshot,
        options: StoreOptions,
        control: &WriteOptions,
        parent: Option<sparkles_budget::ParentHandle>,
        keep: impl FnMut(&[Id; 4]) -> Result<bool>,
    ) -> Result<Store> {
        control.check()?;
        if view.dataset_id != self.owner_dataset_id() {
            return Err(Error::invalid(
                "the simulated view belongs to a different dataset family",
            ));
        }
        let write = WriteOptions {
            cancel: control.cancel.clone(),
            cancellation: control.cancellation.clone(),
            deadline: control.deadline,
            no_wait: control.no_wait,
            ..Default::default()
        };
        let next = self.lock_writer(&write)?.next_bnode;
        self.copy_private_view(view, options, &write, next, parent, keep)
    }

    /// The writer already holds admission while freezing a rollback candidate.
    pub(super) fn copy_view_with_allocator(
        &self,
        view: &Snapshot,
        options: StoreOptions,
        control: &WriteOptions,
        next: u64,
        keep: impl FnMut(&[Id; 4]) -> Result<bool>,
    ) -> Result<Store> {
        self.copy_private_view(view, options, control, next, None, keep)
    }

    fn copy_private_view(
        &self,
        view: &Snapshot,
        options: StoreOptions,
        control: &WriteOptions,
        next: u64,
        parent: Option<sparkles_budget::ParentHandle>,
        mut keep: impl FnMut(&[Id; 4]) -> Result<bool>,
    ) -> Result<Store> {
        control.check()?;
        let write = WriteOptions {
            cancel: control.cancel.clone(),
            cancellation: control.cancellation.clone(),
            deadline: control.deadline,
            ..Default::default()
        };
        let copied = Store::private_memory(options, parent)?;
        let mut next_ordinal = self
            .branching
            .next_ordinal
            .load(Ordering::Relaxed)
            .max(crate::branch::bnode_ordinal(next) + 1);
        let mut transaction = copied.write_with(CommitKind::Load, write);
        transaction.guard.next_bnode = next;
        view.for_each_quad(|ids| {
            control.check()?;
            if !keep(ids)? {
                return Ok(());
            }
            let mut copied_ids = [Id::UNDEF; 4];
            for (pos, &id) in ids.iter().enumerate() {
                copied_ids[pos] = if pos == 3 && id == Id::DEFAULT_GRAPH {
                    Id::DEFAULT_GRAPH
                } else {
                    match id.tag() {
                        crate::id::Tag::Bool
                        | crate::id::Tag::Int
                        | crate::id::Tag::Double
                        | crate::id::Tag::Decimal
                        | crate::id::Tag::DateTime
                        | crate::id::Tag::Date => id,
                        crate::id::Tag::BNode => {
                            reserve(id, &mut transaction.guard.next_bnode, &mut next_ordinal)?;
                            id
                        }
                        crate::id::Tag::Delta => view.generation.dvocab.with(|v| {
                            let key = v.get(id.payload()).ok_or_else(|| {
                                Error::invalid("the simulated view contains an unknown term")
                            })?;
                            copy_key_into(
                                &copied,
                                &mut transaction,
                                key,
                                0,
                                &mut next_ordinal,
                                control,
                            )
                        })?,
                        crate::id::Tag::Vocab => {
                            let base_mem = if copied.opts.max_memory_bytes.is_some()
                                || copied.private_memory.is_some()
                            {
                                transaction.memory_size()
                            } else {
                                0
                            };
                            let scratch_bytes = std::cell::Cell::new(0u64);
                            view.generation
                                .vocab
                                .get_with_controlled_capacity_checked(
                                    id.payload(),
                                    |old, new| {
                                        if copied.opts.max_memory_bytes.is_some()
                                            || copied.private_memory.is_some()
                                        {
                                            copied.check_memory(
                                                base_mem
                                                    .saturating_add(old as u64)
                                                    .saturating_add(new as u64),
                                            )?;
                                        }
                                        Ok(())
                                    },
                                    |cap| {
                                        let cap = cap as u64;
                                        scratch_bytes.set(cap);
                                        if copied.opts.max_memory_bytes.is_some()
                                            || copied.private_memory.is_some()
                                        {
                                            copied.check_memory(base_mem.saturating_add(cap))?;
                                        }
                                        Ok(())
                                    },
                                    |_| control.check(),
                                    |key| {
                                        copy_key_into(
                                            &copied,
                                            &mut transaction,
                                            key,
                                            scratch_bytes.get(),
                                            &mut next_ordinal,
                                            control,
                                        )
                                    },
                                )?
                                .ok_or_else(|| {
                                    Error::invalid("the simulated view contains an unknown term")
                                })?
                        }
                        _ => {
                            return Err(Error::invalid(
                                "the simulated view contains an unknown term",
                            ));
                        }
                    }
                };
            }
            transaction.insert(copied_ids)?;
            transaction.check_storage()?;
            Ok(())
        })?;
        control.check()?;
        transaction.commit()?;
        copied.owned_set()?.reserve_memory_ordinals(next_ordinal)?;
        copied
            .branching
            .next_ordinal
            .store(next_ordinal, Ordering::Relaxed);
        Ok(copied)
    }
}
