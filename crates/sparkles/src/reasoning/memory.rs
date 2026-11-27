//! Admission for facade-owned ontology and analysis payloads.

use crate::{Budget, BudgetKind, Error, Result};
use sparkles_budget::{LiveCredit, ParentHandle, ParentRefused, Scope};

pub(crate) struct Memory {
    bytes: u64,
    limit: Option<u64>,
    credit: Option<LiveCredit>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
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
        Self::query_after(opts, 0)
    }

    pub fn query_after(opts: &crate::sparql::QueryOptions, overlap: u64) -> Self {
        let parent = opts
            .grant
            .as_ref()
            .map(|g| g.memory_parent())
            .or_else(|| opts.pool.as_ref().map(|p| p.work_parent()));
        Self::new(
            opts.max_memory_bytes
                .map(|limit| limit.saturating_sub(overlap)),
            parent,
        )
    }

    pub fn parent(&self) -> Option<ParentHandle> {
        self.credit.as_ref().map(LiveCredit::parent)
    }

    pub fn remaining(&self) -> Option<u64> {
        self.limit.map(|limit| limit.saturating_sub(self.bytes))
    }

    pub fn limit(&self) -> Option<u64> {
        self.limit
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn shrink_to(&mut self, bytes: u64) {
        if bytes < self.bytes {
            if let Some(credit) = &mut self.credit {
                credit
                    .remove(self.bytes - bytes)
                    .expect("shrinking owned credit");
                drop(credit.trim_detached());
            }
            self.bytes = bytes;
        }
    }

    pub fn split(&mut self, bytes: u64) -> Result<Self> {
        self.cover(bytes)?;
        let credit = self
            .credit
            .as_mut()
            .map(|c| c.split_live(bytes))
            .transpose()
            .map_err(parent_error)?;
        self.bytes -= bytes;
        Ok(Self {
            bytes,
            limit: self.limit,
            credit,
        })
    }

    pub fn absorb(&mut self, other: &mut Self) -> Result<()> {
        if let (Some(credit), Some(other_credit)) = (&mut self.credit, &mut other.credit) {
            credit.try_merge(other_credit).map_err(parent_error)?;
        }
        self.bytes = self.bytes.saturating_add(other.bytes);
        other.bytes = 0;
        Ok(())
    }

    pub fn into_lease(self) -> Option<sparkles_budget::MemoryLease> {
        self.credit.map(LiveCredit::into_lease)
    }

    pub fn cover(&mut self, bytes: u64) -> Result<()> {
        self.grow(bytes.saturating_sub(self.bytes))
    }

    pub fn grow(&mut self, bytes: u64) -> Result<()> {
        self.grow_with_overlap(bytes, 0)
    }

    /// The overlapping bytes are already charged to the parent by their owner.
    pub fn grow_with_overlap(&mut self, bytes: u64, overlap: u64) -> Result<()> {
        let next = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| parent_error(ParentRefused::Overflow))?;
        let requested = next.saturating_add(overlap);
        if let Some(limit) = self.limit
            && requested > limit
        {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit,
                requested,
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
