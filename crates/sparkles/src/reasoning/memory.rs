//! Admission for facade-owned ontology and analysis payloads.

use crate::{Budget, BudgetKind, Error, Result};
use sparkles_budget::{LiveCredit, ParentHandle, ParentRefused, Scope};

pub(crate) struct Memory {
    bytes: u64,
    limit: Option<u64>,
    credit: Option<LiveCredit>,
}

impl Memory {
    pub fn new(limit: Option<u64>, parent: Option<ParentHandle>) -> Self {
        Self {
            bytes: 0,
            limit,
            credit: parent.map(LiveCredit::new),
        }
    }

    pub fn query(opts: &crate::sparql::QueryOptions) -> Self {
        let parent = opts
            .grant
            .as_ref()
            .map(|g| g.memory_parent())
            .or_else(|| opts.pool.as_ref().map(|p| p.work_parent()));
        Self::new(opts.max_memory_bytes, parent)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn cover(&mut self, bytes: u64) -> Result<()> {
        self.grow(bytes.saturating_sub(self.bytes))
    }

    pub fn grow(&mut self, bytes: u64) -> Result<()> {
        let next = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| parent_error(ParentRefused::Overflow))?;
        if let Some(limit) = self.limit
            && next > limit
        {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit,
                requested: next,
            }));
        }
        if let Some(credit) = &mut self.credit {
            credit.try_add(bytes).map_err(parent_error)?;
        }
        self.bytes = next;
        Ok(())
    }
}

/// Decode under the same admission as the output that will own the term. Include
/// vocabulary reconstruction while it overlaps the resulting decoded term.
pub(crate) fn term(
    snap: &crate::store::Snapshot,
    id: crate::id::Id,
    memory: &mut Memory,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<oxrdf::Term>> {
    let memory = std::cell::RefCell::new(memory);
    let base = memory.borrow().bytes();
    snap.term_controlled_owned(
        id,
        &mut |old, new| {
            memory
                .borrow_mut()
                .cover(base.saturating_add((old + new) as u64))
        },
        &mut |_| Ok(()),
        &mut |scratch, key| {
            memory.borrow_mut().cover(
                base.saturating_add(scratch as u64)
                    .saturating_add(144 + (key as u64).saturating_mul(8)),
            )
        },
        check,
    )
}

pub(crate) fn parent_error(error: ParentRefused) -> Error {
    match error {
        ParentRefused::Limit {
            limit,
            requested,
            scope,
        } => Error::BudgetExceeded(Budget {
            kind: if scope == Scope::Local {
                BudgetKind::Memory
            } else {
                BudgetKind::ServerMemory
            },
            limit,
            requested,
        }),
        error => Error::invalid(error.to_string()),
    }
}
