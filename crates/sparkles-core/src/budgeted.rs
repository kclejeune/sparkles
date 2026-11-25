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
use crate::store::cold::{ColdBudget, Loaded};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

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

    /// Whether lock waits and reversible-close pauses must fail immediately rather than
    /// sleep or block.
    #[inline]
    fn no_wait(&self) -> bool {
        false
    }

    /// Lock `mutex`. An accounting control checks itself while it waits and refuses to
    /// wait under `no_wait`, as [`ReadLimit::lock`] does. Carriers with their own lock
    /// policy (such as [`crate::guard::WriteOptions`] and [`crate::access::Preparation`])
    /// override this method. Any other control takes the plain lock.
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

    /// Acquire a shared read lock on `lock` under this control's policy.
    #[inline]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        if Self::ACCOUNTS {
            self.read_limit().read_lock(lock)
        } else {
            Ok(lock.read())
        }
    }

    /// The control as a [`ReadLimit`], for the remaining Stage 2 vocabulary readers that
    /// are not generic yet. Stage 2 of item 830 removes it.
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

/// A mutable accounting carrier for cold, commit, index, link, and history metadata
/// readers. Under [`Unlimited`], every accounting step compiles away and file, JSON, and
/// digest helpers lower directly to `std::fs`, `serde_json`, and `sha2`.
pub(crate) trait BudgetControl: Control {
    fn used(&self) -> u64;
    fn charge(&mut self, bytes: u64) -> Result<()>;
    fn release(&mut self, bytes: u64);
    fn exhausted(&self) -> Error;
    fn reserve<T>(&mut self, value: &mut Vec<T>, need: usize) -> Result<()>;
    fn drop_vec<T>(&mut self, value: Vec<T>);
    fn filled<T: Default>(&mut self, count: usize) -> Result<Box<[T]>>;
    fn read_file(&mut self, path: &Path) -> Result<Vec<u8>>;
    fn json<T: DeserializeOwned>(
        &mut self,
        bytes: &[u8],
        label: impl std::fmt::Display,
    ) -> Result<Loaded<T>>;
    fn value<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        label: impl std::fmt::Display,
    ) -> Result<T>;
    fn value_fields<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        fields: &[&str],
        label: impl std::fmt::Display,
    ) -> Result<T>;
    fn sha256(&self, bytes: &[u8]) -> Result<[u8; 32]>;
    fn open_vocab(&mut self, dir: &Path, fsst: bool) -> Result<Loaded<crate::vocab::Vocab>>;
    fn open_dvocab_ro(&mut self, path: &Path) -> Result<Loaded<crate::vocab::DeltaVocab>>;
    fn open_dvocab_layered(
        &mut self,
        prefix: &[(PathBuf, u64)],
        path: &Path,
        read_only: bool,
    ) -> Result<Loaded<crate::vocab::DeltaVocab>>;
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
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        Ok(mutex.lock())
    }

    #[inline(always)]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        Ok(lock.read())
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

impl BudgetControl for Unlimited {
    #[inline(always)]
    fn used(&self) -> u64 {
        0
    }

    #[inline(always)]
    fn charge(&mut self, _bytes: u64) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn release(&mut self, _bytes: u64) {}

    #[inline(always)]
    fn exhausted(&self) -> Error {
        self.refused()
    }

    #[inline(always)]
    fn reserve<T>(&mut self, value: &mut Vec<T>, need: usize) -> Result<()> {
        if need > value.capacity() {
            if value.capacity() == 0 {
                value.reserve_exact(need);
            } else {
                value.reserve(need - value.len());
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn drop_vec<T>(&mut self, value: Vec<T>) {
        drop(value);
    }

    #[inline(always)]
    fn filled<T: Default>(&mut self, count: usize) -> Result<Box<[T]>> {
        Ok((0..count).map(|_| T::default()).collect())
    }

    #[inline(always)]
    fn read_file(&mut self, path: &Path) -> Result<Vec<u8>> {
        Ok(std::fs::read(path)?)
    }

    #[inline(always)]
    fn json<T: DeserializeOwned>(
        &mut self,
        bytes: &[u8],
        label: impl std::fmt::Display,
    ) -> Result<Loaded<T>> {
        let value = serde_json::from_slice(bytes)
            .map_err(|error| Error::Corrupt(format!("{label}: {error}")))?;
        Ok(Loaded { value, charge: 0 })
    }

    #[inline(always)]
    fn value<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        label: impl std::fmt::Display,
    ) -> Result<T> {
        serde_json::from_value(value).map_err(|error| Error::Corrupt(format!("{label}: {error}")))
    }

    #[inline(always)]
    fn value_fields<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        _fields: &[&str],
        label: impl std::fmt::Display,
    ) -> Result<T> {
        serde_json::from_value(value).map_err(|error| Error::Corrupt(format!("{label}: {error}")))
    }

    #[inline(always)]
    fn sha256(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        use sha2::Digest;
        Ok(sha2::Sha256::digest(bytes).into())
    }

    #[inline(always)]
    fn open_vocab(&mut self, dir: &Path, fsst: bool) -> Result<Loaded<crate::vocab::Vocab>> {
        Ok(Loaded {
            value: crate::vocab::Vocab::open_codec(dir, fsst)?,
            charge: 0,
        })
    }

    #[inline(always)]
    fn open_dvocab_ro(&mut self, path: &Path) -> Result<Loaded<crate::vocab::DeltaVocab>> {
        Ok(Loaded {
            value: crate::vocab::DeltaVocab::open_read_only(path)?,
            charge: 0,
        })
    }

    #[inline(always)]
    fn open_dvocab_layered(
        &mut self,
        prefix: &[(PathBuf, u64)],
        path: &Path,
        read_only: bool,
    ) -> Result<Loaded<crate::vocab::DeltaVocab>> {
        Ok(Loaded {
            value: crate::vocab::DeltaVocab::open_layered(prefix, path, read_only)?,
            charge: 0,
        })
    }
}

/// Zero-cost adapter for a fallible check closure (`CHECKS = true, ACCOUNTS = false`),
/// replacing raw `&impl Fn() -> Result<()>` parameters in index and scan readers.
#[derive(Clone, Copy)]
pub(crate) struct FnControl<F>(pub(crate) F);

impl<F: Fn() -> Result<()>> Control for FnControl<F> {
    const CHECKS: bool = true;
    const ACCOUNTS: bool = false;

    #[inline(always)]
    fn check(&self, _bytes: u64) -> Result<()> {
        (self.0)()
    }

    #[inline(always)]
    fn remaining(&self) -> u64 {
        u64::MAX
    }

    #[inline]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        loop {
            (self.0)()?;
            if let Some(guard) = mutex.try_lock_for(std::time::Duration::from_millis(10)) {
                (self.0)()?;
                return Ok(guard);
            }
        }
    }

    #[inline]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        loop {
            (self.0)()?;
            if let Some(guard) = lock.try_read_for(std::time::Duration::from_millis(10)) {
                (self.0)()?;
                return Ok(guard);
            }
        }
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
    fn no_wait(&self) -> bool {
        self.options.no_wait()
    }

    #[inline]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        ReadLimit::lock(self, mutex)
    }

    #[inline]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        ReadLimit::read_lock(self, lock)
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
    fn no_wait(&self) -> bool {
        self.limit().options.no_wait()
    }

    #[inline]
    fn read_limit(&self) -> ReadLimit<'_> {
        let limit = self.limit();
        ReadLimit {
            held: limit.held.saturating_add(self.used()),
            ..limit
        }
    }

    #[cold]
    fn refused(&self) -> Error {
        ColdBudget::exhausted(self)
    }
}

impl BudgetControl for ColdBudget<'_> {
    #[inline]
    fn used(&self) -> u64 {
        ColdBudget::used(self)
    }

    #[inline]
    fn charge(&mut self, bytes: u64) -> Result<()> {
        ColdBudget::charge(self, bytes)
    }

    #[inline]
    fn release(&mut self, bytes: u64) {
        ColdBudget::release(self, bytes)
    }

    #[cold]
    fn exhausted(&self) -> Error {
        ColdBudget::exhausted(self)
    }

    #[inline]
    fn reserve<T>(&mut self, value: &mut Vec<T>, need: usize) -> Result<()> {
        ColdBudget::reserve(self, value, need)
    }

    #[inline]
    fn drop_vec<T>(&mut self, value: Vec<T>) {
        ColdBudget::drop_vec(self, value)
    }

    #[inline]
    fn filled<T: Default>(&mut self, count: usize) -> Result<Box<[T]>> {
        ColdBudget::filled(self, count)
    }

    #[inline]
    fn read_file(&mut self, path: &Path) -> Result<Vec<u8>> {
        ColdBudget::read_file(self, path)
    }

    #[inline]
    fn json<T: DeserializeOwned>(
        &mut self,
        bytes: &[u8],
        _label: impl std::fmt::Display,
    ) -> Result<Loaded<T>> {
        ColdBudget::json(self, bytes)
    }

    #[inline]
    fn value<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        _label: impl std::fmt::Display,
    ) -> Result<T> {
        ColdBudget::value(self, value)
    }

    #[inline]
    fn value_fields<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        fields: &[&str],
        _label: impl std::fmt::Display,
    ) -> Result<T> {
        ColdBudget::value_fields(self, value, fields)
    }

    #[inline]
    fn sha256(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        ColdBudget::sha256(self, bytes)
    }

    #[inline]
    fn open_vocab(&mut self, dir: &Path, fsst: bool) -> Result<Loaded<crate::vocab::Vocab>> {
        crate::vocab::Vocab::open_controlled(dir, fsst, self)
    }

    #[inline]
    fn open_dvocab_ro(&mut self, path: &Path) -> Result<Loaded<crate::vocab::DeltaVocab>> {
        crate::vocab::DeltaVocab::open_read_only_controlled(path, self)
    }

    #[inline]
    fn open_dvocab_layered(
        &mut self,
        prefix: &[(PathBuf, u64)],
        path: &Path,
        read_only: bool,
    ) -> Result<Loaded<crate::vocab::DeltaVocab>> {
        debug_assert!(
            read_only,
            "ColdBudget opens layered delta vocabularies read-only"
        );
        crate::vocab::DeltaVocab::open_layered_controlled(prefix, path, self)
    }
}

impl Control for crate::access::Preparation {
    const CHECKS: bool = true;
    const ACCOUNTS: bool = true;

    #[inline]
    fn check(&self, bytes: u64) -> Result<()> {
        crate::access::Preparation::check(self)?;
        if bytes > 0 && bytes > self.available_room() {
            return Err(self.exhausted());
        }
        Ok(())
    }

    #[inline]
    fn remaining(&self) -> u64 {
        self.available_room()
    }

    #[inline]
    fn no_wait(&self) -> bool {
        self.refuses_wait()
    }

    #[inline]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        crate::access::Preparation::lock(self, mutex)
    }

    #[inline]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        crate::access::Preparation::read_lock(self, lock)
    }

    #[inline]
    fn read_limit(&self) -> ReadLimit<'_> {
        ReadLimit {
            options: self,
            memory: Some(self.available_room()),
            held: 0,
        }
    }

    #[cold]
    fn refused(&self) -> Error {
        self.exhausted()
    }
}

impl Control for crate::guard::WriteOptions {
    const CHECKS: bool = true;
    const ACCOUNTS: bool = false;

    #[inline]
    fn check(&self, _bytes: u64) -> Result<()> {
        crate::guard::WriteOptions::check(self)
    }

    #[inline]
    fn remaining(&self) -> u64 {
        u64::MAX
    }

    #[inline]
    fn no_wait(&self) -> bool {
        self.no_wait
    }

    #[inline]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        loop {
            crate::guard::WriteOptions::check(self)?;
            let guard = if self.no_wait {
                mutex.try_lock()
            } else {
                mutex.try_lock_for(std::time::Duration::from_millis(10))
            };
            if let Some(guard) = guard {
                crate::guard::WriteOptions::check(self)?;
                return Ok(guard);
            }
            if self.no_wait {
                return Err(Error::WriterBusy);
            }
        }
    }

    #[inline]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        loop {
            crate::guard::WriteOptions::check(self)?;
            let guard = if self.no_wait {
                lock.try_read()
            } else {
                lock.try_read_for(std::time::Duration::from_millis(10))
            };
            if let Some(guard) = guard {
                crate::guard::WriteOptions::check(self)?;
                return Ok(guard);
            }
            if self.no_wait {
                return Err(Error::WriterBusy);
            }
        }
    }

    #[inline]
    fn read_limit(&self) -> ReadLimit<'_> {
        ReadLimit {
            options: &NO_CONTROL,
            memory: None,
            held: 0,
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
    fn no_wait(&self) -> bool {
        (**self).no_wait()
    }

    #[inline(always)]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        (**self).lock(mutex)
    }

    #[inline(always)]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        (**self).read_lock(lock)
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

impl<C: Control> Control for &mut C {
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
    fn no_wait(&self) -> bool {
        (**self).no_wait()
    }

    #[inline(always)]
    fn lock<'m, T>(
        &self,
        mutex: &'m parking_lot::Mutex<T>,
    ) -> Result<parking_lot::MutexGuard<'m, T>> {
        (**self).lock(mutex)
    }

    #[inline(always)]
    fn read_lock<'m, T>(
        &self,
        lock: &'m parking_lot::RwLock<T>,
    ) -> Result<parking_lot::RwLockReadGuard<'m, T>> {
        (**self).read_lock(lock)
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

impl<C: BudgetControl> BudgetControl for &mut C {
    #[inline(always)]
    fn used(&self) -> u64 {
        (**self).used()
    }

    #[inline(always)]
    fn charge(&mut self, bytes: u64) -> Result<()> {
        (**self).charge(bytes)
    }

    #[inline(always)]
    fn release(&mut self, bytes: u64) {
        (**self).release(bytes)
    }

    #[inline(always)]
    fn exhausted(&self) -> Error {
        (**self).exhausted()
    }

    #[inline(always)]
    fn reserve<T>(&mut self, value: &mut Vec<T>, need: usize) -> Result<()> {
        (**self).reserve(value, need)
    }

    #[inline(always)]
    fn drop_vec<T>(&mut self, value: Vec<T>) {
        (**self).drop_vec(value)
    }

    #[inline(always)]
    fn filled<T: Default>(&mut self, count: usize) -> Result<Box<[T]>> {
        (**self).filled(count)
    }

    #[inline(always)]
    fn read_file(&mut self, path: &Path) -> Result<Vec<u8>> {
        (**self).read_file(path)
    }

    #[inline(always)]
    fn json<T: DeserializeOwned>(
        &mut self,
        bytes: &[u8],
        label: impl std::fmt::Display,
    ) -> Result<Loaded<T>> {
        (**self).json(bytes, label)
    }

    #[inline(always)]
    fn value<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        label: impl std::fmt::Display,
    ) -> Result<T> {
        (**self).value(value, label)
    }

    #[inline(always)]
    fn value_fields<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        fields: &[&str],
        label: impl std::fmt::Display,
    ) -> Result<T> {
        (**self).value_fields(value, fields, label)
    }

    #[inline(always)]
    fn sha256(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        (**self).sha256(bytes)
    }

    #[inline(always)]
    fn open_vocab(&mut self, dir: &Path, fsst: bool) -> Result<Loaded<crate::vocab::Vocab>> {
        (**self).open_vocab(dir, fsst)
    }

    #[inline(always)]
    fn open_dvocab_ro(&mut self, path: &Path) -> Result<Loaded<crate::vocab::DeltaVocab>> {
        (**self).open_dvocab_ro(path)
    }

    #[inline(always)]
    fn open_dvocab_layered(
        &mut self,
        prefix: &[(PathBuf, u64)],
        path: &Path,
        read_only: bool,
    ) -> Result<Loaded<crate::vocab::DeltaVocab>> {
        (**self).open_dvocab_layered(prefix, path, read_only)
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

    #[test]
    fn fn_control_preparation_and_write_options_enforce_checks_and_locks() {
        use std::time::{Duration, Instant};

        const {
            assert!(<FnControl<fn() -> Result<()>> as Control>::CHECKS);
            assert!(!<FnControl<fn() -> Result<()>> as Control>::ACCOUNTS);
            assert!(<crate::access::Preparation as Control>::CHECKS);
            assert!(<crate::access::Preparation as Control>::ACCOUNTS);
            assert!(<crate::guard::WriteOptions as Control>::CHECKS);
            assert!(!<crate::guard::WriteOptions as Control>::ACCOUNTS);
        }

        // FnControl checks on every call and propagates errors cleanly.
        let hits = std::cell::Cell::new(0usize);
        let ok = FnControl(|| {
            hits.set(hits.get() + 1);
            Ok(())
        });
        Control::check(&ok, 0).unwrap();
        assert_eq!(hits.get(), 1);
        let fail = FnControl(|| Err(Error::Cancelled));
        assert!(matches!(Control::check(&fail, 0), Err(Error::Cancelled)));

        // FnControl polls its closure while waiting on contended locks.
        let contended = parking_lot::Mutex::new(1u32);
        let _held_mutex = contended.lock();
        assert!(matches!(
            Control::lock(&fail, &contended),
            Err(Error::Cancelled)
        ));
        let rw = parking_lot::RwLock::new(1u32);
        let _held_write = rw.write();
        assert!(matches!(
            Control::read_lock(&fail, &rw),
            Err(Error::Cancelled)
        ));
        drop(_held_write);
        drop(_held_mutex);

        // Preparation enforces budget, pool-grant headroom, cancellation, and no_wait lock refusal.
        let prep = crate::access::Preparation::new(crate::task::Control::none(), 4096).unwrap();
        assert!(Control::check(&prep, 0).is_ok());
        assert!(matches!(
            Control::check(&prep, u64::MAX),
            Err(Error::BudgetExceeded(_))
        ));
        let pool = crate::sparql::pool::QueryPool::new(16 << 20);
        let grant = Arc::new(pool.empty_grant());
        let pooled = crate::access::Preparation::with_grant(
            crate::task::Control::none(),
            64 << 20,
            grant.clone(),
        )
        .unwrap();
        let expected_room = pooled.available_room();
        assert!(expected_room <= 16 << 20);
        assert_eq!(Control::remaining(&pooled), expected_room);
        assert_eq!(Control::read_limit(&pooled).memory, Some(expected_room));
        assert!(Control::check(&pooled, 0).is_ok());
        assert!(Control::check(&pooled, expected_room).is_ok());
        assert!(matches!(
            Control::check(&pooled, expected_room + 1),
            Err(Error::BudgetExceeded(_))
        ));
        drop(pooled);
        drop(grant);
        let mutex = parking_lot::Mutex::new(1u32);
        let _guard = mutex.lock();
        let no_wait_prep = prep.with_no_wait(true);
        assert!(Control::no_wait(&no_wait_prep));
        assert!(matches!(
            Control::lock(&no_wait_prep, &mutex),
            Err(Error::Invalid(_))
        ));

        // WriteOptions enforces cancellation, deadline, and WriterBusy under no_wait.
        let no_wait_opts = crate::guard::WriteOptions {
            no_wait: true,
            ..Default::default()
        };
        assert!(Control::no_wait(&no_wait_opts));
        assert!(matches!(
            Control::lock(&no_wait_opts, &mutex),
            Err(Error::WriterBusy)
        ));
        let timed_out = crate::guard::WriteOptions {
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        assert!(matches!(Control::check(&timed_out, 0), Err(Error::Timeout)));
        let cancelled = crate::guard::WriteOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        assert!(matches!(
            Control::check(&cancelled, 0),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn budget_control_unlimited_and_cold_budget_match_and_stop_on_refusal() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        let payload = br#"{"format":2,"id":42}"#;
        std::fs::write(&path, payload).unwrap();

        // Unlimited and ColdBudget agree on file reads, JSON parsing, and SHA-256.
        let mut unlimited = Unlimited;
        let u_bytes = unlimited.read_file(&path).unwrap();
        let u_val: Loaded<serde_json::Value> = unlimited.json(&u_bytes, "sample.json").unwrap();
        let u_hash = unlimited.sha256(&u_bytes).unwrap();
        unlimited.drop_vec(u_bytes);

        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        let c_bytes = budget.read_file(&path).unwrap();
        let c_val: Loaded<serde_json::Value> =
            BudgetControl::json(&mut budget, &c_bytes, "sample.json").unwrap();
        let c_hash = BudgetControl::sha256(&budget, &c_bytes).unwrap();
        assert_eq!(u_val.value, c_val.value);
        assert_eq!(u_hash, c_hash);
        budget.release(c_val.charge);
        budget.drop_vec(c_bytes);
        assert_eq!(budget.used(), 0);

        // Tight budget refuses cleanly.
        let mut tiny = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            BudgetControl::read_file(&mut tiny, &path),
            Err(Error::BudgetExceeded(_))
        ));

        // Uncapped ReadLimit still honours cancellation and deadline.
        let cancel = Arc::new(AtomicBool::new(true));
        let cancelled_opts = HistoryOptions {
            cancel: Some(cancel),
            deadline: None,
        };
        assert!(matches!(
            ColdBudget::new(ReadLimit {
                options: &cancelled_opts,
                memory: None,
                held: 0,
            }),
            Err(Error::Cancelled)
        ));
        let expired_opts = HistoryOptions {
            cancel: None,
            deadline: Some(Instant::now() - Duration::from_secs(1)),
        };
        assert!(matches!(
            ColdBudget::new(ReadLimit {
                options: &expired_opts,
                memory: None,
                held: 0,
            }),
            Err(Error::Timeout)
        ));
    }
}
