//! Checked ownership of estimated memory, without a scheduler or allocator.
//!
//! A backend binds parents through a [`MemoryDomain`]. Consumers can reserve bytes
//! and transfer owned leases, but cannot release somebody else's contribution.
//! Reservation credit bounds an estimate of engine allocations, not process RSS.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

/// The identity of an accounting domain. It neither constructs parents nor retains
/// a dataset, query or payload. Clones refer to the same identity allocation.
#[derive(Clone)]
pub struct DomainId(Arc<()>);

impl PartialEq for DomainId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for DomainId {}
impl Hash for DomainId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}
impl fmt::Debug for DomainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DomainId")
            .field(&Arc::as_ptr(&self.0))
            .finish()
    }
}

/// Which bound refused a reservation. Reserved credit is distinct from live bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Local,
    Work,
    Retained,
}

/// A refused reservation or ownership operation. Every refusal leaves ownership
/// unchanged, including overflow and incompatible transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentRefused {
    Limit {
        limit: u64,
        requested: u64,
        scope: Scope,
    },
    Overflow,
    ForeignDomain,
    DifferentParent,
    InsufficientCredit {
        reserved: u64,
        requested: u64,
    },
}

impl fmt::Display for ParentRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit {
                limit,
                requested,
                scope,
            } => {
                write!(
                    f,
                    "{scope:?} memory limit {limit} refused {requested} bytes"
                )
            }
            Self::Overflow => f.write_str("memory reservation arithmetic overflow"),
            Self::ForeignDomain => f.write_str("memory reservation belongs to another domain"),
            Self::DifferentParent => f.write_str("memory reservations have different parents"),
            Self::InsufficientCredit {
                reserved,
                requested,
            } => {
                write!(
                    f,
                    "cannot transfer or return {requested} of {reserved} reserved bytes"
                )
            }
        }
    }
}
impl std::error::Error for ParentRefused {}

/// The capability to bind reservation backends to one domain.
///
/// This capability is not Clone and cannot be recovered from a [`DomainId`]. Keep
/// it in the accounting root, exposing only parents and identity to consumers.
pub struct MemoryDomain(DomainId);

impl Default for MemoryDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryDomain {
    /// The designated identity for explicitly unpooled ownership. This domain has
    /// no aggregate process allowance and cannot bind a bounded pool's identity.
    pub fn unpooled() -> &'static Self {
        static UNPOOLED: OnceLock<MemoryDomain> = OnceLock::new();
        UNPOOLED.get_or_init(Self::new)
    }

    pub fn new() -> Self {
        Self(DomainId(Arc::new(())))
    }

    pub fn id(&self) -> DomainId {
        self.0.clone()
    }

    /// Bind a backend to this domain. Successful `reserve` must back exactly the
    /// requested additional bytes; refusal must change nothing. `release` returns
    /// exactly those bytes and must not panic. Both callbacks must finish without
    /// retaining locks needed by reentrant consumers, including release wake-ups.
    ///
    /// The callbacks are private after construction. Consumers cannot manufacture
    /// a release or derive a new backend from a parent's public identity.
    pub fn parent(
        &self,
        reserve: impl Fn(u64) -> Result<(), ParentRefused> + Send + Sync + 'static,
        release: impl Fn(u64) + Send + Sync + 'static,
    ) -> Arc<dyn MemoryParent> {
        Arc::new(Parent {
            domain: self.id(),
            reserve: Box::new(reserve),
            release: Box::new(release),
        })
    }
}

mod private {
    pub trait Sealed {}
}

/// An object-safe reservation source. Implementations are bound through
/// [`MemoryDomain::parent`]; there is no public unowned release operation.
///
/// ```compile_fail
/// use sparkles_budget::MemoryDomain;
/// let parent = MemoryDomain::new().parent(|_| Ok(()), |_| {});
/// parent.release(1); // Only an owned lease can return credit.
/// ```
pub trait MemoryParent: private::Sealed + Send + Sync {
    fn domain(&self) -> DomainId;
    fn reserve(self: Arc<Self>, bytes: u64) -> Result<MemoryLease, ParentRefused>;
}

struct Parent {
    domain: DomainId,
    reserve: Box<dyn Fn(u64) -> Result<(), ParentRefused> + Send + Sync>,
    release: Box<dyn Fn(u64) + Send + Sync>,
}

impl private::Sealed for Parent {}
impl MemoryParent for Parent {
    fn domain(&self) -> DomainId {
        self.domain.clone()
    }

    fn reserve(self: Arc<Self>, bytes: u64) -> Result<MemoryLease, ParentRefused> {
        if bytes != 0 {
            (self.reserve)(bytes)?;
        }
        Ok(MemoryLease {
            parent: self,
            bytes,
        })
    }
}

/// Nonduplicable reservation credit. Drop returns exactly the remaining bytes.
/// The payload using these bytes must be freed before dropping its lease; use
/// [`MemoryLease::attach`] or declare the payload before the lease in its owner.
///
/// ```compile_fail
/// use sparkles_budget::MemoryDomain;
/// let parent = MemoryDomain::new().parent(|_| Ok(()), |_| {});
/// let lease = parent.reserve(1).unwrap();
/// let duplicate = lease.clone();
/// ```
pub struct MemoryLease {
    parent: Arc<Parent>,
    bytes: u64,
}

impl fmt::Debug for MemoryLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryLease")
            .field("domain", &self.domain())
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl MemoryLease {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn domain(&self) -> DomainId {
        self.parent.domain.clone()
    }

    /// Add backed credit before publishing the larger owned amount.
    pub fn try_grow(&mut self, bytes: u64) -> Result<(), ParentRefused> {
        let next = self
            .bytes
            .checked_add(bytes)
            .ok_or(ParentRefused::Overflow)?;
        if bytes != 0 {
            (self.parent.reserve)(bytes)?;
            self.bytes = next;
        }
        Ok(())
    }

    /// Return credit only after the allocation it covered has been freed.
    pub fn shrink(&mut self, bytes: u64) -> Result<(), ParentRefused> {
        let next = self
            .bytes
            .checked_sub(bytes)
            .ok_or(ParentRefused::InsufficientCredit {
                reserved: self.bytes,
                requested: bytes,
            })?;
        self.bytes = next;
        if bytes != 0 {
            (self.parent.release)(bytes);
        }
        Ok(())
    }

    pub fn split(&mut self, bytes: u64) -> Result<Self, ParentRefused> {
        let remaining = self
            .bytes
            .checked_sub(bytes)
            .ok_or(ParentRefused::InsufficientCredit {
                reserved: self.bytes,
                requested: bytes,
            })?;
        self.bytes = remaining;
        Ok(Self {
            parent: self.parent.clone(),
            bytes,
        })
    }

    /// Move all of `other`'s credit into this lease without a parent operation.
    /// Domain equality alone cannot authorize merging different branch backends.
    pub fn try_merge(&mut self, other: &mut Self) -> Result<(), ParentRefused> {
        if self.domain() != other.domain() {
            return Err(ParentRefused::ForeignDomain);
        }
        if !Arc::ptr_eq(&self.parent, &other.parent) {
            return Err(ParentRefused::DifferentParent);
        }
        let next = self
            .bytes
            .checked_add(other.bytes)
            .ok_or(ParentRefused::Overflow)?;
        self.bytes = next;
        other.bytes = 0;
        Ok(())
    }

    /// Prepare destination credit before retiring the original contribution. This
    /// may conservatively refuse a move that cannot temporarily back both owners.
    pub fn try_move_to(&mut self, parent: Arc<dyn MemoryParent>) -> Result<(), ParentRefused> {
        if self.domain() != parent.domain() {
            return Err(ParentRefused::ForeignDomain);
        }
        let current: Arc<dyn MemoryParent> = self.parent.clone();
        if Arc::ptr_eq(&current, &parent) {
            return Ok(());
        }
        let replacement = parent.reserve(self.bytes)?;
        let retired = std::mem::replace(self, replacement);
        drop(retired);
        Ok(())
    }

    /// Attach a payload constructed after this reservation was admitted. Private
    /// field order guarantees destruction of the payload before credit return.
    pub fn attach<T>(self, value: T) -> Owned<T> {
        Owned { value, lease: self }
    }
}

impl Drop for MemoryLease {
    fn drop(&mut self) {
        if self.bytes != 0 {
            (self.parent.release)(self.bytes);
        }
    }
}

/// A read-only payload and its reservation. Share it by cloning an Arc, rather
/// than cloning the allocation or duplicating its lease.
pub struct Owned<T> {
    value: T,
    lease: MemoryLease,
}

impl<T> Owned<T> {
    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn reserved(&self) -> u64 {
        self.lease.bytes()
    }

    pub fn domain(&self) -> DomainId {
        self.lease.domain()
    }
}

/// The existing live-account reservation step and release slack. These are
/// separate from query-pool grant granules and reasoner-local metering credit.
pub const LIVE_STEP: u64 = 64 << 10;
pub const LIVE_SLACK: u64 = 1 << 20;

fn rounded(bytes: u64) -> u64 {
    let remainder = bytes % LIVE_STEP;
    if remainder == 0 {
        bytes
    } else {
        bytes.checked_add(LIVE_STEP - remainder).unwrap_or(bytes)
    }
}

/// Single-owner local accounting over a lease. Updates within existing credit
/// use no parent operation or synchronization. Callers must remove live bytes
/// only after freeing their estimated allocation, including retained capacity.
pub struct LiveCredit {
    live: u64,
    lease: MemoryLease,
}

impl LiveCredit {
    pub fn new(parent: Arc<dyn MemoryParent>) -> Self {
        Self {
            live: 0,
            lease: parent.reserve(0).expect("zero reservation cannot refuse"),
        }
    }

    pub fn live(&self) -> u64 {
        self.live
    }

    pub fn reserved(&self) -> u64 {
        self.lease.bytes()
    }

    pub fn domain(&self) -> DomainId {
        self.lease.domain()
    }

    pub fn try_add(&mut self, bytes: u64) -> Result<(), ParentRefused> {
        let next = self
            .live
            .checked_add(bytes)
            .ok_or(ParentRefused::Overflow)?;
        if next > self.reserved() {
            let target = rounded(next);
            match self.lease.try_grow(target - self.reserved()) {
                Ok(()) => (),
                Err(_) if target > next => self.lease.try_grow(next - self.reserved())?,
                Err(error) => return Err(error),
            }
        }
        self.live = next;
        Ok(())
    }

    pub fn remove(&mut self, bytes: u64) -> Result<(), ParentRefused> {
        let next = self
            .live
            .checked_sub(bytes)
            .ok_or(ParentRefused::InsufficientCredit {
                reserved: self.live,
                requested: bytes,
            })?;
        self.live = next;
        if self.reserved() - next > LIVE_SLACK {
            self.lease.shrink(self.reserved() - rounded(next))?;
        }
        Ok(())
    }

    pub fn split_live(&mut self, bytes: u64) -> Result<Self, ParentRefused> {
        let remaining = self
            .live
            .checked_sub(bytes)
            .ok_or(ParentRefused::InsufficientCredit {
                reserved: self.live,
                requested: bytes,
            })?;
        let lease = self.lease.split(bytes)?;
        self.live = remaining;
        Ok(Self { live: bytes, lease })
    }

    pub fn try_merge(&mut self, other: &mut Self) -> Result<(), ParentRefused> {
        let live = self
            .live
            .checked_add(other.live)
            .ok_or(ParentRefused::Overflow)?;
        self.lease.try_merge(&mut other.lease)?;
        self.live = live;
        other.live = 0;
        Ok(())
    }

    pub fn try_move_to(&mut self, parent: Arc<dyn MemoryParent>) -> Result<(), ParentRefused> {
        self.lease.try_move_to(parent)
    }

    pub fn into_lease(self) -> MemoryLease {
        self.lease
    }
}
