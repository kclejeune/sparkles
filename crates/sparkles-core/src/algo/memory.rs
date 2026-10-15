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
        Ok(Self { values, charge })
    }
    pub(crate) fn push(&mut self, value: T) -> Result<()> {
        if self.values.len() == self.values.capacity() {
            return Err(bad("memory", "fixed buffer capacity exceeded"));
        }
        self.values.push(value);
        Ok(())
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
    pub(crate) fn tick(&mut self, phase: &'static str) -> Result<()> {
        #[cfg(test)]
        probe::at(probe::Point::Work(phase));
        let _ = phase;
        self.left -= 1;
        if self.left == 0 {
            self.ctl.check()?;
            self.left = CHECK_EVERY;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod probe {
    use std::cell::RefCell;
    use std::rc::Rc;
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Point {
        Allocation(&'static str, usize),
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
