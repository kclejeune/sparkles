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
use crate::model::{DataRange, Literal};
use rustc_hash::FxHashMap;
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

/// The atoms of an interner and the ranges they name.
#[derive(Default)]
pub struct DtTable {
    pub atoms: Vec<DtKey>,
    ids: FxHashMap<DtKey, u32>,
    pub ranges: Ranges,
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

    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// The bytes the atoms and the ranges retain.
    pub fn bytes(&self) -> usize {
        let key = std::mem::size_of::<DtKey>();
        self.atoms.capacity() * key + self.ids.capacity() * (key + 4 + 1) + self.ranges.bytes()
    }
}

/// What stops a run, read while a range converts.
#[derive(Clone, Default)]
pub struct Stop {
    pub deadline: Option<Instant>,
    pub cancelled: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Stop {
    pub fn stopped(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
            || self.cancelled.as_ref().is_some_and(|f| f())
    }
}

/// A run's components and definitions, which the interner holds while the run
/// normalizes, only when one component is the checker's.
pub struct DtRun {
    pub comps: Comps,
    pub defs: Defs,
    pub stop: Stop,
}

/// Why a data restriction has no atom.
pub const REFUSED: &str = "a data range or literal outside the datatype checker";
pub const STOPPED: &str = "the run stopped while it converted a data range";

impl DtRun {
    pub fn bytes(&self) -> usize {
        self.comps.bytes() + self.defs.bytes()
    }

    /// The range of a data range in `ranges`, with the run's definitions.
    pub fn range(&mut self, ranges: &mut Ranges, r: &DataRange) -> Result<Rid, &'static str> {
        let DtRun { defs, stop, .. } = self;
        match ranges.of_defined(r, defs, &|| stop.stopped()) {
            Ok(id) => Ok(id),
            Err(Fail::Unsupported(_)) => Err(REFUSED),
            Err(Fail::Stopped) => Err(STOPPED),
        }
    }

    /// The point range of a literal.
    pub fn literal(ranges: &mut Ranges, l: &Literal) -> Result<Rid, &'static str> {
        ranges.literal(l).map_err(|_| REFUSED)
    }
}
