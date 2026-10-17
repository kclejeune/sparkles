//! Fixed-capacity call-owned memory. Pinned Rust Global Vec capacity excludes allocator
//! bookkeeping and Rayon scheduler allocations.
use super::{CHECK_EVERY, Control, bad};
use crate::error::Result;
use std::ops::{Deref, DerefMut};

pub(crate) fn bytes<T>(capacity: usize) -> Result<u64> {
    capacity
        .checked_mul(std::mem::size_of::<T>())
        .filter(|&n| n <= isize::MAX as usize)
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| bad("memory", "buffer capacity overflows"))
}

pub(crate) fn sum(parts: &[u64]) -> Result<u64> {
    parts
        .iter()
        .try_fold(0u64, |s, &v| s.checked_add(v))
        .ok_or_else(|| bad("memory", "buffer capacities overflow"))
}

pub(crate) struct Reservation {
    ctl: Control,
    bytes: u64,
}
impl Reservation {
    pub(crate) fn new(ctl: &Control, bytes: u64) -> Result<Self> {
        ctl.check()?;
        ctl.charge(bytes)?;
        Ok(Self {
            ctl: ctl.clone(),
            bytes,
        })
    }
    // Vocab reconstruction retains old+new capacity while its internal Vec grows.
    pub(crate) fn grow_to(&mut self, bytes: u64) -> Result<()> {
        self.ctl.check()?;
        if bytes > self.bytes {
            self.ctl.charge(bytes - self.bytes)?;
            self.bytes = bytes;
        }
        Ok(())
    }
    /// Hold exactly `bytes`: charge more, or give back what is no longer needed.
    pub(crate) fn set(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.bytes {
            self.grow_to(bytes)
        } else {
            self.ctl.release(self.bytes - bytes);
            self.bytes = bytes;
            Ok(())
        }
    }
    pub(crate) fn belongs_to(&self, ctl: &Control) -> bool {
        self.ctl.same(ctl)
    }
    fn keep(&mut self) {
        self.bytes = 0;
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.ctl.release(self.bytes);
    }
}

pub(crate) fn allocate<T>(name: &'static str, capacity: usize, ctl: &Control) -> Result<Vec<T>> {
    ctl.check()?;
    bytes::<T>(capacity)?;
    #[cfg(test)]
    {
        probe::at(probe::Point::Allocation(name, capacity));
        if probe::fails(name) {
            return Err(bad("memory", "injected allocation failure"));
        }
    }
    #[cfg(any(test, feature = "failpoints"))]
    super::observe_projection(super::ProjectionEvent::Allocation(
        name,
        capacity,
        bytes::<T>(capacity)?,
        ctl.observed_used(),
    ));
    let _ = name;
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|e| bad("memory", format!("buffer allocation failed: {e}")))?;
    if std::mem::size_of::<T>() != 0 && values.capacity() != capacity {
        return Err(bad("memory", "allocator returned an unsupported capacity"));
    }
    Ok(values)
}

// Fields drop in declaration order, so allocation dies before reservation.
pub(crate) struct Buffer<T> {
    values: Vec<T>,
    charge: Reservation,
}
impl<T> Buffer<T> {
    pub(crate) fn new(name: &'static str, capacity: usize, ctl: &Control) -> Result<Self> {
        let charge = Reservation::new(ctl, bytes::<T>(capacity)?)?;
        let values = allocate(name, capacity, ctl)?;
        #[cfg(test)]
        probe::at(probe::Point::Physical(
            name,
            bytes::<T>(values.capacity())?,
            true,
        ));
        Ok(Self { values, charge })
    }
    pub(crate) fn adopt(values: Vec<T>, ctl: &Control) -> Result<Self> {
        let charge = Reservation::new(ctl, bytes::<T>(values.capacity())?)?;
        #[cfg(test)]
        probe::at(probe::Point::Physical(
            "adopt",
            bytes::<T>(values.capacity())?,
            true,
        ));
        Ok(Self { values, charge })
    }
    pub(crate) fn capacity(&self) -> usize {
        self.values.capacity()
    }
    pub(crate) fn belongs_to(&self, ctl: &Control) -> bool {
        self.charge.belongs_to(ctl)
    }
    pub(crate) fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }
    pub(crate) fn dedup(&mut self, ctl: &Control) -> Result<()>
    where
        T: PartialEq,
    {
        let mut work = Work::new(ctl)?;
        let mut kept = usize::from(!self.values.is_empty());
        for i in 1..self.values.len() {
            work.tick("buffer-dedup")?;
            if self.values[i] != self.values[kept - 1] {
                self.values.swap(i, kept);
                kept += 1;
            }
        }
        self.values.truncate(kept);
        ctl.check()
    }
    pub(crate) fn reserve(&mut self, name: &'static str, need: usize, ctl: &Control) -> Result<()> {
        if !self.belongs_to(ctl) {
            return Err(bad("memory", "buffer belongs to another call"));
        }
        if need <= self.capacity() {
            return Ok(());
        }
        let capacity = self
            .capacity()
            .checked_mul(2)
            .map(|n| n.max(4).max(need))
            .ok_or_else(|| bad("memory", "buffer growth overflows"))?;
        let mut next = Self::new(name, capacity, ctl)?;
        let mut work = Work::new(ctl)?;
        for value in self.values.drain(..) {
            work.tick("buffer-move")?;
            next.push(value)?;
        }
        ctl.check()?;
        *self = next; // old backing allocation drops before its reservation.
        Ok(())
    }
    pub(crate) fn extend(&mut self, values: &[T]) -> Result<()>
    where
        T: Copy,
    {
        let ctl = self.charge.ctl.clone();
        let mut work = Work::new(&ctl)?;
        for &value in values {
            work.tick("buffer-copy")?;
            self.push(value)?;
        }
        Ok(())
    }
    pub(crate) fn clear(&mut self) {
        self.values.clear();
    }
    pub(crate) fn drain(&mut self) -> std::vec::Drain<'_, T> {
        self.values.drain(..)
    }
    #[inline]
    pub(crate) fn push(&mut self, value: T) -> Result<()> {
        if self.values.len() == self.values.capacity() {
            return Err(bad("memory", "fixed buffer capacity exceeded"));
        }
        self.values.push(value);
        Ok(())
    }
    /// Append a value the caller knows fits, as a hot loop that appends each node at
    /// most once to a buffer of one entry per node does. A value past the capacity is
    /// an error of the caller, which a debug build stops at and a release build keeps
    /// correct at the price of an uncharged growth.
    #[inline]
    pub(crate) fn push_within(&mut self, value: T) {
        debug_assert!(self.values.len() < self.values.capacity());
        self.values.push(value);
    }
    /// The values that still fit in the capacity.
    #[inline]
    pub(crate) fn room(&self) -> usize {
        self.values.capacity() - self.values.len()
    }
    /// Append `values` within the capacity, without a check per value in a hot loop.
    /// Fails, keeping the values that fit, when they do not all fit.
    #[inline]
    pub(crate) fn extend_within(&mut self, values: impl IntoIterator<Item = T>) -> Result<()> {
        let room = self.values.capacity() - self.values.len();
        let mut values = values.into_iter();
        self.values.extend(values.by_ref().take(room));
        if values.next().is_some() {
            return Err(bad("memory", "fixed buffer capacity exceeded"));
        }
        Ok(())
    }
    /// A buffer of `capacity` default values, charged before it is allocated. Unlike
    /// [`Buffer::filled`] it fills with one pass of the allocator's zeroing, so a large
    /// array of numbers costs no per-element bookkeeping.
    pub(crate) fn zeroed(name: &'static str, capacity: usize, ctl: &Control) -> Result<Self>
    where
        T: Copy + Default,
    {
        let mut b = Self::new(name, capacity, ctl)?;
        b.values.resize(capacity, T::default());
        ctl.check()?;
        Ok(b)
    }
    /// A buffer of `capacity` copies of `value`, charged before it is allocated and
    /// filled in one pass, like [`Buffer::zeroed`].
    pub(crate) fn splat(
        name: &'static str,
        capacity: usize,
        value: T,
        ctl: &Control,
    ) -> Result<Self>
    where
        T: Clone,
    {
        let mut b = Self::new(name, capacity, ctl)?;
        b.values.resize(capacity, value);
        ctl.check()?;
        Ok(b)
    }
    /// A buffer of the first `capacity` values of `values`, charged before it is
    /// allocated.
    pub(crate) fn collect(
        name: &'static str,
        capacity: usize,
        values: impl IntoIterator<Item = T>,
        ctl: &Control,
    ) -> Result<Self> {
        let mut b = Self::new(name, capacity, ctl)?;
        b.values.extend(values.into_iter().take(capacity));
        ctl.check()?;
        Ok(b)
    }
    pub(crate) fn filled(
        name: &'static str,
        capacity: usize,
        value: T,
        ctl: &Control,
    ) -> Result<Self>
    where
        T: Copy,
    {
        let mut b = Self::new(name, capacity, ctl)?;
        let mut work = Work::new(ctl)?;
        for _ in 0..capacity {
            work.tick("buffer-fill")?;
            b.push(value)?;
        }
        Ok(b)
    }
    pub(crate) fn into_output(mut self) -> Vec<T> {
        let values = std::mem::take(&mut self.values);
        self.charge.keep();
        values
    }
}
#[cfg(test)]
impl<T> Drop for Buffer<T> {
    fn drop(&mut self) {
        probe::at(probe::Point::Physical(
            "buffer-drop",
            bytes::<T>(self.values.capacity()).unwrap(),
            false,
        ));
    }
}
impl<T> Deref for Buffer<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.values
    }
}
impl<T> DerefMut for Buffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.values
    }
}

pub(crate) struct Work<'a> {
    ctl: &'a Control,
    left: usize,
}
impl<'a> Work<'a> {
    pub(crate) fn new(ctl: &'a Control) -> Result<Self> {
        ctl.check()?;
        Ok(Self {
            ctl,
            left: CHECK_EVERY,
        })
    }
    #[inline(always)]
    pub(crate) fn tick(&mut self, phase: &'static str) -> Result<()> {
        #[cfg(test)]
        probe::at(probe::Point::Work(phase));
        let _ = phase;
        self.left -= 1;
        if self.left == 0 {
            return self.refill();
        }
        Ok(())
    }

    /// Check the control and start a new interval: the rare path of a count, kept out
    /// of the loops that count.
    #[cold]
    #[inline(never)]
    fn refill(&mut self) -> Result<()> {
        self.ctl.check()?;
        self.left = CHECK_EVERY;
        Ok(())
    }

    /// Count `units` operations at once, such as the edges of one row, and check the
    /// control when the count since the last check reaches [`CHECK_EVERY`]. A loop that
    /// counts a row at a time costs one subtraction per row, and a hub row is still
    /// polled before the next row starts.
    #[inline(always)]
    pub(crate) fn tick_n(&mut self, units: usize, phase: &'static str) -> Result<()> {
        #[cfg(test)]
        probe::at(probe::Point::Work(phase));
        let _ = phase;
        if units >= self.left {
            return self.refill();
        }
        self.left -= units;
        Ok(())
    }

    /// Count the rows `lo..hi` of a sparse row array with offsets `off` at once, each
    /// row as its edges and one more, when together they hold at most [`CHECK_EVERY`]
    /// units, so the loop over them needs no count per row. Returns false, counting
    /// nothing, when they hold more: the caller then counts each row with
    /// [`Work::row`] before it reads it, so a hub row is still polled before the next
    /// row starts.
    #[inline(always)]
    pub(crate) fn rows(
        &mut self,
        off: &[u64],
        lo: usize,
        hi: usize,
        phase: &'static str,
    ) -> Result<bool> {
        let units = (off[hi] - off[lo]) as usize + (hi - lo);
        if units > CHECK_EVERY {
            return Ok(false);
        }
        self.tick_n(units, phase)?;
        Ok(true)
    }

    /// Count row `u` of a sparse row array with offsets `off`, unless [`Work::rows`]
    /// counted its block already.
    #[inline(always)]
    pub(crate) fn row(
        &mut self,
        counted: bool,
        off: &[u64],
        u: usize,
        phase: &'static str,
    ) -> Result<()> {
        if counted {
            return Ok(());
        }
        self.tick_n((off[u + 1] - off[u]) as usize + 1, phase)
    }
}

/// A growable vector whose capacity is charged to a call before it grows. A growth
/// holds the old and the new capacity while the values move, and keeps only the new
/// one after. The allocation is fallible, so a failure is an error and not an abort.
pub(crate) struct Growing<T> {
    values: Vec<T>,
    charge: Reservation,
    name: &'static str,
}

impl<T> Growing<T> {
    pub(crate) fn new(name: &'static str, ctl: &Control) -> Result<Self> {
        Ok(Self {
            values: Vec::new(),
            charge: Reservation::new(ctl, 0)?,
            name,
        })
    }

    /// Make room for `extra` more values, growing the capacity at least twofold.
    #[inline]
    pub(crate) fn reserve(&mut self, extra: usize) -> Result<()> {
        if self.values.capacity() - self.values.len() >= extra {
            return Ok(());
        }
        self.grow(extra)
    }

    #[cold]
    fn grow(&mut self, extra: usize) -> Result<()> {
        let need = self
            .values
            .len()
            .checked_add(extra)
            .ok_or_else(|| bad("memory", "buffer growth overflows"))?;
        if need <= self.values.capacity() {
            return Ok(());
        }
        let old = self.values.capacity();
        let capacity = old
            .checked_mul(2)
            .map(|c| c.max(need).max(16))
            .ok_or_else(|| bad("memory", "buffer growth overflows"))?;
        let (was, next) = (bytes::<T>(old)?, bytes::<T>(capacity)?);
        self.charge.grow_to(sum(&[was, next])?)?;
        #[cfg(test)]
        if probe::fails(self.name) {
            return Err(bad("memory", "injected allocation failure"));
        }
        let _ = self.name;
        self.values
            .try_reserve_exact(capacity - self.values.len())
            .map_err(|e| bad("memory", format!("buffer allocation failed: {e}")))?;
        self.charge.set(bytes::<T>(self.values.capacity())?)
    }

    #[inline]
    pub(crate) fn push(&mut self, value: T) -> Result<()> {
        if self.values.len() == self.values.capacity() {
            self.reserve(1)?;
        }
        self.values.push(value);
        Ok(())
    }

    /// Append a value after [`Growing::reserve`] made room for it, without a check that
    /// can fail. A value past the reserved room is an error of the caller, which a debug
    /// build stops at and a release build keeps correct at the price of an uncharged
    /// growth.
    #[inline]
    pub(crate) fn push_reserved(&mut self, value: T) {
        debug_assert!(self.values.len() < self.values.capacity());
        self.values.push(value);
    }

    /// Append the values of an iterator of known length, charging the room first, so a
    /// hot loop has no check per value.
    #[inline]
    pub(crate) fn extend(&mut self, values: impl ExactSizeIterator<Item = T>) -> Result<()> {
        self.reserve(values.len())?;
        self.values.extend(values);
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        self.values.clear();
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.values.pop()
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }
}

impl<T> Deref for Growing<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.values
    }
}

impl<T> DerefMut for Growing<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.values
    }
}

/// A binary heap whose capacity is charged as it grows, like [`Growing`].
pub(crate) struct Heap<T: Ord> {
    heap: std::collections::BinaryHeap<T>,
    charge: Reservation,
}

impl<T: Ord> Heap<T> {
    pub(crate) fn new(ctl: &Control) -> Result<Self> {
        Ok(Self {
            heap: std::collections::BinaryHeap::new(),
            charge: Reservation::new(ctl, 0)?,
        })
    }

    #[inline]
    pub(crate) fn push(&mut self, value: T) -> Result<()> {
        if self.heap.len() == self.heap.capacity() {
            let old = self.heap.capacity();
            let capacity = old
                .checked_mul(2)
                .map(|c| c.max(16))
                .ok_or_else(|| bad("memory", "heap growth overflows"))?;
            let (was, next) = (bytes::<T>(old)?, bytes::<T>(capacity)?);
            self.charge.grow_to(sum(&[was, next])?)?;
            #[cfg(test)]
            if probe::fails("heap") {
                return Err(bad("memory", "injected allocation failure"));
            }
            self.heap
                .try_reserve_exact(capacity - self.heap.len())
                .map_err(|e| bad("memory", format!("heap allocation failed: {e}")))?;
            self.charge.set(bytes::<T>(self.heap.capacity())?)?;
        }
        self.heap.push(value);
        Ok(())
    }

    #[inline]
    pub(crate) fn pop(&mut self) -> Option<T> {
        self.heap.pop()
    }

    pub(crate) fn clear(&mut self) {
        self.heap.clear();
    }
}

#[cfg(test)]
pub(crate) mod probe {
    use std::cell::RefCell;
    use std::rc::Rc;
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Point {
        Allocation(&'static str, usize),
        Physical(&'static str, u64, bool),
        Work(&'static str),
        Waiting,
        Initializing,
        Publishing,
    }
    type Hook = Rc<dyn Fn(Point)>;
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static FAIL: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    }
    pub(crate) struct Scope(Option<Hook>, Option<&'static str>);
    pub(crate) fn install(f: impl Fn(Point) + 'static, fail: Option<&'static str>) -> Scope {
        Scope(
            HOOK.with(|h| h.replace(Some(Rc::new(f)))),
            FAIL.with(|v| v.replace(fail)),
        )
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = self.0.take());
            FAIL.with(|v| *v.borrow_mut() = self.1.take());
        }
    }
    pub(crate) fn fails(name: &str) -> bool {
        FAIL.with(|v| *v.borrow() == Some(name))
    }
    pub(crate) fn at(p: Point) {
        let h = HOOK.with(|h| h.borrow().clone());
        if let Some(h) = h {
            h(p);
        }
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn replacement_charges_old_and_new_and_preserves_values() {
        let ctl = Control::new(Default::default(), None, 96);
        let mut b = Buffer::new("test", 4, &ctl).unwrap();
        b.extend(&[1u64, 1, 2, 2]).unwrap();
        b.reserve("test", 5, &ctl).unwrap();
        assert_eq!(ctl.peak(), 96);
        assert_eq!(ctl.remaining(), 32);
        b.dedup(&ctl).unwrap();
        assert_eq!(&*b, &[1, 2]);
        assert_eq!(b.capacity(), 8);
        drop(b);
        assert_eq!(ctl.remaining(), 96);
        let short = Control::new(Default::default(), None, 95);
        let mut b = Buffer::<u64>::new("test", 4, &short).unwrap();
        b.push(7).unwrap();
        assert!(matches!(
            b.reserve("test", 5, &short),
            Err(crate::Error::BudgetExceeded(_))
        ));
        assert_eq!(&*b, &[7]);
        assert_eq!(b.capacity(), 4);
        drop(b);
        assert_eq!(short.remaining(), 95);
    }
    #[test]
    fn growth_failure_cancel_unwind_and_identity_release() {
        for mode in 0..3 {
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ctl = Control::new(cancel.clone(), None, 1024);
            let mut b = Buffer::new("old", 4, &ctl).unwrap();
            b.extend(&[1u64, 2, 3, 4]).unwrap();
            let scope = probe::install(
                move |point| {
                    if let probe::Point::Work("buffer-move") = point {
                        if mode == 1 {
                            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        if mode == 2 {
                            panic!("owned migration unwind");
                        }
                    }
                },
                (mode == 0).then_some("next"),
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                b.reserve("next", 5, &ctl)
            }));
            drop(scope);
            match mode {
                0 => assert!(result.unwrap().is_err()),
                1 => assert!(matches!(result.unwrap(), Err(crate::Error::Cancelled))),
                _ => assert!(result.is_err()),
            }
            assert_eq!(ctl.remaining(), 1024 - 32);
            drop(b);
            assert_eq!(ctl.remaining(), 1024);
        }
        let a = Control::unlimited();
        let b = Control::unlimited();
        let mut owned = Buffer::<u64>::new("owned", 0, &a).unwrap();
        assert!(owned.reserve("foreign", 1, &b).is_err());
        assert!(bytes::<(u64, u32)>(usize::MAX).is_err());
        assert!(sum(&[u64::MAX, 1]).is_err());
    }
}
