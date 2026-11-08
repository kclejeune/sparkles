//! Checked ownership of estimated memory, without a scheduler or allocator.
//!
//! A backend binds parents through a [`MemoryDomain`]. Consumers can reserve bytes
//! and transfer owned leases, but cannot release somebody else's contribution.
//! Reservation credit bounds an estimate of engine allocations, not process RSS.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

/// A cooperative computation stop, separate from byte admission or authority.
/// A retained partial result may finalize under its original byte parent while
/// its original work control remains stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkStopped {
    Cancelled,
    Timeout,
}

impl fmt::Display for WorkStopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "computation was cancelled",
            Self::Timeout => "computation deadline expired",
        })
    }
}
impl std::error::Error for WorkStopped {}

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
    ) -> ParentHandle {
        // Compatibility constructor for existing trusted backends. It does not
        // certify admission of their separately captured allocations.
        self.parent_with_header(
            reserve,
            move |_: &ParentHandle, bytes| release(bytes),
            (),
            u64::MAX,
        )
        .expect("the representable fixed parent layout fits u64")
    }

    /// Bind a paid backend after admitting both its actual allocation layouts.
    /// The caller supplies its genuine header owner; it is stored inline after
    /// both callbacks. Release borrows the live handle for safe reentrancy.
    /// This does not qualify allocations hidden in unrelated callback captures.
    pub fn parent_with_header<R, G, H>(
        &self,
        reserve: R,
        release: G,
        header: H,
        admitted: u64,
    ) -> Result<ParentHandle, ParentRefused>
    where
        R: Fn(u64) -> Result<(), ParentRefused> + Send + Sync + 'static,
        G: Fn(&ParentHandle, u64) + Send + Sync + 'static,
        H: Send + Sync + 'static,
    {
        // Couple stack-owned captures before any fallible check. On refusal
        // their payloads retire before the real header owner.
        let backend = Backend {
            reserve,
            release,
            _header: header,
        };
        let arc = arc_layout::<ParentState>()?;
        let boxed = u64::try_from(std::alloc::Layout::new::<Backend<R, G, H>>().size())
            .map_err(|_| ParentRefused::Overflow)?;
        let required = arc.checked_add(boxed).ok_or(ParentRefused::Overflow)?;
        if required > admitted {
            return Err(ParentRefused::InsufficientCredit {
                reserved: admitted,
                requested: required,
            });
        }
        let backend = BackendOwner(Some(Box::new(backend)));
        Ok(ParentHandle {
            inner: Shared::new(ParentState {
                domain: self.id(),
                backend,
            }),
        })
    }
}

fn arc_layout<T>() -> Result<u64, ParentRefused> {
    let counters = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>();
    let (layout, _) = counters
        .extend(std::alloc::Layout::new::<T>())
        .map_err(|_| ParentRefused::Overflow)?;
    u64::try_from(layout.pad_to_align().size()).map_err(|_| ParentRefused::Overflow)
}

// This sized owner has no Weak/raw/Arc conversion. Every strong Drop consumes
// its Arc through into_inner. The sole winner receives T only after the Arc
// allocation was freed; losers receive nothing. All clones use this same path.
struct Shared<T>(Option<Arc<T>>);
impl<T> Shared<T> {
    fn new(value: T) -> Self {
        Self(Some(Arc::new(value)))
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(
            self.0.as_ref().expect("live owner"),
            other.0.as_ref().expect("live owner"),
        )
    }
}
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_deref().expect("live owner")
    }
}
impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take().and_then(Arc::into_inner) {
            drop(value);
        }
    }
}

trait BackendOps: Send + Sync {
    fn reserve(&self, bytes: u64) -> Result<(), ParentRefused>;
    fn release(&self, parent: &ParentHandle, bytes: u64);
    fn retire(self: Box<Self>);
}
struct Backend<R, G, H> {
    reserve: R,
    release: G,
    _header: H,
}
// The callee's moved-out Box parameter is freed before its returned T can be
// destroyed by the caller. Stable safe Rust, without Box::into_inner or unsafe.
fn unbox<T>(boxed: Box<T>) -> T {
    *boxed
}
impl<R, G, H> BackendOps for Backend<R, G, H>
where
    R: Fn(u64) -> Result<(), ParentRefused> + Send + Sync,
    G: Fn(&ParentHandle, u64) + Send + Sync,
    H: Send + Sync,
{
    fn reserve(&self, bytes: u64) -> Result<(), ParentRefused> {
        (self.reserve)(bytes)
    }
    fn release(&self, parent: &ParentHandle, bytes: u64) {
        (self.release)(parent, bytes);
    }
    fn retire(self: Box<Self>) {
        drop(unbox(self));
    }
}
struct BackendOwner(Option<Box<dyn BackendOps>>);
impl BackendOwner {
    fn get(&self) -> &dyn BackendOps {
        self.0.as_deref().expect("live backend")
    }
}
impl Drop for BackendOwner {
    fn drop(&mut self) {
        if let Some(backend) = self.0.take() {
            backend.retire();
        }
    }
}
struct ParentState {
    domain: DomainId,
    backend: BackendOwner,
}

/// An opaque shallow byte-admission handle with no raw Arc or Weak escape.
/// It grants no authority or independent allowance. Construct one per operation
/// and share that exact handle; equal domains alone do not permit lease merging.
///
/// ```compile_fail
/// use sparkles_budget::MemoryDomain;
/// let parent = MemoryDomain::new().parent(|_| Ok(()), |_| {});
/// let weak = std::sync::Arc::downgrade(&parent);
/// ```
#[derive(Clone)]
pub struct ParentHandle {
    inner: Shared<ParentState>,
}
impl ParentHandle {
    pub fn domain(&self) -> DomainId {
        self.inner.domain.clone()
    }
    pub fn same_parent(&self, other: &Self) -> bool {
        self.inner.same(&other.inner)
    }
    pub fn reserve(&self, bytes: u64) -> Result<MemoryLease, ParentRefused> {
        if bytes != 0 {
            self.inner.backend.get().reserve(bytes)?;
        }
        Ok(MemoryLease {
            parent: self.clone(),
            bytes,
        })
    }
    fn release(&self, bytes: u64) {
        self.inner.backend.get().release(self, bytes);
    }
}
impl fmt::Debug for ParentHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParentHandle")
            .field("domain", &self.domain())
            .finish_non_exhaustive()
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
    parent: ParentHandle,
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
        self.parent.domain()
    }

    /// Add backed credit before publishing the larger owned amount.
    pub fn try_grow(&mut self, bytes: u64) -> Result<(), ParentRefused> {
        let next = self
            .bytes
            .checked_add(bytes)
            .ok_or(ParentRefused::Overflow)?;
        if bytes != 0 {
            self.parent.inner.backend.get().reserve(bytes)?;
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
            self.parent.release(bytes);
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
        if !self.parent.same_parent(&other.parent) {
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
    pub fn try_move_to(&mut self, parent: ParentHandle) -> Result<(), ParentRefused> {
        if self.domain() != parent.domain() {
            return Err(ParentRefused::ForeignDomain);
        }
        if self.parent.same_parent(&parent) {
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
            self.parent.release(self.bytes);
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
    pub fn new(parent: ParentHandle) -> Self {
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

    pub fn try_move_to(&mut self, parent: ParentHandle) -> Result<(), ParentRefused> {
        self.lease.try_move_to(parent)
    }

    pub fn into_lease(self) -> MemoryLease {
        self.lease
    }
}
