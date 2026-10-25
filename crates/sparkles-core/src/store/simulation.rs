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
            let quad = view
                .quad_to_terms(ids)
                .ok_or_else(|| Error::invalid("the simulated view contains an unknown term"))?;
            for &id in ids {
                reserve(id, &mut transaction.guard.next_bnode, &mut next_ordinal)?;
            }
            reserve_term(
                &quad.object,
                &mut transaction.guard.next_bnode,
                &mut next_ordinal,
            )?;
            let subject = match quad.subject {
                NamedOrBlankNode::NamedNode(node) => Term::NamedNode(node),
                NamedOrBlankNode::BlankNode(node) => Term::BlankNode(node),
            };
            let s = transaction.intern(&subject)?;
            let p = transaction.intern(&Term::NamedNode(quad.predicate))?;
            let o = transaction.intern(&quad.object)?;
            let g = match quad.graph_name {
                GraphName::DefaultGraph => Id::DEFAULT_GRAPH,
                GraphName::NamedNode(node) => transaction.intern(&Term::NamedNode(node))?,
                GraphName::BlankNode(node) => transaction.intern(&Term::BlankNode(node))?,
            };
            transaction.insert([s, p, o, g])?;
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
