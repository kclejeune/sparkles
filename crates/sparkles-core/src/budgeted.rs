//! One reader for every control a read can carry (spec X16 Phase 3, item 830).
//!
//! A reader that can be cancelled, can time out or can be held to a memory cap used to
//! exist in two or more copies, an ordinary one and a `_limited` or `_controlled` one.
//! A reader generic over [`Control`] replaces the pair. A call that passes [`Unlimited`]
//! compiles to the ordinary reader, because [`Control::CHECKS`] and
//! [`Control::ACCOUNTS`] are constant false and every method of [`Unlimited`] is empty,
//! the way a `Meter` of `()` compiles away in `keyset.rs`.
//!
//! The carrier's type, not a runtime test, says which of three kinds of control a read
//! has.
//!
//! * [`Unlimited`] has no control at all. `CHECKS` and `ACCOUNTS` are false.
//! * A stride carrier, such as the history read's ceiling in `store.rs`, checks
//!   cancellation and the deadline at the reader's [`Stride`] and charges nothing per
//!   step. `CHECKS` is true and `ACCOUNTS` false.
//! * An accounting carrier, which is [`ReadLimit`] and [`ColdBudget`], checks each step
//!   and accounts for each allocation before the reader makes it, whether or not its
//!   memory member is present. `CHECKS` and `ACCOUNTS` are true. These are the bodies the
//!   `_limited` twins had, and every former `_limited` call site passes one.
//!
//! The receivers are shared, `&self`, because every check reads the carrier's state and
//! none changes it, and because a reader hands the same carrier to the closures and
//! helpers it calls. A carrier that charges and releases what it holds, as [`ColdBudget`]
//! does, does that through its own `&mut self` methods outside these readers.

use crate::error::{Error, Result};
use crate::history::{HistoryOptions, ReadLimit};
use crate::store::Delta;
use crate::store::cold::ColdBudget;

/// What a reader asks of the control that bounds it.
pub(crate) trait Control {
    /// Whether the control checks anything. A reader skips every check when this is
    /// false.
    const CHECKS: bool;

    /// Whether the reader checks each step and charges each allocation before it makes
    /// it, which also chooses its smaller buffers. A reader without it checks only at
    /// its stride, through [`check_delta`](Self::check_delta).
    const ACCOUNTS: bool;

    /// Cancellation and the deadline, and under a memory cap the sum of `bytes` and what
    /// the read already holds against the cap. `bytes` is what the caller is about to
    /// hold.
    fn check(&self, bytes: u64) -> Result<()>;

    /// The bytes the cap still admits, `u64::MAX` without one.
    fn remaining(&self) -> u64;

    /// The check a stride reader makes with the state it has built so far. A carrier
    /// that knows a ceiling for the state adds it here.
    #[inline]
    fn check_delta(&self, _delta: &Delta) -> Result<()> {
        self.check(0)
    }

    /// Lock `mutex`. An accounting control checks itself while it waits and refuses to
    /// wait under `no_wait`, as [`ReadLimit::lock`] does. Any other control takes the
    /// plain lock, as the readers without a memory cap always have.
    #[inline]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        if Self::ACCOUNTS {
            self.read_limit().lock(mutex)
        } else {
            Ok(mutex.lock())
        }
    }

    /// The control as a [`ReadLimit`], for the index readers that the accounting readers
    /// call and that are not generic yet. Stage 3 of item 830 removes it.
    fn read_limit(&self) -> ReadLimit<'_>;

    /// The error of an allocation that the system refused although the cap admitted it.
    #[cold]
    fn refused(&self) -> Error {
        let memory = self.read_limit().memory.unwrap_or(u64::MAX);
        Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Memory,
            limit: memory,
            requested: memory.saturating_add(1),
        })
    }
}

/// The control of a read that has none, for the bridge of [`Unlimited`].
static NO_CONTROL: HistoryOptions = HistoryOptions {
    cancel: None,
    deadline: None,
};

/// A read with no control at all.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Unlimited;

impl Control for Unlimited {
    const CHECKS: bool = false;
    const ACCOUNTS: bool = false;

    #[inline(always)]
    fn check(&self, _bytes: u64) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn remaining(&self) -> u64 {
        u64::MAX
    }

    #[inline(always)]
    fn check_delta(&self, _delta: &Delta) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn read_limit(&self) -> ReadLimit<'_> {
        ReadLimit {
            options: &NO_CONTROL,
            memory: None,
            held: 0,
        }
    }
}

impl Control for ReadLimit<'_> {
    const CHECKS: bool = true;
    const ACCOUNTS: bool = true;

    #[inline]
    fn check(&self, bytes: u64) -> Result<()> {
        ReadLimit::check(self, bytes)
    }

    #[inline]
    fn remaining(&self) -> u64 {
        ReadLimit::remaining(self)
    }

    #[inline]
    fn read_limit(&self) -> ReadLimit<'_> {
        *self
    }
}

/// A cold budget checks what it has charged beside the bytes of each check, as the
/// `ReadLimit` of [`ColdBudget::owned_limit`] does.
impl Control for ColdBudget<'_> {
    const CHECKS: bool = true;
    const ACCOUNTS: bool = true;

    #[inline]
    fn check(&self, bytes: u64) -> Result<()> {
        self.limit().check(self.used().saturating_add(bytes))
    }

    #[inline]
    fn remaining(&self) -> u64 {
        self.available()
    }

    #[inline]
    fn read_limit(&self) -> ReadLimit<'_> {
        let limit = self.limit();
        ReadLimit {
            held: limit.held.saturating_add(self.used()),
            ..limit
        }
    }
}

impl<C: Control> Control for &C {
    const CHECKS: bool = C::CHECKS;
    const ACCOUNTS: bool = C::ACCOUNTS;

    #[inline(always)]
    fn check(&self, bytes: u64) -> Result<()> {
        (**self).check(bytes)
    }

    #[inline(always)]
    fn remaining(&self) -> u64 {
        (**self).remaining()
    }

    #[inline(always)]
    fn check_delta(&self, delta: &Delta) -> Result<()> {
        (**self).check_delta(delta)
    }

    #[inline(always)]
    fn read_limit(&self) -> ReadLimit<'_> {
        (**self).read_limit()
    }

    #[inline(always)]
    fn refused(&self) -> Error {
        (**self).refused()
    }
}

/// How often a stride reader checks its control, in steps. `N` must be a power of two,
/// so that the test is a mask. The stride is a type, not a field, so a reader that is
/// generic over [`Unlimited`] keeps no counter for it, and the step count that the reader
/// keeps for its own result is the only state.
pub(crate) struct Stride<const N: u64>;

impl<const N: u64> Stride<N> {
    const POWER_OF_TWO: () = assert!(N.is_power_of_two());

    /// Whether the `count`th step, counted from one, is due a stride check under `C`. An
    /// accounting carrier checks every step itself, so it is never due one.
    #[inline(always)]
    pub(crate) fn due<C: Control>(count: u64) -> bool {
        let () = Self::POWER_OF_TWO;
        C::CHECKS && !C::ACCOUNTS && count & (N - 1) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The two forms of a cold budget's control agree: the carrier itself and the
    /// `ReadLimit` it lends, for the cap, for what it has charged and for cancellation.
    #[test]
    fn a_cold_budget_controls_as_its_owned_limit() {
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1000),
            held: 100,
        })
        .unwrap();
        budget.charge(300).unwrap();
        let owned = budget.owned_limit().unwrap();
        assert_eq!(Control::remaining(&budget), 600);
        assert_eq!(Control::remaining(&owned), 600);
        assert_eq!(budget.read_limit().remaining(), 600);
        assert!(Control::check(&budget, 600).is_ok());
        assert!(Control::check(&budget, 601).is_err());
        assert!(Control::check(&owned, 601).is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(Control::check(&budget, 0), Err(Error::Cancelled)));
        assert!(matches!(
            Control::check(&budget.read_limit(), 0),
            Err(Error::Cancelled)
        ));
    }

    /// A read with no cap still checks cancellation through an accounting carrier, and
    /// [`Unlimited`] checks nothing.
    #[test]
    fn an_uncapped_limit_still_cancels_and_unlimited_never_does() {
        let cancel = Arc::new(AtomicBool::new(true));
        let options = HistoryOptions {
            cancel: Some(cancel),
            deadline: None,
        };
        let limit = ReadLimit {
            options: &options,
            memory: None,
            held: 0,
        };
        assert!(matches!(Control::check(&limit, 0), Err(Error::Cancelled)));
        assert_eq!(Control::remaining(&limit), u64::MAX);
        assert!(Control::check(&Unlimited, u64::MAX).is_ok());
        assert!(!Stride::<16>::due::<Unlimited>(16));
        assert!(!Stride::<16>::due::<ReadLimit<'_>>(16));
    }
}
