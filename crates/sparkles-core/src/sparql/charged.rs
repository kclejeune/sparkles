//! Buffers whose memory charge is their allocation (item 1183).
//!
//! A [`Charged`] owns one growable collection and the charge for it. It grows in one
//! place, [`Charged::grow`], which admits the old allocation and the new one together
//! before it allocates, since both are live while the entries move, and then sets the
//! charge to the size the collection reports. Its size is read from the collection, a
//! vector's capacity or a hash table's allocation, and never from a formula. The charge
//! is lowered only after the storage is freed, by [`Charged::clear_free`], and on drop,
//! where the collection is declared before the charge and so freed first. A buffer
//! starts empty, since a hash table's growth is predicted from its length and room, and
//! a table with removed entries grows sooner than that.
//!
//! The collection is reachable through `Deref` and through the mutators this module
//! names, never through a `&mut` to the collection, so no caller has a second growth
//! site. A mutator that adds entries to reserved room checks in release builds that the
//! room was made and panics before it changes anything, and every growth checks in debug
//! builds that the charge equals the size.
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
//! buffer's `target` before it grows, so an operator can predict its size. The smallest
//! budget at which a buffer reaches a given length is therefore set by its last growth,
//! the old allocation plus the new one, and not by the length alone. Buffers that grow
//! together for each batch are one buffer, as [`Rows`] holds a table and its arena, and
//! admit their sequential growths as one before either allocates, so a refusal never
//! leaves one grown and the other not. An operator that keeps several copies
//! of its state, such as one that grows a held run while an earlier run is live, pays
//! this rule for each buffer, and its budget parity against a formula that charged the
//! final size alone moves by the old allocation at each growth.
//!
//! The private `shrink` is for storage that is freed and not moved. A shrink that copies
//! into a smaller block, as `Vec::shrink_to` may, holds both blocks for the copy while
//! `shrink` admits nothing beyond the old size, so it goes through `grow` with the
//! smaller size instead.
//!
//! A query without a budget or a grant pays for the charge only when a collection
//! grows, which a doubling vector does a logarithmic number of times. Operators reserve
//! the room for a batch at once, so the per-row path is a plain insert.

use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{OwnedCharge, RetainedCharge};
use rustc_hash::FxBuildHasher;
use std::hash::BuildHasher;
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
    /// An empty buffer that charges `account`, which holds no bytes yet. A buffer is
    /// never built over a collection that already holds entries, since a hash table's
    /// growth is predicted from its length and room alone, and a table with removed
    /// entries grows sooner than those predict.
    pub(super) fn empty(account: A) -> Result<Self>
    where
        V: Default,
    {
        Self::new(V::default(), account)
    }

    /// A buffer over `value`, which holds no entries, that charges `account`. Its
    /// allocation is admitted before it is kept.
    fn new(value: V, mut account: A) -> Result<Self> {
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
    #[allow(dead_code, reason = "tests and the operators of item 1192 read it")]
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

    /// The one growth site. `admit` bytes beyond the old allocation are admitted
    /// before `f` runs, which for one collection is the size of the allocation that `f`
    /// makes, or an upper bound on it. `bound` is an upper bound on the size the
    /// collection reports after `f`. Afterwards the charge is set to what the
    /// collection reports, whether `f` failed or not. A refused admission leaves the
    /// buffer as it was.
    #[cold]
    fn grow<E>(
        &mut self,
        admit: u64,
        bound: u64,
        f: impl FnOnce(&mut V) -> std::result::Result<(), E>,
    ) -> Result<()> {
        self.check();
        let old = self.charged;
        self.account.resize(old.saturating_add(admit))?;
        let done = f(&mut self.value);
        let now = self.value.heap_bytes();
        debug_assert!(
            now <= bound.max(old) && now <= old.saturating_add(admit),
            "a grown allocation of {now} bytes above the {bound} predicted"
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
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
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
        let grow = |this: &mut Self, to: usize| {
            this.grow(size(to), size(to), |v| v.try_reserve_exact(to - len))
        };
        match grow(self, doubled) {
            Err(Error::BudgetExceeded(_)) if doubled > need => grow(self, need),
            r => r,
        }
    }

    /// Add `items` to room that [`Self::reserve`] made.
    #[inline]
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
    pub(super) fn extend_reserved(&mut self, items: &[T])
    where
        T: Clone,
    {
        // A check in release builds too: an extend past the room would allocate
        // what no admission covered. It panics before the vector changes.
        assert!(
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
    #[allow(dead_code, reason = "the operators of item 1192 use it")]
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

/// The room for entries a hashbrown table of `len` entries and room for `capacity` has
/// once it makes room for `more`.
fn table_capacity_after<T>(len: usize, capacity: usize, more: usize) -> usize {
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

#[allow(dead_code, reason = "the operators of item 1192 use it")]
impl<T, A: Account> Charged<hashbrown::HashTable<T>, A> {
    /// The room for entries the table has once [`Self::reserve`] makes room for `more`.
    /// A caller that sizes a buffer beside the table from it reads it before the table
    /// grows, to predict the pair's size.
    pub(super) fn capacity_after(&self, more: usize) -> usize {
        table_capacity_after::<T>(self.value.len(), self.value.capacity(), more)
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
        self.grow(grown, grown, |t| t.try_reserve(more, hasher))
    }

    /// Add an entry to room that [`Self::reserve`] made.
    #[inline]
    pub(super) fn insert_reserved(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) {
        // A check in release builds too, as for `extend_reserved`.
        assert!(
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
        self.grow(grown, grown, |s| s.try_reserve(more))
    }

    /// Add an entry to room that [`Self::reserve`] made. Returns whether it was new.
    #[inline]
    pub(super) fn insert_reserved(&mut self, value: T) -> bool {
        // A check in release builds too, as for `extend_reserved`.
        assert!(
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

/// A set of rows of `width` IDs each: a flat arena of the rows' IDs and a hash table of
/// their row numbers, which hashes and compares a row by its IDs in the arena. The two
/// grow together for each batch, so one [`Charged`] holds both on one charge, and a
/// growth admits the pair before either allocates. The table grows first, while the
/// old arena is live, and then the arena, while the new table is live, so the pair
/// needs `max(T, T' - t + A)` bytes beyond what it holds, where `t` is the old table,
/// `T` the table's growth and `T'` its size after, and `A` the arena's growth.
pub(super) struct Rows {
    table: hashbrown::HashTable<u32>,
    ids: Vec<Id>,
    width: usize,
}

impl Storage for Rows {
    #[inline]
    fn heap_bytes(&self) -> u64 {
        self.table_bytes().saturating_add(self.arena_bytes())
    }
}

/// The hash of row `r` of an arena of rows of `width` IDs.
#[inline]
fn row_hash(ids: &[Id], width: usize, r: u32) -> u64 {
    let r = r as usize;
    FxBuildHasher.hash_one(&ids[r * width..(r + 1) * width])
}

impl Rows {
    /// The number of rows.
    #[inline]
    pub(super) fn len(&self) -> usize {
        self.table.len()
    }

    /// The rows the table has room for.
    #[inline]
    pub(super) fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// The IDs the arena holds.
    #[cfg(test)]
    pub(super) fn arena_len(&self) -> usize {
        self.ids.len()
    }

    /// The IDs the arena has room for.
    #[cfg(test)]
    pub(super) fn arena_capacity(&self) -> usize {
        self.ids.capacity()
    }

    /// The bytes of the table's allocation.
    #[inline]
    pub(super) fn table_bytes(&self) -> u64 {
        self.table.allocation_size() as u64
    }

    /// The bytes of the arena's allocation.
    #[inline]
    pub(super) fn arena_bytes(&self) -> u64 {
        Storage::heap_bytes(&self.ids)
    }

    /// The hash of a row of `width` IDs.
    #[inline]
    pub(super) fn hash(key: &[Id]) -> u64 {
        FxBuildHasher.hash_one(key)
    }

    /// Whether the set holds `key`, whose hash is `hash`.
    #[inline]
    pub(super) fn contains(&self, key: &[Id], hash: u64) -> bool {
        let (ids, width) = (&self.ids, self.width);
        self.table
            .find(hash, |&r| {
                let r = r as usize;
                ids[r * width..(r + 1) * width] == *key
            })
            .is_some()
    }

    /// What a growth for `more` rows makes: whether the table and the arena grow, the
    /// table's bound, and the arena's IDs where the budget allows the arena to grow to
    /// the table's room and where it does not.
    fn plan(&self, more: usize) -> Option<RowsGrowth> {
        let (len, capacity) = (self.table.len(), self.table.capacity());
        let need = self.ids.len().checked_add(more.checked_mul(self.width)?)?;
        let table = capacity - len < more;
        let arena = self.ids.capacity() < need;
        if !table && !arena {
            return None;
        }
        // The arena grows toward the rows the table has room for, which keeps the two
        // in step and keeps the arena from doubling past the table.
        let toward = table_capacity_after::<u32>(len, capacity, more).saturating_mul(self.width);
        Some(RowsGrowth {
            table: table.then(|| table_growth::<u32>(len, capacity, more)),
            arena: arena.then_some((need.max(toward), need)),
        })
    }

    fn admit(&self, growth: &RowsGrowth, ids: Option<usize>) -> (u64, u64) {
        let old = self.table_bytes();
        let grown = growth.table.unwrap_or(0);
        let after = growth.table.unwrap_or(old);
        let arena = ids.map_or(0, ids_size);
        let admit = grown.max(after.saturating_sub(old).saturating_add(arena));
        let bound = after.saturating_add(ids.map_or(self.arena_bytes(), ids_size));
        (admit, bound)
    }
}

/// The bytes of an arena of `ids` IDs.
fn ids_size(ids: usize) -> u64 {
    (ids as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
}

/// What [`Rows::plan`] finds a growth makes.
struct RowsGrowth {
    /// The table's bound, when it grows.
    table: Option<u64>,
    /// The arena's IDs where its growth to the table's room fits and where only its
    /// need does, when it grows.
    arena: Option<(usize, usize)>,
}

impl<A: Account> Charged<Rows, A> {
    /// An empty set of rows of `width` IDs that charges `account`.
    pub(super) fn rows(width: usize, account: A) -> Result<Self> {
        assert!(width > 0, "a row of no IDs");
        Self::new(
            Rows {
                table: hashbrown::HashTable::new(),
                ids: Vec::new(),
                width,
            },
            account,
        )
    }

    /// The bytes the set holds once [`Self::reserve`] makes room for `more` rows where
    /// the budget allows the arena to grow to the table's room, or zero when the set has
    /// room. A caller reads it to predict the set's size before it grows.
    pub(super) fn growth(&self, more: usize) -> u64 {
        match self.value.plan(more) {
            None => 0,
            Some(growth) => {
                let ids = growth.arena.map(|(preferred, _)| preferred);
                self.value.admit(&growth, ids).1
            }
        }
    }

    /// The bytes of the new allocations that [`Self::reserve`] makes for `more` rows
    /// where the budget allows the arena to grow to the table's room, the new table's
    /// bound and the new arena together, or zero when the set has room. It counts each
    /// new allocation whole, while the growth's admission counts only what the set holds
    /// beyond its charge at the growth's peak.
    pub(super) fn allocations(&self, more: usize) -> u64 {
        match self.value.plan(more) {
            None => 0,
            Some(growth) => growth
                .table
                .unwrap_or(0)
                .saturating_add(growth.arena.map_or(0, |(preferred, _)| ids_size(preferred))),
        }
    }

    /// Room for `more` rows. The growth of the table and of the arena is admitted as
    /// one before either allocates, with the arena at the table's room where the budget
    /// allows it and at what it needs where it does not. A refusal leaves both as they
    /// were.
    pub(super) fn reserve(&mut self, more: usize) -> Result<()> {
        let Some(growth) = self.value.plan(more) else {
            if more.checked_mul(self.value.width).is_none() {
                return Err(no_room());
            }
            return Ok(());
        };
        let grow = |this: &mut Self, ids: Option<usize>| {
            let (admit, bound) = this.value.admit(&growth, ids);
            let table = growth.table.is_some();
            this.grow(admit, bound, |v: &mut Rows| {
                let Rows {
                    table: t,
                    ids: arena,
                    width,
                } = v;
                if table {
                    t.try_reserve(more, |&r| row_hash(arena, *width, r))
                        .map_err(|_| ())?;
                }
                if let Some(to) = ids {
                    arena.try_reserve_exact(to - arena.len()).map_err(|_| ())?;
                }
                Ok::<(), ()>(())
            })
        };
        match growth.arena {
            Some((preferred, need)) if preferred > need => match grow(self, Some(preferred)) {
                Err(Error::BudgetExceeded(_)) => grow(self, Some(need)),
                r => r,
            },
            arena => grow(self, arena.map(|(_, need)| need)),
        }
    }

    /// Add `key`, a row of the set's width whose hash is `hash`, to room that
    /// [`Self::reserve`] made, unless the set holds it. Returns whether it was new.
    #[inline]
    pub(super) fn insert_reserved(&mut self, key: &[Id], hash: u64) -> bool {
        if self.value.contains(key, hash) {
            return false;
        }
        let Rows { table, ids, width } = &mut self.value;
        let width = *width;
        // Checks in release builds too, as for `extend_reserved`. They panic before
        // the set changes.
        assert_eq!(key.len(), width, "a row of another width");
        assert!(
            table.len() < table.capacity() && ids.capacity() - ids.len() >= width,
            "an insert without room"
        );
        let number = u32::try_from(table.len()).expect("a set of fewer than 2^32 rows");
        ids.extend_from_slice(key);
        table.insert_unique(hash, number, |&r| row_hash(ids, width, r));
        true
    }

    /// Drop the rows and keep the allocations.
    #[inline]
    pub(super) fn clear(&mut self) {
        self.value.table.clear();
        self.value.ids.clear();
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
        let mut v = Charged::<Vec<u64>, _>::empty(ctx.retained_charge(0).unwrap()).unwrap();
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
        let mut v = Charged::<Vec<u64>, _>::empty(ctx.retained_charge(0).unwrap()).unwrap();
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
        let mut t = Charged::<hashbrown::HashTable<u64>, _>::empty(ctx.retained_charge(0).unwrap())
            .unwrap();
        let mut s = Charged::<hashbrown::HashSet<u64, rustc_hash::FxBuildHasher>, _>::empty(
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
        let mut t = Charged::<hashbrown::HashTable<u32>, _>::empty(ctx.retained_charge(0).unwrap())
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
        let mut t =
            Charged::<hashbrown::HashTable<u8>, _>::empty(ctx.retained_charge(0).unwrap()).unwrap();
        let hash = |&x: &u8| x as u64;
        for x in 0..=255u8 {
            let room = t.capacity_after(1);
            t.reserve(1, hash).unwrap();
            assert_eq!(t.capacity(), room, "{x}");
            t.insert_reserved(x as u64, x, hash);
            assert_eq!(t.bytes(), t.allocation_size() as u64);
        }
    }

    fn ids(xs: &[i64]) -> Vec<Id> {
        xs.iter().map(|&x| Id::from_i64(x).unwrap()).collect()
    }

    /// The bytes a context holds beyond `base`.
    fn held(ctx: &Ctx, base: u64) -> u64 {
        ctx.live_bytes() - base
    }

    /// A set of rows admits its table's and its arena's growth as one, the larger of the
    /// table's growth and the table's growth less the old table plus the arena's, and a
    /// refusal leaves both. With the arena at the table's room refused it grows to what
    /// it needs.
    #[test]
    fn a_set_of_rows_admits_its_pair_before_either_grows() {
        for width in [1, 2, 3] {
            // the room the second growth needs, then a budget one byte short of it
            let grow = |limit: Option<u64>, exact: bool| -> std::result::Result<u64, u64> {
                let ctx = context(limit.unwrap_or(1 << 24));
                let base = ctx.live_bytes();
                let mut set = Charged::rows(width, ctx.retained_charge(0).unwrap()).unwrap();
                let row = |i: usize| ids(&vec![i as i64; width]);
                set.reserve(32).unwrap();
                for i in 0..32 {
                    assert!(set.insert_reserved(&row(i), Rows::hash(&row(i))));
                }
                let (t, a) = (set.table_bytes(), set.arena_bytes());
                let (cap, arena) = (set.capacity(), set.arena_capacity());
                assert_eq!(held(&ctx, base), t + a);
                let predicted = set.growth(40);
                let before = ctx.mem_peak();
                match set.reserve(40) {
                    Ok(()) => {
                        // the table's bound and the arena at the table's room, or at its
                        // need
                        let ids = if exact { 72 * width } else { 112 * width };
                        assert_eq!(set.arena_capacity(), ids);
                        assert_eq!(set.capacity(), 112);
                        if !exact {
                            assert!(set.bytes() <= predicted);
                        }
                        assert_eq!(held(&ctx, base), set.bytes());
                        let table = predicted - 112 * width as u64 * 8;
                        let arena = ids as u64 * 8;
                        let admitted = t + a + table.max(table - t + arena);
                        assert!(ctx.mem_peak().max(before) - base >= admitted - 16);
                        Ok(admitted)
                    }
                    Err(Error::BudgetExceeded(_)) => {
                        assert_eq!((set.capacity(), set.arena_capacity()), (cap, arena));
                        assert_eq!((set.table_bytes(), set.arena_bytes()), (t, a));
                        assert_eq!(held(&ctx, base), t + a);
                        assert_eq!(set.len(), 32);
                        Err(base)
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            let full = grow(None, false).unwrap();
            let ctx = context(1 << 24);
            let base = ctx.live_bytes();
            drop(ctx);
            // the budget a growth needs, the context's own charges included
            let at = |admitted: u64| Some(base + admitted);
            grow(at(full), false).unwrap();
            let exact = grow(at(full - 1), true).unwrap();
            assert!(exact < full);
            grow(at(exact), true).unwrap();
            grow(at(exact - 1), true).unwrap_err();
        }
    }

    /// A growth of the arena alone admits the arena, and a set that has room admits
    /// nothing. Rows already held are not added twice.
    #[test]
    fn a_set_of_rows_grows_its_arena_alone_and_keeps_rows_once() {
        let ctx = context(1 << 24);
        let base = ctx.live_bytes();
        let mut set = Charged::rows(2, ctx.retained_charge(0).unwrap()).unwrap();
        set.reserve(3).unwrap();
        let (table, arena) = (set.capacity(), set.arena_capacity());
        assert_eq!(table, 3);
        assert_eq!(arena, 6);
        for i in 0..3 {
            let row = ids(&[i, i]);
            assert!(set.insert_reserved(&row, Rows::hash(&row)));
            assert!(!set.insert_reserved(&row, Rows::hash(&row)));
        }
        assert_eq!(set.growth(0), 0);
        set.reserve(0).unwrap();
        assert_eq!(set.bytes(), held(&ctx, base));
        set.clear();
        assert_eq!((set.capacity(), set.arena_capacity()), (table, arena));
        drop(set);
        assert_eq!(ctx.live_bytes(), base);
    }

    /// Each reserved mutator checks its room in release builds too, and panics before
    /// it changes the buffer or its charge.
    #[test]
    fn reserved_mutators_without_room_panic_before_they_change_anything() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let ctx = context(1 << 24);
        let base = ctx.live_bytes();
        let mut v = Charged::<Vec<u64>, _>::empty(ctx.retained_charge(0).unwrap()).unwrap();
        v.reserve(16).unwrap();
        v.extend_reserved(&[]);
        v.extend_reserved(&[7; 16]);
        let (len, cap, bytes) = (v.len(), v.capacity(), v.bytes());
        assert!(catch_unwind(AssertUnwindSafe(|| v.extend_reserved(&[1]))).is_err());
        assert_eq!((v.len(), v.capacity(), v.bytes()), (len, cap, bytes));

        let hash = |x: &u64| x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut t = Charged::<hashbrown::HashTable<u64>, _>::empty(ctx.retained_charge(0).unwrap())
            .unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| t.insert_reserved(hash(&1), 1, hash))).is_err());
        assert_eq!((t.len(), t.capacity(), t.bytes()), (0, 0, 0));
        t.reserve(3, hash).unwrap();
        let room = t.capacity();
        for x in 0..room as u64 {
            t.insert_reserved(hash(&x), x, hash);
        }
        let bytes = t.bytes();
        assert!(catch_unwind(AssertUnwindSafe(|| t.insert_reserved(hash(&99), 99, hash))).is_err());
        assert_eq!((t.len(), t.capacity(), t.bytes()), (room, room, bytes));

        let mut s = Charged::<hashbrown::HashSet<u64, rustc_hash::FxBuildHasher>, _>::empty(
            ctx.retained_charge(0).unwrap(),
        )
        .unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| s.insert_reserved(1))).is_err());
        assert_eq!((s.len(), s.capacity(), s.bytes()), (0, 0, 0));

        let mut r = Charged::rows(2, ctx.retained_charge(0).unwrap()).unwrap();
        let row = ids(&[1, 2]);
        assert!(
            catch_unwind(AssertUnwindSafe(
                || r.insert_reserved(&row, Rows::hash(&row))
            ))
            .is_err()
        );
        assert_eq!((r.len(), r.arena_len(), r.bytes()), (0, 0, 0));
        r.reserve(1).unwrap();
        let wide = ids(&[1, 2, 3]);
        assert!(
            catch_unwind(AssertUnwindSafe(
                || r.insert_reserved(&wide, Rows::hash(&wide))
            ))
            .is_err()
        );
        assert_eq!(r.len(), 0);
        drop((v, t, s, r));
        assert_eq!(ctx.live_bytes(), base);
    }

    #[test]
    fn an_uncharged_buffer_grows_without_an_account() {
        let mut v = Charged::<Vec<u8>, _>::empty(None::<RetainedCharge>).unwrap();
        v.extend_from_slice(&[1; 100]).unwrap();
        assert_eq!(v.bytes(), v.capacity() as u64);
    }
}
