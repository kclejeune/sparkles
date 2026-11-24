//! The checker's data atoms as the interner keeps them, and what a run knows of its
//! components (F24, "The EL and Horn tiers").
//!
//! A data restriction over a property of a checker component is one atom `≥ n d.R` or
//! `≤ n d.R`, with R interned in the checker's [`Ranges`]. The table lives as long as
//! the interner, like its lattice values, so that an atom keeps its id across runs. The
//! components and the datatype definitions are the run's, set before its normalization
//! and dropped after it.

use super::comp::Comps;
use super::range::{Defs, Fail, Ranges, Rid};
use crate::classify::Error;
use crate::classify::numeric::Budget;
use crate::model::{DataRange, Literal};
use rustc_hash::FxHashMap;
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Instant;

/// One atom: the data role, whether it is an at-least atom, the count and the range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DtKey {
    pub role: u32,
    pub at_least: bool,
    pub n: u64,
    pub range: Rid,
}

/// The atoms of an interner and the ranges they name. Lazily allocates its [`Ranges`]
/// table only once a run with a checker component actually converts a range or literal.
#[derive(Default)]
pub struct DtTable {
    pub atoms: Vec<DtKey>,
    ids: FxHashMap<DtKey, u32>,
    ranges: Option<Ranges>,
}

impl DtTable {
    pub fn id(&mut self, k: DtKey) -> u32 {
        if let Some(&id) = self.ids.get(&k) {
            return id;
        }
        let id = self.atoms.len() as u32;
        self.atoms.push(k);
        self.ids.insert(k, id);
        id
    }

    pub fn lookup(&self, k: &DtKey) -> Option<u32> {
        self.ids.get(k).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// True when no checker atom has been interned and no checker [`Ranges`] table has
    /// been allocated.
    pub fn is_unallocated(&self) -> bool {
        self.atoms.is_empty() && self.ids.is_empty() && self.ranges.is_none()
    }

    pub fn ranges(&self) -> Option<&Ranges> {
        self.ranges.as_ref()
    }

    pub fn ranges_mut(&mut self) -> &mut Ranges {
        self.ranges.get_or_insert_with(Ranges::new)
    }

    /// Worst-case capacity growth in bytes if one new [`DtKey`] is inserted.
    pub fn next_growth_bytes(&self) -> usize {
        let key = std::mem::size_of::<DtKey>();
        let vec_add = if self.atoms.len() == self.atoms.capacity() {
            self.atoms.capacity().max(4).saturating_mul(key)
        } else {
            0
        };
        let map_add = super::range::map_growth(&self.ids);
        vec_add.saturating_add(map_add)
    }

    /// The bytes the atoms and the ranges retain.
    pub fn bytes(&self) -> usize {
        let key = std::mem::size_of::<DtKey>();
        self.atoms
            .capacity()
            .saturating_mul(key)
            .saturating_add(super::range::map_backing_bytes(&self.ids))
            .saturating_add(self.ranges.as_ref().map_or(0, Ranges::bytes))
    }
}

/// What stops a run, read while a range converts.
#[derive(Clone, Default)]
pub struct Stop {
    pub deadline: Option<Instant>,
    pub cancelled: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// Which of a run's stops came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halted {
    Timeout,
    Cancelled,
    Memory(usize),
}

impl Stop {
    /// The stop that has come, reading each source once, since a caller's cancellation
    /// may answer true only once.
    pub fn check(&self) -> Option<Halted> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            Some(Halted::Timeout)
        } else if self.cancelled.as_ref().is_some_and(|f| f()) {
            Some(Halted::Cancelled)
        } else {
            None
        }
    }
}

/// A run's components and definitions, which the interner holds while the run
/// normalizes, only when one component is the checker's.
pub struct DtRun {
    pub comps: Comps,
    pub defs: Defs,
    pub stop: Stop,
    pub budget: Option<Arc<Budget>>,
    /// the stop that a conversion saw, which stops the run once its normalization ends
    pub stopped: Option<Halted>,
}

/// Why a data restriction has no atom.
pub const REFUSED: &str = "a data range or literal outside the datatype checker";
pub const STOPPED: &str = "the run stopped while it converted a data range";

impl DtRun {
    /// Bytes of the immutable component and definition tables.
    pub fn static_bytes(&self) -> usize {
        self.comps.bytes().saturating_add(self.defs.map_bytes())
    }

    /// O(1) bytes of the mutable conversion memo table.
    pub fn memo_bytes(&self) -> usize {
        self.defs.memo_bytes()
    }

    pub fn bytes(&self) -> usize {
        self.static_bytes().saturating_add(self.memo_bytes())
    }

    fn admit_with<T>(
        &mut self,
        table: &mut DtTable,
        f: impl FnOnce(
            &mut Ranges,
            &mut Defs,
            &dyn Fn() -> bool,
            &dyn Fn(usize) -> bool,
        ) -> Result<T, Fail>,
    ) -> Result<T, &'static str> {
        if self.stopped.is_some() {
            return Err(STOPPED);
        }
        let before = table.bytes().saturating_add(self.defs.bytes());
        let DtRun {
            defs,
            stop,
            budget,
            stopped,
            ..
        } = self;
        let guard = RefCell::new(budget.as_ref().map(Budget::scratch_guard));
        let mem_used = Cell::new(0usize);
        let admit = |bytes: usize| -> bool {
            let mut g = guard.borrow_mut();
            if let Some(g) = g.as_mut() {
                match g.grow(bytes) {
                    Ok(()) => true,
                    Err(Error::Memory { used, .. }) => {
                        mem_used.set(used.max(1));
                        false
                    }
                    Err(_) => false,
                }
            } else {
                true
            }
        };
        if table.ranges.is_none() && !admit(4096) {
            *stopped = Some(Halted::Memory(mem_used.get().max(1)));
            return Err(STOPPED);
        }
        let ranges = table.ranges_mut();
        let seen = Cell::new(None);
        let poll = || {
            let h = stop.check();
            seen.set(h);
            h.is_some()
        };
        let res = f(ranges, defs, &poll, &admit);
        let retained = table
            .bytes()
            .saturating_add(defs.bytes())
            .saturating_sub(before);
        if let Some(g) = guard.into_inner()
            && let Err(Error::Memory { used, .. }) = g.commit_retained(retained)
        {
            *stopped = Some(Halted::Memory(used.max(1)));
            return Err(STOPPED);
        }
        match res {
            Ok(v) => Ok(v),
            Err(Fail::Unsupported(_)) => Err(REFUSED),
            Err(Fail::Stopped) => {
                *stopped = Some(seen.get().unwrap_or(Halted::Cancelled));
                Err(STOPPED)
            }
            Err(Fail::Memory) => {
                *stopped = Some(Halted::Memory(mem_used.get().max(1)));
                Err(STOPPED)
            }
        }
    }

    /// Admits and interns one atom key into `table`, latching [`Halted::Memory`] on refusal.
    pub fn atom_id(&mut self, table: &mut DtTable, k: DtKey) -> Option<u32> {
        if self.stopped.is_some() {
            return None;
        }
        if let Some(id) = table.lookup(&k) {
            return Some(id);
        }
        let before = table.bytes();
        let est = table.next_growth_bytes();
        let mut guard = self.budget.as_ref().map(Budget::scratch_guard);
        if let Some(g) = guard.as_mut()
            && let Err(Error::Memory { used, .. }) = g.grow(est)
        {
            self.stopped = Some(Halted::Memory(used.max(1)));
            return None;
        }
        let id = table.id(k);
        let retained = table.bytes().saturating_sub(before);
        debug_assert!(retained <= est, "retained {retained} > est {est}");
        if let Some(g) = guard
            && let Err(Error::Memory { used, .. }) = g.commit_retained(retained)
        {
            self.stopped = Some(Halted::Memory(used.max(1)));
            return None;
        }
        Some(id)
    }

    /// The range of a data range in `table`, with the run's definitions.
    pub fn range(&mut self, table: &mut DtTable, r: &DataRange) -> Result<Rid, &'static str> {
        self.admit_with(table, |ranges, defs, poll, admit| {
            ranges.of_defined_bounded(r, defs, poll, admit)
        })
    }

    /// The point range of a literal in `table`.
    pub fn literal(&mut self, table: &mut DtTable, l: &Literal) -> Result<Rid, &'static str> {
        self.admit_with(table, |ranges, _, _, admit| {
            ranges.literal_bounded(l, admit)
        })
    }

    /// The complement of an interned range in `table`.
    pub fn not(&mut self, table: &mut DtTable, x: Rid) -> Result<Rid, &'static str> {
        self.admit_with(table, |ranges, _, _, admit| ranges.not_bounded(x, admit))
    }
}
