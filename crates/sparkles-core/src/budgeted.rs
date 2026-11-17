//! One reader for every control a read can carry (spec X16 Phase 3, item 830).
//!
//! A reader that can be cancelled, can time out or can be held to a memory cap used to
//! exist in two or more copies, an ordinary one and a `_limited` or `_controlled` one.
//! A reader generic over [`Control`] replaces the pair. A call that passes [`Unlimited`]
//! compiles to the ordinary reader, because [`Control::CHECKS`] is a constant false and
//! every method of [`Unlimited`] is empty, the way a `Meter` of `()` compiles away in
//! `keyset.rs`.
//!
//! A read has one of three kinds of control, and a reader tells them apart as follows.
//!
//! * No control at all, which is [`Unlimited`]. `CHECKS` is false.
//! * Cancellation and a deadline without a memory cap, which is a [`ReadLimit`] whose
//!   memory member is absent. `CHECKS` is true and [`Control::capped_limit`] is `None`,
//!   so a reader checks at its stride and charges nothing per step.
//! * Cancellation, a deadline and a memory cap, which is a [`ReadLimit`] with memory.
//!   [`Control::capped_limit`] is the limit, and a reader accounts for each allocation
//!   before it makes it.
//!
//! [`Stride`] names how often a reader checks, and removes the counter with the checks.

use crate::error::{Error, Result};
use crate::history::ReadLimit;
use crate::store::Delta;

/// What a reader asks of the control that bounds it.
pub(crate) trait Control {
    /// Whether the control checks anything. A reader skips every check and every
    /// accounting expression when this is false.
    const CHECKS: bool;

    /// Cancellation and the deadline, and for a memory cap the sum of `bytes` and what the
    /// read already holds against the cap. `bytes` is what the caller is about to hold.
    fn check(&self, bytes: u64) -> Result<()>;

    /// The memory cap and what the read holds, or `None` when the read has no cap. A
    /// reader that accounts for its allocations does so only when this is `Some`.
    ///
    /// This still names the concrete carrier, because the index and cold readers that
    /// the accounting calls are not generic yet. Stage 3 of item 830 removes it.
    fn capped_limit(&self) -> Option<&ReadLimit<'_>>;

    /// The check a reader makes at its stride when it has no cap, with the state it has
    /// built so far. A carrier that knows a ceiling for the state adds it here.
    #[inline]
    fn check_delta(&self, _delta: &Delta) -> Result<()> {
        self.check(0)
    }

    /// The error of an allocation that the system refused although the cap admitted it.
    #[cold]
    fn refused(&self) -> Error {
        let memory = self
            .capped_limit()
            .and_then(|limit| limit.memory)
            .unwrap_or(u64::MAX);
        Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Memory,
            limit: memory,
            requested: memory.saturating_add(1),
        })
    }
}

/// A read with no control at all.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Unlimited;

impl Control for Unlimited {
    const CHECKS: bool = false;

    #[inline(always)]
    fn check(&self, _bytes: u64) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn capped_limit(&self) -> Option<&ReadLimit<'_>> {
        None
    }

    #[inline(always)]
    fn check_delta(&self, _delta: &Delta) -> Result<()> {
        Ok(())
    }
}

impl Control for ReadLimit<'_> {
    const CHECKS: bool = true;

    #[inline]
    fn check(&self, bytes: u64) -> Result<()> {
        ReadLimit::check(self, bytes)
    }

    #[inline]
    fn capped_limit(&self) -> Option<&ReadLimit<'_>> {
        self.memory.is_some().then_some(self)
    }
}

impl<C: Control + ?Sized> Control for &C {
    const CHECKS: bool = C::CHECKS;

    #[inline]
    fn check(&self, bytes: u64) -> Result<()> {
        (**self).check(bytes)
    }

    #[inline]
    fn capped_limit(&self) -> Option<&ReadLimit<'_>> {
        (**self).capped_limit()
    }

    #[inline]
    fn check_delta(&self, delta: &Delta) -> Result<()> {
        (**self).check_delta(delta)
    }
}

/// How often a reader checks its control, in steps. `N` must be a power of two, so that
/// the test is a mask. The stride is a type, not a field, so a reader that is generic
/// over [`Unlimited`] keeps no counter for it: the step count that the reader keeps for
/// its own result is the only state.
pub(crate) struct Stride<const N: u64>;

impl<const N: u64> Stride<N> {
    const POWER_OF_TWO: () = assert!(N.is_power_of_two());

    /// Whether the `count`th step, counted from one, is due a check under `C`.
    #[inline(always)]
    pub(crate) fn due<C: Control>(count: u64) -> bool {
        let () = Self::POWER_OF_TWO;
        C::CHECKS && count & (N - 1) == 0
    }
}
