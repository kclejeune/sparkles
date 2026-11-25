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

fn reserve_term(term: &Term, next: &mut u64, ordinals: &mut u64) -> Result<()> {
    match term {
        Term::BlankNode(node) => {
            if let Some(id) = parse_bnode_label(node.as_str()) {
                reserve(id, next, ordinals)?;
            }
        }
        Term::Triple(triple) => {
            if let NamedOrBlankNode::BlankNode(node) = &triple.subject
                && let Some(id) = parse_bnode_label(node.as_str())
            {
                reserve(id, next, ordinals)?;
            }
            reserve_term(&triple.object, next, ordinals)?;
        }
        Term::Literal(literal) if crate::sparql::cdt::may_name_bnodes(literal) => {
            let mut failed = None;
            let _ = crate::sparql::cdt::relabel_term(term, &mut |label| {
                if failed.is_none()
                    && let Some(id) = parse_bnode_label(label)
                    && let Err(error) = reserve(id, next, ordinals)
                {
                    failed = Some(error);
                }
                label.to_string()
            });
            if let Some(error) = failed {
                return Err(error);
            }
        }
        _ => {}
    }
    Ok(())
}

fn key_may_name_bnodes(key: &[u8]) -> bool {
    match key.first() {
        Some(b'(') => true,
        Some(b'"') => {
            let Some(sep) = key.iter().rposition(|&b| b == id::KEY_SEP) else {
                return false;
            };
            let suffix = &key[sep + 1..];
            if suffix.is_empty() || suffix[0] == b'@' {
                return false;
            }
            let Ok(dt) = std::str::from_utf8(suffix) else {
                return false;
            };
            crate::sparql::cdt::is_cdt(dt) && key[1..sep].windows(2).any(|w| w == b"_:")
        }
        _ => false,
    }
}

fn copy_key_into(
    copied: &Store,
    transaction: &mut WriteTxn<'_>,
    key: &[u8],
    next_ordinal: &mut u64,
) -> Result<Id> {
    if key_may_name_bnodes(key) {
        if copied.opts.max_memory_bytes.is_some() {
            let decode_bytes = id::key_decode_bytes(key)?;
            copied.check_memory(transaction.memory_size().saturating_add(decode_bytes))?;
        }
        let term = id::key_to_term(key);
        crate::nesting::check_triple_term(&term)?;
        reserve_term(&term, &mut transaction.guard.next_bnode, next_ordinal)?;
    }
    if let Some(existing) = transaction.lookup_key(key) {
        return Ok(existing);
    }
    if copied.opts.max_memory_bytes.is_some() {
        copied.check_memory(transaction.memory_size().saturating_add(key.len() as u64))?;
    }
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
        self.copy_view_with_allocator(view, options, &write, next, keep)
    }

    /// The writer already holds admission while freezing a rollback candidate.
    pub(super) fn copy_view_with_allocator(
        &self,
        view: &Snapshot,
        options: StoreOptions,
        control: &WriteOptions,
        next: u64,
        mut keep: impl FnMut(&[Id; 4]) -> Result<bool>,
    ) -> Result<Store> {
        control.check()?;
        let write = WriteOptions {
            cancel: control.cancel.clone(),
            cancellation: control.cancellation.clone(),
            deadline: control.deadline,
            ..Default::default()
        };
        let copied = Store::in_memory(options);
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
                            copy_key_into(&copied, &mut transaction, key, &mut next_ordinal)
                        })?,
                        crate::id::Tag::Vocab => {
                            let base_mem = transaction.memory_size();
                            view.generation
                                .vocab
                                .get_with_controlled_capacity_checked(
                                    id.payload(),
                                    |old, new| {
                                        if copied.opts.max_memory_bytes.is_some() {
                                            copied
                                                .check_memory(base_mem.saturating_add(
                                                    old.saturating_add(new) as u64,
                                                ))?;
                                        }
                                        Ok(())
                                    },
                                    |cap| {
                                        if copied.opts.max_memory_bytes.is_some() {
                                            copied.check_memory(
                                                base_mem.saturating_add(cap as u64),
                                            )?;
                                        }
                                        Ok(())
                                    },
                                    |_| control.check(),
                                    |key| {
                                        copy_key_into(
                                            &copied,
                                            &mut transaction,
                                            key,
                                            &mut next_ordinal,
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
