//! Buffers whose memory charge is their allocation (item 1183).
//!
//! A [`Charged`] owns one growable collection and the charge for it. It grows in one
//! place, [`Charged::grow`], which admits the old allocation and the new one together
//! before it allocates, since both are live while the entries move, and then sets the
//! charge to the size the collection reports. Its size is read from the collection, a
//! vector's capacity or a hash table's allocation, and never from a formula. The charge
//! is lowered only after the storage is freed, by [`Charged::shrink`] or [`Charged::clear_free`],
//! and on drop, where the collection is declared before the charge and so freed first.
//!
//! The collection is reachable through `Deref` and through the mutators this module
//! names, never through a `&mut` to the collection, so no caller has a second growth
//! site. A mutator that adds entries asserts in debug builds that room was made for them
//! first, and every growth checks in debug builds that the charge equals the size.
//!
//! # The growth rule
//!
//! Let a buffer charge `old` bytes, its collection's allocation, and let a growth aim for
//! an allocation of `target` bytes. The rule has four parts.
//!
//! 1. Before the collection allocates, the account is resized to `old + target`. Both
//!    blocks are live while the entries move, and a block that the allocator grows in
//!    place is charged the same, since whether it can stay in place depends on what the
//!    allocator holds beside it and not on the operator.
//! 2. If that admission is refused with `BudgetExceeded`, a vector tries once more at
//!    `old + need`, where `need` is the bytes of its exact need, its length plus the new
//!    entries. A refusal of any other kind, a refusal of the exact need and any refusal
//!    of a hash table's growth return the error, since a table has no smaller growth.
//! 3. A refused admission changes nothing. The account, the charge and the collection
//!    stay as they were, and the caller may freeze, spill or fail.
//! 4. After the collection allocates, the account is resized to the size it reports,
//!    `new`, which is at most the admitted target. The `old` bytes are released only
//!    then, after their block is freed.
//!
//! So a growth from `old` to `new` needs `old + target` bytes of headroom at its
//! admission and holds `new` afterwards. A vector's `target` is double its capacity, or
//! the `toward` of [`Charged::reserve_toward`], with at least 16 entries. A hash table's
//! is the bound of what hashbrown allocates for the larger of its need and one more
//! entry than its capacity, which doubles its buckets. The `growth` methods give a
//! buffer's `target` before it grows, so an operator can predict its size. The smallest budget at which a buffer reaches a given length is
//! therefore set by its last growth, the old allocation plus the new one, and not by the
//! length alone. Two buffers of one operator grow one after the other, so their
//! transient peak is the larger of their growths. An operator that keeps several copies
//! of its state, such as one that grows a held run while an earlier run is live, pays
//! this rule for each buffer, and its budget parity against a formula that charged the
//! final size alone moves by the old allocation at each growth.
//!
//! [`Charged::shrink`] is for storage that is freed and not moved. A shrink that copies
//! into a smaller block, as `Vec::shrink_to` may, holds both blocks for the copy while
//! `shrink` admits nothing beyond the old size, so it goes through `grow` with the
//! smaller size instead.
//!
//! A query without a budget or a grant pays for the charge only when a collection
//! grows, which a doubling vector does a logarithmic number of times. Operators reserve
//! the room for a batch at once, so the per-row path is a plain insert.

use crate::error::{Error, Result};
use crate::sparql::ctx::{OwnedCharge, RetainedCharge};
use std::ops::Deref;

/// The account a [`Charged`] holds its bytes on. An operator keeps the account type it
/// already uses, so moving it onto charged buffers does not move it between accounts.
pub(super) trait Account {
    /// Set the account's bytes, admitting growth before the caller allocates it. A
    /// refusal changes nothing.
    fn resize(&mut self, bytes: u64) -> Result<()>;
}

impl Account for RetainedCharge {
    fn resize(&mut self, bytes: u64) -> Result<()> {
        RetainedCharge::resize(self, bytes)
    }
}

impl Account for OwnedCharge {
    fn resize(&mut self, bytes: u64) -> Result<()> {
        OwnedCharge::resize(self, bytes)
    }
}

/// An eager context with no cursor owner makes no retained charge, and a buffer on it
/// is uncharged.
impl Account for Option<RetainedCharge> {
    fn resize(&mut self, bytes: u64) -> Result<()> {
        match self {
            Some(charge) => charge.resize(bytes),
            None => Ok(()),
        }
    }
}

/// A collection whose allocated bytes can be read from the collection itself.
pub(super) trait Storage {
    fn heap_bytes(&self) -> u64;
}

impl<T> Storage for Vec<T> {
    #[inline]
    fn heap_bytes(&self) -> u64 {
        (self.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64)
    }
}

impl<T> Storage for hashbrown::HashTable<T> {
    #[inline]
    fn heap_bytes(&self) -> u64 {
        self.allocation_size() as u64
    }
}

impl<T: Eq + std::hash::Hash, S: std::hash::BuildHasher> Storage for hashbrown::HashSet<T, S> {
    #[inline]
    fn heap_bytes(&self) -> u64 {
        self.allocation_size() as u64
    }
}

/// The fewest entries a charged vector allocates when it grows.
const MIN: usize = 16;

fn no_room() -> Error {
    Error::invalid("a query operator could not allocate its state")
}

/// A collection and the charge for its allocation. See the module.
pub(super) struct Charged<V: Storage, A: Account> {
    /// Declared before the account, so that it is freed before the charge is released.
    value: V,
    /// The bytes this buffer has set on the account, which equal the collection's.
    charged: u64,
    account: A,
}

impl<V: Storage, A: Account> Deref for Charged<V, A> {
    type Target = V;
    #[inline]
    fn deref(&self) -> &V {
        &self.value
    }
}

impl<V: Storage, A: Account> Charged<V, A> {
    /// A buffer over `value` that charges `account`, which holds no bytes yet. The
    /// value's existing allocation is admitted before it is kept.
    pub(super) fn new(value: V, mut account: A) -> Result<Self> {
        let charged = value.heap_bytes();
        account.resize(charged)?;
        Ok(Self {
            value,
            charged,
            account,
        })
    }

    /// The bytes the buffer charges, which are its collection's allocation.
    #[inline]
    pub(super) fn bytes(&self) -> u64 {
        self.charged
    }

    #[inline]
    fn check(&self) {
        debug_assert_eq!(
            self.charged,
            self.value.heap_bytes(),
            "a charged buffer's charge is its allocation"
        );
    }

    /// The one growth site. `grown` is the size of the allocation that `f` makes, or an
    /// upper bound on it, which is admitted beside the old allocation before `f` runs.
    /// Afterwards the charge is set to what the collection reports, whether `f` failed
    /// or not. A refused admission leaves the buffer as it was.
    #[cold]
    fn grow<E>(
        &mut self,
        grown: u64,
        f: impl FnOnce(&mut V) -> std::result::Result<(), E>,
    ) -> Result<()> {
        self.check();
        let old = self.charged;
        self.account.resize(old.saturating_add(grown))?;
        let done = f(&mut self.value);
        let now = self.value.heap_bytes();
        debug_assert!(
            now <= grown.max(old),
            "a grown allocation of {now} bytes above the {grown} admitted"
        );
        self.settle(now)?;
        done.map_err(|_| no_room())
    }

    /// Set the charge to the collection's size, after the collection freed or grew. A
    /// size that rose past what was admitted is charged here, so a release build that
    /// misses a debug assertion still charges what it holds.
    fn settle(&mut self, now: u64) -> Result<()> {
        let result = self.account.resize(now);
        if result.is_ok() {
            self.charged = now;
        }
        result
    }

    /// Run `f`, which frees storage and does not grow it, then lower the charge to what
    /// the collection holds.
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
    fn shrink(&mut self, f: impl FnOnce(&mut V)) -> Result<()> {
        f(&mut self.value);
        let now = self.value.heap_bytes();
        debug_assert!(now <= self.charged, "a shrink grew the allocation");
        self.settle(now)
    }

    /// Free the allocation and release its charge.
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
    pub(super) fn clear_free(&mut self) -> Result<()>
    where
        V: Default,
    {
        self.shrink(|v| *v = V::default())
    }
}

impl<T, A: Account> Charged<Vec<T>, A> {
    /// Room for `more` entries beyond the length. The vector doubles where the budget
    /// allows it and grows to exactly what it needs where it does not.
    #[inline]
    pub(super) fn reserve(&mut self, more: usize) -> Result<()> {
        self.reserve_toward(more, 0)
    }

    /// Room for `more` entries beyond the length, growing to `toward` entries or to
    /// double the capacity when `toward` is zero, where the budget allows it, and to
    /// exactly what it needs where it does not. A buffer that keeps one entry for each
    /// entry of a table grows toward the table's capacity.
    #[inline]
    pub(super) fn reserve_toward(&mut self, more: usize, toward: usize) -> Result<()> {
        if self.value.capacity() - self.value.len() >= more {
            return Ok(());
        }
        self.reserve_slow(more, toward)
    }

    /// The bytes [`Self::reserve_toward`] would allocate for `more` entries where the
    /// budget allows its target, or zero when the vector has room.
    #[inline]
    pub(super) fn growth(&self, more: usize, toward: usize) -> u64 {
        if self.value.capacity() - self.value.len() >= more {
            return 0;
        }
        let need = self.value.len().saturating_add(more);
        Self::size(self.target(need, toward))
    }

    /// The entries a growth to `need` aims for.
    fn target(&self, need: usize, toward: usize) -> usize {
        if toward > 0 {
            need.max(toward)
        } else {
            need.max(self.value.capacity().saturating_mul(2)).max(MIN)
        }
    }

    fn size(to: usize) -> u64 {
        (to as u64).saturating_mul(std::mem::size_of::<T>() as u64)
    }

    #[cold]
    fn reserve_slow(&mut self, more: usize, toward: usize) -> Result<()> {
        let len = self.value.len();
        let need = len.checked_add(more).ok_or_else(no_room)?;
        let size = Self::size;
        let doubled = self.target(need, toward);
        match self.grow(size(doubled), |v| v.try_reserve_exact(doubled - len)) {
            Err(Error::BudgetExceeded(_)) if doubled > need => {
                self.grow(size(need), |v| v.try_reserve_exact(need - len))
            }
            r => r,
        }
    }

    /// Add `items` to room that [`Self::reserve`] made.
    #[inline]
    pub(super) fn extend_reserved(&mut self, items: &[T])
    where
        T: Clone,
    {
        debug_assert!(
            self.value.capacity() - self.value.len() >= items.len(),
            "an extend without room"
        );
        self.value.extend_from_slice(items);
    }

    /// Add an entry, growing the vector first when it is full.
    #[inline]
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
    pub(super) fn push(&mut self, item: T) -> Result<()> {
        self.reserve(1)?;
        self.value.push(item);
        Ok(())
    }

    /// Add `items`, growing the vector first when they do not fit.
    #[inline]
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
    pub(super) fn extend_from_slice(&mut self, items: &[T]) -> Result<()>
    where
        T: Clone,
    {
        self.reserve(items.len())?;
        self.value.extend_from_slice(items);
        Ok(())
    }

    /// Drop the entries and keep the allocation.
    #[inline]
    pub(super) fn clear(&mut self) {
        self.value.clear();
    }
}

/// The bound on what a hashbrown table of `len` entries and room for `capacity` makes
/// when it is asked for `more` entries: room for the larger of what it needs and one
/// more than it has, which doubles its buckets.
fn table_growth<T>(len: usize, capacity: usize, more: usize) -> u64 {
    let target = len.saturating_add(more).max(capacity.saturating_add(1));
    table_bound(target, std::mem::size_of::<T>())
}

/// The buckets a hashbrown table of entries of `entry` bytes allocates for room for
/// `capacity` entries, which are a power of two at most seven eighths full, and at
/// least four. A small table of entries of one to three bytes is given room for at
/// least 14 or 7 entries, so that its buckets fill a control group of 16 bytes. That is
/// the rule for the 16-byte groups of SSE2, and an upper bound for the 8-byte groups of
/// other targets.
fn buckets(capacity: usize, entry: usize) -> usize {
    let capacity = match entry {
        0..=1 => capacity.max(14),
        2..=3 => capacity.max(7),
        _ => capacity,
    };
    if capacity < 8 {
        if capacity < 4 { 4 } else { 8 }
    } else {
        (capacity.saturating_mul(8) / 7).next_power_of_two()
    }
}

impl<T, A: Account> Charged<hashbrown::HashTable<T>, A> {
    /// The room for entries the table has once [`Self::reserve`] makes room for `more`.
    /// A caller that sizes a buffer beside the table from it reads it before the table
    /// grows, to predict the pair's size.
    pub(super) fn capacity_after(&self, more: usize) -> usize {
        let (len, capacity) = (self.value.len(), self.value.capacity());
        if capacity - len >= more {
            return capacity;
        }
        let buckets = buckets(
            len.saturating_add(more).max(capacity.saturating_add(1)),
            std::mem::size_of::<T>(),
        );
        if buckets <= 8 {
            buckets - 1
        } else {
            buckets / 8 * 7
        }
    }

    /// The bound on what [`Self::reserve`] allocates for `more` entries, or zero when
    /// the table has room.
    #[inline]
    pub(super) fn growth(&self, more: usize) -> u64 {
        if self.value.capacity() - self.value.len() >= more {
            return 0;
        }
        table_growth::<T>(self.value.len(), self.value.capacity(), more)
    }

    /// Room for `more` entries beyond the length. The bound of the table's new
    /// allocation is admitted before it allocates, since hashbrown reports its size only
    /// after, and the charge is then set from that size.
    #[inline]
    pub(super) fn reserve(&mut self, more: usize, hasher: impl Fn(&T) -> u64) -> Result<()> {
        if self.value.capacity() - self.value.len() >= more {
            return Ok(());
        }
        let grown = table_growth::<T>(self.value.len(), self.value.capacity(), more);
        self.grow(grown, |t| t.try_reserve(more, hasher))
    }

    /// Add an entry to room that [`Self::reserve`] made.
    #[inline]
    pub(super) fn insert_reserved(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) {
        debug_assert!(
            self.value.len() < self.value.capacity(),
            "an insert without room"
        );
        self.value.insert_unique(hash, value, hasher);
    }

    /// Drop the entries and keep the allocation.
    #[inline]
    pub(super) fn clear(&mut self) {
        self.value.clear();
    }
}

impl<T: Eq + std::hash::Hash, S: std::hash::BuildHasher, A: Account>
    Charged<hashbrown::HashSet<T, S>, A>
{
    /// The bound on what [`Self::reserve`] allocates for `more` entries, or zero when
    /// the set has room.
    #[inline]
    pub(super) fn growth(&self, more: usize) -> u64 {
        if self.value.capacity() - self.value.len() >= more {
            return 0;
        }
        table_growth::<T>(self.value.len(), self.value.capacity(), more)
    }

    /// Room for `more` entries beyond the length, admitted as for a table.
    #[inline]
    pub(super) fn reserve(&mut self, more: usize) -> Result<()> {
        if self.value.capacity() - self.value.len() >= more {
            return Ok(());
        }
        let grown = table_growth::<T>(self.value.len(), self.value.capacity(), more);
        self.grow(grown, |s| s.try_reserve(more))
    }

    /// Add an entry to room that [`Self::reserve`] made. Returns whether it was new.
    #[inline]
    pub(super) fn insert_reserved(&mut self, value: T) -> bool {
        debug_assert!(
            self.value.len() < self.value.capacity(),
            "an insert without room"
        );
        self.value.insert(value)
    }

    /// Drop the entries and keep the allocation.
    #[inline]
    pub(super) fn clear(&mut self) {
        self.value.clear();
    }
}

/// The bytes a hashbrown table of entries of `entry` bytes allocates once it has room
/// for `capacity` of them, or more. It is the bound a caller admits before the table
/// grows, and the charge is then set from the table's own size.
pub(super) fn table_bound(capacity: usize, entry: usize) -> u64 {
    if capacity == 0 {
        return 0;
    }
    let buckets = buckets(capacity, entry);
    // a bucket of `entry` bytes and one control byte, a control group of 16 bytes
    // beyond the buckets, and the alignment of the entries before the control bytes
    (buckets as u64)
        .saturating_mul(entry as u64 + 1)
        .saturating_add(16 + 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql::ctx::Ctx;
    use crate::store::{Store, StoreOptions};
    use std::sync::Arc;

    fn context(limit: u64) -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = limit;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        ctx
    }

    #[test]
    fn a_vector_charges_its_capacity_and_releases_it_on_drop() {
        let ctx = context(1 << 20);
        let base = ctx.live_bytes();
        let mut v = Charged::new(Vec::<u64>::new(), ctx.retained_charge(0).unwrap()).unwrap();
        for i in 0..1000 {
            v.push(i).unwrap();
            assert_eq!(ctx.live_bytes() - base, v.capacity() as u64 * 8);
            assert_eq!(v.bytes(), v.capacity() as u64 * 8);
        }
        // the old and the new storage were admitted together when it last doubled
        assert!(ctx.mem_peak() - base >= (v.capacity() + v.capacity() / 2) as u64 * 8);
        // a free that moves nothing, as `shrink` requires
        v.clear_free().unwrap();
        assert_eq!(ctx.live_bytes(), base);
        drop(v);
        assert_eq!(ctx.live_bytes(), base);
    }

    #[test]
    fn a_refused_growth_leaves_the_buffer_and_its_charge_as_they_were() {
        let ctx = context(64 << 10);
        let base = ctx.live_bytes();
        let mut v = Charged::new(Vec::<u64>::new(), ctx.retained_charge(0).unwrap()).unwrap();
        let mut refused = false;
        for i in 0..100_000 {
            let (len, cap) = (v.len(), v.capacity());
            match v.push(i) {
                Ok(()) => {}
                Err(Error::BudgetExceeded(_)) => {
                    assert_eq!((v.len(), v.capacity()), (len, cap));
                    assert_eq!(ctx.live_bytes() - base, cap as u64 * 8);
                    refused = true;
                    break;
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(refused);
        // a doubling that is refused falls back to the exact need, so the vector is
        // full when the budget refuses it
        assert_eq!(v.len(), v.capacity());
        drop(v);
        assert_eq!(ctx.live_bytes(), base);
    }

    #[test]
    fn hash_tables_charge_their_allocation() {
        let ctx = context(1 << 24);
        let base = ctx.live_bytes();
        let hash = |x: &u64| x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut t = Charged::new(
            hashbrown::HashTable::<u64>::new(),
            ctx.retained_charge(0).unwrap(),
        )
        .unwrap();
        let mut s = Charged::new(
            hashbrown::HashSet::<u64, rustc_hash::FxBuildHasher>::default(),
            ctx.retained_charge(0).unwrap(),
        )
        .unwrap();
        for batch in 0..40u64 {
            t.reserve(100, hash).unwrap();
            s.reserve(100).unwrap();
            for i in batch * 100..batch * 100 + 100 {
                t.insert_reserved(hash(&i), i, hash);
                assert!(s.insert_reserved(i));
            }
            assert_eq!(
                ctx.live_bytes() - base,
                (t.allocation_size() + s.allocation_size()) as u64
            );
        }
        t.clear();
        s.clear_free().unwrap();
        assert_eq!(ctx.live_bytes() - base, t.allocation_size() as u64);
        drop(t);
        drop(s);
        assert_eq!(ctx.live_bytes(), base);
    }

    #[test]
    fn the_predicted_capacity_is_the_tables() {
        let ctx = context(1 << 30);
        let hash = |x: &u32| *x as u64;
        let mut t = Charged::new(
            hashbrown::HashTable::<u32>::new(),
            ctx.retained_charge(0).unwrap(),
        )
        .unwrap();
        for batch in [1, 3, 7, 1, 50, 200, 1, 1000, 5000, 1] {
            let predicted = t.capacity_after(batch);
            t.reserve(batch, hash).unwrap();
            assert_eq!(t.capacity(), predicted, "{} + {batch}", t.len());
            for _ in 0..batch {
                let x = t.len() as u32;
                t.insert_reserved(hash(&x), x, hash);
            }
        }
    }

    #[test]
    fn the_bound_covers_every_table_size() {
        for cap in 0..5000 {
            let mut t = hashbrown::HashTable::<u32>::new();
            t.reserve(cap, |&x| x as u64);
            assert!(t.allocation_size() as u64 <= table_bound(cap, 4), "{cap}");
            let mut t = hashbrown::HashTable::<[u64; 3]>::new();
            t.reserve(cap, |x| x[0]);
            assert!(t.allocation_size() as u64 <= table_bound(cap, 24), "{cap}");
            // Entries of one to three bytes get a larger smallest table.
            let mut t = hashbrown::HashTable::<u8>::new();
            t.reserve(cap, |&x| x as u64);
            assert!(
                t.allocation_size() as u64 <= table_bound(cap, 1),
                "u8 {cap}"
            );
            let mut t = hashbrown::HashTable::<u16>::new();
            t.reserve(cap, |&x| x as u64);
            assert!(
                t.allocation_size() as u64 <= table_bound(cap, 2),
                "u16 {cap}"
            );
        }
    }

    /// A small table of byte entries grows within its admitted bound and predicts its
    /// room, where the bound for four-byte entries fell short of hashbrown's smallest
    /// table of 16 buckets.
    #[test]
    fn a_table_of_bytes_grows_within_its_bound() {
        let ctx = context(1 << 20);
        let mut t = Charged::new(
            hashbrown::HashTable::<u8>::new(),
            ctx.retained_charge(0).unwrap(),
        )
        .unwrap();
        let hash = |&x: &u8| x as u64;
        for x in 0..=255u8 {
            let room = t.capacity_after(1);
            t.reserve(1, hash).unwrap();
            assert_eq!(t.capacity(), room, "{x}");
            t.insert_reserved(x as u64, x, hash);
            assert_eq!(t.bytes(), t.allocation_size() as u64);
        }
    }

    #[test]
    fn an_uncharged_buffer_grows_without_an_account() {
        let mut v = Charged::new(Vec::<u8>::new(), None::<RetainedCharge>).unwrap();
        v.extend_from_slice(&[1; 100]).unwrap();
        assert_eq!(v.bytes(), v.capacity() as u64);
    }
}
